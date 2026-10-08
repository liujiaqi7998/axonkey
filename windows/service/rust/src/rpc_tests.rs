use super::*;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Echo;
impl Handler for Echo {
    async fn handle(&self, request: proto::Request) -> proto::Response {
        proto::Response {
            request_id: request.request_id,
            success: request.method == "Echo",
            error: String::new(),
            payload: request.payload,
        }
    }
}
fn server() -> (Server, Arc<Hub>, String) {
    let name = format!(
        r"\\.\pipe\AxonkeyService.rust-test.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let hub = Arc::new(Hub::default());
    let server = Server::start(Arc::new(Echo), hub.clone(), name.clone()).unwrap();
    (server, hub, name)
}
async fn wait_for(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("condition did not converge");
}
async fn connect(name: &str) -> NamedPipeClient {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ClientOptions::new().open(name) {
                Ok(pipe) => return pipe,
                Err(e) if e.raw_os_error() == Some(231) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(e) => panic!("open: {e}"),
            }
        }
    })
    .await
    .unwrap()
}
async fn request(
    pipe: &mut NamedPipeClient,
    id: u64,
    method: &str,
    payload: Vec<u8>,
) -> proto::Response {
    let request = proto::Request {
        request_id: id,
        method: method.into(),
        payload,
    };
    write_frame(pipe, &request.encode_to_vec()).await.unwrap();
    let bytes = tokio::time::timeout(Duration::from_secs(5), read_frame(pipe))
        .await
        .unwrap()
        .unwrap();
    let response = proto::Response::decode(bytes.as_slice()).unwrap();
    assert_eq!(response.request_id, id);
    response
}
#[test]
fn both_queue_limits_release_the_entire_budget() {
    for size in [1, MAX_FRAME] {
        let (sender, mut receiver) = mpsc::channel(MAX_QUEUED_FRAMES);
        let client = Client {
            topics: AtomicU8::new(0),
            sender,
            bytes: Arc::new(AtomicUsize::new(0)),
            cancel: Cancel::default(),
        };
        let frame: Arc<[u8]> = vec![0; size].into();
        let mut accepted = 0;
        while client.enqueue(frame.clone()) {
            accepted += 1;
        }
        assert_eq!(accepted, (MAX_QUEUED_BYTES / size).min(MAX_QUEUED_FRAMES));
        assert!(client.cancel.is_requested());
        while receiver.try_recv().is_ok() {}
        assert_eq!(client.bytes.load(Ordering::Acquire), 0);
    }
}
#[tokio::test]
async fn subscriptions_ack_first_all_masks_and_resubscribe() {
    let (mut server, hub, name) = server();
    let mut pipe = connect(&name).await;
    for mask in 0..16_u8 {
        let sub = proto::SubscribeRequest {
            keyboard: mask & 1 != 0,
            audio_level: mask & 2 != 0,
            voice_status: mask & 4 != 0,
            service_issues: mask & 8 != 0,
        };
        // Queue publications as soon as the mask is visible, before consuming the ACK.
        write_frame(
            &mut pipe,
            &proto::Request {
                request_id: u64::from(mask) + 1,
                method: "Subscribe".into(),
                payload: sub.encode_to_vec(),
            }
            .encode_to_vec(),
        )
        .await
        .unwrap();
        wait_for(|| {
            lock(&hub.clients)
                .values()
                .any(|c| c.topics.load(Ordering::Acquire) == mask)
        })
        .await;
        hub.keyboard("键盘", vec![1, 2, 3]);
        hub.level(&proto::AudioLevel::default());
        hub.voice(&proto::VoiceStatus::default());
        hub.issue("test", "", &Error::new(5, "fault"));
        let ack = read_frame(&mut pipe).await.unwrap();
        assert_eq!(
            proto::Response::decode(ack.as_slice()).unwrap().request_id,
            u64::from(mask) + 1
        );
        for (bit, kind) in [
            (1, "keyboard"),
            (2, "audio_level"),
            (4, "voice_status"),
            (8, "service_issue"),
        ] {
            if mask & bit == 0 {
                continue;
            }
            let bytes = read_frame(&mut pipe).await.unwrap();
            let event = proto::Event::decode(bytes.as_slice()).unwrap();
            assert_eq!(event.r#type, kind);
        }
        assert_eq!(
            request(&mut pipe, 100, "Echo", vec![0, 128, 255])
                .await
                .payload,
            [0, 128, 255]
        );
    }
    server.stop();
    assert_eq!(hub.client_count(), 0);
}
#[tokio::test]
async fn malformed_frames_and_partial_disconnect_do_not_damage_listener() {
    let (mut server, hub, name) = server();
    for bytes in [
        vec![1, 0, 16, 0],
        vec![1, 0, 0, 0, 255],
        vec![10, 0, 0, 0, 1],
        vec![1, 0],
    ] {
        let mut pipe = connect(&name).await;
        pipe.write_all(&bytes).await.unwrap();
        drop(pipe);
        // A valid client proves accept is still progressing even after bad data.
        let mut valid = connect(&name).await;
        assert!(request(&mut valid, 12, "Echo", vec![]).await.success);
        drop(valid);
        wait_for(|| hub.client_count() == 0).await;
    }
    let mut client = connect(&name).await;
    assert!(
        !request(&mut client, 13, "Subscribe", vec![255])
            .await
            .success
    );
    assert!(request(&mut client, 14, "Echo", vec![]).await.success);
    // A partial read is pending when stop occurs; shutdown must drain all tasks.
    client.write_all(&[20, 0, 0, 0, 1]).await.unwrap();
    server.stop();
    assert_eq!(hub.client_count(), 0);
}
#[tokio::test]
async fn slow_reader_times_out_without_blocking_other_clients() {
    let (mut server, hub, name) = server();
    let mut slow = connect(&name).await;
    let sub = proto::SubscribeRequest {
        keyboard: true,
        ..Default::default()
    };
    request(&mut slow, 1, "Subscribe", sub.encode_to_vec()).await;
    let slow_client = lock(&hub.clients).values().next().unwrap().clone();
    let frame: Arc<[u8]> = vec![0; MAX_FRAME].into();
    for _ in 0..3 {
        assert!(slow_client.enqueue(frame.clone()));
    }
    let mut healthy = connect(&name).await;
    assert!(request(&mut healthy, 2, "Echo", vec![42]).await.success);
    wait_for(|| slow_client.cancel.is_requested()).await;
    wait_for(|| slow_client.bytes.load(Ordering::Acquire) == 0).await;
    assert!(request(&mut healthy, 3, "Echo", vec![43]).await.success);
    server.stop();
    assert_eq!(hub.client_count(), 0);
}
#[tokio::test]
async fn connection_cap_and_one_thousand_reconnects_reclaim_registrations() {
    let (mut server, hub, name) = server();
    let mut clients = Vec::new();
    for id in 0..MAX_CLIENTS {
        let mut pipe = connect(&name).await;
        request(&mut pipe, id as u64, "Echo", vec![]).await;
        clients.push(pipe);
    }
    assert_eq!(hub.client_count(), MAX_CLIENTS);
    let mut pending = connect(&name).await;
    write_frame(
        &mut pending,
        &proto::Request {
            request_id: 100,
            method: "Echo".into(),
            payload: vec![],
        }
        .encode_to_vec(),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), read_frame(&mut pending))
            .await
            .is_err()
    );
    clients.pop();
    let bytes = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut pending))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        proto::Response::decode(bytes.as_slice())
            .unwrap()
            .request_id,
        100
    );
    clients.clear();
    drop(pending);
    wait_for(|| hub.client_count() == 0).await;
    for id in 0..1000 {
        let mut pipe = connect(&name).await;
        request(&mut pipe, id, "Echo", vec![1, 2]).await;
        drop(pipe);
    }
    wait_for(|| hub.client_count() == 0).await;
    assert!(!server.is_finished());
    server.stop();
    assert!(server.is_finished());
}
#[test]
fn second_server_cannot_take_the_first_pipe_instance() {
    let (_server, hub, name) = server();
    assert!(Server::start(Arc::new(Echo), hub, name).is_err());
}
