use crate::{cancel::Cancel, lock, proto, Error, Result};
use prost::Message;
use std::{
    collections::BTreeMap,
    future::Future,
    io,
    sync::{
        atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::mpsc,
    task::JoinSet,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
    },
};

pub const PIPE_NAME: &str = r"\\.\pipe\AxonkeyService.v1";
pub const PROTOCOL: &str = "axonkey.service.v1";
pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_FRAMES: usize = 256;
const MAX_CLIENTS: usize = 32;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: proto::Request) -> impl Future<Output = proto::Response> + Send;
}
struct Frame {
    bytes: Arc<[u8]>,
    budget: Arc<AtomicUsize>,
}
impl Drop for Frame {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}
struct Client {
    topics: AtomicU8,
    sender: mpsc::Sender<Frame>,
    bytes: Arc<AtomicUsize>,
    cancel: Cancel,
}
impl Client {
    fn enqueue(&self, bytes: Arc<[u8]>) -> bool {
        if bytes.len() > MAX_FRAME || self.cancel.is_requested() {
            self.cancel.request();
            return false;
        }
        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes.len())
                    .filter(|total| *total <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            self.cancel.request();
            return false;
        }
        let frame = Frame {
            bytes,
            budget: self.bytes.clone(),
        };
        if self.sender.try_send(frame).is_err() {
            self.cancel.request();
            return false;
        }
        true
    }
}
#[derive(Default)]
pub struct Hub {
    clients: Mutex<BTreeMap<u64, Arc<Client>>>,
    next_id: AtomicU64,
}
impl Hub {
    pub fn publish<M: Message>(&self, topic: u8, kind: &str, value: &M) {
        let event = proto::Event {
            r#type: kind.into(),
            payload: value.encode_to_vec(),
        };
        if event.encoded_len() > MAX_FRAME {
            log::warn!("RPC event exceeds frame limit: {kind}");
            return;
        }
        let bytes: Arc<[u8]> = event.encode_to_vec().into();
        let clients: Vec<_> = lock(&self.clients)
            .values()
            .filter(|client| client.topics.load(Ordering::Acquire) & topic != 0)
            .cloned()
            .collect();
        for client in clients {
            if !client.enqueue(bytes.clone()) {
                log::warn!("RPC client outbound limit reached; disconnecting");
            }
        }
    }
    pub fn keyboard(&self, device: &str, report: Vec<u8>) {
        self.publish(
            1,
            "keyboard",
            &proto::KeyboardEvent {
                device_instance_id: device.into(),
                report,
                timestamp_ms: crate::now_ms(),
            },
        );
    }
    pub fn issue(&self, code: &str, device: &str, error: &Error) {
        log::warn!("Service issue [{code}] device={device}: {error}");
        self.publish(
            8,
            "service_issue",
            &proto::ServiceIssue {
                code: code.into(),
                message: error.message.clone(),
                device_instance_id: device.into(),
                native_error: error.native,
                recoverable: true,
                timestamp_ms: crate::now_ms(),
            },
        );
    }
    pub fn voice(&self, status: &proto::VoiceStatus) {
        self.publish(4, "voice_status", status);
    }
    pub fn level(&self, level: &proto::AudioLevel) {
        self.publish(2, "audio_level", level);
    }
    pub fn client_count(&self) -> usize {
        lock(&self.clients).len()
    }
}
struct Registration {
    hub: Arc<Hub>,
    id: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.hub.clients).remove(&self.id);
    }
}

