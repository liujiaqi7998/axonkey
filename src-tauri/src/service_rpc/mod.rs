//! Windows named-pipe transport for AxonkeyService. The service uses nanopb;
//! prost generates wire-compatible Rust types from the same schema.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        OnceLock,
    },
    time::Duration,
};

use prost::Message;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
    sync::{broadcast, watch},
};
use tauri::Emitter;

use crate::audio_service::{AudioLevel, AudioServiceStatus};

#[allow(dead_code)] // Other schema messages are reserved for future RPC methods.
mod proto {
    include!(concat!(env!("OUT_DIR"), "/axonkey.service.v1.rs"));
}

const PIPE_NAME: &str = r"\\.\pipe\AxonkeyService.v1";
const PROTOCOL_VERSION: &str = "axonkey.service.v1";
const MAX_FRAME: usize = 1024 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const KEYBOARD_EVENT_CAPACITY: usize = 256;

/// A keyboard report published by AxonkeyService.
///
/// The service sends the complete HID report, including its report-ID byte;
/// the Windows input backend owns HID usage parsing. Keeping this wrapper
/// independent of the generated prost type lets the backend consume events
/// without depending on the private schema module. An empty report is an
/// endpoint reset notification emitted when the service reader disconnects.
/// `connection_generation` is local metadata, not part of the protobuf; it
/// lets the desktop discard reports queued before a pipe reconnect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardEvent {
    pub device_instance_id: String,
    pub report: Vec<u8>,
    pub timestamp_ms: u64,
    pub connection_generation: u64,
}

// The service connection is created after InputService during Tauri startup.
// A process-wide sender lets the input worker subscribe before the connection
// task begins, without opening a second pipe or changing platform startup
// ordering. The bounded channel also prevents a stalled worker from growing
// memory without limit; the worker handles Lagged by resetting its key state.
static KEYBOARD_EVENTS: OnceLock<broadcast::Sender<KeyboardEvent>> = OnceLock::new();
static KEYBOARD_CONNECTION_ACTIVE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_CONNECTION_GENERATION: AtomicU64 = AtomicU64::new(0);

fn keyboard_sender() -> &'static broadcast::Sender<KeyboardEvent> {
    KEYBOARD_EVENTS.get_or_init(|| {
        let (sender, _receiver) = broadcast::channel(KEYBOARD_EVENT_CAPACITY);
        sender
    })
}

/// Subscribe to keyboard reports from the persistent AxonkeyService listener.
///
/// This is intentionally a non-blocking receiver API: the Windows input
/// worker can use `try_recv` while checking its shutdown flag and processing
/// gesture timers. Reports are delivered in service order for each receiver.
pub fn subscribe_keyboard_events() -> broadcast::Receiver<KeyboardEvent> {
    keyboard_sender().subscribe()
}

/// Returns whether the persistent Subscribe pipe currently accepts keyboard
/// reports. The Windows input worker uses this edge to release held outputs
/// when the service disconnects; a broadcast sender itself remains alive for
/// the lifetime of the process.
pub(crate) fn keyboard_connection_active() -> bool {
    KEYBOARD_CONNECTION_ACTIVE.load(Ordering::Acquire)
}

/// Returns the monotonically increasing local connection generation. A worker
/// can use this value to distinguish reports buffered before a reconnect from
/// reports belonging to the current Subscribe session.
pub(crate) fn keyboard_connection_generation() -> u64 {
    KEYBOARD_CONNECTION_GENERATION.load(Ordering::Acquire)
}

