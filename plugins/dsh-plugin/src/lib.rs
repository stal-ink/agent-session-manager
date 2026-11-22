mod writer;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use agent_core::ir::{Block, Compaction, Message, Role, Session, SessionSummary};

pub use writer::{inject, overwrite};

pub const ID: &str = "dsh";

pub struct Paths {
    pub home: PathBuf,
    pub sessions: PathBuf,
    pub storages: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = std::env::var("DSH_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".dsh")
            });
        Self {
            sessions: home.join("sessions"),
            storages: home.join("storages"),
            home,
        }
    }
}

struct Parsed {
    id: String,
    title: String,
    cwd: String,
    created_ms: i64,
    updated_ms: i64,
    messages: Vec<Message>,
    compaction: Option<Compaction>,
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let archived = archived_ids(paths);
    let mut out = Vec::new();
    for (id, log) in session_logs(paths) {
        let Ok(parsed) = parse_file(&log, &id) else {
            continue;
        };
        out.push(SessionSummary {
            agent: ID.into(),
            id: parsed.id,
            title: parsed.title,
            cwd: parsed.cwd,
            created_ms: parsed.created_ms,
            updated_ms: parsed.updated_ms,
            message_count: parsed.messages.len() as i64,
            archived: archived.contains(&id),
            deleted: false,
            workspace_deleted: false,
        });
    }
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let dir = session_dir(paths, id).with_context(|| format!("DSH 会话不存在：{id}"))?;
    let log = session_log(&dir).with_context(|| format!("DSH 会话日志不存在：{id}"))?;
    let parsed = parse_file(&log, id)?;
    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title: parsed.title,
        cwd: parsed.cwd,
        created_ms: parsed.created_ms,
        updated_ms: parsed.updated_ms,
        messages: parsed.messages,
        compaction: parsed.compaction,
    })
}

/// 软删除：把会话加入工作区归档集合，会话日志保留。
pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    if session_dir(paths, id).is_none() {
        anyhow::bail!("DSH 会话不存在：{id}");
    }
    let path = workspace_path(paths);
    let mut doc = read_json(&path)?.with_context(|| "DSH 工作区索引不存在")?;
    let global = doc
        .pointer_mut("/global")
        .and_then(Value::as_object_mut)
        .with_context(|| "DSH 工作区索引缺少 global")?;
    let archived = global
        .entry("archivedSessionIds")
        .or_insert_with(|| Value::Array(Vec::new()));
    let list = archived
        .as_array_mut()
        .with_context(|| "DSH archivedSessionIds 不是数组")?;
    if !list.iter().any(|item| item.as_str() == Some(id)) {
        list.push(Value::String(id.to_string()));
    }
    write_json(&path, &doc)
}

pub fn active(paths: &Paths, id: &str) -> bool {
    session_dir(paths, id).is_some() && !archived_ids(paths).contains(id)
}

/// 物理销毁：删除会话目录（日志）、投影缓存、工作区索引与反馈记录。
pub fn destroy(paths: &Paths, id: &str) -> Result<()> {
    let dir = session_dir(paths, id).with_context(|| format!("DSH 会话不存在：{id}"))?;
    fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;

    let cache = paths
        .storages
        .join("session_projcache")
        .join("sessions")
        .join(format!("{id}.json"));
    if cache.is_file() {
        fs::remove_file(&cache).with_context(|| format!("remove {}", cache.display()))?;
    }
    remove_table_row(paths, "session_projcache.json", id)?;
    remove_table_row(paths, "message_feedback.json", id)?;
    detach_workspace(paths, id)?;
    Ok(())
}

fn remove_table_row(paths: &Paths, file: &str, id: &str) -> Result<()> {
    let path = paths.storages.join(file);
    let Some(mut doc) = read_json(&path)? else {
        return Ok(());
    };
    let mut changed = false;
    if let Some(sessions) = doc
        .pointer_mut("/tables/sessions")
        .and_then(Value::as_object_mut)
    {
        changed = sessions.remove(id).is_some();
    }
    if changed {
        write_json(&path, &doc)?;
    }
    Ok(())
}

