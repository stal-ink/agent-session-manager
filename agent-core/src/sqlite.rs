//! 可选的 SQLite 连接工具。
//!
//! 通过 `sqlite` feature 启用，统一设置写连接的 `busy_timeout`。

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 只读连接；库不存在时由 SQLite 报错。
pub fn open_ro(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))
}

/// 可写连接，统一设置 busy_timeout：目标应用刚关闭、WAL 尚未合并时不立即报忙。
pub fn open_rw(path: &Path) -> Result<Connection> {
    let db =
        Connection::open(path).with_context(|| format!("open {} for write", path.display()))?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    Ok(db)
}
