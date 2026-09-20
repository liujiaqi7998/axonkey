//! Windows UAC launch shared by driver and service management.
use std::path::Path;

pub fn run_elevated(
    installer: &Path,
    parameters: Option<&str>,
    wait: bool,
) -> Result<Option<u32>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // Tauri may reuse a worker with a different COM apartment already initialized.
    if initialized.is_err() && initialized != HRESULT(0x80010106u32 as i32) {
        return Err(format!("无法初始化管理员操作：{initialized:?}"));
    }
    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let _com = ComGuard(initialized.is_ok());
    let file: Vec<u16> = installer.as_os_str().encode_wide().chain(Some(0)).collect();
    let directory: Vec<u16> = installer
        .parent()
        .ok_or("管理员程序路径无效")?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let parameters: Vec<u16> = parameters
        .unwrap_or("")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // Finish the UAC launch synchronously; service actions also retain the
        // process handle so their result can be checked before refreshing state.
        fMask: SEE_MASK_NOASYNC
            | SEE_MASK_FLAG_NO_UI
            | if wait { SEE_MASK_NOCLOSEPROCESS } else { 0 },
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: PCWSTR(directory.as_ptr()),
        nShow: if wait { SW_HIDE.0 } else { SW_SHOWNORMAL.0 },
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| {
        if error.code() == HRESULT::from_win32(1223) {
            "已取消管理员授权，可重试。".into()
        } else {
            format!("无法启动管理员程序：{error}")
        }
    })?;
    if !wait {
        return Ok(None);
    }
    if info.hProcess.is_invalid() {
        return Err("管理员操作未返回进程句柄".into());
    }
    struct ProcessHandle(HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let process = ProcessHandle(info.hProcess);
    match unsafe { WaitForSingleObject(process.0, 90_000) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return Err("管理员操作超时，请刷新状态确认结果后再重试。".into()),
        _ => {
            return Err(format!(
                "无法等待管理员操作：{}",
                std::io::Error::last_os_error()
            ))
        }
    }
    let mut code = 0;
    unsafe { GetExitCodeProcess(process.0, &mut code) }
        .map_err(|e| format!("无法读取操作结果：{e}"))?;
    Ok(Some(code))
}
