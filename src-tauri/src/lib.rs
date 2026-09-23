mod audio_service;
mod input_service;
#[cfg(target_os = "windows")]
mod service_rpc;

use audio_service::{AudioService, AudioServiceStatus};
use input_service::mouse::MouseService;
use input_service::{InputService, NativeSettings};
use tauri::{Manager, PhysicalPosition, PhysicalSize};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_SHOW_ID: &str = "tray-show";
const TRAY_QUIT_ID: &str = "tray-quit";
const AUTOSTART_ARG: &str = "--autostart";
const LEGACY_AUTOSTART_MARKER: &str = "autostart-initialized";
const BACKGROUND_AUTOSTART_MARKER: &str = "autostart-background-initialized";
const PERMISSION_HELPER_WIDTH: f64 = 430.0;
const PERMISSION_HELPER_HEIGHT: f64 = 560.0;
const RUNTIME_LOG_FILE_BASENAME: &str = "axonkey";
const RUNTIME_LOG_MAX_BYTES: u128 = 5_000_000;
const RUNTIME_LOG_KEEP_FILES: usize = 5;

#[derive(Clone, Copy)]
struct WindowGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    resizable: bool,
    always_on_top: bool,
}

#[derive(Default)]
struct PermissionHelperWindowState(std::sync::Mutex<Option<WindowGeometry>>);

fn initialize_autostart(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_autostart::ManagerExt;

    let directory = app.path().app_config_dir()?;
    let marker = directory.join(BACKGROUND_AUTOSTART_MARKER);
    // Apply the default once and migrate legacy entries, preserving user changes.
    if marker.try_exists()? {
        return Ok(());
    }
    std::fs::create_dir_all(&directory)?;
    let autostart = app.autolaunch();
    let was_enabled = autostart.is_enabled()?;
    let legacy_marker = directory.join(LEGACY_AUTOSTART_MARKER);
    let had_legacy_initialization = legacy_marker.try_exists()?;
    let should_be_enabled = !had_legacy_initialization || was_enabled;
    if should_be_enabled {
        autostart.enable()?;
    }
    if should_be_enabled && !autostart.is_enabled()? {
        return Err("Autostart did not become enabled".into());
    }
    std::fs::write(marker, b"initialized\n")?;
    Ok(())
}

fn args_request_autostart<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .skip(1)
        .any(|arg| arg.as_ref() == AUTOSTART_ARG)
}

fn launched_from_autostart() -> bool {
    args_request_autostart(std::env::args())
}

fn app_bundle_for_executable(executable: &std::path::Path) -> Option<std::path::PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(std::path::Path::to_path_buf)
}

#[cfg(target_os = "macos")]
fn apply_permission_helper_mode(
    window: &tauri::WebviewWindow,
    state: &PermissionHelperWindowState,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        let geometry = WindowGeometry {
            position: window
                .outer_position()
                .map_err(|error| format!("Cannot read window position: {error}"))?,
            size: window
                .outer_size()
                .map_err(|error| format!("Cannot read window size: {error}"))?,
            resizable: window
                .is_resizable()
                .map_err(|error| format!("Cannot read resizable state: {error}"))?,
            always_on_top: window
                .is_always_on_top()
                .map_err(|error| format!("Cannot read always-on-top state: {error}"))?,
        };
        let mut saved = state
            .0
            .lock()
            .map_err(|_| "Permission helper window state is unavailable".to_string())?;
        if saved.is_none() {
            *saved = Some(geometry);
        }
        drop(saved);

        let helper_size =
            tauri::LogicalSize::new(PERMISSION_HELPER_WIDTH, PERMISSION_HELPER_HEIGHT);
        window
            .set_min_size(Some(helper_size))
            .map_err(|error| format!("Cannot set helper minimum size: {error}"))?;
        window
            .set_resizable(false)
            .map_err(|error| format!("Cannot lock helper size: {error}"))?;
        window
            .set_size(helper_size)
            .map_err(|error| format!("Cannot resize permission helper: {error}"))?;
        window
            .set_always_on_top(true)
            .map_err(|error| format!("Cannot keep permission helper visible: {error}"))?;

        if let Some(monitor) = window
            .current_monitor()
            .map_err(|error| format!("Cannot read current monitor: {error}"))?
        {
            let scale = monitor.scale_factor();
            let work_area = monitor.work_area();
            let width = (PERMISSION_HELPER_WIDTH * scale).round() as i32;
            let margin = (20.0 * scale).round() as i32;
            let x = work_area.position.x + work_area.size.width as i32 - width - margin;
            let y = work_area.position.y + margin;
            window
                .set_position(PhysicalPosition::new(x, y))
                .map_err(|error| format!("Cannot position permission helper: {error}"))?;
        }
        window
            .set_focus()
            .map_err(|error| format!("Cannot focus permission helper: {error}"))?;
        return Ok(());
    }

    let geometry = state
        .0
        .lock()
        .map_err(|_| "Permission helper window state is unavailable".to_string())?
        .take();
    if let Some(geometry) = geometry {
        window
            .set_min_size(Some(tauri::LogicalSize::new(980.0, 680.0)))
            .map_err(|error| format!("Cannot restore minimum window size: {error}"))?;
        window
            .set_size(geometry.size)
            .map_err(|error| format!("Cannot restore window size: {error}"))?;
        window
            .set_position(geometry.position)
            .map_err(|error| format!("Cannot restore window position: {error}"))?;
        window
            .set_resizable(geometry.resizable)
            .map_err(|error| format!("Cannot restore resizable state: {error}"))?;
        window
            .set_always_on_top(geometry.always_on_top)
            .map_err(|error| format!("Cannot restore always-on-top state: {error}"))?;
    }
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::TrayIconBuilder,
    };

    let show = MenuItem::with_id(app, TRAY_SHOW_ID, "显示 Axonkey", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, TRAY_QUIT_ID, "退出 Axonkey", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let tray = TrayIconBuilder::with_id("axonkey-tray")
        .icon(tauri::include_image!("./icons/32x32.png"))
        .tooltip("Axonkey")
        .menu(&menu)
        .on_menu_event(|app, event| {
            if event.id() == TRAY_SHOW_ID {
                show_main_window(app);
            } else if event.id() == TRAY_QUIT_ID {
                app.exit(0);
            }
        });

    #[cfg(target_os = "windows")]
    let tray = {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

        tray.show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    show_main_window(tray.app_handle());
                }
            })
    };

    tray.build(app)?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn install_tray(_app: &tauri::App) -> tauri::Result<()> {
    Ok(())
}