fn detach_workspace(paths: &Paths, id: &str) -> Result<()> {
    let path = workspace_path(paths);
    let Some(mut doc) = read_json(&path)? else {
        return Ok(());
    };
    let mut changed = false;
    if let Some(global) = doc.pointer_mut("/global").and_then(Value::as_object_mut) {
        for key in ["archivedSessionIds", "pinnedSessionIds"] {
            if let Some(list) = global.get_mut(key).and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|item| item.as_str() != Some(id));
                changed |= list.len() != before;
            }
        }
    }
    if let Some(tables) = doc
        .pointer_mut("/tables/workspaces")
        .and_then(Value::as_object_mut)
    {
        for workspace in tables.values_mut() {
            if let Some(list) = workspace
                .get_mut("sessionIds")
                .and_then(Value::as_array_mut)
            {
                let before = list.len();
                list.retain(|item| item.as_str() != Some(id));
                changed |= list.len() != before;
            }
        }
    }
    if changed {
        write_json(&path, &doc)?;
    }
    Ok(())
}

fn archived_ids(paths: &Paths) -> HashSet<String> {
    let Some(doc) = read_json(&workspace_path(paths)).ok().flatten() else {
        return HashSet::new();
    };
    doc.pointer("/global/archivedSessionIds")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn workspace_path(paths: &Paths) -> PathBuf {
    paths.storages.join("workspace.json")
}

pub(crate) fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    agent_core::jsonl::write_atomic(path, &serde_json::to_string_pretty(value)?)
}

/// 工作区目录键：分隔符折叠为 `-`，其余不安全字符按 `~XXXX` 转义，两端 `--`。
pub(crate) fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        let code = unit as u32;
        let allowed = (48..=57).contains(&code)
            || (65..=90).contains(&code)
            || (97..=122).contains(&code)
            || code == b'.' as u32
            || code == b'_' as u32
            || code == b'-' as u32;
        if unit == b'/' as u16 || unit == b'\\' as u16 || unit == b':' as u16 {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if allowed {
            readable.push(unit as u8 as char);
            separator_run = false;
        } else {
            readable.push_str(&format!("~{code:04X}"));
            separator_run = false;
        }
    }
    let trimmed = readable.trim_start_matches('-');
    let core = if trimmed.is_empty() { "root" } else { trimmed };
    let core: String = core.chars().take(251).collect();
    format!("--{core}--")
}

fn session_logs(paths: &Paths) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(&paths.sessions) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if let Some(log) = session_log(&entry.path()) {
                out.push((id, log));
            }
        }
    }
    out
}

pub(crate) fn session_dir(paths: &Paths, id: &str) -> Option<PathBuf> {
    let projects = fs::read_dir(&paths.sessions).ok()?;
    for project in projects.flatten() {
        let candidate = project.path().join(id);
        if candidate.is_dir() && session_log(&candidate).is_some() {
            return Some(candidate);
        }
    }
    None
}

