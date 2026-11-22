use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agents::{self, AgentInfo};
use crate::export;
use crate::jsonl;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Native,
    Export,
}

#[derive(Clone)]
pub struct Request {
    pub source: String,
    pub target: String,
    pub source_id: String,
    pub target_cwd: String,
    pub mode: Mode,
}

#[derive(Debug)]
pub struct TargetRunning {
    pub target: String,
}

impl std::fmt::Display for TargetRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} 正在运行，原生转接需要其完全关闭后重试",
            agents::label(&self.target)
        )
    }
}

impl std::error::Error for TargetRunning {}

#[derive(Debug)]
pub enum Outcome {
    Injected { target_id: String },
    Updated { target_id: String },
    Exported { path: PathBuf },
    Already { target_id: String },
}

#[derive(Serialize, Deserialize)]
struct LedgerEntry {
    source: String,
    source_id: String,
    target: String,
    target_id: String,
    ts: i64,
    #[serde(default)]
    source_updated_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct BackupEntry {
    original: String,
    name: String,
    kind: String,
    existed: bool,
}

impl LedgerEntry {
    fn matches(&self, source: &str, source_id: &str, target: &str) -> bool {
        agents::normalize(&self.source) == source
            && self.source_id == source_id
            && agents::normalize(&self.target) == target
    }
}

fn should_retransfer(session_updated_ms: i64, entry: &LedgerEntry) -> bool {
    let baseline = if entry.source_updated_ms > 0 {
        entry.source_updated_ms
    } else {
        entry.ts
    };
    session_updated_ms > baseline
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("agent-session-manager")
}

/// 进程探测桩：测试可注入，None 时使用真实 tasklist 检测。
pub type ProcessProbe = fn(&str) -> Result<bool>;

static PROCESS_PROBE: RwLock<Option<ProcessProbe>> = RwLock::new(None);

/// tasklist 探测进程是否运行。Err 表示探测本身失败，调用方不应据此放行写入。
pub fn process_running_checked(image: &str) -> Result<bool> {
    if let Ok(probe) = PROCESS_PROBE.read() {
        if let Some(probe) = *probe {
            return probe(image);
        }
    }
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .with_context(|| format!("tasklist 检测 {image} 进程失败"))?;
    // tasklist 过滤器不区分大小写，输出按磁盘原始大小写；比对两侧统一小写。
    let needle = image.to_lowercase();
    Ok(String::from_utf8_lossy(&out.stdout)
        .to_lowercase()
        .contains(&needle))
}

/// 便捷封装：探测失败按未运行处理，仅用于打开、关闭等非写入路径。
pub fn process_running(image: &str) -> bool {
    process_running_checked(image).unwrap_or(false)
}

/// 仅供测试注入进程探测桩。
#[doc(hidden)]
pub fn set_process_probe(probe: Option<ProcessProbe>) {
    *PROCESS_PROBE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = probe;
}

/// 写入路径专用：目标运行中返回 TargetRunning，探测失败按错误中止，均不放行。
fn ensure_target_closed(info: &AgentInfo) -> Result<()> {
    match process_running_checked(&info.process) {
        Ok(false) => Ok(()),
        Ok(true) => Err(anyhow::Error::new(TargetRunning {
            target: info.id.to_string(),
        })),
        Err(error) => Err(error.context(format!(
            "无法确认 {} 进程状态，为避免写入活跃存储，转接已中止",
            info.label
        ))),
    }
}

/// 删除路径专用：探测失败同样不放行。
fn ensure_target_closed_for_delete(info: &AgentInfo) -> Result<()> {
    match process_running_checked(&info.process) {
        Ok(false) => Ok(()),
        Ok(true) => anyhow::bail!("{} 正在运行，删除前请先关闭", info.label),
        Err(error) => Err(error.context(format!(
            "无法确认 {} 进程状态，为避免误删活跃会话，删除已中止",
            info.label
        ))),
    }
}

/// 转接设置「仅注入最近压缩之后的消息」：以最近一次压缩边界为起点写目标端。
/// 边界之前的原始消息不再写入；压缩摘要保留（它承载压缩前的上下文），
/// `tail_start_index` 归零——目标端存储里没有被总结的消息。
/// 没有压缩边界时整份注入，与开关关闭行为一致。
fn truncate_to_compaction(session: &mut crate::ir::Session) {
    let Some(compaction) = session.compaction.take() else {
        return;
    };
    let start = compaction.tail_start_index.min(session.messages.len());
    session.messages.drain(0..start);
    session.compaction = Some(crate::ir::Compaction {
        summary_text: compaction.summary_text,
        tail_start_index: 0,
        time_ms: compaction.time_ms,
    });
}

pub fn execute(req: &Request) -> Result<Outcome> {
    let source_info =
        agents::get(&req.source).with_context(|| format!("未知 agent：{}", req.source))?;
    let target_info =
        agents::get(&req.target).with_context(|| format!("未知 agent：{}", req.target))?;
    let session = source_info.adapter.load(&req.source_id)?;

    if req.mode == Mode::Export {
        // 转接的"交接文档"与批量导出的交接文档是同一实现、同一目录、同一设置。
        let path = export::export_with_config(&session, export::Mode::Handoff)?;
        return Ok(Outcome::Exported { path });
    }

    // 转接设置：开启「仅注入最近压缩之后的消息」时，原生注入以最近一次压缩边界为起点。
    let mut session = session;
    if crate::config::load().transfer.compaction_only {
        truncate_to_compaction(&mut session);
    }

    if !target_info.can_write {
        anyhow::bail!("{} 暂不支持原生写入", target_info.label);
    }

    let source = agents::normalize(&req.source);
    let target = agents::normalize(&req.target);
    let ledger_path = data_dir().join("ledger.json");
    let mut ledger: Vec<LedgerEntry> = match fs::read_to_string(&ledger_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(ledger) => ledger,
            Err(_) => {
                // 台账损坏时保留现场重建，避免把旧映射丢进未知状态造成目标端重复。
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let _ = fs::rename(
                    &ledger_path,
                    ledger_path.with_extension(format!("json.corrupt-{stamp}")),
                );
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    };
    let prior = ledger
        .iter()
        .find(|e| e.matches(&source, &req.source_id, &target));
    let overwrite_id = match prior {
        Some(e) => {
            let source_newer = should_retransfer(session.updated_ms, e);
            let target_active = target_info.adapter.active(&e.target_id)?
                && !target_info.adapter.needs_rewrite(&e.target_id);
            if !source_newer && target_active {
                return Ok(Outcome::Already {
                    target_id: e.target_id.clone(),
                });
            }
            Some(e.target_id.clone())
        }
        None => None,
    };

    if target_info.adapter.requires_exclusive() {
        ensure_target_closed(&target_info)?;
    }

    let backup_dir = backup_target(&target_info)?;
    if target_info.adapter.requires_exclusive() {
        // 备份耗时期间进程可能被用户重新启动，写入前复查一遍（TOCTOU）。
        ensure_target_closed(&target_info)?;
    }
    let written = match &overwrite_id {
        Some(old_id) => target_info
            .adapter
            .write(&session, &req.target_cwd, Some(old_id))
            .map(|id| (id, true)),
        None => target_info
            .adapter
            .write(&session, &req.target_cwd, None)
            .map(|id| (id, false)),
    };
    let (target_id, overwritten) = match written {
        Ok(value) => value,
        Err(error) => {
            if !target_info.adapter.restore_on_error() {
                return Err(anyhow::anyhow!(
                    "写入失败：{error:#}（目标端原生写入器未执行整库回滚）"
                ));
            }
            return Err(match restore_target(&target_info.process, &backup_dir) {
                Ok(()) => anyhow::anyhow!("写入失败，已回滚到写入前状态：{error:#}"),
                Err(rollback) => {
                    anyhow::anyhow!("写入失败：{error:#}；自动回滚失败：{rollback:#}")
                }
            });
        }
    };

    let now = crate::ir::now_ms();
    if let Some(e) = ledger
        .iter_mut()
        .find(|e| e.matches(&source, &req.source_id, &target))
    {
        e.source = source.clone();
        e.target = target.clone();
        e.target_id = target_id.clone();
        e.ts = now;
        e.source_updated_ms = session.updated_ms;
    } else {
        ledger.push(LedgerEntry {
            source,
            source_id: req.source_id.clone(),
            target,
            target_id: target_id.clone(),
            ts: now,
            source_updated_ms: session.updated_ms,
        });
    }
    fs::create_dir_all(data_dir())?;
    jsonl::write_atomic(&ledger_path, &serde_json::to_string_pretty(&ledger)?)?;
    Ok(if overwritten {
        Outcome::Updated { target_id }
    } else {
        Outcome::Injected { target_id }
    })
}

pub fn delete_sessions(agent_id: &str, ids: &[String]) -> Result<()> {
    delete_groups(&[(agent_id.to_string(), ids.to_vec())])
}

/// 销毁结果：逐 agent 报告行，failed 表示任一 agent 有失败。
#[derive(Debug)]
pub struct DestroyReport {
    pub lines: Vec<String>,
    pub failed: bool,
}

/// 物理销毁：与软删除并存。流程与 delete_groups 一致——fail-closed 进程检查、
/// 逐 agent 整库备份；区别在于逐 agent 调用 adapter.destroy 物理删除，且单个
/// id 失败后停止该 agent 剩余 id（已销毁的不回滚），继续下一个 agent，结果
/// 逐 agent 汇报一行（如「Qoder：3 条已销毁」「ZCode：2 条已销毁，1 条失败：…」）。
pub fn destroy_groups(groups: &[(String, Vec<String>)]) -> Result<DestroyReport> {
    if groups.is_empty() {
        anyhow::bail!("没有选择会话");
    }
    let mut prepared: Vec<(std::sync::Arc<AgentInfo>, Vec<String>)> = Vec::new();
    for (agent_id, ids) in groups {
        if ids.is_empty() {
            continue;
        }
        let info = agents::get(agent_id).with_context(|| format!("未知 agent：{agent_id}"))?;
        prepared.push((info, ids.clone()));
    }
    if prepared.is_empty() {
        anyhow::bail!("没有选择会话");
    }
    // fail-closed：需要排他的 agent 进程未关闭或探测失败，整体中止。
    for (info, _) in &prepared {
        if info.adapter.requires_exclusive() {
            ensure_target_closed_for_delete(&info)?;
        }
    }
    // 全部备份完成后再开始删除；备份期间进程可能再次启动，删除前复查一遍。
    for (info, _) in &prepared {
        backup_target_unpruned(info)?;
    }
    for (info, _) in &prepared {
        if info.adapter.requires_exclusive() {
            if let Err(error) = ensure_target_closed_for_delete(info) {
                prune_backups();
                return Err(error);
            }
        }
    }

    let mut lines = Vec::new();
    let mut failed = false;
    for (info, ids) in &prepared {
        let mut destroyed = 0usize;
        let mut failure: Option<String> = None;
        for id in ids {
            match info.adapter.destroy(id) {
                Ok(()) => destroyed += 1,
                Err(error) => {
                    // 停止该 agent 剩余 id；已销毁的不回滚，继续下一个 agent。
                    failure = Some(format!("{error:#}"));
                    break;
                }
            }
        }
        match failure {
            None => lines.push(format!("{}：{} 条已销毁", info.label, destroyed)),
            Some(reason) => {
                failed = true;
                lines.push(format!(
                    "{}：{} 条已销毁，{} 条失败：{reason}",
                    info.label,
                    destroyed,
                    ids.len() - destroyed
                ));
            }
        }
    }
    prune_backups();
    Ok(DestroyReport { lines, failed })
}

pub fn garbage_groups() -> Vec<(String, Vec<String>)> {
    agents::all()
        .iter()
        .filter_map(|info| {
            let ids = empty_ids(info.adapter.list().ok()?);
            (!ids.is_empty()).then(|| (info.id.to_string(), ids))
        })
        .collect()
}

fn empty_ids(sessions: Vec<crate::ir::SessionSummary>) -> Vec<String> {
    sessions
        .into_iter()
        .filter(|session| !session.deleted && session.message_count == 0)
        .map(|session| session.id)
        .collect()
}

pub fn delete_groups(groups: &[(String, Vec<String>)]) -> Result<()> {
    if groups.is_empty() {
        anyhow::bail!("没有选择会话");
    }
    let mut prepared = Vec::new();
    for (agent_id, ids) in groups {
        if ids.is_empty() {
            continue;
        }
        let info = agents::get(agent_id).with_context(|| format!("未知 agent：{agent_id}"))?;
        if info.adapter.requires_exclusive() {
            ensure_target_closed_for_delete(&info)?;
        }
        let backup_dir = backup_target_unpruned(&info)?;
        prepared.push((info, ids.clone(), backup_dir));
    }
    if prepared.is_empty() {
        anyhow::bail!("没有选择会话");
    }
    for (info, _, _) in &prepared {
        if info.adapter.requires_exclusive() {
            if let Err(error) = ensure_target_closed_for_delete(info) {
                prune_backups();
                return Err(error);
            }
        }
    }

    for (info, ids, _) in &prepared {
        for id in ids {
            if let Err(error) = info.adapter.delete(id) {
                let mut rollback_errors = Vec::new();
                let mut restored_any = false;
                for (restore_info, _, backup_dir) in prepared.iter().rev() {
                    if !restore_info.adapter.restore_on_error() {
                        continue;
                    }
                    restored_any = true;
                    if let Err(rollback) = restore_target(&restore_info.process, backup_dir) {
                        rollback_errors.push(format!("{}：{rollback}", restore_info.label));
                    }
                }
                prune_backups();
                return Err(if !rollback_errors.is_empty() {
                    anyhow::anyhow!(
                        "{} 删除失败：{error:#}；自动回滚失败：{}",
                        info.label,
                        rollback_errors.join("；")
                    )
                } else if restored_any {
                    anyhow::anyhow!("{} 删除失败，已回滚到删除前状态：{error:#}", info.label)
                } else {
                    anyhow::anyhow!("{} 删除失败：{error:#}", info.label)
                });
            }
        }
    }
    prune_backups();
    Ok(())
}

fn backup_target(target: &AgentInfo) -> Result<PathBuf> {
    let dir = backup_target_unpruned(target)?;
    prune_backups();
    Ok(dir)
}

fn backup_target_unpruned(target: &AgentInfo) -> Result<PathBuf> {
    let files = target.adapter.backup_paths()?;
    let dir = data_dir().join("backups").join(format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        target.id
    ));
    backup_snapshot(&files, &dir)?;
    Ok(dir)
}

fn backup_snapshot(files: &[PathBuf], dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let mut entries = Vec::new();
    for f in files {
        let name = f
            .file_name()
            .context("backup file name")?
            .to_string_lossy()
            .to_string();
        if f.is_dir() {
            copy_dir_all(f, &dir.join(&name))?;
            entries.push(BackupEntry {
                original: f.to_string_lossy().to_string(),
                name,
                kind: "dir".to_string(),
                existed: true,
            });
            continue;
        }
        let existed = f.is_file();
        if existed {
            for suffix in ["", "-wal", "-shm"] {
                let src = with_suffix(f, suffix);
                if src.exists() {
                    let name = src
                        .file_name()
                        .context("backup file name")?
                        .to_string_lossy()
                        .to_string();
                    fs::copy(&src, dir.join(name))?;
                }
            }
        }
        entries.push(BackupEntry {
            original: f.to_string_lossy().to_string(),
            name,
            kind: "file".to_string(),
            existed,
        });
    }
    fs::write(dir.join("manifest.json"), serde_json::to_string(&entries)?)?;
    Ok(())
}

fn restore_target(process: &str, backup: &Path) -> Result<()> {
    match process_running_checked(process) {
        Ok(false) => {}
        Ok(true) => anyhow::bail!("目标应用已启动，未执行自动回滚"),
        Err(error) => return Err(error.context("无法确认目标进程状态，未执行自动回滚")),
    }
    let manifest = fs::read_to_string(backup.join("manifest.json"))
        .context("读取备份清单失败，无法自动回滚")?;
    let entries: Vec<BackupEntry> =
        serde_json::from_str(&manifest).context("解析备份清单失败，无法自动回滚")?;
    // 先确认备份文件齐全，再动目标数据：避免删除目标文件后才发现无法恢复。
    for entry in &entries {
        if entry.kind == "file" && entry.existed && !backup.join(&entry.name).is_file() {
            anyhow::bail!("备份缺少 {}，无法自动回滚", entry.name);
        }
    }
    let mut failures = Vec::new();
    for entry in entries {
        if let Err(error) = restore_entry(backup, &entry) {
            failures.push(format!("{}：{error:#}", entry.original));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("部分内容未能回滚：{}", failures.join("；")))
    }
}

fn restore_entry(backup: &Path, entry: &BackupEntry) -> Result<()> {
    let target_path = PathBuf::from(&entry.original);
    if entry.kind == "dir" {
        remove_any(&target_path)?;
        if entry.existed {
            copy_dir_all(&backup.join(&entry.name), &target_path)?;
        }
        return Ok(());
    }
    remove_file_with_suffix(&target_path)?;
    if entry.existed {
        let source = backup.join(&entry.name);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &target_path)?;
        for suffix in ["-wal", "-shm"] {
            let source = backup.join(with_suffix(Path::new(&entry.name), suffix));
            if source.is_file() {
                fs::copy(&source, with_suffix(&target_path, suffix))?;
            }
        }
    }
    Ok(())
}