#[tauri::command]
fn ping() -> &'static str {
    "ok"
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LogInfo {
    directory: String,
    current_file: String,
}

fn runtime_log_info(app: &tauri::AppHandle) -> Result<LogInfo, String> {
    let directory = app
        .path()
        .app_log_dir()
        .map_err(|error| format!("Cannot resolve the Axonkey log directory: {error}"))?;
    let current_file = directory.join(format!("{RUNTIME_LOG_FILE_BASENAME}.log"));
    Ok(LogInfo {
        directory: directory.to_string_lossy().into_owned(),
        current_file: current_file.to_string_lossy().into_owned(),
    })
}

#[tauri::command]
fn get_log_info(app: tauri::AppHandle) -> Result<LogInfo, String> {
    runtime_log_info(&app)
}

#[tauri::command]
fn open_log_directory(app: tauri::AppHandle) -> Result<LogInfo, String> {
    let info = runtime_log_info(&app)?;
    let directory = std::path::Path::new(&info.directory);
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Cannot create the Axonkey log directory: {error}"))?;

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer.exe")
            .arg(directory)
            .spawn()
            .map_err(|error| format!("Cannot open the Axonkey log directory: {error}"))?;
    }

    #[cfg(target_os = "macos")]
    {
        // The bundle identifier makes the log directory end in `.app`.
        // Opening it launches it as an application; reveal a log inside instead.
        let current_file = std::path::Path::new(&info.current_file);
        let reveal_target = if current_file.is_file() {
            current_file
        } else {
            directory
        };
        let output = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(reveal_target)
            .output()
            .map_err(|error| format!("Cannot open the Axonkey log directory: {error}"))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Cannot open the Axonkey log directory ({}): {}",
                output.status,
                detail.trim(),
            ));
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = directory;
        return Err("Opening the Axonkey log directory is not supported on this platform".into());
    }

    Ok(info)
}

