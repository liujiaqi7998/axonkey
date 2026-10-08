use super::{handles::Apartment, microphone::Sink};
use crate::{
    audio::session::{Microphone, Session},
    cancel::Cancel,
    lock, proto,
    rpc::Hub,
    Error, Result,
};
use std::{
    collections::VecDeque,
    future::IntoFuture,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use windows::{
    core::{Interface, RuntimeType, GUID},
    Devices::{
        Bluetooth::{
            BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice,
            GenericAttributeProfile::*,
        },
        Enumeration::DeviceInformation,
    },
    Foundation::{IClosable, TypedEventHandler},
    Storage::Streams::{DataReader, DataWriter},
};
use windows_future::IAsyncOperation;

const SERVICE: GUID = GUID::from_u128(0xab5e0001_5a21_4f05_bc7d_af01f617b664);
const TX: GUID = GUID::from_u128(0xab5e0002_5a21_4f05_bc7d_af01f617b664);
const AUDIO: GUID = GUID::from_u128(0xab5e0003_5a21_4f05_bc7d_af01f617b664);
const CONTROL: GUID = GUID::from_u128(0xab5e0004_5a21_4f05_bc7d_af01f617b664);
const BATTERY: GUID = GUID::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb);
const BATTERY_LEVEL: GUID = GUID::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb);

fn close_object<T: Interface>(object: T) {
    if let Ok(closable) = object.cast::<IClosable>() {
        if let Err(e) = closable.Close() {
            log::warn!("WinRT Close failed: {e}");
        }
    }
}
struct Close<T: Interface>(Option<T>);
impl<T: Interface> Close<T> {
    fn new(value: T) -> Self {
        Self(Some(value))
    }
}
impl<T: Interface> std::ops::Deref for Close<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_ref().expect("live WinRT guard")
    }
}
impl<T: Interface> Drop for Close<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            close_object(value);
        }
    }
}

