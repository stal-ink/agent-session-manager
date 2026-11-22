#![recursion_limit = "256"]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use agent_core::ir::{Block, Message, Role, Session, SessionSummary};
use agent_core::sqlite::{open_ro, open_rw};
use agent_core::time::iso_to_ms;

mod writer;

pub use writer::{inject, overwrite};

pub const ID: &str = "cursor";

pub struct Paths {
    pub state_db: PathBuf,
    pub workspace_storage: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let appdata = PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
        let user = appdata.join("Cursor").join("User");
        Self {
            state_db: user.join("globalStorage").join("state.vscdb"),
            workspace_storage: user.join("workspaceStorage"),
        }
    }
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let db = open_ro(&paths.state_db)?;
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut keys = db.prepare("SELECT key FROM cursorDiskKV WHERE key LIKE 'bubbleId:%'")?;
    for key in keys.query_map([], |r| r.get::<_, String>(0))? {
        let key = key?;
        let rest = &key["bubbleId:".len()..];
        if let Some(pos) = rest.find(':') {
            *counts.entry(rest[..pos].to_string()).or_default() += 1;
        }
    }
    // 同一 workspace 的 workspace.json 只读一次；负结果也缓存，避免每个会话重复读盘。
    let mut workspace_cache: HashMap<String, Option<String>> = HashMap::new();

    let mut stmt = db.prepare(
        "SELECT composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, value
         FROM composerHeaders
         WHERE isSubagent = 0
         ORDER BY lastUpdatedAt DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: String = r.get(0)?;
        let workspace_id: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        let created: i64 = r.get::<_, Option<i64>>(2)?.unwrap_or(0);
        let updated: i64 = r.get::<_, Option<i64>>(3)?.unwrap_or(created);
        let archived: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
        let head: Value = r
            .get::<_, Option<String>>(5)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let title = head["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| head["subtitle"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or_default()
            .to_string();
        let cwd = workspace_cache
            .entry(workspace_id.clone())
            .or_insert_with(|| read_workspace_folder(paths, &workspace_id))
            .clone()
            .unwrap_or_else(|| head_fallback(&head));
        let message_count = counts.get(&id).copied().unwrap_or(0);
        Ok(SessionSummary {
            agent: ID.into(),
            id,
            title,
            cwd,
            created_ms: created,
            updated_ms: updated,
            message_count,
            archived: archived != 0,
            deleted: false,
            workspace_deleted: false,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let db = open_rw(&paths.state_db)?;
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM composerHeaders WHERE composerId = ?1", [id])?;
    tx.execute(
        "DELETE FROM cursorDiskKV WHERE key = ?1 OR key LIKE ?2 OR key LIKE ?3",
        rusqlite::params![
            format!("composerData:{id}"),
            format!("bubbleId:{id}:%"),
            format!("checkpointId:{id}:%")
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn active(paths: &Paths, id: &str) -> bool {
    let Ok(db) = open_ro(&paths.state_db) else {
        return false;
    };
    db.query_row(
        "SELECT COALESCE(isArchived, 0) FROM composerHeaders WHERE composerId = ?1",
        [id],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v == 0)
    .unwrap_or(false)
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let db = open_ro(&paths.state_db)?;
    let (title, workspace_id, created, updated, head) = db.query_row(
        "SELECT composerId, workspaceId, createdAt, lastUpdatedAt, value
         FROM composerHeaders WHERE composerId = ?1",
        [id],
        |r| {
            let head: Value = r
                .get::<_, Option<String>>(4)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok((
                head["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| head["subtitle"].as_str().filter(|s| !s.is_empty()))
                    .unwrap_or_default()
                    .to_string(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                head,
            ))
        },
    )?;

    let mut bubbles: Vec<(String, Value)> = Vec::new();
    let mut stmt = db.prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE ?1")?;
    let rows = stmt.query_map([format!("bubbleId:{id}:%")], |r| {
        Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or_default(),
        ))
    })?;
    for row in rows {
        bubbles.push(row?);
    }
    bubbles.sort_by_key(|(key, bubble)| {
        (
            bubble["createdAt"].as_str().map(iso_to_ms).unwrap_or(0),
            key.clone(),
        )
    });

    let mut messages = Vec::new();
    for (_, bubble) in bubbles {
        let role = match bubble["type"].as_i64().unwrap_or(0) {
            2 => Role::Assistant,
            _ => Role::User,
        };
        let mut blocks = Vec::new();
        if let Some(text) = bubble["text"].as_str().filter(|s| !s.is_empty()) {
            blocks.push(Block::Text(text.to_string()));
        }
        if let Some(thinking) = bubble["thinking"]["text"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            blocks.push(Block::Thinking(thinking.to_string()));
        }
        if let Some(tool) = bubble.get("toolFormerData").filter(|v| v.is_object()) {
            let name = tool["name"].as_str().unwrap_or_default();
            if !name.is_empty() {
                let input = tool["params"]
                    .as_str()
                    .or_else(|| tool["rawArgs"].as_str())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                blocks.push(Block::ToolCall {
                    id: tool["toolCallId"].as_str().unwrap_or_default().into(),
                    name: name.into(),
                    input,
                    output: Some(match &tool["result"] {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    }),
                    status: tool["status"].as_str().unwrap_or("completed").into(),
                });
            }
        }
        if blocks.is_empty() {
            continue;
        }
        messages.push(Message {
            role,
            time_ms: bubble["createdAt"].as_str().map(iso_to_ms).unwrap_or(0),
            blocks,
        });
    }

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title,
        cwd: workspace_path(paths, &workspace_id, &head),
        created_ms: created,
        updated_ms: updated,
        messages,
        compaction: None,
    })
}

fn workspace_path(paths: &Paths, workspace_id: &str, head: &Value) -> String {
    read_workspace_folder(paths, workspace_id).unwrap_or_else(|| head_fallback(head))
}

fn head_fallback(head: &Value) -> String {
    head["workspaceIdentifier"]["uri"]["fsPath"]
        .as_str()
        .or_else(|| head["draftTarget"]["environment"]["uri"]["fsPath"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn read_workspace_folder(paths: &Paths, workspace_id: &str) -> Option<String> {
    let file = paths
        .workspace_storage
        .join(workspace_id)
        .join("workspace.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    Some(uri_to_path(value["folder"].as_str()?))
}

fn uri_to_path(uri: &str) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8_lossy(&out);
    decoded.trim_start_matches('/').replace('/', "\\")
}