#[tauri::command]
fn get_platform() -> &'static str {
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    return "unsupported";
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DriverActionResult {
    log_path: String,
    exit_code: i32,
    reboot_required: Option<bool>,
    outcome: String,
    message: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverInstallerComponentStatus {
    packages: Option<u32>,
    service: Option<String>,
    enabled: Option<bool>,
    ready: Option<bool>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverInstallerDevice {
    instance_id: String,
    present: bool,
    started: bool,
    driver_bound: bool,
    reboot_required: bool,
    problem: u32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverInstallerStatus {
    complete: bool,
    reboot_required: Option<bool>,
    hid: DriverInstallerComponentStatus,
    microphone: DriverInstallerComponentStatus,
    devices: Option<Vec<DriverInstallerDevice>>,
    errors: Vec<serde_json::Value>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverInstallerReport {
    schema_version: u32,
    action: String,
    outcome: String,
    exit_code: i32,
    reboot_required: Option<bool>,
    message: String,
    status: Option<DriverInstallerStatus>,
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum WindowsServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowsServiceStatus {
    state: String,
    process_id: u32,
    exit_code: u32,
    #[cfg(target_os = "windows")]
    rpc: Option<service_rpc::ServiceRpcStatus>,
}

#[cfg(target_os = "windows")]
fn windows_service_resource(
    resource_dir: &std::path::Path,
    relative: &str,
    source_relative: &str,
) -> Result<std::path::PathBuf, String> {
    let mut candidates = vec![resource_dir.join(relative)];
    let manifest_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "Cannot resolve the repository root".to_string())?;
    candidates.push(manifest_root.join(source_relative));
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join(source_relative));
        candidates.push(current.join("src-tauri").join(source_relative));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("缺少 Windows 服务组件：{source_relative}"))
}

#[cfg(target_os = "windows")]
fn powershell_path() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

#[cfg(target_os = "windows")]
fn query_windows_service() -> Result<WindowsServiceStatus, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = r#"$service = Get-CimInstance -ClassName Win32_Service -Filter "Name='AxonkeyService'" -ErrorAction Stop; $result = if ($null -eq $service) { [pscustomobject]@{ state = 'notInstalled'; processId = 0; exitCode = 0 } } else { [pscustomobject]@{ state = [string]$service.State; processId = [int]$service.ProcessId; exitCode = [uint32]$service.ExitCode } }; $result | ConvertTo-Json -Compress"#;
    let output = std::process::Command::new(powershell_path())
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
        .map_err(|error| format!("无法查询 AxonkeyService：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "无法查询 AxonkeyService：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("无法解析 AxonkeyService 状态：{error}"))?;
    let raw_state = value
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown");
    let state = match raw_state {
        "Running" => "running",
        "Stopped" => "stopped",
        "Start Pending" => "startPending",
        "Stop Pending" => "stopPending",
        "Paused" => "paused",
        "Delete Pending" => "deletePending",
        "notInstalled" => "notInstalled",
        _ => "unknown",
    };
    Ok(WindowsServiceStatus {
        state: state.into(),
        process_id: value
            .get("processId")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        exit_code: value
            .get("exitCode")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32,
        rpc: None,
    })
}

#[cfg(target_os = "windows")]
fn run_windows_service_action(
    resource_dir: &std::path::Path,
    action: WindowsServiceAction,
) -> Result<WindowsServiceStatus, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = windows_service_resource(
        resource_dir,
        "scripts/manage-windows-service.ps1",
        "scripts/manage-windows-service.ps1",
    )?;
    let action_name = match action {
        WindowsServiceAction::Install => "Install",
        WindowsServiceAction::Uninstall => "Uninstall",
        WindowsServiceAction::Start => "Start",
        WindowsServiceAction::Stop => "Stop",
    };
    let mut command = std::process::Command::new(powershell_path());
    command
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args(["-Action", action_name]);
    if matches!(action, WindowsServiceAction::Install) {
        let executable = windows_service_resource(
            resource_dir,
            "windows/service/AxonkeyService.exe",
            "windows/service/dist/AxonkeyService.exe",
        )?;
        command.args(["-ServiceExecutable"]).arg(executable);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法启动 Windows 服务管理：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!(
                "服务操作失败（退出码 {}）。",
                output.status.code().unwrap_or(1)
            )
        } else {
            detail
        });
    }
    query_windows_service()
}

#[tauri::command]
async fn get_windows_service_status(
    #[cfg(target_os = "windows")] rpc: tauri::State<'_, service_rpc::ServiceConnection>,
) -> Result<WindowsServiceStatus, String> {
    #[cfg(target_os = "windows")]
    {
        let mut status = tauri::async_runtime::spawn_blocking(query_windows_service)
            .await
            .map_err(|error| format!("服务查询失败：{error}"))??;
        status.rpc = Some(rpc.status());
        return Ok(status);
    }
    #[cfg(not(target_os = "windows"))]
    Err("服务管理仅支持 Windows。".into())
}

#[tauri::command]
async fn get_windows_service_rpc_status(
    #[cfg(target_os = "windows")] rpc: tauri::State<'_, service_rpc::ServiceConnection>,
) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        return rpc
            .get_service_status()
            .await
            .map_err(|error| format!("无法读取 AxonkeyService 功能状态：{error}"));
    }
    #[cfg(not(target_os = "windows"))]
    Err("服务状态仅支持 Windows。".into())
}

#[tauri::command]
async fn set_windows_service_status(
    enabled: bool,
    #[cfg(target_os = "windows")] rpc: tauri::State<'_, service_rpc::ServiceConnection>,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        return rpc
            .set_service_status(enabled)
            .await
            .map_err(|error| format!("无法更新 AxonkeyService 功能状态：{error}"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled;
        Err("服务状态仅支持 Windows。".into())
    }
}

#[tauri::command]
async fn get_audio_gain(
    app: tauri::AppHandle,
) -> Result<i16, String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        return app
            .state::<service_rpc::ServiceConnection>()
            .get_audio_gain()
            .await
            .map_err(|error| format!("无法读取 AxonkeyService 音频增益：{error}"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("音频增益服务读取仅支持 Windows。".into())
    }
}

#[tauri::command]
async fn manage_windows_service(
    app: tauri::AppHandle,
    action: WindowsServiceAction,
    #[cfg(target_os = "windows")] rpc: tauri::State<'_, service_rpc::ServiceConnection>,
) -> Result<WindowsServiceStatus, String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let resource_dir = app
            .path()
            .resource_dir()
            .map_err(|error| format!("Cannot resolve bundled resources: {error}"))?;
        let mut status = tauri::async_runtime::spawn_blocking(move || {
            run_windows_service_action(&resource_dir, action)
        })
        .await
        .map_err(|error| format!("服务操作失败：{error}"))??;
        status.rpc = Some(rpc.status());
        return Ok(status);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, action);
        Err("服务管理仅支持 Windows。".into())
    }
}