fn session_log(dir: &Path) -> Option<PathBuf> {
    for name in ["session.v4.jsonl.zstd", "session.jsonl.zstd"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let raw = dir.join("session.jsonl");
    raw.is_file().then_some(raw)
}

fn read_log(path: &Path) -> Result<String> {
    if path.extension().and_then(|e| e.to_str()) == Some("zstd") {
        let file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
        let mut decoder = zstd::stream::read::Decoder::new(file)
            .with_context(|| format!("decode {}", path.display()))?;
        let mut text = String::new();
        decoder
            .read_to_string(&mut text)
            .with_context(|| format!("decode {}", path.display()))?;
        Ok(text)
    } else {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }
}

fn parse_file(path: &Path, hint_id: &str) -> Result<Parsed> {
    let text = read_log(path)?;
    Ok(parse_text(&text, hint_id))
}

fn parse_text(text: &str, hint_id: &str) -> Parsed {
    let mut messages: Vec<Message> = Vec::new();
    let mut tool_sites: HashMap<String, (usize, usize)> = HashMap::new();
    let mut compaction: Option<Compaction> = None;
    let mut title = String::new();
    let mut cwd = String::new();
    let mut created = 0i64;
    let mut updated = 0i64;
    let mut first_user_text = String::new();
    let mut session_id = hint_id.to_string();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let time = v["time"].as_i64().unwrap_or(0);
        updated = updated.max(time);
        match v["type"].as_str().unwrap_or_default() {
            "session" => {
                if let Some(id) = v["id"].as_str() {
                    session_id = id.to_string();
                }
                if let Some(value) = v["cwd"].as_str() {
                    cwd = value.to_string();
                }
                created = v["createdAt"].as_i64().unwrap_or(0);
            }
            "session/title" => {
                if let Some(value) = v["data"]["title"].as_str() {
                    if !value.trim().is_empty() {
                        title = value.to_string();
                    }
                }
            }
            "user/message" => {
                let source = &v["data"]["source"];
                let kind = source["kind"].as_str().unwrap_or_default();
                let content = &v["data"]["content"];
                // v4 的检查点来源是 compact-checkpoint；v3 日志是 plugin/compact。
                let checkpoint = kind == "compact-checkpoint"
                    || (kind == "plugin" && source["plugin"].as_str() == Some("compact"));
                if checkpoint {
                    let summary = strip_checkpoint_framing(&content_text(content));
                    if !summary.trim().is_empty() {
                        compaction = Some(Compaction {
                            summary_text: summary,
                            tail_start_index: messages.len(),
                            time_ms: time,
                        });
                    }
                    continue;
                }
                if kind != "user" {
                    continue;
                }
                let blocks = user_blocks(content);
                if blocks.is_empty() {
                    continue;
                }
                if first_user_text.is_empty() {
                    if let Some(Block::Text(text)) =
                        blocks.iter().find(|b| matches!(b, Block::Text(_)))
                    {
                        first_user_text = text.clone();
                    }
                }
                messages.push(Message {
                    role: Role::User,
                    time_ms: time,
                    blocks,
                });
            }
            "assistant/message" => {
                let content = &v["data"]["message"]["content"];
                let mut blocks = Vec::new();
                if let Some(items) = content.as_array() {
                    for b in items {
                        match b["type"].as_str().unwrap_or_default() {
                            "reasoning" => {
                                let text = b["text"].as_str().unwrap_or_default();
                                if !text.trim().is_empty() {
                                    blocks.push(Block::Thinking(text.to_string()));
                                }
                            }
                            "text" => {
                                let text = b["text"].as_str().unwrap_or_default();
                                if !text.trim().is_empty() {
                                    blocks.push(Block::Text(text.to_string()));
                                }
                            }
                            "tool-call" => {
                                let id = b["id"].as_str().unwrap_or_default().to_string();
                                tool_sites.insert(id.clone(), (messages.len(), blocks.len()));
                                blocks.push(Block::ToolCall {
                                    id,
                                    name: b["name"].as_str().unwrap_or_default().into(),
                                    input: arguments_value(&b["arguments"]),
                                    output: None,
                                    status: "completed".into(),
                                });
                            }
                            "image" => blocks.push(Block::Image {
                                reference: "图片".into(),
                            }),
                            _ => {}
                        }
                    }
                }
                if blocks.is_empty() {
                    continue;
                }
                messages.push(Message {
                    role: Role::Assistant,
                    time_ms: time,
                    blocks,
                });
            }
            "tool/result" => {
                let data = &v["data"];
                let call_id = data["toolCallId"]
                    .as_str()
                    .or_else(|| data["message"]["toolCallId"].as_str())
                    .unwrap_or_default();
                if let Some(&(mi, bi)) = tool_sites.get(call_id) {
                    if let Some(Block::ToolCall { output, status, .. }) =
                        messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                    {
                        *output = Some(content_text(&data["message"]["content"]));
                        *status = if data["message"]["isError"].as_bool().unwrap_or(false) {
                            "error".into()
                        } else {
                            "completed".into()
                        };
                    }
                }
            }
            _ => {}
        }
    }

    if created == 0 {
        created = updated;
    }
    if title.trim().is_empty() {
        title = if first_user_text.trim().is_empty() {
            let short: String = session_id
                .trim_start_matches("session-")
                .chars()
                .take(8)
                .collect();
            format!("DSH 会话 {short}")
        } else {
            agent_core::text::truncate(first_user_text.trim(), 60)
        };
    }
    Parsed {
        id: session_id,
        title,
        cwd,
        created_ms: created,
        updated_ms: updated.max(created),
        messages,
        compaction,
    }
}

