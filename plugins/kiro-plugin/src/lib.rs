mod writer;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use agent_core::ir::{Block, Message, Role, Session, SessionSummary};
use agent_core::jsonl::read_events;
use agent_core::time::iso_to_ms;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};

pub use writer::{inject, overwrite};

pub const ID: &str = "kiro";

pub struct Paths {
    pub home: PathBuf,
    pub sessions: PathBuf,
    pub session_index: PathBuf,
    pub spec_sessions: PathBuf,
    pub workspace_storage: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".kiro");
        let appdata = PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
        Self {
            sessions: home.join("sessions"),
            session_index: home.join("session-index"),
            spec_sessions: home.join("spec-sessions"),
            workspace_storage: appdata.join("Kiro").join("User").join("workspaceStorage"),
            home,
        }
    }
}

/// Kiro 工作区目录名：sha256(全小写正斜杠路径) 前 16 位十六进制。
pub fn workspace_hash(cwd: &str) -> String {
    let key = agent_core::path::key(cwd).replace('\\', "/");
    sha256_hex(key.as_bytes())[..16].to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn directory_names_hash(names: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for name in names {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// 更新工作区的 `.index/index.json`，Kiro 用它展示会话列表。
pub(crate) fn update_index(paths: &Paths, hash: &str, changed: Option<&str>) -> Result<()> {
    let workspace = paths.sessions.join(hash);
    if !workspace.is_dir() {
        return Ok(());
    }
    let index_dir = workspace.join(".index");
    let index_path = index_dir.join("index.json");
    let old = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let old_entries = old
        .as_ref()
        .and_then(|value| value["entries"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut stamps = old
        .as_ref()
        .and_then(|value| value["stamps"].as_object())
        .cloned()
        .unwrap_or_default();

    let mut dirs = Vec::new();
    let mut ids = Vec::new();
    for entry in
        std::fs::read_dir(&workspace).with_context(|| format!("read {}", workspace.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name() == ".index" {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("sess_") && workspace.join(&name).join("session.json").is_file() {
            ids.push(name.clone());
        }
        dirs.push(name);
    }
    dirs.sort();
    ids.sort();

    let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for old_entry in old_entries {
        let Some(id) = old_entry["id"].as_str().map(str::to_string) else {
            continue;
        };
        if !id_set.contains(id.as_str()) || Some(id.as_str()) == changed {
            continue;
        }
        entries.push(old_entry);
        seen.insert(id);
    }
    for id in &ids {
        if seen.contains(id) {
            continue;
        }
        let meta_path = workspace.join(id).join("session.json");
        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(&meta_path)
                .with_context(|| format!("read {}", meta_path.display()))?,
        )
        .with_context(|| format!("parse {}", meta_path.display()))?;
        entries.push(index_entry(&meta, id));
        seen.insert(id.clone());
    }

    stamps.retain(|id, _| id_set.contains(id.as_str()) && Some(id.as_str()) != changed);
    let mut manifest = json!({
        "schemaVersion": 2,
        "walkedAtMs": agent_core::ir::now_ms(),
        "dirMtimeMs": modified_ms(&workspace),
        "dirNamesHash": directory_names_hash(&dirs),
        "entries": entries,
    });
    if !stamps.is_empty() {
        manifest["stamps"] = Value::Object(stamps);
    }

    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("create {}", index_dir.display()))?;
    agent_core::jsonl::write_atomic(&index_path, &serde_json::to_string(&manifest)?)?;
    Ok(())
}

fn index_entry(meta: &Value, id: &str) -> Value {
    let mut entry = json!({
        "id": id,
        "title": meta["title"].as_str().unwrap_or_default(),
        "agentMode": meta["agentMode"].as_str().unwrap_or("vibe"),
        "createdAt": meta["createdAt"].as_str().unwrap_or_default(),
        "lastModifiedAt": meta["lastModifiedAt"].as_str().unwrap_or_default(),
        "workspacePaths": meta["workspacePaths"].as_array().cloned().unwrap_or_default(),
    });
    if meta["rootPaths"].is_array() {
        entry["rootPaths"] = meta["rootPaths"].clone();
    }
    for key in [
        "description",
        "status",
        "_meta",
        "parentSessionId",
        "executionTarget",
    ] {
        if !meta[key].is_null() {
            entry[key] = meta[key].clone();
        }
    }
    if let Some(repositories) = meta["repositories"].as_array() {
        if !repositories.is_empty() {
            entry["repositories"] = meta["repositories"].clone();
        }
    }
    entry
}

fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_else(agent_core::ir::now_ms)
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    if !paths.sessions.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let workspaces = std::fs::read_dir(&paths.sessions)
        .with_context(|| format!("read {}", paths.sessions.display()))?;
    for workspace in workspaces {
        let workspace = workspace?;
        if !workspace.path().is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(workspace.path())?.flatten() {
            let dir = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".staging-") || name.starts_with(".retired-") {
                continue;
            }
            if !dir.is_dir() || !dir.join("session.json").is_file() {
                continue;
            }
            // 单个损坏的 session.json 只跳过该会话，不拖垮整个列表；
            // 与 messages.jsonl 的坏行容错策略一致。
            let Ok(text) = std::fs::read_to_string(dir.join("session.json")) else {
                continue;
            };
            let Ok(raw) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let id = raw["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string());
            let cwd = raw["workspacePaths"][0]
                .as_str()
                .or_else(|| raw["rootPaths"][0].as_str())
                .unwrap_or_default()
                .to_string();
            out.push(SessionSummary {
                agent: ID.into(),
                archived: false,
                deleted: false,
                workspace_deleted: false,
                message_count: count_messages(&dir.join("messages.jsonl")),
                id,
                title: raw["title"].as_str().unwrap_or_default().to_string(),
                cwd,
                created_ms: iso_to_ms(raw["createdAt"].as_str().unwrap_or_default()),
                updated_ms: iso_to_ms(raw["lastModifiedAt"].as_str().unwrap_or_default()),
            });
        }
    }
    out.sort_by_key(|summary| std::cmp::Reverse(summary.updated_ms));
    Ok(out)
}

/// Kiro 当前打开的会话面板。None 表示 state.vscdb 不可读，UI 不显示面板标记。
pub fn open_session_ids(paths: &Paths) -> Option<HashSet<String>> {
    let mut found = false;
    let mut ids = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&paths.workspace_storage) {
        for entry in entries.flatten() {
            let db = entry.path().join("state.vscdb");
            if !db.is_file() {
                continue;
            }
            if let Some(values) = panel_ids(&db) {
                found = true;
                ids.extend(values);
            }
        }
    }
    let global = paths
        .workspace_storage
        .parent()
        .map(|user| user.join("globalStorage").join("state.vscdb"));
    if let Some(db) = global.filter(|db| db.is_file()) {
        if let Some(values) = panel_ids(&db) {
            found = true;
            ids.extend(values);
        }
    }
    found.then_some(ids)
}

fn panel_ids(db: &Path) -> Option<Vec<String>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'kiro.kiroAgent'",
            [],
            |row| row.get(0),
        )
        .optional()
        .ok()?;
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Some(Vec::new());
    };
    let raw: Value = serde_json::from_str(&value).ok()?;
    Some(
        raw["sessionPanels"]["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["id"].as_str().map(str::to_string))
            .collect(),
    )
}

pub fn active(paths: &Paths, id: &str) -> bool {
    find_session_dir(paths, id).is_ok()
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let dir = find_session_dir(paths, id)?;
    let rel = dir
        .strip_prefix(&paths.sessions)
        .with_context(|| format!("resolve {}", dir.display()))?
        .to_string_lossy()
        .replace('\\', "/");
    let hash = rel
        .split_once('/')
        .map(|(hash, _)| hash)
        .unwrap_or_default()
        .to_string();
    std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    append_oplog(paths, &rel, "remove", agent_core::ir::now_ms())?;
    update_index(paths, &hash, Some(id))
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let dir = find_session_dir(paths, id)?;
    let meta_path = dir.join("session.json");
    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(&meta_path)
            .with_context(|| format!("read {}", meta_path.display()))?,
    )
    .with_context(|| format!("parse {}", meta_path.display()))?;

    let mut tools: HashMap<String, (String, Value)> = HashMap::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut current: Option<Message> = None;
    for event in read_events(&dir.join("messages.jsonl"))? {
        let timestamp = event["timestamp"].as_str().unwrap_or_default();
        let time_ms = iso_to_ms(timestamp);
        let payload = &event["payload"];
        match payload["type"].as_str().unwrap_or_default() {
            "turn_start" => {
                flush(&mut messages, &mut current);
            }
            "user" => {
                flush(&mut messages, &mut current);
                let mut blocks = Vec::new();
                let text = payload["content"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    blocks.push(Block::Text(text));
                }
                if let Some(images) = payload["images"].as_array() {
                    for image in images {
                        if let Some(data) = image["data"].as_str() {
                            let mime = image["mimeType"]
                                .as_str()
                                .filter(|mime| mime.starts_with("image/"))
                                .unwrap_or("image/png");
                            blocks.push(Block::Image {
                                reference: format!("data:{mime};base64,{data}"),
                            });
                        }
                    }
                }
                if !blocks.is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        time_ms,
                        blocks,
                    });
                }
            }
            "assistant" => {
                let text = payload["content"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if text.is_empty() {
                    continue;
                }
                let block = match payload["operationType"].as_str() {
                    Some("Reasoning") => Block::Thinking(text),
                    _ => Block::Text(text),
                };
                let message = current.get_or_insert_with(|| Message {
                    role: Role::Assistant,
                    time_ms,
                    blocks: Vec::new(),
                });
                message.blocks.push(block);
            }
            "tool_call" => {
                let call_id = payload["toolCallId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if call_id.is_empty() {
                    continue;
                }
                tools.insert(
                    call_id,
                    (
                        payload["toolName"].as_str().unwrap_or_default().to_string(),
                        payload["args"].clone(),
                    ),
                );
            }
            "tool_result" => {
                let call_id = payload["toolCallId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let Some((name, input)) = tools.remove(&call_id) else {
                    continue;
                };
                let status = if payload["success"].as_bool().unwrap_or(true) {
                    "completed"
                } else {
                    "error"
                };
                let message = current.get_or_insert_with(|| Message {
                    role: Role::Assistant,
                    time_ms,
                    blocks: Vec::new(),
                });
                message.blocks.push(Block::ToolCall {
                    id: call_id,
                    name,
                    input,
                    output: payload["content"].as_str().map(str::to_string),
                    status: status.into(),
                });
            }
            _ => {}
        }
    }
    flush(&mut messages, &mut current);

    Ok(Session {
        agent: ID.into(),
        id: raw["id"].as_str().unwrap_or(id).to_string(),
        title: raw["title"].as_str().unwrap_or_default().to_string(),
        cwd: raw["workspacePaths"][0]
            .as_str()
            .or_else(|| raw["rootPaths"][0].as_str())
            .unwrap_or_default()
            .to_string(),
        created_ms: iso_to_ms(raw["createdAt"].as_str().unwrap_or_default()),
        updated_ms: iso_to_ms(raw["lastModifiedAt"].as_str().unwrap_or_default()),
        messages,
        compaction: None,
    })
}

