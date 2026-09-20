use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 100 * 1024;

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLog {
    path: String,
    content: String,
    exists: bool,
    truncated: bool,
}

fn read_log(path: &Path) -> Result<ServiceLog, String> {
    let mut result = ServiceLog {
        path: path.to_string_lossy().into_owned(),
        content: String::new(),
        exists: false,
        truncated: false,
    };
    // Windows' default sharing permits the service to keep writing/rotating.
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => {
            return Err(format!(
                "无法读取服务运行日志（{}）：{error}",
                path.display()
            ))
        }
    };
    result.exists = true;
    let size = file
        .metadata()
        .map_err(|e| format!("无法读取日志信息：{e}"))?
        .len();
    let offset = size.saturating_sub(MAX_LOG_BYTES);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("无法定位日志内容：{e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("无法读取日志内容：{e}"))?;
    result.truncated = offset > 0;
    if offset > 0 {
        // The tail may begin in the middle of a UTF-8 character or record.
        if let Some(newline) = bytes.iter().position(|b| *b == b'\n') {
            bytes.drain(..=newline);
        } else {
            while bytes.first().is_some_and(|b| b & 0xc0 == 0x80) {
                bytes.remove(0);
            }
        }
    }
    result.content = String::from_utf8_lossy(&bytes)
        .trim_start_matches('\u{feff}')
        .to_string();
    Ok(result)
}

fn log_path_for_image(image: &str) -> Result<PathBuf, String> {
    let image = image.trim();
    let executable = if let Some(quoted) = image.strip_prefix('"') {
        quoted
            .split_once('"')
            .map(|(path, _)| path)
            .ok_or("服务程序路径的引号不完整")?
    } else {
        let end = image
            .to_ascii_lowercase()
            .find(".exe")
            .ok_or("无法识别服务程序路径")?
            + 4;
        &image[..end]
    };
    let executable = Path::new(executable);
    if !executable.is_absolute() {
        return Err("服务程序路径不是绝对路径".into());
    }
    Ok(executable
        .parent()
        .ok_or("服务程序目录无效")?
        .join("AxonkeyService.log"))
}

#[cfg(windows)]
fn service_log_path() -> Result<PathBuf, String> {
    use winreg::{
        enums::{HKEY_LOCAL_MACHINE, KEY_READ},
        RegKey,
    };
    let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
    match machine.open_subkey_with_flags(
        "SYSTEM\\CurrentControlSet\\Services\\AxonkeyService",
        KEY_READ,
    ) {
        Ok(service) => {
            let image: String = service
                .get_value("ImagePath")
                .map_err(|e| format!("无法读取服务程序路径：{e}"))?;
            use windows::core::PCWSTR;
            use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
            let input: Vec<u16> = image.encode_utf16().chain(Some(0)).collect();
            let length = unsafe { ExpandEnvironmentStringsW(PCWSTR(input.as_ptr()), None) };
            if length == 0 || length > 32768 {
                return Err("无法展开服务程序路径".into());
            }
            let mut expanded = vec![0u16; length as usize];
            let written =
                unsafe { ExpandEnvironmentStringsW(PCWSTR(input.as_ptr()), Some(&mut expanded)) };
            if written == 0 || written > length {
                return Err("无法展开服务程序路径".into());
            }
            log_path_for_image(&String::from_utf16_lossy(&expanded[..written as usize - 1]))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let root = std::env::var_os("ProgramW6432")
                .or_else(|| std::env::var_os("ProgramFiles"))
                .ok_or("无法定位服务安装目录")?;
            Ok(PathBuf::from(root).join("Axonkey/Service/AxonkeyService.log"))
        }
        Err(error) => Err(format!("无法查询服务日志位置：{error}")),
    }
}

#[tauri::command]
pub async fn get_windows_service_log() -> Result<ServiceLog, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(|| read_log(&service_log_path()?))
            .await
            .map_err(|e| format!("服务日志查询失败：{e}"))?
    }
    #[cfg(not(windows))]
    Err("服务运行日志仅支持 Windows。".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn resolves_log_next_to_registered_executable() {
        for image in [
            r#""C:\应用 Files\AxonkeyService.exe" --service"#,
            r"C:\应用 Files\AxonkeyService.exe",
        ] {
            assert_eq!(
                log_path_for_image(image).unwrap(),
                PathBuf::from(r"C:\应用 Files\AxonkeyService.log")
            );
        }
        assert!(log_path_for_image("relative.exe").is_err());
        assert!(log_path_for_image("\"unfinished.exe").is_err());
    }
    #[test]
    fn reads_utf8_missing_empty_and_replaced_logs_with_bounded_tail() {
        let directory =
            std::env::temp_dir().join(format!("axonkey-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("AxonkeyService.log");
        assert!(!read_log(&path).unwrap().exists);
        std::fs::write(&path, "中文运行日志\n").unwrap();
        assert_eq!(read_log(&path).unwrap().content, "中文运行日志\n");
        let large = "中文记录\n".repeat(20000) + "最后一条\n";
        std::fs::write(&path, large).unwrap();
        let tail = read_log(&path).unwrap();
        assert!(tail.truncated);
        assert!(tail.content.len() <= MAX_LOG_BYTES as usize);
        assert!(!tail.content.contains('\u{fffd}'));
        assert!(tail.content.ends_with("最后一条\n"));
        std::fs::write(&path, "").unwrap();
        let empty = read_log(&path).unwrap();
        assert!(empty.exists && empty.content.is_empty() && !empty.truncated);
        std::fs::write(&path, "轮转后的日志").unwrap();
        assert_eq!(read_log(&path).unwrap().content, "轮转后的日志");
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn resolves_actual_service_log_read_only() {
        read_log(&service_log_path().unwrap()).unwrap();
    }
}