#[cfg(target_os = "windows")]
fn driver_log_path(_driver: &str, action: &str) -> Result<std::path::PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "Cannot locate the Windows local application data directory".to_string())?;
    let directory = std::path::PathBuf::from(local_app_data)
        .join("Axonkey")
        .join("logs");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create the driver log directory: {error}"))?;
    Ok(directory.join(format!("driver-suite-{action}.log")))
}

#[cfg(target_os = "windows")]
fn find_driver_installer(resource_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let mut roots = vec![resource_dir.to_path_buf()];
    if let Ok(current) = std::env::current_dir() {
        roots.push(current.clone());
        roots.push(current.join("src-tauri"));
        roots.push(current.join("windows"));
        if let Some(parent) = current.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            roots.push(directory.to_path_buf());
        }
    }

    for root in roots {
        for candidate in [
            root.join("driver").join("QuarborAxonkeyDriverInstaller.exe"),
            root.join("windows").join("driver").join("QuarborAxonkeyDriverInstaller.exe"),
            root.join("QuarborAxonkeyDriverInstaller.exe"),
        ] {
            if candidate.is_file() {
                return candidate
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve the driver installer: {error}"));
            }
        }
    }

    Err("QuarborAxonkeyDriverInstaller.exe was not found".into())
}

#[cfg(target_os = "windows")]
fn driver_status_output_path() -> Result<std::path::PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("Cannot create a driver status file name: {error}"))?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "axonkey-driver-status-{}-{stamp}.json",
        std::process::id()
    )))
}

#[cfg(target_os = "windows")]
fn read_driver_installer_status(
    resource_dir: &std::path::Path,
) -> Result<DriverInstallerReport, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let installer = find_driver_installer(resource_dir)?;
    let output_path = driver_status_output_path()?;
    let process = std::process::Command::new(&installer)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--status")
        .arg("--output")
        .arg(&output_path)
        .status()
        .map_err(|error| format!("Cannot launch the driver status query: {error}"))?;
    let raw = std::fs::read_to_string(&output_path).map_err(|error| {
        format!(
            "Driver status query returned {} without a result file: {error}",
            process
        )
    });
    let _ = std::fs::remove_file(&output_path);
    let raw = raw?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("Cannot parse the driver status result: {error}"))
}

#[cfg(target_os = "macos")]
fn macos_driver_log_path(action: &str) -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "Cannot locate the macOS home directory".to_string())?;
    let directory = std::path::PathBuf::from(home)
        .join("Library")
        .join("Logs")
        .join("Axonkey");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create the driver log directory: {error}"))?;
    Ok(directory.join(format!("miremotev-{action}.log")))
}

#[cfg(target_os = "macos")]
fn find_macos_driver_package(
    action: &str,
    resource_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let file_name = match action {
        "install" => "MiRemoteV2ch-Install.pkg",
        "uninstall" => "MiRemoteV2ch-Uninstall.pkg",
        _ => return Err("Unsupported driver action".into()),
    };
    let mut roots = vec![resource_dir.to_path_buf()];
    if let Ok(current) = std::env::current_dir() {
        roots.push(current.clone());
        roots.push(current.join("src-tauri"));
        if let Some(parent) = current.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    for root in roots {
        for candidate in [
            root.join(file_name),
            root.join("macos").join(file_name),
            root.join("resources").join("macos").join(file_name),
            root.join("src-tauri")
                .join("resources")
                .join("macos")
                .join(file_name),
        ] {
            if candidate.is_file() {
                return candidate
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve the driver package: {error}"));
            }
        }
    }
    Err(format!(
        "{file_name} was not found. Run `make build-macos-audio` before testing installation."
    ))
}

#[cfg(target_os = "macos")]
fn run_macos_driver_action(
    action: &str,
    resource_dir: &std::path::Path,
) -> Result<DriverActionResult, String> {
    let package = find_macos_driver_package(action, resource_dir)?;
    let log_path = macos_driver_log_path(action)?;
    let script = r#"on run argv
set packagePath to item 1 of argv
return do shell script "/usr/sbin/installer -pkg " & quoted form of packagePath & " -target /" with administrator privileges
end run"#;
    let output = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg(&package)
        .output()
        .map_err(|error| format!("Cannot launch the macOS driver installer: {error}"))?;
    let mut log = format!(
        "Axonkey MiRemoteV 2ch {action}\nPackage: {}\n",
        package.display()
    );
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    std::fs::write(&log_path, log)
        .map_err(|error| format!("Cannot write the driver log: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "MiRemoteV 2ch {action} was cancelled or failed. Log: {}",
            log_path.display()
        ));
    }
    Ok(DriverActionResult {
        log_path: log_path.to_string_lossy().into_owned(),
        exit_code: 0,
        reboot_required: Some(false),
        outcome: "success".into(),
        message: format!("MiRemoteV 2ch {action} completed."),
    })
}

