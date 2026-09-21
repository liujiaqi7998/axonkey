//! Windows named-pipe transport for AxonkeyService. The service uses nanopb;
//! prost generates wire-compatible Rust types from the same schema.

use std::{io, time::Duration};

use prost::Message;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
    sync::watch,
};

#[allow(dead_code)] // Other schema messages are reserved for future RPC methods.
mod proto {
    include!(concat!(env!("OUT_DIR"), "/axonkey.service.v1.rs"));
}

const PIPE_NAME: &str = r"\\.\pipe\AxonkeyService.v1";
const PROTOCOL_VERSION: &str = "axonkey.service.v1";
const MAX_FRAME: usize = 1024 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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
    connected: bool,
    info: Option<ServiceRpcInfo>,
    error: Option<String>,
}

// Managed by Tauri for the lifetime of the desktop process.
pub struct ServiceConnection {
    state: watch::Receiver<ConnectionState>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl ServiceConnection {
    pub fn start() -> Self {
        let (sender, state) = watch::channel(ConnectionState::Disconnected { error: None });
        let task = tauri::async_runtime::spawn(run(sender));
        Self { state, task }
    }

    pub fn status(&self) -> ServiceRpcStatus {
        status_from_state(&self.state.borrow())
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
        self.task.abort();
    }
}

async fn run(state: watch::Sender<ConnectionState>) {
    let mut last_error = None;
    loop {
        let result = connect_and_monitor(&state).await;
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
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

async fn connect_and_monitor(state: &watch::Sender<ConnectionState>) -> io::Result<()> {
    let mut pipe = ClientOptions::new().open(PIPE_NAME)?;
    let info = tokio::time::timeout(PROBE_TIMEOUT, request_service_info(&mut pipe))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GetServiceInfo timed out"))??;

    log::info!(target: "axonkey::service_rpc", "Connected to {} {}", info.name, info.version);
    state.send_replace(ConnectionState::Connected(info));

    // No subscriptions or other calls are issued yet. Reading also detects a
    // closed pipe while the desktop is otherwise idle.
    let frame = read_frame(&mut pipe).await?;
    let response = proto::Response::decode(frame.as_slice()).map_err(invalid_data)?;
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "unexpected unsolicited RPC response: {}",
            response.request_id
        ),
    ))
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
            assert_eq!(
                request_service_info(&mut client).await.unwrap().name,
                "AxonkeyService"
            );
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
