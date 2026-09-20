//! Opens the standalone Windows driver installer; installation stays in its UI.

#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
fn find_installer(resource_dir: &Path, development_root: Option<&Path>) -> Result<PathBuf, String> {
    const RELATIVE_PATH: &str = "windows/driver/QuarborAxonkeyDriverInstaller.exe";
    // A debug build uses the installer in this checkout. Release builds only use
    // bundled resources, never a binary found in the current working directory.
    let candidate = development_root
        .map(|root| root.join(RELATIVE_PATH))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| resource_dir.join(RELATIVE_PATH));
    if !candidate.is_file() {
        return Err(format!(
            "未找到驱动安装器，请重新安装 Axonkey。路径：{}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

#[tauri::command]
pub async fn launch_driver_installer(_app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;
        let resource_dir = _app
            .path()
            .resource_dir()
            .map_err(|error| format!("无法定位驱动安装器资源：{error}"))?;
        let development_root = if cfg!(debug_assertions) {
            Path::new(env!("CARGO_MANIFEST_DIR")).parent()
        } else {
            None
        };
        let installer = find_installer(&resource_dir, development_root)?;
        tauri::async_runtime::spawn_blocking(move || {
            crate::windows_elevation::run_elevated(&installer, None, false).map(|_| ())
        })
        .await
        .map_err(|error| format!("无法启动驱动安装任务：{error}"))?
    }
    #[cfg(not(target_os = "windows"))]
    Err("此驱动安装器仅支持 Windows。".into())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn resolves_bundled_and_development_installers_without_cwd_fallback() {
        let root =
            std::env::temp_dir().join(format!("axonkey installer 测试 {}", std::process::id()));
        let bundled = root.join("resources");
        let development = root.join("checkout");
        let relative = "windows/driver/QuarborAxonkeyDriverInstaller.exe";
        std::fs::create_dir_all(bundled.join("windows/driver")).unwrap();
        std::fs::create_dir_all(development.join("windows/driver")).unwrap();
        std::fs::write(development.join(relative), b"development fixture").unwrap();
        assert!(find_installer(&bundled, None).is_err());
        assert_eq!(
            find_installer(&bundled, Some(&development)).unwrap(),
            development.join(relative)
        );
        std::fs::write(bundled.join(relative), b"bundled fixture").unwrap();
        assert_eq!(
            find_installer(&bundled, None).unwrap(),
            bundled.join(relative)
        );
        assert_eq!(
            find_installer(&bundled, Some(&development)).unwrap(),
            development.join(relative)
        );
        std::fs::remove_file(development.join(relative)).unwrap();
        assert_eq!(
            find_installer(&bundled, Some(&development)).unwrap(),
            bundled.join(relative)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