#[cfg(target_os = "windows")]
fn run_driver_action(
    _driver: &str,
    action: &str,
    resource_dir: &std::path::Path,
) -> Result<DriverActionResult, String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let log_path = driver_log_path("input", action)?;
    let started = format!(
        "Axonkey Quarbor driver suite {action} requested: {:?}\r\n",
        std::time::SystemTime::now()
    );
    std::fs::write(&log_path, started)
        .map_err(|error| format!("Cannot initialize the driver log: {error}"))?;

    let installer = find_driver_installer(resource_dir).map_err(|error| {
        let _ = std::fs::write(&log_path, format!("ERROR: {error}\r\n"));
        format!("{error}. Log: {}", log_path.display())
    })?;
    let status = std::process::Command::new(&installer)
        .creation_flags(CREATE_NO_WINDOW)
        .arg(format!("--{action}"))
        .status()
        .map_err(|error| {
            let message = format!("Cannot launch the driver installer: {error}");
            let _ = std::fs::write(&log_path, format!("ERROR: {message}\r\n"));
            format!("{message}. Log: {}", log_path.display())
        })?;

    let exit_code = status.code().unwrap_or(1);
    let reboot_required = exit_code == 3010;
    let outcome = if exit_code == 0 {
        "success"
    } else if reboot_required {
        "reboot_required"
    } else {
        "failed"
    };
    let message = if exit_code == 0 {
        "Quarbor 驱动安装器已完成操作。"
    } else if reboot_required {
        "Quarbor 驱动安装器已完成操作，但需要重启 Windows。"
    } else if exit_code == 1223 {
        "已取消管理员授权。"
    } else {
        "Quarbor 驱动安装器执行失败。"
    };
    let log_entry = format!(
        "ExitCode: {exit_code}\r\nOutcome: {outcome}\r\nMessage: {message}\r\n"
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, log_entry.as_bytes()));
    if exit_code != 0 && !reboot_required {
        return Err(format!(
            "Quarbor driver {action} failed with exit code {exit_code}. Log: {}",
            log_path.display()
        ));
    }

    Ok(DriverActionResult {
        log_path: log_path.to_string_lossy().into_owned(),
        exit_code,
        reboot_required: Some(reboot_required),
        outcome: outcome.into(),
        message: message.into(),
    })
}

#[tauri::command]
async fn probe_driver_installer(
    _app: tauri::AppHandle,
) -> Result<DriverInstallerReport, String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let resource_dir = _app
            .path()
            .resource_dir()
            .map_err(|error| format!("Cannot resolve bundled resources: {error}"))?;
        return tauri::async_runtime::spawn_blocking(move || {
            read_driver_installer_status(&resource_dir)
        })
        .await
        .map_err(|error| format!("Driver status task failed unexpectedly: {error}"))?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = _app;
        Err("The Quarbor driver installer is only available on Windows".into())
    }
}

#[tauri::command]
async fn launch_driver_action(
    _app: tauri::AppHandle,
    driver: String,
    action: String,
) -> Result<DriverActionResult, String> {
    if !matches!(driver.as_str(), "input" | "audio") {
        return Err("Unsupported driver kind".into());
    }
    if !matches!(action.as_str(), "install" | "uninstall") {
        return Err("Unsupported driver action".into());
    }
    log::info!(target: "axonkey::runtime", "Driver action requested: {driver}/{action}");

    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let resource_dir = _app
            .path()
            .resource_dir()
            .map_err(|error| format!("Cannot resolve bundled resources: {error}"))?;
        tauri::async_runtime::spawn_blocking(move || {
            run_driver_action(&driver, &action, &resource_dir)
        })
        .await
        .map_err(|error| format!("Driver task failed unexpectedly: {error}"))?
    }

    #[cfg(target_os = "macos")]
    {
        if driver != "audio" {
            return Err("macOS uses system permissions instead of an input driver".into());
        }
        let resource_dir = _app
            .path()
            .resource_dir()
            .map_err(|error| format!("Cannot resolve bundled resources: {error}"))?;
        let action_for_task = action.clone();
        _app.state::<AudioService>().pause();
        let task_result = tauri::async_runtime::spawn_blocking(move || {
            run_macos_driver_action(&action_for_task, &resource_dir)
        })
        .await;
        _app.state::<AudioService>().resume();
        let result =
            task_result.map_err(|error| format!("Driver task failed unexpectedly: {error}"))??;
        Ok(result)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = _app;
        let _ = driver;
        let _ = action;
        Err("Driver installation is only supported on Windows and macOS".into())
    }
}

