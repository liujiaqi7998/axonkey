use crate::{
    cancel::Cancel,
    config, lock, proto,
    rpc::{self, Hub},
    win::{
        device_setup, gatt,
        handles::{Apartment, Event},
        hid, scm,
    },
    Error, Result,
};
use prost::Message;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use windows::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::{
        Services::{SERVICE_RUNNING, SERVICE_STOP_PENDING},
        Threading::WaitForSingleObject,
    },
};

struct Shared {
    enabled: AtomicBool,
    gain: Arc<AtomicI32>,
    voice: Mutex<proto::VoiceStatus>,
    level: Arc<Mutex<proto::AudioLevel>>,
}
enum Action {
    Enable(bool),
    Autostart(bool),
    Gain(i32),
    Devices,
}
struct Command {
    id: u64,
    action: Action,
    reply: oneshot::Sender<proto::Response>,
}
struct Control {
    commands: mpsc::SyncSender<Command>,
    shared: Arc<Shared>,
    stop: Arc<Cancel>,
    wake: Arc<Event>,
}
impl rpc::Handler for Control {
    async fn handle(&self, request: proto::Request) -> proto::Response {
        let id = request.request_id;
        let action = match request.method.as_str() {
            "GetServiceInfo" => {
                return rpc::response(
                    id,
                    &proto::ServiceInfo {
                        name: "AxonkeyService".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                        protocol_version: rpc::PROTOCOL.into(),
                        pipe_name: rpc::PIPE_NAME.into(),
                        audio_gain_db: self.shared.gain.load(Ordering::Acquire),
                    },
                )
            }
            "GetServiceStatus" => {
                return rpc::response(
                    id,
                    &proto::ServiceStatus {
                        enabled: self.shared.enabled.load(Ordering::Acquire),
                    },
                )
            }
            "GetVoiceStatus" => return rpc::response(id, &*lock(&self.shared.voice)),
            "GetAudioLevel" => return rpc::response(id, &*lock(&self.shared.level)),
            "GetDevices" => Action::Devices,
            "SetServiceStatus" => {
                match proto::SetServiceStatusRequest::decode(request.payload.as_slice()) {
                    Ok(v) => Action::Enable(v.enabled),
                    Err(_) => return rpc::failure(id, "invalid SetServiceStatus protobuf payload"),
                }
            }
            "SetServiceEnable" => {
                match proto::SetServiceEnableRequest::decode(request.payload.as_slice()) {
                    Ok(v) => Action::Autostart(v.enabled),
                    Err(_) => return rpc::failure(id, "invalid SetServiceEnable protobuf payload"),
                }
            }
            "SetAudioGain" => {
                match proto::SetAudioGainRequest::decode(request.payload.as_slice()) {
                    Ok(v) => Action::Gain(v.gain_db.clamp(-30, 30)),
                    Err(_) => return rpc::failure(id, "invalid SetAudioGain protobuf payload"),
                }
            }
            _ => return rpc::failure(id, "unknown Axonkey RPC method"),
        };
        if self.stop.is_requested() {
            return rpc::failure(id, "AxonkeyService is stopping");
        }
        let (reply, response) = oneshot::channel();
        if self
            .commands
            .try_send(Command { id, action, reply })
            .is_err()
        {
            return rpc::failure(id, "AxonkeyService is busy or stopping");
        }
        self.wake.set();
        tokio::select! {
            biased;
            response = response => response.unwrap_or_else(|_| rpc::failure(id, "Device coordinator stopped")),
            _ = self.stop.cancelled() => rpc::failure(id, "AxonkeyService is stopping"),
        }
    }
}
struct Coordinator {
    shared: Arc<Shared>,
    hub: Arc<Hub>,
    stop: Arc<Cancel>,
    targets: Vec<String>,
    endpoints: BTreeMap<String, hid::Endpoint>,
    voices: BTreeMap<String, gatt::Receiver>,
    metadata: Option<(Option<u32>, String)>,
    metadata_at: Option<Instant>,
}
impl Coordinator {
    fn stop_devices(&mut self) {
        for endpoint in self.endpoints.values() {
            endpoint.request_stop();
        }
        for voice in self.voices.values() {
            voice.request_stop();
        }
        for endpoint in self.endpoints.values_mut() {
            endpoint.join();
        }
        for voice in self.voices.values_mut() {
            voice.join();
        }
        self.endpoints.clear();
        self.voices.clear();
        self.targets.clear();
        self.metadata = None;
        self.metadata_at = None;
        *lock(&self.shared.voice) = proto::VoiceStatus::default();
    }
    fn detach(&self) {
        match device_setup::keyboards(false) {
            Ok(devices) => {
                for device in devices.into_iter().filter(|d| d.mounted) {
                    if let Err(e) = device_setup::attachment(&device.instance, false) {
                        self.hub
                            .issue("hid_filter_driver_error", &device.instance, &e);
                    }
                }
            }
            Err(e) => self.hub.issue("hid_filter_driver_error", "", &e),
        }
    }
    fn reconcile(&mut self) -> Result<()> {
        if !self.shared.enabled.load(Ordering::Acquire) || self.stop.is_requested() {
            return Ok(());
        }
        let targets: Vec<_> = device_setup::keyboards(true)?
            .into_iter()
            .filter(|d| device_setup::is_rc003(&d.instance))
            .collect();
        for device in &targets {
            if self.stop.is_requested() {
                return Ok(());
            }
            if !device.mounted {
                if let Err(e) = device_setup::attachment(&device.instance, true) {
                    self.hub
                        .issue("hid_filter_driver_error", &device.instance, &e);
                }
            }
        }
        let paths = device_setup::endpoints()?;
        self.targets = targets.into_iter().map(|d| d.instance).collect();
        self.targets.sort();
        self.targets.dedup();
        self.endpoints.retain(|_, endpoint| {
            endpoint.valid() && self.targets.contains(&endpoint.info.instance)
        });
        self.voices
            .retain(|id, voice| self.targets.contains(id) && !voice.finished());
        for target in &self.targets {
            if self.stop.is_requested() {
                break;
            }
            if !self.voices.contains_key(target) {
                match gatt::Receiver::start(
                    target.clone(),
                    self.shared.gain.clone(),
                    self.hub.clone(),
                    self.shared.level.clone(),
                ) {
                    Ok(voice) => {
                        self.voices.insert(target.clone(), voice);
                    }
                    Err(e) => self
                        .hub
                        .issue("bluetooth_initialization_failed", target, &e),
                }
            }
        }
        for path in paths {
            if self.stop.is_requested() {
                break;
            }
            if !device_setup::is_rc003(&path.instance)
                || !self.targets.contains(&path.instance)
                || self.endpoints.contains_key(&path.path)
            {
                continue;
            }
            let instance = path.instance.clone();
            let key = path.path.clone();
            match hid::Endpoint::start(path, self.hub.clone()) {
                Ok(endpoint) => {
                    self.endpoints.insert(key, endpoint);
                }
                Err(e) => self.hub.issue("hid_filter_driver_error", &instance, &e),
            }
        }
        Ok(())
    }
    fn snapshot(&self) {
        let statuses: Vec<_> = self
            .voices
            .values()
            .map(|voice| voice.snapshot().status)
            .collect();
        let status = select_voice(&statuses);
        *lock(&self.shared.voice) = status;
    }
    fn devices(&mut self, runtime: Option<&tokio::runtime::Runtime>) -> proto::DeviceList {
        let fallback_needed = self.targets.iter().any(|id| {
            self.voices.get(id).is_none_or(|v| {
                let s = v.snapshot();
                s.battery.is_none() || s.description.is_empty()
            })
        });
        if fallback_needed
            && self
                .metadata_at
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            self.metadata_at = Some(Instant::now());
            self.metadata =
                runtime.and_then(|runtime| runtime.block_on(gatt::metadata(&self.stop)).ok());
        }
        let devices = self
            .targets
            .iter()
            .map(|id| {
                let voice = self
                    .voices
                    .get(id)
                    .map(gatt::Receiver::snapshot)
                    .unwrap_or_default();
                let endpoint = self.endpoints.values().rfind(|e| e.info.instance == *id);
                proto::Device {
                    instance_id: id.clone(),
                    driver_mounted: true,
                    endpoint_path: endpoint.map_or_else(String::new, |e| e.info.path.clone()),
                    connected: endpoint.is_some_and(hid::Endpoint::valid),
                    input_blocked: endpoint.is_some_and(hid::Endpoint::valid),
                    data_forward_enabled: endpoint.is_some_and(hid::Endpoint::valid),
                    battery_level: voice
                        .battery
                        .or_else(|| self.metadata.as_ref().and_then(|v| v.0)),
                    description_name: if voice.description.is_empty() {
                        self.metadata
                            .as_ref()
                            .map_or_else(String::new, |v| v.1.clone())
                    } else {
                        voice.description
                    },
                }
            })
            .collect();
        proto::DeviceList { devices }
    }
    fn command(&mut self, command: Command, runtime: Option<&tokio::runtime::Runtime>) {
        if self.stop.is_requested() {
            let _ = command
                .reply
                .send(rpc::failure(command.id, "AxonkeyService is stopping"));
            return;
        }
        let result = match command.action {
            Action::Devices => {
                let response = rpc::response(command.id, &self.devices(runtime));
                let _ = command.reply.send(response);
                return;
            }
            Action::Enable(enabled) => {
                self.shared.enabled.store(enabled, Ordering::Release);
                if enabled {
                    if let Err(e) = self.reconcile() {
                        self.hub.issue("device_reconciliation_failed", "", &e);
                    }
                } else {
                    self.stop_devices();
                    self.detach();
                }
                config::save_enabled(enabled)
            }
            Action::Gain(gain) => {
                self.shared.gain.store(gain, Ordering::Release);
                config::save_gain(gain)
            }
            Action::Autostart(enabled) => scm::set_autostart(enabled),
        };
        let error = result.err().map(|e| e.to_string()).unwrap_or_default();
        let operation = proto::OperationResult {
            success: error.is_empty(),
            error: error.clone(),
        };
        let mut response = rpc::response(command.id, &operation);
        response.success = operation.success;
        response.error = error;
        let _ = command.reply.send(response);
    }
    fn run(&mut self, commands: mpsc::Receiver<Command>, wake: &Event) {
        let apartment = Apartment::mta().ok();
        let runtime = apartment.as_ref().and_then(|_| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
        });
        let mut last_scan = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .unwrap_or_else(Instant::now);
        while !self.stop.is_requested() {
            let scan_requested = wake.signaled();
            if scan_requested {
                let _ = wake.reset();
            }
            for _ in 0..64 {
                match commands.try_recv() {
                    Ok(command) => self.command(command, runtime.as_ref()),
                    Err(_) => break,
                }
            }
            if scan_requested || last_scan.elapsed() >= Duration::from_secs(2) {
                if let Err(e) = self.reconcile() {
                    self.hub.issue("device_reconciliation_failed", "", &e);
                }
                last_scan = Instant::now();
            }
            self.snapshot();
            // SAFETY: owned wake event stays live for the entire coordinator; bounded wait also observes the cancellation flag.
            let _ = unsafe { WaitForSingleObject(wake.handle(), 100) } == WAIT_OBJECT_0;
        }
    }
}
fn select_voice(statuses: &[proto::VoiceStatus]) -> proto::VoiceStatus {
    statuses
        .iter()
        .find(|s| s.active)
        .or_else(|| statuses.iter().find(|s| s.connected))
        .or_else(|| statuses.first())
        .cloned()
        .unwrap_or_default()
}