async fn wait_op<T: RuntimeType + 'static>(
    op: IAsyncOperation<T>,
    cancel: &Cancel,
    timeout: Duration,
    late: impl FnOnce(T),
) -> Result<T> {
    let pending = op.clone().into_future();
    tokio::pin!(pending);
    let error = tokio::select! {
        biased;
        value = &mut pending => { let _ = op.Close(); return value.map_err(Error::from); },
        _ = cancel.cancelled() => Error::new(995, "GATT operation cancelled"),
        _ = tokio::time::sleep(timeout) => Error::new(1460, "GATT operation timed out"),
    };
    let _ = op.Cancel();
    // Keep the SAME Future/Completed handler and underlying COM operation alive until terminal.
    // A driver that never completes cancellation must not cause a replacement operation to leak behind it.
    log::info!("GATT cancellation requested; waiting for completion");
    if let Ok(value) = pending.await {
        late(value);
    }
    let _ = op.Close();
    Err(error)
}
async fn value<T: RuntimeType + 'static>(
    op: IAsyncOperation<T>,
    cancel: &Cancel,
    seconds: u64,
) -> Result<T> {
    wait_op(op, cancel, Duration::from_secs(seconds), drop).await
}
async fn object<T: RuntimeType + Interface + 'static>(
    op: IAsyncOperation<T>,
    cancel: &Cancel,
) -> Result<Close<T>> {
    Ok(Close::new(
        wait_op(op, cancel, Duration::from_secs(10), close_object).await?,
    ))
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub status: proto::VoiceStatus,
    pub battery: Option<u32>,
    pub description: String,
}
struct Packet {
    audio: bool,
    bytes: Vec<u8>,
}
#[derive(Default)]
struct Packets {
    items: VecDeque<Packet>,
    bytes: usize,
    error: Option<Error>,
    closed: bool,
}
#[derive(Default)]
struct Queue {
    packets: Mutex<Packets>,
    wake: Notify,
}
impl Queue {
    fn push(&self, audio: bool, bytes: Vec<u8>) {
        let mut state = lock(&self.packets);
        if state.closed || state.error.is_some() || bytes.is_empty() {
            return;
        }
        if state.items.len() >= 1024 || bytes.len() > 65536_usize.saturating_sub(state.bytes) {
            state.error = Some(Error::new(111, "RC003 voice event queue overflow"));
        } else {
            state.bytes += bytes.len();
            state.items.push_back(Packet { audio, bytes });
        }
        self.wake.notify_one();
    }
    fn fail(&self, error: Error) {
        let mut state = lock(&self.packets);
        if !state.closed {
            state.error = Some(error);
            self.wake.notify_one();
        }
    }
    fn pop(&self) -> Result<Option<Packet>> {
        let mut state = lock(&self.packets);
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        let packet = state.items.pop_front();
        if let Some(p) = &packet {
            state.bytes -= p.bytes.len();
        }
        Ok(packet)
    }
    fn close(&self) {
        let mut state = lock(&self.packets);
        state.closed = true;
        state.items.clear();
        state.bytes = 0;
        self.wake.notify_waiters();
    }
}
struct QueueGuard(Arc<Queue>);
impl Drop for QueueGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}
struct Subscription {
    characteristic: GattCharacteristic,
    token: i64,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Err(e) = self.characteristic.RemoveValueChanged(self.token) {
            log::warn!("GATT unsubscribe failed: {e}");
        }
    }
}
struct Connection {
    subscriptions: Vec<Subscription>,
    transmit: GattCharacteristic,
    audio: GattCharacteristic,
    control: GattCharacteristic,
    _session: Close<GattSession>,
    _service: Close<GattDeviceService>,
    device: Close<BluetoothLEDevice>,
}
fn matches_service(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.contains("vid&012717_pid&32b8") || id.contains("vid_2717&pid_32b8")
}
/// Best-effort fallback for GetDevices when a voice connection has no metadata.
/// Called on the coordinator's MTA thread, and cancelled by service shutdown.
pub async fn metadata(cancel: &Cancel) -> Result<(Option<u32>, String)> {
    let selector = GattDeviceService::GetDeviceSelectorFromUuid(SERVICE)?;
    let services = value(
        DeviceInformation::FindAllAsyncAqsFilter(&selector)?,
        cancel,
        2,
    )
    .await?;
    for index in 0..services.Size()? {
        let id = services.GetAt(index)?.Id()?;
        if !matches_service(&id.to_string()) {
            continue;
        }
        let result = async {
            let service = object(GattDeviceService::FromIdAsync(&id)?, cancel).await?;
            let session = Close::new(service.Session()?);
            let device = object(
                BluetoothLEDevice::FromIdAsync(&session.DeviceId()?.Id()?)?,
                cancel,
            )
            .await?;
            let name = device
                .Name()
                .or_else(|_| device.DeviceInformation()?.Name())
                .map(|n| n.to_string())
                .unwrap_or_default();
            Ok::<_, Error>((battery(&device, cancel).await, name))
        }
        .await;
        if let Ok(result) = result {
            return Ok(result);
        }
        if cancel.is_requested() {
            break;
        }
    }
    Err(Error::new(1168, "RC003 metadata unavailable"))
}
async fn characteristic(
    service: &GattDeviceService,
    id: GUID,
    cancel: &Cancel,
) -> Result<GattCharacteristic> {
    let result = value(service.GetCharacteristicsForUuidAsync(id)?, cancel, 10).await?;
    if result.Status()? != GattCommunicationStatus::Success {
        return Err(Error::new(31, "RC003 characteristic discovery failed"));
    }
    Ok(result.Characteristics()?.GetAt(0)?)
}
async fn connect(instance: &str, cancel: &Cancel) -> Result<Connection> {
    if !super::device_setup::is_rc003(instance) {
        return Err(Error::new(87, "Not an RC003 instance"));
    }
    let selector = GattDeviceService::GetDeviceSelectorFromUuid(SERVICE)?;
    let services = value(
        DeviceInformation::FindAllAsyncAqsFilter(&selector)?,
        cancel,
        10,
    )
    .await?;
    let mut last = Error::new(1168, "RC003 ATVV service not found");
    for index in 0..services.Size()? {
        if cancel.is_requested() {
            return Err(Error::new(995, "GATT discovery cancelled"));
        }
        let id = services.GetAt(index)?.Id()?;
        if !matches_service(&id.to_string()) {
            continue;
        }
        let attempt = async {
            let service = object(GattDeviceService::FromIdAsync(&id)?, cancel).await?;
            let session = Close::new(service.Session()?);
            let device = object(
                BluetoothLEDevice::FromIdAsync(&session.DeviceId()?.Id()?)?,
                cancel,
            )
            .await?;
            let transmit = characteristic(&service, TX, cancel).await?;
            let audio = characteristic(&service, AUDIO, cancel).await?;
            let control = characteristic(&service, CONTROL, cancel).await?;
            Ok(Connection {
                subscriptions: Vec::new(),
                transmit,
                audio,
                control,
                _session: session,
                _service: service,
                device,
            })
        }
        .await;
        match attempt {
            Ok(connection) => return Ok(connection),
            Err(e) => last = e,
        }
    }
    Err(last)
}
async fn subscribe(
    characteristic: &GattCharacteristic,
    audio: bool,
    queue: &Arc<Queue>,
    cancel: &Cancel,
) -> Result<Subscription> {
    let weak = Arc::downgrade(queue);
    let handler =
        TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(move |_, args| {
            // Do not let a Rust panic unwind into WinRT. The closure owns no connection or service.
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let Some(queue) = weak.upgrade() else {
                        return Ok(());
                    };
                    if lock(&queue.packets).closed {
                        return Ok(());
                    }
                    let Some(args) = args.as_ref() else {
                        return Ok(());
                    };
                    let reader = Close::new(DataReader::FromBuffer(&args.CharacteristicValue()?)?);
                    let count = reader.UnconsumedBufferLength()? as usize;
                    if count > 65536 {
                        return Err(Error::new(111, "Oversized GATT notification"));
                    }
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(count)
                        .map_err(|_| Error::new(8, "GATT allocation failed"))?;
                    bytes.resize(count, 0);
                    reader.ReadBytes(&mut bytes)?;
                    queue.push(audio, bytes);
                    Ok(())
                }));
            let error = match result {
                Ok(Err(e)) => Some(e),
                Err(_) => Some(Error::new(574, "GATT callback panicked")),
                _ => None,
            };
            if let (Some(queue), Some(error)) = (weak.upgrade(), error) {
                queue.fail(error);
            }
            Ok(())
        });
    let subscription = Subscription {
        characteristic: characteristic.clone(),
        token: characteristic.ValueChanged(&handler)?,
    };
    let properties = characteristic.CharacteristicProperties()?;
    let mode = if properties & GattCharacteristicProperties::Notify
        != GattCharacteristicProperties::None
    {
        GattClientCharacteristicConfigurationDescriptorValue::Notify
    } else if properties & GattCharacteristicProperties::Indicate
        != GattCharacteristicProperties::None
    {
        GattClientCharacteristicConfigurationDescriptorValue::Indicate
    } else {
        return Err(Error::new(50, "GATT characteristic has no notifications"));
    };
    if value(
        characteristic.WriteClientCharacteristicConfigurationDescriptorAsync(mode)?,
        cancel,
        5,
    )
    .await?
        != GattCommunicationStatus::Success
    {
        return Err(Error::new(31, "GATT notification setup failed"));
    }
    Ok(subscription)
}
async fn send(characteristic: &GattCharacteristic, bytes: &[u8], cancel: &Cancel) -> Result<()> {
    let writer = Close::new(DataWriter::new()?);
    writer.WriteBytes(bytes)?;
    let mode = if characteristic.CharacteristicProperties()?
        & GattCharacteristicProperties::WriteWithoutResponse
        != GattCharacteristicProperties::None
    {
        GattWriteOption::WriteWithoutResponse
    } else {
        GattWriteOption::WriteWithResponse
    };
    if value(
        characteristic.WriteValueWithOptionAsync(&writer.DetachBuffer()?, mode)?,
        cancel,
        2,
    )
    .await?
        != GattCommunicationStatus::Success
    {
        return Err(Error::new(31, "GATT command write failed"));
    }
    Ok(())
}
fn close_service_result(result: GattDeviceServicesResult) {
    if let Ok(services) = result.Services() {
        if let Ok(count) = services.Size() {
            for i in 0..count {
                if let Ok(service) = services.GetAt(i) {
                    close_object(service);
                }
            }
        }
    }
}
async fn battery(device: &BluetoothLEDevice, cancel: &Cancel) -> Option<u32> {
    for mode in [BluetoothCacheMode::Uncached, BluetoothCacheMode::Cached] {
        if cancel.is_requested() {
            break;
        }
        let attempt = async {
            let result = wait_op(
                device.GetGattServicesForUuidWithCacheModeAsync(BATTERY, mode)?,
                cancel,
                Duration::from_secs(2),
                close_service_result,
            )
            .await?;
            if result.Status()? != GattCommunicationStatus::Success {
                return Ok::<_, Error>(None);
            }
            let services = result.Services()?;
            // Own every returned service so early returns also explicitly close the unvisited services.
            let mut owned = Vec::new();
            for i in 0..services.Size()? {
                owned.push(Close::new(services.GetAt(i)?));
            }
            for service in &owned {
                let characteristics = value(
                    service.GetCharacteristicsForUuidWithCacheModeAsync(BATTERY_LEVEL, mode)?,
                    cancel,
                    2,
                )
                .await?;
                if characteristics.Status()? != GattCommunicationStatus::Success {
                    continue;
                }
                let collection = characteristics.Characteristics()?;
                if collection.Size()? == 0 {
                    continue;
                }
                let read = value(
                    collection.GetAt(0)?.ReadValueWithCacheModeAsync(mode)?,
                    cancel,
                    2,
                )
                .await?;
                if read.Status()? != GattCommunicationStatus::Success {
                    continue;
                }
                let reader = Close::new(DataReader::FromBuffer(&read.Value()?)?);
                if reader.UnconsumedBufferLength()? > 0 {
                    let level = reader.ReadByte()?;
                    if level <= 100 {
                        return Ok(Some(u32::from(level)));
                    }
                }
            }
            Ok(None)
        }
        .await;
        if let Ok(Some(level)) = attempt {
            return Some(level);
        }
    }
    None
}
pub struct Receiver {
    cancel: Arc<Cancel>,
    snapshot: Arc<Mutex<Snapshot>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Receiver {
    pub fn start(
        instance: String,
        gain: Arc<AtomicI32>,
        hub: Arc<Hub>,
        level: Arc<Mutex<proto::AudioLevel>>,
    ) -> Result<Self> {
        let cancel = Arc::new(Cancel::default());
        let snapshot = Arc::new(Mutex::new(Snapshot {
            status: proto::VoiceStatus {
                state: "connecting".into(),
                device_instance_id: instance.clone(),
                ..Default::default()
            },
            ..Default::default()
        }));
        let stop = cancel.clone();
        let state = snapshot.clone();
        let worker = thread::Builder::new()
            .name("axonkey-gatt".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                        let _apartment = Apartment::mta()?;
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()?;
                        runtime.block_on(run(&instance, &stop, &state, &gain, &hub, &level))
                    }));
                let error = match result {
                    Ok(Err(e)) if e.native != 995 => Some(e),
                    Err(_) => Some(Error::new(574, "GATT worker panicked")),
                    _ => None,
                };
                if let Some(error) = error {
                    let connected = lock(&state).status.connected;
                    hub.issue(
                        if connected {
                            "bluetooth_runtime_failed"
                        } else {
                            "bluetooth_initialization_failed"
                        },
                        &instance,
                        &error,
                    );
                }
                let mut snapshot = lock(&state);
                snapshot.status.state = "stopped".into();
                snapshot.status.connected = false;
                snapshot.status.active = false;
                snapshot.status.microphone_open = false;
                snapshot.battery = None;
                let status = snapshot.status.clone();
                drop(snapshot);
                hub.voice(&status);
            })?;
        Ok(Self {
            cancel,
            snapshot,
            worker: Some(worker),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.snapshot).clone()
    }
    pub fn finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }
    pub fn request_stop(&self) {
        self.cancel.request();
    }
    pub fn join(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.join();
    }
}
async fn run(
    instance: &str,
    cancel: &Arc<Cancel>,
    snapshot: &Mutex<Snapshot>,
    gain: &AtomicI32,
    hub: &Hub,
    level: &Mutex<proto::AudioLevel>,
) -> Result<()> {
    let mut connection = connect(instance, cancel).await?;
    let queue = Arc::new(Queue::default());
    let _close_queue = QueueGuard(queue.clone());
    let initial_battery = battery(&connection.device, cancel).await;
    {
        let mut state = lock(snapshot);
        state.status.state = "connected".into();
        state.status.connected = true;
        state.battery = initial_battery;
        state.description = connection
            .device
            .Name()
            .or_else(|_| connection.device.DeviceInformation()?.Name())
            .map(|n| n.to_string())
            .unwrap_or_default();
        hub.voice(&state.status);
    }
    let mut session = Session::new(Sink::driver(cancel.clone()), gain.load(Ordering::Acquire));
    let outcome = async {
        connection.subscriptions.push(subscribe(&connection.audio, true, &queue, cancel).await?);
        connection.subscriptions.push(subscribe(&connection.control, false, &queue, cancel).await?);
        send(&connection.transmit, &[0x0a,1,0,0,3,3], cancel).await?;
        let mut next_battery = Instant::now() + Duration::from_secs(30);
        loop {
            if cancel.is_requested() { break; }
            if connection.device.ConnectionStatus()? == BluetoothConnectionStatus::Disconnected { return Err(Error::new(1167, "RC003 voice channel disconnected")); }
            session.set_gain(gain.load(Ordering::Acquire));
            let packet = queue.pop()?;
            let has_packet = packet.is_some();
            let effects = match packet { Some(packet) if packet.audio => session.audio(&packet.bytes), Some(packet) => session.control(&packet.bytes), None => Default::default() };
            if let Some(command) = effects.command { send(&connection.transmit, &command, cancel).await?; }
            if let Some((peak, rms)) = effects.level { let current = proto::AudioLevel { peak, rms, timestamp_ms: crate::now_ms() }; *lock(level) = current; hub.level(&current); }
            if let Some((code, error)) = effects.issue { hub.issue(code, instance, &error); }
            {
                let mut state = lock(snapshot);
                state.status.active = session.active;
                state.status.microphone_open = session.microphone_open;
                state.status.protocol_version = u32::from(session.protocol_version);
                state.status.session_id = u32::from(session.session_id);
                if has_packet { hub.voice(&state.status); }
            }
            if Instant::now() >= next_battery {
                if let Some(value) = battery(&connection.device, cancel).await { lock(snapshot).battery = Some(value); }
                next_battery = Instant::now() + Duration::from_secs(30);
            }
            if lock(&queue.packets).items.is_empty() {
                tokio::select! { _ = cancel.cancelled() => break, _ = queue.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
            }
        }
        Ok(())
    }.await;
    queue.close();
    session.microphone.stop(false);
    if let Some(command) = session.close_command() {
        if let Err(e) = send(&connection.transmit, &command, &Cancel::default()).await {
            log::warn!("RC003 close command failed: {e}");
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_is_ordered_bounded_and_drops_late_callbacks() {
        let queue = Queue::default();
        queue.push(false, vec![8]);
        queue.push(true, vec![1, 2]);
        assert!(!queue.pop().unwrap().unwrap().audio);
        assert!(queue.pop().unwrap().unwrap().audio);
        assert_eq!(lock(&queue.packets).bytes, 0);
        queue.push(true, vec![0; 65536]);
        queue.push(false, vec![0]);
        assert!(queue.pop().is_err());
        queue.close();
        queue.push(true, vec![1]);
        assert_eq!(lock(&queue.packets).bytes, 0);
    }
    #[test]
    fn repeated_queue_lifetimes_have_no_callback_ownership_cycle() {
        for _ in 0..1000 {
            let queue = Arc::new(Queue::default());
            let weak = Arc::downgrade(&queue);
            let guard = QueueGuard(queue.clone());
            for _ in 0..1024 {
                queue.push(false, vec![8]);
            }
            assert!(queue.pop().is_ok());
            queue.push(false, vec![8]);
            queue.push(false, vec![8]);
            assert!(queue.pop().is_err());
            drop(guard);
            assert_eq!(lock(&queue.packets).bytes, 0);
            drop(queue);
            assert!(weak.upgrade().is_none());
        }
    }
    #[tokio::test]
    async fn winrt_timeout_and_cancel_drain_same_future_and_cleanup_late_result() {
        for cancel_now in [false, true] {
            let cancel = Cancel::default();
            let (release, wait) = std::sync::mpsc::channel();
            let operation = IAsyncOperation::<u32>::spawn(move || Ok(wait.recv().unwrap()));
            let completed = std::cell::Cell::new(0);
            let pending = wait_op(operation, &cancel, Duration::from_millis(5), |v| {
                completed.set(v)
            });
            if cancel_now {
                cancel.request();
            }
            let release = async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.send(42).unwrap();
            };
            let (result, ()) = tokio::join!(pending, release);
            assert_eq!(
                result.unwrap_err().native,
                if cancel_now { 995 } else { 1460 }
            );
            assert_eq!(completed.get(), 42); // spawn's Cancel is advisory; completion still owns a result.
        }
        let result = value(IAsyncOperation::<u32>::ready(Ok(7)), &Cancel::default(), 1)
            .await
            .unwrap();
        assert_eq!(result, 7);
        assert!(value(
            IAsyncOperation::<u32>::ready(Err(windows::core::Error::from_hresult(
                windows::core::HRESULT(0x80004005_u32 as i32)
            ))),
            &Cancel::default(),
            1
        )
        .await
        .is_err());
    }
}
