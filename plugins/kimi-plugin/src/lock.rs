//! Kimi 会话锁：锁文件带 `pid`、`hostname`、`startedAt`，按 Kimi 自身规则判活。
//! 活跃锁仍拒绝写入；已退出进程留下的过期锁不再阻塞。

use anyhow::Result;
use serde::Deserialize;

use super::Paths;

#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockFile {
    pid: u32,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    started_at: u64,
    #[serde(default)]
    client_display_name: Option<String>,
}

pub(super) fn ensure_unlocked(paths: &Paths, action: &str) -> Result<()> {
    let entries = match std::fs::read_dir(&paths.locks) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "读取会话锁目录失败 {}：{error}",
                paths.locks.display()
            ))
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "读取会话锁失败 {}：{error}",
                    path.display()
                ))
            }
        };
        // 解析不了的锁按崩溃残留处理，与 Kimi 的过期锁判定一致。
        let Ok(lock) = serde_json::from_str::<LockFile>(&text) else {
            continue;
        };
        if !is_live(&lock) {
            continue;
        }
        match lock
            .client_display_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            Some(client) => {
                anyhow::bail!("Kimi 正在使用会话（{client}），请先关闭 Kimi 再{action}")
            }
            None => anyhow::bail!("Kimi 正在使用会话，请先关闭 Kimi 再{action}"),
        }
    }
    Ok(())
}

fn is_live(lock: &LockFile) -> bool {
    if lock.pid == 0 {
        return false;
    }
    let local = local_hostname();
    if !lock.hostname.is_empty() && !local.is_empty() && !lock.hostname.eq_ignore_ascii_case(&local)
    {
        // 其他主机的锁无法探活，按占用处理。
        return true;
    }
    if !process_alive(lock.pid) {
        return false;
    }
    // 无时间戳的旧锁在进程仍存活时保守处理为占用。
    lock.started_at == 0 || lock.started_at >= boot_time_ms()
}

fn local_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

#[cfg(windows)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        pub fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn GetLastError() -> u32;
        pub fn GetTickCount64() -> u64;
    }

    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    pub const STILL_ACTIVE: u32 = 259;
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    unsafe {
        let handle = win::OpenProcess(win::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // 拒绝访问说明进程存在，只是无法查询；其余失败按已退出处理。
            return win::GetLastError() == win::ERROR_ACCESS_DENIED;
        }
        let mut code = 0;
        let ok = win::GetExitCodeProcess(handle, &mut code) != 0;
        win::CloseHandle(handle);
        !ok || code == win::STILL_ACTIVE
    }
}

#[cfg(windows)]
fn boot_time_ms() -> u64 {
    now_ms().saturating_sub(unsafe { win::GetTickCount64() })
}

#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(not(windows))]
fn boot_time_ms() -> u64 {
    0
}
