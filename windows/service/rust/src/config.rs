#![forbid(unsafe_code)]
use crate::{
    audio::gain::{DEFAULT_GAIN, MAX_GAIN, MIN_GAIN},
    win::abi::{empty_remap, valid_remap},
    Result,
};
use winreg::{enums::*, RegKey, RegValue};

const PATH: &str = r"SOFTWARE\Axonkey\Service";
#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub gain_db: i32,
    pub remap: Vec<u8>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            gain_db: DEFAULT_GAIN,
            remap: empty_remap(),
        }
    }
}
fn open() -> std::io::Result<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(PATH, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
        .map(|(key, _)| key)
}
pub fn load() -> Config {
    match open() {
        Ok(key) => load_key(&key),
        Err(e) => {
            log::warn!("Service registry unavailable; using defaults: {e}");
            Config::default()
        }
    }
}
fn save_raw(key: &RegKey, name: &str, value: RegValue) {
    if let Err(e) = key.set_raw_value(name, &value) {
        log::warn!("Cannot restore {name}: {e}");
    }
}
fn dword(bytes: Vec<u8>) -> RegValue {
    RegValue {
        bytes,
        vtype: REG_DWORD,
    }
}
fn read_dword(key: &RegKey, name: &str) -> Option<u32> {
    let value = key.get_raw_value(name).ok()?;
    if value.vtype != REG_DWORD {
        return None;
    }
    Some(u32::from_le_bytes(value.bytes.as_slice().try_into().ok()?))
}
pub fn load_key(key: &RegKey) -> Config {
    let mut config = Config::default();
    match read_dword(key, "Enabled") {
        Some(value @ 0..=1) => config.enabled = value != 0,
        _ => save_raw(key, "Enabled", dword(1_u32.to_le_bytes().to_vec())),
    }
    match read_dword(key, "AudioGainDb").map(|bits| bits as i32) {
        Some(value) if (MIN_GAIN..=MAX_GAIN).contains(&value) => config.gain_db = value,
        _ => save_raw(
            key,
            "AudioGainDb",
            dword(DEFAULT_GAIN.to_le_bytes().to_vec()),
        ),
    }
    match key.get_raw_value("RemapConfig") {
        Ok(value) if value.vtype == REG_BINARY && valid_remap(&value.bytes) => {
            config.remap = value.bytes
        }
        _ => save_raw(
            key,
            "RemapConfig",
            RegValue {
                bytes: config.remap.clone(),
                vtype: REG_BINARY,
            },
        ),
    }
    config
}
pub fn save_enabled(enabled: bool) -> Result<()> {
    open()?.set_value("Enabled", &u32::from(enabled))?;
    Ok(())
}
pub fn save_gain(gain: i32) -> Result<()> {
    open()?.set_value("AudioGainDb", &(gain.clamp(MIN_GAIN, MAX_GAIN) as u32))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_repairs_invalid_values_and_retains_signed_gain() {
        let parent = RegKey::predef(HKEY_CURRENT_USER);
        let path = format!("Software\\AxonkeyServiceRustTests\\{}", std::process::id());
        let (key, _) = parent.create_subkey(&path).unwrap();
        key.set_value("AudioGainDb", &(-6_i32 as u32)).unwrap();
        key.set_value("Enabled", &9_u32).unwrap();
        key.set_value("RemapConfig", &"invalid").unwrap();
        let config = load_key(&key);
        assert_eq!(config.gain_db, -6);
        assert!(config.enabled);
        assert!(valid_remap(&config.remap));
        assert_eq!(key.get_value::<u32, _>("Enabled").unwrap(), 1);
        key.set_value("AudioGainDb", &31_u32).unwrap();
        assert_eq!(load_key(&key).gain_db, 2);
        for invalid in [
            RegValue {
                bytes: vec![0, 0, 0, 3],
                vtype: REG_DWORD_BIG_ENDIAN,
            },
            RegValue {
                bytes: vec![3],
                vtype: REG_DWORD,
            },
        ] {
            key.set_raw_value("AudioGainDb", &invalid).unwrap();
            assert_eq!(load_key(&key).gain_db, 2);
        }
        drop(key);
        parent.delete_subkey_all(path).unwrap();
    }
}
