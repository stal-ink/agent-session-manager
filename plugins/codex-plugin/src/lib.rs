//! Codex 适配器：会话索引与正文读取在 `parse`，原生写入走 app-server
//! （`initialize` → `thread/start` → `thread/inject_items` → `thread/name/set`）。

mod parse;
mod writer;

pub use parse::{
    active, backup_paths, find_rollout, is_ours, list, load, needs_rewrite, state_db, Paths, ID,
};
pub use writer::{delete, write};

/// 兼容早期默认 paginated 写出的线程，并补上 Codex UI 读取的 legacy 事件。
pub fn ensure_legacy(id: &str) -> bool {
    let paths = Paths::default();
    if !parse::is_ours(&paths, id) {
        return false;
    }
    let changed = parse::ensure_legacy(&paths, id).unwrap_or(false);
    let _ = writer::augment_history(&paths, id);
    changed
}
