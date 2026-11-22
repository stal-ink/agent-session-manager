//! 窗口外观与设置持久化：深色标题栏、前置窗口、config.json 主题读写、目录打开。

use agent_session_manager::config::{self, Config};

#[cfg(windows)]
pub fn enable_dark_title_bar(dark: bool) {
    use std::os::windows::ffi::OsStrExt;
    let title: Vec<u16> = std::ffi::OsStr::new("Agent Session Manager")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> *mut core::ffi::c_void;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            attr: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            let on: u32 = if dark { 1 } else { 0 };
            // DWMWA_USE_IMMERSIVE_DARK_MODE = 20。
            DwmSetWindowAttribute(hwnd, 20, &on as *const u32 as *const _, 4);
            // DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2。
            let round: u32 = 2;
            DwmSetWindowAttribute(hwnd, 33, &round as *const u32 as *const _, 4);
        }
    }
}

/// 窗口句柄需窗口已创建，故按重试方式应用标题栏主题（切换后也走这里）。
#[cfg(windows)]
pub fn apply_title_bar(dark: bool, attempts: u32) {
    std::thread::spawn(move || {
        for i in 0..attempts {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            enable_dark_title_bar(dark);
        }
    });
}

/// 读取 Windows 应用主题（AppsUseLightTheme），读不到按深色处理。
#[cfg(windows)]
pub fn system_prefers_dark() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let mut cmd = Command::new("reg");
    cmd.args([
        "query",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "/v",
        "AppsUseLightTheme",
    ]);
    cmd.creation_flags(0x0800_0000);
    let Ok(output) = cmd.output() else {
        return true;
    };
    apps_use_light(&String::from_utf8_lossy(&output.stdout))
        .map(|light| !light)
        .unwrap_or(true)
}

/// AppsUseLightTheme=1 表示系统为浅色；0 表示深色。
fn apps_use_light(output: &str) -> Option<bool> {
    output
        .lines()
        .find(|line| line.contains("AppsUseLightTheme"))
        .and_then(|line| line.split_whitespace().last())
        .map(|value| matches!(value, "0x1" | "1"))
}

#[cfg(not(windows))]
pub fn system_prefers_dark() -> bool {
    true
}

pub fn load_config() -> Config {
    config::load()
}

pub fn save_config(config: &Config) {
    let _ = config::save(config);
}

/// 用资源管理器打开目录；目录不存在时先创建，保证打开必有结果。
pub fn open_path(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .arg(path)
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// 用默认浏览器打开 URL（下载页等）。
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .arg(url)
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(windows)]
pub fn bring_to_front() {
    use std::os::windows::ffi::OsStrExt;
    let title: Vec<u16> = std::ffi::OsStr::new("Agent Session Manager")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> *mut core::ffi::c_void;
        fn ShowWindow(hwnd: *mut core::ffi::c_void, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            // SW_RESTORE = 9：最小化时先还原再置前台。
            ShowWindow(hwnd, 9);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apps_use_light;

    #[test]
    fn apps_use_light_reads_registry_value() {
        let light = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x1\r\n";
        assert_eq!(apps_use_light(light), Some(true));
        let dark = light.replace("0x1", "0x0");
        assert_eq!(apps_use_light(&dark), Some(false));
        let decimal = light.replace("0x1", "1");
        assert_eq!(apps_use_light(&decimal), Some(true));
        assert_eq!(apps_use_light("错误: 系统找不到指定的注册表项或值。"), None);
    }
}