pub(crate) fn append_oplog(paths: &Paths, session_path: &str, op: &str, at_ms: i64) -> Result<()> {
    std::fs::create_dir_all(&paths.session_index)
        .with_context(|| format!("create {}", paths.session_index.display()))?;
    let (hash, _) = session_path.split_once('/').unwrap_or((session_path, ""));
    let file = paths.session_index.join(format!("{hash}.jsonl"));
    let line = serde_json::json!({
        "op": op,
        "sessionPath": session_path,
        "at": at_ms,
    });
    use std::io::Write;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .with_context(|| format!("open {}", file.display()))?;
    writeln!(out, "{line}").with_context(|| format!("append {}", file.display()))
}

fn find_session_dir(paths: &Paths, id: &str) -> Result<PathBuf> {
    let workspaces = std::fs::read_dir(&paths.sessions)
        .with_context(|| format!("read {}", paths.sessions.display()))?;
    for workspace in workspaces.flatten() {
        if !workspace.path().is_dir() {
            continue;
        }
        let dir = workspace.path().join(id);
        if dir.join("session.json").is_file() {
            return Ok(dir);
        }
    }
    anyhow::bail!("Kiro 会话不存在：{id}")
}

fn flush(messages: &mut Vec<Message>, current: &mut Option<Message>) {
    if let Some(message) = current.take() {
        if !message.blocks.is_empty() {
            messages.push(message);
        }
    }
}

