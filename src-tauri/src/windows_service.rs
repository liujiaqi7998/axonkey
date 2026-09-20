#[cfg(windows)]
use crate::windows_service_rpc;

#[derive(Clone, Copy, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    state: &'static str,
    process_id: u32,
    exit_code: u32,
}

#[cfg(windows)]
mod platform {
use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::core::{w, HRESULT, PCWSTR};
    use windows::Win32::System::Services::*;

    static ACTION_RUNNING: AtomicBool = AtomicBool::new(false);
    struct ServiceHandle(SC_HANDLE);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }

    pub fn state_name(state: SERVICE_STATUS_CURRENT_STATE) -> &'static str {
        match state {
            SERVICE_RUNNING => "running",
            SERVICE_STOPPED => "stopped",
            SERVICE_START_PENDING | SERVICE_CONTINUE_PENDING => "startPending",
            SERVICE_STOP_PENDING => "stopPending",
            SERVICE_PAUSED => "paused",
            _ => "unknown",
        }
    }

    pub fn query() -> Result<ServiceStatus, String> {
        let manager = ServiceHandle(
            unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
                .map_err(|e| format!("无法连接 Windows 服务管理器：{e}"))?,
        );
        let service =
            match unsafe { OpenServiceW(manager.0, w!("AxonkeyService"), SERVICE_QUERY_STATUS) } {
                Ok(handle) => ServiceHandle(handle),
                Err(e) if e.code() == HRESULT::from_win32(1060) => {
                    return Ok(ServiceStatus {
                        state: "notInstalled",
                        process_id: 0,
                        exit_code: 0,
                    })
                }
                Err(e) if e.code() == HRESULT::from_win32(1072) => {
                    return Ok(ServiceStatus {
                        state: "deletePending",
                        process_id: 0,
                        exit_code: 0,
                    })
                }
                Err(e) => return Err(format!("无法读取 AxonkeyService 状态：{e}")),
            };
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0;
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
                std::mem::size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        unsafe {
            QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(buffer), &mut needed)
        }
        .map_err(|e| format!("无法查询 AxonkeyService 状态：{e}"))?;
        Ok(ServiceStatus {
            state: state_name(status.dwCurrentState),
            process_id: status.dwProcessId,
            exit_code: if status.dwWin32ExitCode == 1066 {
                status.dwServiceSpecificExitCode
            } else {
                status.dwWin32ExitCode
            },
        })
    }

    fn resource(resource_dir: &Path, relative: &str) -> Result<PathBuf, String> {
        let path = if cfg!(debug_assertions) {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join(relative)
        } else {
            resource_dir.join(relative)
        };
        if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "缺少服务组件，请重新构建或安装 Axonkey：{}",
                path.display()
            ))
        }
    }

    // Windows paths cannot contain quotes. Reject them rather than allowing a
    // command-line boundary to be changed by a malformed resource path.
    pub fn quote_path(path: &Path) -> Result<String, String> {
        let value = path.to_str().ok_or("服务组件路径无法编码")?;
        if value.contains(['"', '\0', '\r', '\n']) || value.ends_with('\\') {
            return Err("服务组件路径无效".into());
        }
        Ok(format!("\"{value}\""))
    }

    pub fn manage(resource_dir: &Path, action: ServiceAction) -> Result<ServiceStatus, String> {
        if ACTION_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("已有服务操作正在执行，请稍候。".into());
        }
        struct ActionGuard;
        impl Drop for ActionGuard {
            fn drop(&mut self) {
                ACTION_RUNNING.store(false, Ordering::SeqCst);
            }
        }
        let _guard = ActionGuard;
        let script = resource(resource_dir, "scripts/manage-windows-service.ps1")?;
        let name = match action {
            ServiceAction::Install => "Install",
            ServiceAction::Uninstall => "Uninstall",
            ServiceAction::Start => "Start",
            ServiceAction::Stop => "Stop",
        };
        let mut parameters = format!("-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File {} -Action {name}", quote_path(&script)?);
        if matches!(action, ServiceAction::Install) {
            let relative = if cfg!(debug_assertions) {
                ".build/service-bundle/AxonkeyService.exe"
            } else {
                "windows/service/AxonkeyService.exe"
            };
            let executable = resource(resource_dir, relative)?;
            parameters.push_str(&format!(" -ServiceExecutable {}", quote_path(&executable)?));
        }
        let system_root = std::env::var_os("SystemRoot").ok_or("无法定位 Windows 目录")?;
        let powershell =
            Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let code = crate::windows_elevation::run_elevated(&powershell, Some(&parameters), true)?;
        if code != Some(0) {
            return Err(format!("服务操作失败（退出码 {}）。详情请查看 %ProgramData%\\Axonkey\\Logs\\ServiceManagement.log。", code.unwrap_or(1)));
        }
        query()
    }
}

