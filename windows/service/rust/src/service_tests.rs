use super::*;
use crate::rpc::Handler;
fn shared() -> Arc<Shared> {
    Arc::new(Shared {
        enabled: AtomicBool::new(false),
        gain: Arc::new(AtomicI32::new(-6)),
        voice: Mutex::new(proto::VoiceStatus::default()),
        level: Arc::new(Mutex::new(proto::AudioLevel::default())),
    })
}
fn request(method: &str, payload: Vec<u8>) -> proto::Request {
    proto::Request {
        request_id: 42,
        method: method.into(),
        payload,
    }
}
#[tokio::test]
async fn business_dispatch_getters_invalid_payloads_and_shutdown() {
    let (commands, receiver) = mpsc::sync_channel(64);
    let control = Control {
        commands,
        shared: shared(),
        stop: Arc::new(Cancel::default()),
        wake: Arc::new(Event::new().unwrap()),
    };
    let info = control.handle(request("GetServiceInfo", vec![])).await;
    let info = proto::ServiceInfo::decode(info.payload.as_slice()).unwrap();
    assert_eq!(info.name, "AxonkeyService");
    assert_eq!(info.version, "0.3.1");
    assert_eq!(info.pipe_name, rpc::PIPE_NAME);
    assert_eq!(info.protocol_version, rpc::PROTOCOL);
    assert_eq!(info.audio_gain_db, -6);
    for method in ["GetServiceStatus", "GetVoiceStatus", "GetAudioLevel"] {
        assert!(control.handle(request(method, vec![])).await.success);
    }
    for method in [
        "SetServiceStatus",
        "SetServiceEnable",
        "SetAudioGain",
        "Unknown",
    ] {
        assert!(!control.handle(request(method, vec![255])).await.success);
    }
    assert!(receiver.try_recv().is_err());
    control.stop.request();
    assert!(!control.handle(request("GetDevices", vec![])).await.success);
}
#[tokio::test]
async fn mutation_dispatch_clamps_gain_and_preserves_operation_failure() {
    let (commands, receiver) = mpsc::sync_channel(64);
    let control = Control {
        commands,
        shared: shared(),
        stop: Arc::new(Cancel::default()),
        wake: Arc::new(Event::new().unwrap()),
    };
    let worker = thread::spawn(move || {
        for expected in 0..4 {
            let cmd: Command = receiver.recv().unwrap();
            match (&cmd.action, expected) {
                (Action::Gain(30), 0)
                | (Action::Enable(false), 1)
                | (Action::Autostart(true), 2)
                | (Action::Devices, 3) => {}
                _ => panic!("wrong action"),
            }
            let mut result = rpc::response(
                cmd.id,
                &proto::OperationResult {
                    success: false,
                    error: "injected persistence failure".into(),
                },
            );
            result.success = false;
            result.error = "injected persistence failure".into();
            cmd.reply.send(result).unwrap();
        }
    });
    for (method, payload) in [
        (
            "SetAudioGain",
            proto::SetAudioGainRequest { gain_db: 100 }.encode_to_vec(),
        ),
        ("SetServiceStatus", vec![]),
        (
            "SetServiceEnable",
            proto::SetServiceEnableRequest { enabled: true }.encode_to_vec(),
        ),
        ("GetDevices", vec![]),
    ] {
        let result = control.handle(request(method, payload)).await;
        assert_eq!(result.request_id, 42);
        assert!(!result.success);
        assert!(result.error.contains("persistence"));
        assert!(
            !proto::OperationResult::decode(result.payload.as_slice())
                .unwrap()
                .success
        );
    }
    worker.join().unwrap();
}
#[test]
fn disabled_reconciliation_skips_device_work_and_voice_selection_is_stable() {
    let mut owner = Coordinator {
        shared: shared(),
        hub: Arc::new(Hub::default()),
        stop: Arc::new(Cancel::default()),
        targets: vec!["sentinel".into()],
        endpoints: BTreeMap::new(),
        voices: BTreeMap::new(),
        metadata: None,
        metadata_at: None,
    };
    owner.reconcile().unwrap();
    assert_eq!(owner.targets, ["sentinel"]);
    let stopped = proto::VoiceStatus {
        device_instance_id: "a".into(),
        ..Default::default()
    };
    let connected = proto::VoiceStatus {
        device_instance_id: "b".into(),
        connected: true,
        ..Default::default()
    };
    let active = proto::VoiceStatus {
        device_instance_id: "c".into(),
        active: true,
        ..Default::default()
    };
    assert_eq!(select_voice(&[]), proto::VoiceStatus::default());
    assert_eq!(
        select_voice(&[stopped.clone(), connected.clone()]),
        connected
    );
    assert_eq!(select_voice(&[stopped, connected, active.clone()]), active);
}