fn set_keyboard_connection_active(active: bool) {
    let previous = KEYBOARD_CONNECTION_ACTIVE.swap(active, Ordering::AcqRel);
    if previous != active {
        KEYBOARD_CONNECTION_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
enum ConnectionState {
    Disconnected { error: Option<String> },
    Connected(proto::ServiceInfo),
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRpcInfo {
    name: String,
    version: String,
    protocol_version: String,
    pipe_name: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRpcStatus {
    pub connected: bool,
    pub info: Option<ServiceRpcInfo>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRpcDevice {
    pub instance_id: String,
    pub endpoint_path: String,
    pub driver_mounted: bool,
    pub input_blocked: bool,
    pub data_forward_enabled: bool,
    pub connected: bool,
    pub battery_level: Option<u32>,
    pub description_name: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceIssue {
    pub code: String,
    pub message: String,
    pub device_instance_id: String,
    pub native_error: u32,
    pub recoverable: bool,
    pub timestamp_ms: u64,
}

pub enum GetDevicesError {
    ServiceUnavailable(io::Error),
    Request(io::Error),
}

impl std::fmt::Display for GetDevicesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServiceUnavailable(error) => write!(formatter, "{error}"),
            Self::Request(error) => write!(formatter, "{error}"),
        }
    }
}

// Managed by Tauri for the lifetime of the desktop process.
pub struct ServiceConnection {
    state: watch::Receiver<ConnectionState>,
    audio_level: watch::Receiver<Option<proto::AudioLevel>>,
    voice_status: watch::Receiver<Option<proto::VoiceStatus>>,
    issue: watch::Receiver<Option<proto::ServiceIssue>>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl ServiceConnection {
    pub fn start(app: tauri::AppHandle) -> Self {
        // Initialize the keyboard broadcaster before spawning the listener so
        // a report cannot be dropped solely because startup is concurrent
        // with the input worker's receiver registration.
        let _ = keyboard_sender();
        set_keyboard_connection_active(false);
        let (sender, state) = watch::channel(ConnectionState::Disconnected { error: None });
        let (audio_sender, audio_level) = watch::channel(None);
        let (voice_sender, voice_status) = watch::channel(None);
        let (issue_sender, issue) = watch::channel(None);
        let task = tauri::async_runtime::spawn(run(app, sender, audio_sender, voice_sender, issue_sender));
        Self {
            state,
            audio_level,
            voice_status,
            issue,
            task,
        }
    }

    pub fn status(&self) -> ServiceRpcStatus {
        status_from_state(&self.state.borrow())
    }

    /// Returns the most recent service-published level and voice state. These
    /// snapshots are updated by the persistent Subscribe pipe listener.
    pub fn audio_test_state(&self) -> (AudioServiceStatus, AudioLevel) {
        let connected = matches!(*self.state.borrow(), ConnectionState::Connected(_));
        let level = self.audio_level.borrow().clone();
        let voice = self.voice_status.borrow().clone();
        let voice_connected = voice.as_ref().is_some_and(|value| value.connected);
        let forwarding = voice.as_ref().is_some_and(|value| value.active);
        let state = if !connected {
            "error"
        } else if forwarding {
            "forwarding"
        } else if voice_connected {
            "ready"
        } else {
            "connecting"
        };
        let error = if connected {
            None
        } else {
            match &*self.state.borrow() {
                ConnectionState::Disconnected { error } => error.clone(),
                ConnectionState::Connected(_) => None,
            }
        };
        let status = AudioServiceStatus {
            // The service owns the Windows virtual microphone. A connected
            // RPC endpoint is the authoritative driver/service availability
            // signal for this client-side diagnostic view.
            driver_installed: connected,
            state: state.into(),
            bluetooth_connected: voice_connected,
            forwarding,
            received_data: level.is_some(),
            output_ready: connected,
            event_version: level.as_ref().map_or(0, |value| value.timestamp_ms),
            error,
            ..AudioServiceStatus::default()
        };
        let level = level.map_or_else(AudioLevel::default, |value| AudioLevel {
            peak: f64::from(value.peak),
            rms: f64::from(value.rms),
        });
        (status, level)
    }

    pub async fn get_service_status(&self) -> io::Result<bool> {
        let mut pipe = open_pipe().await?;
        let status: proto::ServiceStatus = tokio::time::timeout(
            PROBE_TIMEOUT,
            call(
                &mut pipe,
                next_request_id(),
                "GetServiceStatus",
                &proto::ServiceStatusRequest {},
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GetServiceStatus timed out"))??;
        Ok(status.enabled)
    }

    pub async fn set_service_status(&self, enabled: bool) -> io::Result<()> {
        let mut pipe = open_pipe().await?;
        let result: proto::OperationResult = tokio::time::timeout(
            PROBE_TIMEOUT,
            call(
                &mut pipe,
                next_request_id(),
                "SetServiceStatus",
                &proto::SetServiceStatusRequest { enabled },
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SetServiceStatus timed out"))??;
        if result.success {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                if result.error.is_empty() {
                    "AxonkeyService rejected the status change"
                } else {
                    result.error.as_str()
                },
            ))
        }
    }

    pub fn latest_issue(&self) -> Option<ServiceIssue> {
        self.issue.borrow().as_ref().map(|value| ServiceIssue {
            code: value.code.clone(),
            message: value.message.clone(),
            device_instance_id: value.device_instance_id.clone(),
            native_error: value.native_error,
            recoverable: value.recoverable,
            timestamp_ms: value.timestamp_ms,
        })
    }

    pub async fn set_service_enable(&self, enabled: bool) -> io::Result<()> {
        let mut pipe = open_pipe().await?;
        let result: proto::OperationResult = tokio::time::timeout(
            PROBE_TIMEOUT,
            call(
                &mut pipe,
                next_request_id(),
                "SetServiceEnable",
                &proto::SetServiceEnableRequest { enabled },
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SetServiceEnable timed out"))??;
        if result.success {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                if result.error.is_empty() {
                    "AxonkeyService rejected the startup setting change"
                } else {
                    result.error.as_str()
                },
            ))
        }
    }

    pub async fn get_audio_gain(&self) -> io::Result<i16> {
        let mut pipe = open_pipe().await?;
        let info = tokio::time::timeout(PROBE_TIMEOUT, request_service_info(&mut pipe))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GetServiceInfo timed out"))??;
        i16::try_from(info.audio_gain_db).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "AxonkeyService returned an invalid audio gain: {} dB",
                    info.audio_gain_db
                ),
            )
        })
    }

    pub async fn set_audio_gain(&self, gain: i16) -> io::Result<()> {
        let mut pipe = open_pipe().await?;
        let result: proto::OperationResult = tokio::time::timeout(
            PROBE_TIMEOUT,
            call(
                &mut pipe,
                next_request_id(),
                "SetAudioGain",
                &proto::SetAudioGainRequest {
                    gain_db: i32::from(gain),
                },
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SetAudioGain timed out"))??;
        if result.success {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                if result.error.is_empty() {
                    "AxonkeyService rejected the audio gain"
                } else {
                    result.error.as_str()
                },
            ))
        }
    }

    pub async fn get_devices(&self) -> Result<Vec<ServiceRpcDevice>, GetDevicesError> {
        let mut pipe = open_pipe()
            .await
            .map_err(GetDevicesError::ServiceUnavailable)?;
        let devices: proto::DeviceList = tokio::time::timeout(
            PROBE_TIMEOUT,
            call(
                &mut pipe,
                next_request_id(),
                "GetDevices",
                &proto::DeviceListRequest {},
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GetDevices timed out"))
        .and_then(|result| result)
        .map_err(GetDevicesError::Request)?;
        Ok(devices
            .devices
            .into_iter()
            .map(|device| ServiceRpcDevice {
                instance_id: device.instance_id,
                endpoint_path: device.endpoint_path,
                driver_mounted: device.driver_mounted,
                input_blocked: device.input_blocked,
                data_forward_enabled: device.data_forward_enabled,
                connected: device.connected,
                battery_level: device.battery_level,
                description_name: device.description_name,
            })
            .collect())
    }
}

fn status_from_state(state: &ConnectionState) -> ServiceRpcStatus {
    match state {
        ConnectionState::Disconnected { error } => ServiceRpcStatus {
            connected: false,
            info: None,
            error: error.clone(),
        },
        ConnectionState::Connected(info) => ServiceRpcStatus {
            connected: true,
            info: Some(ServiceRpcInfo {
                name: info.name.clone(),
                version: info.version.clone(),
                protocol_version: info.protocol_version.clone(),
                pipe_name: info.pipe_name.clone(),
            }),
            error: None,
        },
    }
}

impl Drop for ServiceConnection {
    fn drop(&mut self) {
        set_keyboard_connection_active(false);
        self.task.abort();
    }
}

async fn run(
    app: tauri::AppHandle,
    state: watch::Sender<ConnectionState>,
    audio_sender: watch::Sender<Option<proto::AudioLevel>>,
    voice_sender: watch::Sender<Option<proto::VoiceStatus>>,
    issue_sender: watch::Sender<Option<proto::ServiceIssue>>,
) {
    let mut last_error = None;
    loop {
        let result = connect_and_monitor(&app, &state, &audio_sender, &voice_sender, &issue_sender).await;
        set_keyboard_connection_active(false);
        if matches!(*state.borrow(), ConnectionState::Connected(_)) {
            last_error = None;
        }
        let error = match result {
            Ok(()) => "RPC connection closed".to_string(),
            Err(error) => error.to_string(),
        };
        if last_error.as_deref() != Some(error.as_str()) {
            log::warn!(target: "axonkey::service_rpc", "AxonkeyService RPC unavailable: {error}");
            last_error = Some(error.clone());
        }
        state.send_replace(ConnectionState::Disconnected { error: Some(error) });
        audio_sender.send_replace(None);
        voice_sender.send_replace(None);
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

async fn connect_and_monitor(
    app: &tauri::AppHandle,
    state: &watch::Sender<ConnectionState>,
    audio_sender: &watch::Sender<Option<proto::AudioLevel>>,
    voice_sender: &watch::Sender<Option<proto::VoiceStatus>>,
    issue_sender: &watch::Sender<Option<proto::ServiceIssue>>,
) -> io::Result<()> {
    let mut pipe = ClientOptions::new().open(PIPE_NAME)?;
    let info = tokio::time::timeout(PROBE_TIMEOUT, request_service_info(&mut pipe))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GetServiceInfo timed out"))??;

    log::info!(target: "axonkey::service_rpc", "Connected to {} {}", info.name, info.version);
    state.send_replace(ConnectionState::Connected(info));

    let subscription: proto::OperationResult = tokio::time::timeout(
        PROBE_TIMEOUT,
        call(
            &mut pipe,
            next_request_id(),
            "Subscribe",
            &event_subscription(),
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Subscribe timed out"))??;
    if !subscription.success {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            if subscription.error.is_empty() {
                "AxonkeyService rejected event subscription"
            } else {
                subscription.error.as_str()
            },
        ));
    }

    set_keyboard_connection_active(true);

    loop {
        let frame = read_frame(&mut pipe).await?;
        let event = proto::Event::decode(frame.as_slice()).map_err(invalid_data)?;
        handle_event_with_issue(event, audio_sender, voice_sender, issue_sender, Some(app))?;
    }
}

fn event_subscription() -> proto::SubscribeRequest {
    proto::SubscribeRequest {
        keyboard: true,
        audio_level: true,
        voice_status: true,
        service_issues: true,
    }
}

#[cfg(test)]
fn handle_event(
    event: proto::Event,
    audio_sender: &watch::Sender<Option<proto::AudioLevel>>,
    voice_sender: &watch::Sender<Option<proto::VoiceStatus>>,
) -> io::Result<()> {
    let (issue_sender, _) = watch::channel::<Option<proto::ServiceIssue>>(None);
    handle_event_with_issue(event, audio_sender, voice_sender, &issue_sender, None)
}

fn handle_event_with_issue(
    event: proto::Event,
    audio_sender: &watch::Sender<Option<proto::AudioLevel>>,
    voice_sender: &watch::Sender<Option<proto::VoiceStatus>>,
    issue_sender: &watch::Sender<Option<proto::ServiceIssue>>,
    app: Option<&tauri::AppHandle>,
) -> io::Result<()> {
    match event.r#type.as_str() {
        "keyboard" => {
            let value =
                proto::KeyboardEvent::decode(event.payload.as_slice()).map_err(invalid_data)?;
            // A receiver may not exist during early startup or after the
            // input worker has shut down. `send` then simply drops this event;
            // the pipe listener must continue servicing audio/voice events.
            let _ = keyboard_sender().send(KeyboardEvent {
                device_instance_id: value.device_instance_id,
                report: value.report,
                timestamp_ms: value.timestamp_ms,
                connection_generation: keyboard_connection_generation(),
            });
        }
        "audio_level" => {
            let level =
                proto::AudioLevel::decode(event.payload.as_slice()).map_err(invalid_data)?;
            audio_sender.send_replace(Some(level));
        }
        "voice_status" => {
            let status =
                proto::VoiceStatus::decode(event.payload.as_slice()).map_err(invalid_data)?;
            if !status.active {
                audio_sender.send_replace(None);
            }
            voice_sender.send_replace(Some(status));
        }
        "service_issue" => {
            let issue = proto::ServiceIssue::decode(event.payload.as_slice()).map_err(invalid_data)?;
            issue_sender.send_replace(Some(issue.clone()));
            let payload = ServiceIssue {
                code: issue.code,
                message: issue.message,
                device_instance_id: issue.device_instance_id,
                native_error: issue.native_error,
                recoverable: issue.recoverable,
                timestamp_ms: issue.timestamp_ms,
            };
            if let Some(app) = app {
                if let Err(error) = app.emit("windows-service-issue", payload) {
                    log::debug!(target: "axonkey::service_rpc", "Could not emit Windows service issue: {error}");
                }
            }
        }
        _ => {
            log::debug!(target: "axonkey::service_rpc", "Ignoring AxonkeyService event {}", event.r#type)
        }
    }
    Ok(())
}

async fn request_service_info<S>(pipe: &mut S) -> io::Result<proto::ServiceInfo>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let info: proto::ServiceInfo =
        call(pipe, 1, "GetServiceInfo", &proto::ServiceInfoRequest {}).await?;
    if info.name != "AxonkeyService"
        || info.pipe_name != PIPE_NAME
        || info.protocol_version != PROTOCOL_VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected AxonkeyService RPC identity or protocol version",
        ));
    }
    Ok(info)
}

async fn open_pipe() -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    ClientOptions::new().open(PIPE_NAME)
}

fn next_request_id() -> u64 {
    static NEXT_REQUEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2);
    NEXT_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// Calls are serialized on this pipe; no other reader may consume its response.
async fn call<S, Q, R>(pipe: &mut S, request_id: u64, method: &str, payload: &Q) -> io::Result<R>
where
    S: AsyncRead + AsyncWrite + Unpin,
    Q: Message,
    R: Message + Default,
{
    let request = proto::Request {
        request_id,
        method: method.into(),
        payload: payload.encode_to_vec(),
    };
    write_frame(pipe, &request).await?;
    let frame = read_frame(pipe).await?;
    let response = proto::Response::decode(frame.as_slice()).map_err(invalid_data)?;
    if response.request_id != request_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RPC request ID mismatch",
        ));
    }
    if !response.success {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{method} failed: {}", response.error),
        ));
    }
    R::decode(response.payload.as_slice()).map_err(invalid_data)
}