/// 给文件路径追加后缀（如 -wal/-shm），使用 OsString 拼接避免非 UTF-8 路径截断。
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_any(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn remove_file_with_suffix(path: &Path) -> Result<()> {
    remove_any(path)?;
    for suffix in ["-wal", "-shm"] {
        let path = with_suffix(path, suffix);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

const KEEP_BACKUPS: usize = 1;

fn prune_backups() {
    prune_backups_in(&data_dir().join("backups"));
}

fn prune_backups_in(root: &Path) {
    let Ok(read) = fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for old in dirs.iter().rev().skip(KEEP_BACKUPS) {
        let _ = fs::remove_dir_all(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 失败时也清理临时目录的守卫。
    struct TempGuard(PathBuf);

    impl TempGuard {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("asm-transfer-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(ts: i64, marker: i64) -> LedgerEntry {
        LedgerEntry {
            source: "qoder".into(),
            source_id: "s".into(),
            target: "zcode".into(),
            target_id: "t".into(),
            ts,
            source_updated_ms: marker,
        }
    }

    fn text_message(text: &str) -> crate::ir::Message {
        crate::ir::Message {
            role: crate::ir::Role::User,
            time_ms: 0,
            blocks: vec![crate::ir::Block::Text(text.into())],
        }
    }

    fn compacted_session(tail_start_index: usize) -> crate::ir::Session {
        crate::ir::Session {
            agent: "zcode".into(),
            id: "s".into(),
            title: String::new(),
            cwd: String::new(),
            created_ms: 0,
            updated_ms: 0,
            messages: vec![
                text_message("a"),
                text_message("b"),
                text_message("c"),
                text_message("d"),
            ],
            compaction: Some(crate::ir::Compaction {
                summary_text: "摘要正文".into(),
                tail_start_index,
                time_ms: 5,
            }),
        }
    }

    #[test]
    fn truncation_keeps_summary_and_tail_only() {
        let mut session = compacted_session(3);
        truncate_to_compaction(&mut session);
        assert_eq!(session.messages.len(), 1);
        assert!(matches!(
            &session.messages[0].blocks[0],
            crate::ir::Block::Text(t) if t == "d"
        ));
        // 摘要保留并归零下标：目标端存储里没有被总结的消息。
        let compaction = session.compaction.expect("摘要必须保留");
        assert_eq!(compaction.summary_text, "摘要正文");
        assert_eq!(compaction.tail_start_index, 0);
        assert_eq!(compaction.time_ms, 5);
    }

    #[test]
    fn truncation_without_compaction_keeps_everything() {
        let mut session = compacted_session(0);
        session.compaction = None;
        truncate_to_compaction(&mut session);
        assert_eq!(session.messages.len(), 4);
        assert!(session.compaction.is_none());
    }

    #[test]
    fn truncation_clamps_out_of_range_boundary() {
        let mut session = compacted_session(99);
        truncate_to_compaction(&mut session);
        assert!(session.messages.is_empty());
        assert_eq!(session.compaction.unwrap().tail_start_index, 0);
    }

    #[test]
    fn garbage_keeps_only_empty_undeleted_sessions() {
        let base = crate::ir::SessionSummary {
            agent: "qoder".into(),
            id: "kept".into(),
            title: String::new(),
            cwd: String::new(),
            created_ms: 0,
            updated_ms: 0,
            message_count: 0,
            archived: false,
            deleted: false,
            workspace_deleted: false,
        };
        let deleted = crate::ir::SessionSummary {
            id: "deleted".into(),
            deleted: true,
            ..base.clone()
        };
        let non_empty = crate::ir::SessionSummary {
            id: "non_empty".into(),
            message_count: 2,
            ..base.clone()
        };
        let ids = empty_ids(vec![base, deleted, non_empty]);
        assert_eq!(ids, vec!["kept".to_string()]);
    }

    #[test]
    fn retransfer_only_when_source_is_newer() {
        assert!(!should_retransfer(1000, &entry(2000, 1000)));
        assert!(should_retransfer(1001, &entry(2000, 1000)));
    }

    #[test]
    fn legacy_ledger_uses_transfer_time_as_marker() {
        assert!(!should_retransfer(1500, &entry(2000, 0)));
        assert!(should_retransfer(2001, &entry(2000, 0)));
    }

    #[test]
    fn restore_rolls_back_files_and_directories() {
        let base = TempGuard::new();
        let target = base.0.join("target");
        let backup = base.0.join("backup");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(target.join("projects")).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(target.join("data.db"), b"old").unwrap();
        std::fs::write(target.join("projects").join("old.txt"), b"old").unwrap();
        backup_snapshot(
            &[
                target.join("data.db"),
                target.join("projects"),
                target.join("new"),
            ],
            &backup,
        )
        .unwrap();

        std::fs::write(target.join("data.db"), b"new").unwrap();
        std::fs::write(target.join("data.db-wal"), b"wal").unwrap();
        std::fs::write(target.join("projects").join("new.txt"), b"new").unwrap();
        std::fs::create_dir_all(target.join("new")).unwrap();

        restore_target("asm-definitely-not-running.exe", &backup).unwrap();
        assert_eq!(std::fs::read(target.join("data.db")).unwrap(), b"old");
        assert!(!target.join("data.db-wal").exists());
        assert_eq!(
            std::fs::read(target.join("projects").join("old.txt")).unwrap(),
            b"old"
        );
        assert!(!target.join("projects").join("new.txt").exists());
        assert!(!target.join("new").exists());
    }

    #[test]
    fn restore_refuses_when_backup_file_missing() {
        let base = TempGuard::new();
        let target = base.0.join("target");
        let backup = base.0.join("backup");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(target.join("data.db"), b"old").unwrap();
        backup_snapshot(&[target.join("data.db")], &backup).unwrap();
        std::fs::write(target.join("data.db"), b"new").unwrap();
        // 备份内的 data.db 被删掉后，回滚必须在触碰目标前整体中止。
        std::fs::remove_file(backup.join("data.db")).unwrap();
        let error = restore_target("asm-definitely-not-running.exe", &backup).unwrap_err();
        assert!(error.to_string().contains("备份缺少"));
        assert_eq!(std::fs::read(target.join("data.db")).unwrap(), b"new");
    }

    #[test]
    fn restore_collects_per_entry_failures() {
        let base = TempGuard::new();
        let target = base.0.join("target");
        let backup = base.0.join("backup");
        std::fs::create_dir_all(target.join("a")).unwrap();
        std::fs::create_dir_all(target.join("b")).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(target.join("a").join("1.txt"), b"a").unwrap();
        std::fs::write(target.join("b").join("2.txt"), b"b").unwrap();
        backup_snapshot(&[target.join("a"), target.join("b")], &backup).unwrap();
        // 让 b 目录的复制目标变成只读不可行，改为构造父目录被文件占用的冲突：
        // 将 target/b 变成文件，remove_any 会先删掉它再复制，仍成功；
        // 因此这里验证“单条失败不阻断其余恢复”用可复现的方式：b 的备份内容损坏。
        std::fs::write(target.join("a").join("1.txt"), b"changed").unwrap();
        std::fs::write(target.join("b").join("2.txt"), b"changed").unwrap();
        // 删除备份中 b 的内容目录并替换为同名文件，使 copy_dir_all 失败。
        std::fs::remove_dir_all(backup.join("b")).unwrap();
        std::fs::write(backup.join("b"), b"not a dir").unwrap();

        let error = restore_target("asm-definitely-not-running.exe", &backup).unwrap_err();
        assert!(error.to_string().contains("部分内容未能回滚"));
        assert_eq!(
            std::fs::read(target.join("a").join("1.txt")).unwrap(),
            b"a",
            "其他条目必须继续恢复"
        );
    }

    #[test]
    fn prune_keeps_only_latest_backup() {
        let base = TempGuard::new();
        let backups = base.0.join("backups");
        for name in [
            "20260101-000001-qoder",
            "20260102-000001-zcode",
            "20260103-000001-kiro",
        ] {
            fs::create_dir_all(backups.join(name)).unwrap();
        }
        prune_backups_in(&backups);
        let mut left: Vec<String> = fs::read_dir(&backups)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, vec!["20260103-000001-kiro"]);
    }

    #[test]
    fn legacy_ledger_names_match_canonical_ids() {
        let legacy = LedgerEntry {
            source: "Qoder".into(),
            source_id: "s".into(),
            target: "Zcode".into(),
            target_id: "t".into(),
            ts: 1,
            source_updated_ms: 0,
        };
        assert!(legacy.matches("qoder", "s", "zcode"));
    }
}