pub struct Server {
    cancel: Arc<Cancel>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub fn start<H: Handler>(handler: Arc<H>, hub: Arc<Hub>, name: String) -> Result<Self> {
        let cancel = Arc::new(Cancel::default());
        let stop = cancel.clone();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("axonkey-rpc".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()?;
                        runtime.block_on(async {
                            let pipe = match create_pipe(&name, true) {
                                Ok(pipe) => pipe,
                                Err(error) => {
                                    let _ = ready.send(Err(error.clone()));
                                    return Err(error);
                                }
                            };
                            let _ = ready.send(Ok(()));
                            accept(handler, hub, name, pipe, stop).await
                        })
                    }));
                match result {
                    Ok(Err(e)) => log::error!("RPC server stopped: {e}"),
                    Err(_) => log::error!("RPC server panicked"),
                    _ => {}
                }
            })?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                cancel,
                thread: Some(thread),
            }),
            other => {
                cancel.request();
                let _ = thread.join();
                Err(other
                    .ok()
                    .and_then(std::result::Result::err)
                    .unwrap_or_else(|| Error::new(31, "RPC worker failed during startup")))
            }
        }
    }
    pub fn request_stop(&self) {
        self.cancel.request();
    }
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }
    pub fn stop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn create_pipe(name: &str, first: bool) -> Result<NamedPipeServer> {
    let sddl =
        crate::win::handles::wide("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)(A;;GA;;;IU)(A;;GA;;;WD)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: null-terminated SDDL and valid output pointer. Windows allocates descriptor.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
    }
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: this is the allocation returned by the SDDL conversion API.
            unsafe {
                LocalFree(Some(HLOCAL(self.0 .0)));
            }
        }
    }
    let descriptor = Descriptor(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0 .0,
        bInheritHandle: false.into(),
    };
    // SAFETY: attributes and its descriptor remain alive until CreateNamedPipe returns; it copies the security descriptor.
    Ok(unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size((MAX_FRAME + 4) as u32)
            .out_buffer_size((MAX_FRAME + 4) as u32)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )?
    })
}
async fn accept<H: Handler>(
    handler: Arc<H>,
    hub: Arc<Hub>,
    name: String,
    mut pipe: NamedPipeServer,
    stop: Arc<Cancel>,
) -> Result<()> {
    let mut clients = JoinSet::new();
    let result = async {
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = clients.join_next(), if !clients.is_empty() => {},
                connected = pipe.connect(), if clients.len() < MAX_CLIENTS => {
                    connected?;
                    // Keep one listening instance alive while clients are being served.
                    let next = create_pipe(&name, false)?;
                    let active = std::mem::replace(&mut pipe, next);
                    clients.spawn(connection(active, handler.clone(), hub.clone(), stop.clone()));
                }
            }
        }
        Ok(())
    }
    .await;
    stop.request();
    // Futures use Tokio-owned overlapped operations. Dropping them cancels I/O safely.
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    result
}
async fn connection<H: Handler>(
    pipe: NamedPipeServer,
    handler: Arc<H>,
    hub: Arc<Hub>,
    stop: Arc<Cancel>,
) {
    let (sender, mut receiver) = mpsc::channel(MAX_QUEUED_FRAMES);
    let client = Arc::new(Client {
        topics: AtomicU8::new(0),
        sender,
        bytes: Arc::new(AtomicUsize::new(0)),
        cancel: Cancel::default(),
    });
    let id = hub.next_id.fetch_add(1, Ordering::Relaxed);
    lock(&hub.clients).insert(id, client.clone());
    let _registration = Registration { hub, id };
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let receive = async {
        loop {
            let bytes = read_frame(&mut reader).await?;
            let request = proto::Request::decode(bytes.as_slice()).map_err(invalid)?;
            let mut subscription = None;
            let response = if request.method == "Subscribe" {
                match proto::SubscribeRequest::decode(request.payload.as_slice()) {
                    Ok(value) => {
                        subscription = Some(
                            u8::from(value.keyboard)
                                | (u8::from(value.audio_level) << 1)
                                | (u8::from(value.voice_status) << 2)
                                | (u8::from(value.service_issues) << 3),
                        );
                        response(
                            request.request_id,
                            &proto::OperationResult {
                                success: true,
                                error: String::new(),
                            },
                        )
                    }
                    Err(_) => failure(request.request_id, "invalid Subscribe protobuf payload"),
                }
            } else {
                handler.handle(request).await
            };
            if !client.enqueue(response.encode_to_vec().into()) {
                return Ok::<_, io::Error>(());
            }
            // A publisher cannot observe this subscription before the ACK is queued.
            if let Some(topics) = subscription {
                client.topics.store(topics, Ordering::Release);
            }
        }
    };
    let send = async {
        while let Some(frame) = receiver.recv().await {
            tokio::time::timeout(WRITE_TIMEOUT, write_frame(&mut writer, &frame.bytes))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "RPC write exceeded two seconds")
                })??;
        }
        Ok::<_, io::Error>(())
    };
    tokio::select! {
        _ = stop.cancelled() => {},
        _ = client.cancel.cancelled() => {},
        result = receive => { if let Err(e) = result { log::info!("RPC client disconnected: {e}"); } },
        result = send => { if let Err(e) = result { log::info!("RPC writer disconnected: {e}"); } },
    }
    client.cancel.request();
}
pub fn response<M: Message>(id: u64, message: &M) -> proto::Response {
    proto::Response {
        request_id: id,
        success: true,
        error: String::new(),
        payload: message.encode_to_vec(),
    }
}
pub fn failure(id: u64, error: impl Into<String>) -> proto::Response {
    proto::Response {
        request_id: id,
        success: false,
        error: error.into(),
        payload: Vec::new(),
    }
}
fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let size = reader.read_u32_le().await? as usize;
    if size > MAX_FRAME {
        return Err(invalid("RPC frame exceeds 1 MiB"));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(invalid)?;
    bytes.resize(size, 0);
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err(invalid("RPC frame exceeds 1 MiB"));
    }
    writer.write_u32_le(bytes.len() as u32).await?;
    writer.write_all(bytes).await
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
