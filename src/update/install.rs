//! Windows 退出后替换 exe 的 helper。
//! helper 只处理本次 PID 与给定的两个路径，不按映像名结束进程。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// 当前 exe 目录是否可写；用于决定能否自动替换。
pub fn can_replace(current_exe: &Path) -> bool {
    let Some(dir) = current_exe.parent() else {
        return false;
    };
    let probe = dir.join(format!(".asm-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 生成等待 PID 退出、备份、替换并在失败时回滚的 cmd 脚本。
///
/// cmd 没有 sleep 命令，脚本用 `ping -n 2 127.0.0.1` 借延时等待约 1 秒；
/// 末尾 `del "%~f0"` 让脚本执行后自删。
pub fn helper_script(pid: u32, current_exe: &Path, new_exe: &Path) -> String {
    let old = current_exe.to_string_lossy();
    let new = new_exe.to_string_lossy();
    format!(
        "@echo off\r\n\
setlocal\r\n\
set \"OLD={old}\"\r\n\
set \"NEW={new}\"\r\n\
set \"BAK=%OLD%.bak\"\r\n\
for /l %%i in (1,1,60) do (\r\n\
  tasklist /FI \"PID eq {pid}\" /NH 2>nul | find \"{pid}\" >nul\r\n\
  if errorlevel 1 goto :replace\r\n\
  ping -n 2 127.0.0.1 >nul\r\n\
)\r\n\
:replace\r\n\
copy /y \"%OLD%\" \"%BAK%\" >nul 2>&1\r\n\
for /l %%i in (1,1,10) do (\r\n\
  copy /y \"%NEW%\" \"%OLD%\" >nul 2>&1\r\n\
  if not errorlevel 1 goto :start\r\n\
  ping -n 2 127.0.0.1 >nul\r\n\
)\r\n\
copy /y \"%BAK%\" \"%OLD%\" >nul 2>&1\r\n\
:start\r\n\
start \"\" \"%OLD%\"\r\n\
del \"%~f0\" >nul 2>&1\r\n\
exit /b\r\n"
    )
}

/// 写入 helper 并返回路径。
pub fn prepare(data_dir: &Path, pid: u32, current_exe: &Path, new_exe: &Path) -> Result<PathBuf> {
    if !current_exe.is_file() {
        bail!("当前程序不存在：{}", current_exe.display());
    }
    if !new_exe.is_file() {
        bail!("已下载的新版本不存在：{}", new_exe.display());
    }
    let dir = data_dir.join("updates").join("app");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建更新目录失败：{}", dir.display()))?;
    let helper = dir.join("update.cmd");
    std::fs::write(&helper, helper_script(pid, current_exe, new_exe))
        .with_context(|| format!("写入更新脚本失败：{}", helper.display()))?;
    Ok(helper)
}

/// 启动 helper；调用方随后应退出 app。
pub fn spawn(helper: &Path) -> Result<()> {
    let mut command = std::process::Command::new("cmd");
    command.args(["/C", &helper.to_string_lossy()]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：隐藏 cmd helper 的控制台窗口。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .with_context(|| format!("启动更新脚本失败：{}", helper.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_script_waits_for_pid_and_backs_up() {
        let script = helper_script(
            1234,
            Path::new(r"C:\Apps\agent-session-manager.exe"),
            Path::new(r"D:\data\updates\agent-session-manager.exe"),
        );
        assert!(script.contains("PID eq 1234"));
        assert!(script.contains(r#"set "OLD=C:\Apps\agent-session-manager.exe""#));
        assert!(script.contains("%OLD%.bak"));
        assert!(script.contains(r#"copy /y "%NEW%" "%OLD%""#));
        assert!(script.contains(r#"start "" "%OLD%""#));
    }

    #[test]
    fn writable_directory_is_detected() {
        let dir = std::env::temp_dir().join(format!("asm-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(can_replace(&dir.join("app.exe")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