#[tauri::command]
fn open_windows_settings(page: String) -> Result<(), String> {
    let uri = match page.as_str() {
        "sound" => "ms-settings:sound",
        _ => return Err("Unsupported settings page".into()),
    };
    log::info!(target: "axonkey::runtime", "Opening Windows settings page: {page}");

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(uri)
            .spawn()
            .map_err(|error| format!("Cannot open Windows settings: {error}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = uri;
        Err("Windows settings are only available on Windows".into())
    }
}

#[tauri::command]
fn open_system_settings(page: String) -> Result<(), String> {
    log::info!(target: "axonkey::runtime", "Opening system settings page: {page}");
    #[cfg(target_os = "windows")]
    {
        return open_windows_settings(page);
    }

    #[cfg(target_os = "macos")]
    {
        let uri = match page.as_str() {
            "bluetooth" => "x-apple.systempreferences:com.apple.BluetoothSettings",
            "sound" => "x-apple.systempreferences:com.apple.Sound-Settings.extension",
            "inputMonitoring" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            "accessibility" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            _ => return Err("Unsupported settings page".into()),
        };
        std::process::Command::new("open")
            .arg(uri)
            .spawn()
            .map_err(|error| format!("Cannot open macOS System Settings: {error}"))?;
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = page;
        Err("System settings are not supported on this platform".into())
    }
}

#[tauri::command]
fn set_permission_helper_mode(
    app: tauri::AppHandle,
    state: tauri::State<'_, PermissionHelperWindowState>,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let window = app
            .get_webview_window(MAIN_WINDOW_LABEL)
            .ok_or_else(|| "Main window is unavailable".to_string())?;
        apply_permission_helper_mode(&window, &state, enabled)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = state;
        let _ = enabled;
        Err("Permission helper mode is only available on macOS".into())
    }
}

#[tauri::command]
fn reveal_current_app() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Cannot locate the current executable: {error}"))?;
        let app_bundle = app_bundle_for_executable(&executable).ok_or_else(|| {
            "Axonkey.app was not found. Install or open the packaged app first.".to_string()
        })?;
        std::process::Command::new("open")
            .arg("-R")
            .arg(app_bundle)
            .spawn()
            .map_err(|error| format!("Cannot reveal Axonkey.app in Finder: {error}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    Err("Revealing the current app is only available on macOS".into())
}

#[tauri::command]
fn request_macos_permission(kind: String) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let result = InputService::request_permission(&kind);
        match &result {
            Ok(granted) => {
                log::info!(target: "axonkey::runtime", "macOS permission request completed: kind={kind}, granted={granted}")
            }
            Err(error) => {
                log::warn!(target: "axonkey::runtime", "macOS permission request failed: kind={kind}, error={error}")
            }
        }
        result
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        Err("macOS permissions are only available on macOS".into())
    }
}

#[tauri::command]
fn open_external_page(page: String) -> Result<(), String> {
    let url = match page.as_str() {
        "github" => "https://github.com/leowzz/axonkey",
        "releases" => "https://github.com/leowzz/axonkey/releases/latest",
        _ => return Err("Unsupported external page".into()),
    };
    log::info!(target: "axonkey::runtime", "Opening external page: {page}");

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(url)
            .spawn()
            .map_err(|error| format!("Cannot open the external page: {error}"))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|error| format!("Cannot open the external page: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("Cannot open the external page in the default browser".into())
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = url;
        Err("External pages are only supported on macOS and Windows".into())
    }
}

#[derive(serde::Serialize)]
struct SystemProbe {
    platform: &'static str,
    rc003_connected: bool,
    input_backend_ready: bool,
    input_backend_error: Option<String>,
    device_hardware_id: Option<String>,
    input_monitoring_granted: Option<bool>,
    input_authorization_stale: Option<bool>,
    accessibility_granted: Option<bool>,
    capture_active: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowsDeviceInfo {
    instance_id: String,
    endpoint_path: String,
    driver_mounted: bool,
    input_blocked: bool,
    data_forward_enabled: bool,
    connected: bool,
    battery_level: Option<u32>,
    description_name: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowsDevicesProbe {
    service_available: bool,
    device: Option<WindowsDeviceInfo>,
    error: Option<String>,
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn probe_rc003_battery_level(app: tauri::AppHandle) -> Option<u8> {
    use tauri::Manager;

    app.state::<AudioService>().status().battery_level
}

#[tauri::command]
async fn get_windows_devices(
    #[cfg(target_os = "windows")] rpc: tauri::State<'_, service_rpc::ServiceConnection>,
) -> Result<WindowsDevicesProbe, String> {
    #[cfg(target_os = "windows")]
    {
        match rpc.get_devices().await {
            Ok(mut devices) => Ok(WindowsDevicesProbe {
                service_available: true,
                device: devices.drain(..).next().map(|device| WindowsDeviceInfo {
                    instance_id: device.instance_id,
                    endpoint_path: device.endpoint_path,
                    driver_mounted: device.driver_mounted,
                    input_blocked: device.input_blocked,
                    data_forward_enabled: device.data_forward_enabled,
                    connected: device.connected,
                    battery_level: device.battery_level,
                    description_name: device.description_name,
                }),
                error: None,
            }),
            Err(service_rpc::GetDevicesError::ServiceUnavailable(error)) => Ok(WindowsDevicesProbe {
                service_available: false,
                device: None,
                error: Some(error.to_string()),
            }),
            Err(service_rpc::GetDevicesError::Request(error)) => Ok(WindowsDevicesProbe {
                service_available: true,
                device: None,
                error: Some(error.to_string()),
            }),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("设备服务仅支持 Windows。".into())
    }
}

#[tauri::command]
fn probe_audio_available(
    app: tauri::AppHandle,
    audio_service: tauri::State<'_, AudioService>,
) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let _ = audio_service;
        return Ok(app
            .state::<service_rpc::ServiceConnection>()
            .audio_test_state()
            .0
            .driver_installed);
    }

    #[cfg(target_os = "macos")]
    {
        let _ = app;
        Ok(audio_service.status().driver_installed)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (app, audio_service);
        Ok(false)
    }
}

#[tauri::command]
fn probe_audio_state(
    app: tauri::AppHandle,
    audio_service: tauri::State<'_, AudioService>,
) -> AudioServiceStatus {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let _ = audio_service;
        return app.state::<service_rpc::ServiceConnection>().audio_test_state().0;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        audio_service.refresh();
        audio_service.status()
    }
}

#[tauri::command]
fn get_audio_test_state(
    app: tauri::AppHandle,
    audio_service: tauri::State<'_, AudioService>,
) -> (AudioServiceStatus, audio_service::AudioLevel) {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        let _ = audio_service;
        return app.state::<service_rpc::ServiceConnection>().audio_test_state();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        (audio_service.status(), audio_service.level())
    }
}

