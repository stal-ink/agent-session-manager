//! 容错 JSONL 读取与原子文件替换。
//!
//! 读取时跳过无法解析的行；写入时先落盘临时文件，再整体替换目标文件。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

#[cfg(windows)]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
    let new: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(tmp, path)
}

/// 逐行读取 UTF-8 jsonl；解析失败的行跳过（遇到非 UTF-8 字节则按
/// `BufRead::lines` 语义报错中止），与各读取器的容错策略一致。
pub fn read_events(path: &Path) -> Result<Vec<Value>> {
    let file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            events.push(value);
        }
    }
    Ok(events)
}

/// 写入临时文件并落盘后整体替换目标。Windows 用 MoveFileExW 替换，
/// 避免先删旧文件再改名之间目标短暂缺失；读取方始终看到完整内容。
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        uuid::Uuid::new_v4().simple()
    ));
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
    }
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("write {}", tmp.display()));
    }
    if let Err(error) = replace_file(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_existing_without_leaving_temp() {
        let dir = std::env::temp_dir().join(format!("asm-jsonl-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, "old").unwrap();

        write_atomic(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