struct Running {
    stop: Arc<Cancel>,
    wake: Arc<Event>,
    coordinator: Option<thread::JoinHandle<()>>,
    rpc: rpc::Server,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop.request();
        self.wake.set();
        if let Some(worker) = self.coordinator.take() {
            let _ = worker.join();
        }
        self.rpc.stop();
    }
}
pub fn run(context: &scm::Context) -> Result<()> {
    if !crate::win::abi::verify_layouts() {
        return Err(Error::new(1306, "Rust/C driver ABI mismatch"));
    }
    let config = config::load();
    let shared = Arc::new(Shared {
        enabled: AtomicBool::new(config.enabled),
        gain: Arc::new(AtomicI32::new(config.gain_db)),
        voice: Mutex::new(proto::VoiceStatus::default()),
        level: Arc::new(Mutex::new(proto::AudioLevel::default())),
    });
    let hub = Arc::new(Hub::default());
    let (sender, commands) = mpsc::sync_channel(64);
    let control = Arc::new(Control {
        commands: sender,
        shared: shared.clone(),
        stop: context.stop.clone(),
        wake: context.rescan.clone(),
    });
    // Bind RPC before any device can be attached or input blocked.
    let rpc = rpc::Server::start(control, hub.clone(), rpc::PIPE_NAME.into())?;
    let _notification = match scm::Notification::register(context) {
        Ok(n) => Some(n),
        Err(e) => {
            log::warn!("Device notification unavailable; periodic scanning remains active: {e}");
            None
        }
    };
    let stop = context.stop.clone();
    let wake = context.rescan.clone();
    let worker_failure = Arc::new(Mutex::new(None));
    let coordinator_failure = worker_failure.clone();
    let coordinator = thread::Builder::new()
        .name("axonkey-devices".into())
        .spawn(move || {
            let mut owner = Coordinator {
                shared,
                hub,
                stop: stop.clone(),
                targets: Vec::new(),
                endpoints: BTreeMap::new(),
                voices: BTreeMap::new(),
                metadata: None,
                metadata_at: None,
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.run(commands, &wake)))
                .is_err()
            {
                *lock(&coordinator_failure) = Some(Error::new(574, "Device coordinator panicked"));
                owner.hub.issue(
                    "device_reconciliation_failed",
                    "",
                    &Error::new(574, "Device coordinator panicked"),
                );
                stop.request();
            }
            owner.stop_devices();
            // Disabled startup does not enumerate/mutate devices, including when stopped.
            if owner.shared.enabled.load(Ordering::Acquire) {
                owner.detach();
            }
        })?;
    let mut running = Running {
        stop: context.stop.clone(),
        wake: context.rescan.clone(),
        coordinator: Some(coordinator),
        rpc,
    };
    context.update(SERVICE_RUNNING, 0);
    log::info!("AxonkeyService Rust {} running", env!("CARGO_PKG_VERSION"));
    let mut failure = None;
    while !context.stop.is_requested() {
        if running.rpc.is_finished()
            || running
                .coordinator
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished)
        {
            failure = Some(Error::new(31, "Required service worker ended"));
            context.stop.request();
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    context.rescan.set();
    while running
        .coordinator
        .as_ref()
        .is_some_and(|w| !w.is_finished())
    {
        context.update(SERVICE_STOP_PENDING, 0);
        thread::sleep(Duration::from_secs(1));
    }
    if let Some(worker) = running.coordinator.take() {
        if worker.join().is_err() {
            failure = Some(Error::new(574, "Device cleanup panicked"));
        }
    }
    if failure.is_none() {
        failure = lock(&worker_failure).take();
    }
    drop(running);
    log::logger().flush();
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