fn user_blocks(content: &Value) -> Vec<Block> {
    let mut blocks = Vec::new();
    if let Some(items) = content.as_array() {
        for b in items {
            match b["type"].as_str().unwrap_or_default() {
                "text" => {
                    let text = b["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        blocks.push(Block::Text(text.to_string()));
                    }
                }
                "image" => blocks.push(Block::Image {
                    reference: "图片".into(),
                }),
                _ => {}
            }
        }
    }
    blocks
}

fn arguments_value(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| value.clone()),
        _ => value.clone(),
    }
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// v4 检查点把摘要包在说明文字与 <compacted-summary> 里；对外只保留摘要正文。
fn strip_checkpoint_framing(text: &str) -> String {
    const OPEN: &str = "<compacted-summary>";
    const CLOSE: &str = "</compacted-summary>";
    if let Some(start) = text.find(OPEN) {
        let after = &text[start + OPEN.len()..];
        if let Some(end) = after.find(CLOSE) {
            return after[..end].trim().to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_matches_dsh_layout() {
        assert_eq!(project_key(r"D:\Work\Ink"), "--D-Work-Ink--");
        assert_eq!(
            project_key(r"D:\Work\Alpha\tools\serial-console-rs"),
            "--D-Work-Alpha-tools-serial-console-rs--"
        );
        assert_eq!(
            project_key(r"C:\Users\Me\项目"),
            "--C-Users-Me-~9879~76EE--"
        );
    }

    #[test]
    fn parse_reads_messages_tools_and_compaction() {
        let text = r#"{"type":"session","version":4,"id":"session-x","createdAt":1000,"cwd":"D:\\work","isSeeded":false,"delegationDepth":0}
{"type":"session/title","seq":0,"time":1001,"data":{"title":"标题","messageSeqs":[],"source":{"kind":"user"}}}
{"type":"turn/start","seq":1,"time":1002,"data":{"turn":1}}
{"type":"user/message","seq":2,"time":1003,"data":{"content":[{"type":"text","text":"你好"}],"source":{"kind":"user"},"role":"user","id":"u1"},"surfaceOp":"append"}
{"type":"assistant/message","seq":3,"time":1004,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"想想"},{"type":"tool-call","id":"t1","name":"read","arguments":"{\"p\":\"a\"}"}],"source":{"kind":"model"}}}}
{"type":"tool/result","seq":4,"time":1005,"data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"t1"},"toolCallId":"t1","content":[{"type":"text","text":"内容"}],"isError":false}}}
{"type":"user/message","seq":5,"time":1006,"data":{"content":[{"type":"text","text":"摘要"}],"source":{"kind":"plugin","plugin":"compact"},"role":"user","id":"c1"},"surfaceOp":{"op":"replace","start":2,"end":4}}
{"type":"user/message","seq":6,"time":1007,"data":{"content":[{"type":"text","text":"尾部"}],"source":{"kind":"user"},"role":"user","id":"u2"},"surfaceOp":"append"}
"#;
        let parsed = parse_text(text, "session-x");
        assert_eq!(parsed.title, "标题");
        assert_eq!(parsed.cwd, r"D:\work");
        assert_eq!(parsed.messages.len(), 3);
        let compaction = parsed.compaction.expect("compaction");
        assert_eq!(compaction.summary_text, "摘要");
        assert_eq!(compaction.tail_start_index, 2);
        match &parsed.messages[1].blocks[1] {
            Block::ToolCall { output, .. } => assert_eq!(output.as_deref(), Some("内容")),
            _ => panic!("expected tool call"),
        }
    }
}