#[tauri::command]
pub async fn get_windows_service_status() -> Result<ServiceStatus, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(platform::query)
            .await
            .map_err(|e| format!("服务查询失败：{e}"))?
    }
    #[cfg(not(windows))]
    Err("服务管理仅支持 Windows。".into())
}

#[tauri::command]
pub async fn manage_windows_service(
    _app: tauri::AppHandle,
    action: ServiceAction,
) -> Result<ServiceStatus, String> {
    #[cfg(windows)]
    {
        use tauri::Manager;
        let resources = _app.path().resource_dir().map_err(|e| e.to_string())?;
        tauri::async_runtime::spawn_blocking(move || platform::manage(&resources, action))
            .await
            .map_err(|e| format!("服务操作失败：{e}"))?
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("服务管理仅支持 Windows。".into())
    }
}

#[tauri::command]
pub async fn get_windows_service_info() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    { tauri::async_runtime::spawn_blocking(|| windows_service_rpc::service_info().and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))).await.map_err(|e| e.to_string())? }
    #[cfg(not(windows))]
    { Err("服务接口仅支持 Windows。".into()) }
}

#[tauri::command]
pub async fn set_windows_audio_gain(gain_db: i32) -> Result<(), String> {
    #[cfg(windows)]
    { tauri::async_runtime::spawn_blocking(move || windows_service_rpc::set_audio_gain(gain_db)).await.map_err(|e| e.to_string())? }
    #[cfg(not(windows))]
    { let _ = gain_db; Err("服务接口仅支持 Windows。".into()) }
}

#[tauri::command]
pub async fn get_windows_service_devices() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    { tauri::async_runtime::spawn_blocking(|| windows_service_rpc::devices().and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))).await.map_err(|e| e.to_string())? }
    #[cfg(not(windows))]
    { Err("服务接口仅支持 Windows。".into()) }
}

#[tauri::command]
pub async fn get_windows_voice_status() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    { tauri::async_runtime::spawn_blocking(|| windows_service_rpc::voice_status().and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))).await.map_err(|e| e.to_string())? }
    #[cfg(not(windows))]
    { Err("服务接口仅支持 Windows。".into()) }
}

#[tauri::command]
pub async fn get_windows_audio_level() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    { tauri::async_runtime::spawn_blocking(|| windows_service_rpc::audio_level().and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))).await.map_err(|e| e.to_string())? }
    #[cfg(not(windows))]
    { Err("服务接口仅支持 Windows。".into()) }
}

#[tauri::command]
pub fn subscribe_windows_service_events(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    { windows_service_rpc::subscribe_events(app) }
    #[cfg(not(windows))]
    { let _ = app; Err("服务接口仅支持 Windows。".into()) }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::Path;
    use windows::Win32::System::Services::*;
    #[test]
    fn exposes_real_service_states() {
        assert_eq!(platform::state_name(SERVICE_RUNNING), "running");
        assert_eq!(platform::state_name(SERVICE_STOPPED), "stopped");
        assert_eq!(platform::state_name(SERVICE_START_PENDING), "startPending");
        assert_eq!(platform::state_name(SERVICE_STOP_PENDING), "stopPending");
        assert_eq!(platform::state_name(SERVICE_PAUSED), "paused");
        assert_eq!(
            platform::state_name(SERVICE_STATUS_CURRENT_STATE(999)),
            "unknown"
        );
    }
    #[test]
    fn restricts_actions_and_quotes_paths_with_spaces_and_unicode() {
        assert!(serde_json::from_str::<ServiceAction>("\"restart\"").is_err());
        assert!(serde_json::from_str::<ServiceAction>("\"start\"").is_ok());
        assert_eq!(
            platform::quote_path(Path::new("C:\\应用 Files\\service.exe")).unwrap(),
            "\"C:\\应用 Files\\service.exe\""
        );
        assert!(platform::quote_path(Path::new("C:\\bad\"argument")).is_err());
    }
    #[test]
    fn queries_local_service_without_elevation() {
        platform::query().unwrap();
    }
}