#[tauri::command]
async fn set_audio_gain(
    gain: i16,
    app: tauri::AppHandle,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;

        return app
            .state::<service_rpc::ServiceConnection>()
            .set_audio_gain(gain)
            .await
            .map_err(|error| format!("无法更新 AxonkeyService 音频增益：{error}"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        use tauri::Manager;

        app.state::<AudioService>().set_gain_db(gain)
    }
}

#[tauri::command]
async fn probe_rc003_connected(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;

    if app.state::<InputService>().status().device_connected {
        return Ok(true);
    }

    #[cfg(target_os = "macos")]
    {
        let audio_service = app.state::<AudioService>();
        audio_service.refresh();
        return Ok(rc003_connected(
            false,
            audio_service.status().bluetooth_connected,
        ));
    }

    #[cfg(target_os = "windows")]
    {
        let _ = app;
        Ok(false)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok(false)
    }
}

#[tauri::command]
fn update_input_settings(
    settings: NativeSettings,
    input_service: tauri::State<'_, InputService>,
    mouse_service: tauri::State<'_, MouseService>,
) -> Result<(), String> {
    log::debug!(target: "axonkey::runtime", "Applying input settings from frontend");
    input_service.update_settings(settings.clone())?;
    mouse_service.update_settings(&settings)
}

#[tauri::command]
fn write_mapping_file(path: String, content: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("Mapping file path is empty".into());
    }
    let result = std::fs::write(&path, content)
        .map_err(|error| format!("Cannot save mapping file: {error}"));
    match &result {
        Ok(()) => log::info!(target: "axonkey::runtime", "Mapping file exported successfully"),
        Err(error) => log::warn!(target: "axonkey::runtime", "Mapping file export failed: {error}"),
    }
    result
}

#[tauri::command]
async fn probe_system_state(app: tauri::AppHandle) -> Result<SystemProbe, String> {
    use tauri::Manager;

    let input_status = app.state::<InputService>().status();
    log::debug!(target: "axonkey::runtime", "Running system state probe");

    #[cfg(target_os = "windows")]
    {
        Ok(SystemProbe {
            platform: "windows",
            rc003_connected: input_status.device_connected,
            input_backend_ready: input_status.backend_ready,
            input_backend_error: input_status.error,
            device_hardware_id: input_status.hardware_id,
            input_monitoring_granted: None,
            input_authorization_stale: None,
            accessibility_granted: None,
            capture_active: input_status.capture_active,
        })
    }

    #[cfg(target_os = "macos")]
    {
        let audio_service = app.state::<AudioService>();
        audio_service.refresh();
        let audio_status = audio_service.status();
        Ok(SystemProbe {
            platform: "macos",
            rc003_connected: rc003_connected(
                input_status.device_connected,
                audio_status.bluetooth_connected,
            ),
            input_backend_ready: input_status.backend_ready,
            input_backend_error: input_status.error,
            device_hardware_id: input_status.hardware_id,
            input_monitoring_granted: input_status.input_monitoring_granted,
            input_authorization_stale: Some(input_status.input_monitoring_open_denied),
            accessibility_granted: input_status.accessibility_granted,
            capture_active: input_status.capture_active,
        })
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok(SystemProbe {
            platform: "unsupported",
            rc003_connected: false,
            input_backend_ready: input_status.backend_ready,
            input_backend_error: input_status.error,
            device_hardware_id: input_status.hardware_id,
            input_monitoring_granted: None,
            input_authorization_stale: None,
            accessibility_granted: None,
            capture_active: false,
        })
    }
}