fn invalid_data(error: prost::DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let size = u32::from_le_bytes(header) as usize;
    if size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RPC frame exceeds 1 MiB",
        ));
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_frame<W: AsyncWrite + Unpin, M: Message>(
    writer: &mut W,
    message: &M,
) -> io::Result<()> {
    if message.encoded_len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "RPC frame exceeds 1 MiB",
        ));
    }
    let payload = message.encode_to_vec();
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .await?;
    writer.write_all(&payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn test_info() -> proto::ServiceInfo {
        proto::ServiceInfo {
            name: "AxonkeyService".into(),
            version: "0.3.1".into(),
            protocol_version: PROTOCOL_VERSION.into(),
            pipe_name: PIPE_NAME.into(),
            audio_gain_db: 2,
        }
    }

    #[test]
    fn get_service_info_round_trip_and_disconnect() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                let request =
                    proto::Request::decode(read_frame(&mut server).await.unwrap().as_slice())
                        .unwrap();
                assert_eq!(request.request_id, 1);
                assert_eq!(request.method, "GetServiceInfo");
                assert!(request.payload.is_empty());
                write_frame(
                    &mut server,
                    &proto::Response {
                        request_id: request.request_id,
                        success: true,
                        error: String::new(),
                        payload: test_info().encode_to_vec(),
                    },
                )
                .await
                .unwrap();
            });
            let info = request_service_info(&mut client).await.unwrap();
            assert_eq!(info.name, "AxonkeyService");
            assert_eq!(info.audio_gain_db, 2);
            server_task.await.unwrap();
            assert_eq!(
                read_frame(&mut client).await.unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        });
    }

    #[test]
    fn rejects_oversized_frame_before_reading_payload() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut server) = duplex(16);
            server
                .write_all(&((MAX_FRAME + 1) as u32).to_le_bytes())
                .await
                .unwrap();
            assert_eq!(
                read_frame(&mut client).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        });
    }

    #[test]
    fn rejects_wrong_response_id() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                read_frame(&mut server).await.unwrap();
                write_frame(
                    &mut server,
                    &proto::Response {
                        request_id: 2,
                        success: true,
                        error: String::new(),
                        payload: test_info().encode_to_vec(),
                    },
                )
                .await
                .unwrap();
            });
            assert_eq!(
                request_service_info(&mut client).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            server_task.await.unwrap();
        });
    }

    #[test]
    fn decodes_service_nanopb_response_wire_format() {
        // The C++ codec's classic Response{7, true, "", {0x08, 0x01}}.
        let bytes = [0x08, 0x07, 0x10, 0x01, 0x22, 0x02, 0x08, 0x01];
        let response = proto::Response::decode(bytes.as_slice()).unwrap();
        assert_eq!(response.request_id, 7);
        assert!(response.success);
        assert_eq!(response.payload, [0x08, 0x01]);
        assert_eq!(response.encode_to_vec(), bytes);
    }

    #[test]
    fn subscribes_to_keyboard_events_on_the_shared_pipe() {
        let subscription = event_subscription();
        assert!(subscription.keyboard);
        assert!(subscription.audio_level);
        assert!(subscription.voice_status);

        let request = proto::Request {
            request_id: 9,
            method: "Subscribe".into(),
            payload: subscription.encode_to_vec(),
        };
        let decoded = proto::SubscribeRequest::decode(request.payload.as_slice()).unwrap();
        assert!(decoded.keyboard);
        assert!(decoded.audio_level);
        assert!(decoded.voice_status);
    }

    #[test]
    fn status_drops_service_info_after_disconnect() {
        let connected = status_from_state(&ConnectionState::Connected(test_info()));
        let value = serde_json::to_value(connected).unwrap();
        assert_eq!(value["connected"], true);
        assert_eq!(value["info"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["info"]["pipeName"], PIPE_NAME);

        let disconnected = status_from_state(&ConnectionState::Disconnected {
            error: Some("pipe closed".into()),
        });
        let value = serde_json::to_value(disconnected).unwrap();
        assert_eq!(value["connected"], false);
        assert!(value["info"].is_null());
        assert_eq!(value["error"], "pipe closed");
    }

    #[test]
    fn service_status_request_round_trip() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                let request =
                    proto::Request::decode(read_frame(&mut server).await.unwrap().as_slice())
                        .unwrap();
                assert_eq!(request.method, "GetServiceStatus");
                assert!(request.payload.is_empty());
                write_frame(
                    &mut server,
                    &proto::Response {
                        request_id: request.request_id,
                        success: true,
                        error: String::new(),
                        payload: proto::ServiceStatus { enabled: false }.encode_to_vec(),
                    },
                )
                .await
                .unwrap();
            });
            let status: proto::ServiceStatus = call(
                &mut client,
                23,
                "GetServiceStatus",
                &proto::ServiceStatusRequest {},
            )
            .await
            .unwrap();
            assert!(!status.enabled);
            server_task.await.unwrap();
        });
    }

    #[test]
    fn subscribed_events_update_global_snapshots() {
        let (audio_sender, audio_level) = watch::channel(None);
        let (voice_sender, voice_status) = watch::channel(None);
        handle_event(
            proto::Event {
                r#type: "audio_level".into(),
                payload: proto::AudioLevel {
                    peak: 0.25,
                    rms: 0.1,
                    timestamp_ms: 42,
                }
                .encode_to_vec(),
            },
            &audio_sender,
            &voice_sender,
        )
        .unwrap();
        assert_eq!(
            audio_level
                .borrow()
                .as_ref()
                .map(|value| value.timestamp_ms),
            Some(42)
        );
        assert_eq!(
            audio_level.borrow().as_ref().map(|value| value.peak),
            Some(0.25)
        );

        handle_event(
            proto::Event {
                r#type: "voice_status".into(),
                payload: proto::VoiceStatus {
                    state: "connected".into(),
                    connected: true,
                    active: true,
                    ..Default::default()
                }
                .encode_to_vec(),
            },
            &audio_sender,
            &voice_sender,
        )
        .unwrap();
        assert!(voice_status
            .borrow()
            .as_ref()
            .is_some_and(|value| value.active));

        handle_event(
            proto::Event {
                r#type: "voice_status".into(),
                payload: proto::VoiceStatus {
                    state: "connected".into(),
                    connected: true,
                    active: false,
                    ..Default::default()
                }
                .encode_to_vec(),
            },
            &audio_sender,
            &voice_sender,
        )
        .unwrap();
        assert!(audio_level.borrow().is_none());
        assert!(voice_status
            .borrow()
            .as_ref()
            .is_some_and(|value| !value.active));
    }

    #[test]
    fn keyboard_event_is_decoded_and_broadcast_in_order() {
        let mut receiver = subscribe_keyboard_events();
        let (audio_sender, _audio_level) = watch::channel(None);
        let (voice_sender, _voice_status) = watch::channel(None);
        let report = vec![1, 0, 0, 0xf1, 0, 0x80, 0, 0x81, 0];

        handle_event(
            proto::Event {
                r#type: "keyboard".into(),
                payload: proto::KeyboardEvent {
                    device_instance_id: "HID\\RC003".into(),
                    report: report.clone(),
                    timestamp_ms: 123,
                }
                .encode_to_vec(),
            },
            &audio_sender,
            &voice_sender,
        )
        .unwrap();

        let event = receiver.try_recv().unwrap();
        assert_eq!(event.device_instance_id, "HID\\RC003");
        assert_eq!(event.report, report);
        assert_eq!(event.timestamp_ms, 123);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn service_status_write_preserves_service_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                let request =
                    proto::Request::decode(read_frame(&mut server).await.unwrap().as_slice())
                        .unwrap();
                assert_eq!(request.method, "SetServiceStatus");
                let payload =
                    proto::SetServiceStatusRequest::decode(request.payload.as_slice()).unwrap();
                assert!(payload.enabled);
                write_frame(
                    &mut server,
                    &proto::Response {
                        request_id: request.request_id,
                        success: true,
                        error: String::new(),
                        payload: proto::OperationResult {
                            success: false,
                            error: "service rejected request".into(),
                        }
                        .encode_to_vec(),
                    },
                )
                .await
                .unwrap();
            });
            let result: proto::OperationResult = call(
                &mut client,
                24,
                "SetServiceStatus",
                &proto::SetServiceStatusRequest { enabled: true },
            )
            .await
            .unwrap();
            assert!(!result.success);
            assert_eq!(result.error, "service rejected request");
            server_task.await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires a running local AxonkeyService"]
    fn probes_running_windows_service() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut pipe = ClientOptions::new().open(PIPE_NAME).unwrap();
            let info = tokio::time::timeout(PROBE_TIMEOUT, request_service_info(&mut pipe))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(info.name, "AxonkeyService");
        });
    }
}
