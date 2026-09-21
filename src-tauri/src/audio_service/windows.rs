use super::{AudioLevel, AudioServiceStatus};

/// Windows audio is owned by AxonkeyService.
///
/// The desktop client keeps this small compatibility surface because the
/// existing audio test dialog still invokes the audio commands. It must not
/// initialize Windows Bluetooth or an audio output endpoint on Windows.
pub struct AudioService;

impl AudioService {
    pub fn level(&self) -> AudioLevel {
        AudioLevel::default()
    }

    pub fn start() -> Self {
        log::info!(
            target: "axonkey::audio",
            "Windows audio forwarding is managed by AxonkeyService; client audio bridge disabled"
        );
        Self
    }

    pub fn refresh(&self) {}

    pub fn set_gain_db(&self, _gain: i16) -> Result<(), String> {
        Err("Windows 音频由 AxonkeyService 管理，客户端不再控制音频增益".into())
    }

    pub fn status(&self) -> AudioServiceStatus {
        AudioServiceStatus {
            state: "unsupported".into(),
            error: Some("Windows 音频由 AxonkeyService 管理".into()),
            ..AudioServiceStatus::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AudioService;

    #[test]
    fn windows_audio_service_does_not_start_a_client_audio_bridge() {
        let status = AudioService::start().status();
        assert_eq!(status.state, "unsupported");
        assert!(!status.driver_installed);
        assert!(!status.bluetooth_connected);
        assert!(!status.forwarding);
        assert_eq!(
            status.error.as_deref(),
            Some("Windows 音频由 AxonkeyService 管理")
        );
    }
}