fn rc003_connected(input_connected: bool, bluetooth_connected: bool) -> bool {
    input_connected || bluetooth_connected
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir {
                        file_name: Some(RUNTIME_LOG_FILE_BASENAME.into()),
                    }),
                ])
                .max_file_size(RUNTIME_LOG_MAX_BYTES)
                .rotation_strategy(RotationStrategy::KeepSome(RUNTIME_LOG_KEEP_FILES))
                .timezone_strategy(TimezoneStrategy::UseLocal)
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args_request_autostart(args) {
                log::info!(target: "axonkey::runtime", "Existing instance received an autostart launch");
                return;
            }
            log::info!(target: "axonkey::runtime", "Existing instance requested focus");
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .setup(|app| {
            use tauri::Manager;

            let default_panic_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic| {
                log::error!(target: "axonkey::panic", "Unhandled Rust panic: {panic}");
                default_panic_hook(panic);
            }));
            log::info!(
                target: "axonkey::runtime",
                "Axonkey {} starting on {} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
            );
            match runtime_log_info(app.handle()) {
                Ok(info) => log::info!(
                    target: "axonkey::runtime",
                    "Runtime log file: {} (max {} bytes, {} rotated files kept)",
                    info.current_file,
                    RUNTIME_LOG_MAX_BYTES,
                    RUNTIME_LOG_KEEP_FILES,
                ),
                Err(error) => log::warn!(target: "axonkey::runtime", "Cannot resolve runtime log path: {error}"),
            }
            if let Err(error) = initialize_autostart(app.handle()) {
                log::warn!(target: "axonkey::runtime", "Cannot initialize autostart: {error}");
            }
            app.manage(AudioService::start());
            app.manage(InputService::start());
            app.manage(MouseService::start());
            #[cfg(target_os = "windows")]
            app.manage(service_rpc::ServiceConnection::start());
            app.manage(PermissionHelperWindowState::default());
            app.state::<InputService>()
                .set_event_app(app.handle().clone());
            if let Err(error) = install_tray(app) {
                log::error!(target: "axonkey::runtime", "Failed to install the system tray: {error}");
                return Err(error.into());
            }
            if launched_from_autostart() {
                #[cfg(target_os = "macos")]
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                log::info!(target: "axonkey::runtime", "Autostart launch will remain in the background");
            } else {
                show_main_window(app.handle());
            }
            log::info!(target: "axonkey::runtime", "Axonkey startup completed");
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != MAIN_WINDOW_LABEL {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                log::debug!(target: "axonkey::runtime", "Main window hidden after close request");

                #[cfg(target_os = "macos")]
                let _ = window
                    .app_handle()
                    .set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            get_platform,
            get_log_info,
            open_log_directory,
            launch_driver_action,
            probe_driver_installer,
            get_windows_service_status,
            get_windows_service_rpc_status,
            set_windows_service_status,
            manage_windows_service,
            open_windows_settings,
            open_system_settings,
            set_permission_helper_mode,
            reveal_current_app,
            open_external_page,
            request_macos_permission,
            probe_system_state,
            probe_audio_available,
            probe_audio_state,
            get_audio_test_state,
            get_audio_gain,
            set_audio_gain,
            probe_rc003_connected,
            #[cfg(target_os = "macos")]
            probe_rc003_battery_level,
            get_windows_devices,
            update_input_settings,
            write_mapping_file,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Axonkey")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<MouseService>().shutdown();
            }
            #[cfg(windows)]
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<InputService>().shutdown();
            }
            #[cfg(not(windows))]
            let _ = (app, event);
        });
}

#[cfg(test)]
mod tests {
    use super::{app_bundle_for_executable, args_request_autostart, rc003_connected};

    #[test]
    fn recognizes_only_the_autostart_launch_argument() {
        assert!(args_request_autostart(["axonkey", "--autostart"]));
        assert!(args_request_autostart([
            "axonkey",
            "--other",
            "--autostart",
        ]));
        assert!(!args_request_autostart(["axonkey"]));
        assert!(!args_request_autostart(["axonkey", "--autostarted"]));
    }

    #[test]
    fn macos_connection_uses_bluetooth_when_hid_is_not_visible() {
        assert!(rc003_connected(false, true));
        assert!(rc003_connected(true, false));
        assert!(!rc003_connected(false, false));
    }

    #[test]
    fn finds_the_packaged_app_bundle_from_its_executable() {
        let executable = std::path::Path::new("/Applications/Axonkey.app/Contents/MacOS/axonkey");
        assert_eq!(
            app_bundle_for_executable(executable),
            Some(std::path::PathBuf::from("/Applications/Axonkey.app"))
        );
        assert_eq!(
            app_bundle_for_executable(std::path::Path::new("/tmp/axonkey")),
            None
        );
    }
}