fn count_messages(path: &Path) -> i64 {
    let Ok(file) = File::open(path) else {
        return 0;
    };
    BufReader::new(file)
        .lines()
        .map_while(|line| line.ok())
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .map(|value| match value["payload"]["type"].as_str() {
                    Some("user") => true,
                    Some("assistant") => value["payload"]["operationType"].as_str() == Some("Say"),
                    _ => false,
                })
                .unwrap_or(false)
        })
        .count() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_hash_uses_lowercase_forward_slash_path() {
        assert_eq!(
            workspace_hash(r"D:\Work\Proj"),
            workspace_hash("d:/work/proj")
        );
        assert_eq!(
            workspace_hash("D:/Work/Proj/"),
            workspace_hash(r"D:\Work\Proj")
        );
        assert_eq!(workspace_hash("d:/work/alpha/tools"), "5b9aa2113f3e598e");
        assert_eq!(
            workspace_hash("c:/users/demo/desktop/sample-app"),
            "113a54c023969581"
        );
    }

    #[test]
    fn directory_names_hash_matches_kiro() {
        assert_eq!(
            directory_names_hash(&["sess_a80fd34c-7514-4a5e-89e9-dff3d3a91fc8".to_string()]),
            "80f3f49b61a83020c0c94133d94ed50e86709e38d69c21877ff9e780562f47fd"
        );
    }
}
