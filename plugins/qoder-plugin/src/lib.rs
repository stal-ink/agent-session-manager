mod writer;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use agent_core::sqlite::{open_ro, open_rw};
use agent_core::{Block, Compaction, Message, Role, Session, SessionSummary};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use serde_json::Value;

pub use writer::{inject, overwrite};

pub const ID: &str = "qoder";

pub struct Paths {
    pub projects: PathBuf,
    pub main_db: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
        let appdata = PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
        Self {
            projects: home.join(".qoder-cn").join("projects"),
            main_db: appdata.join("com.qodercn.app.stable").join("main.sqlite"),
        }
    }
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let db = open_ro(&paths.main_db)?;
    let mut stmt = db.prepare(
        "SELECT s.session_id, s.title, s.cwd, s.created_at, s.updated_at,
                COALESCE(mc.cnt, 0),
                COALESCE(s.archived, 0),
                CASE WHEN COALESCE(w.archived, 0) = 1 OR w.deleted_at IS NOT NULL THEN 1 ELSE 0 END,
                s.deleted_at IS NOT NULL
         FROM chat_sessions s
         LEFT JOIN workspaces w ON w.workspace_id = s.workspace_id
         LEFT JOIN (
             SELECT session_id, COUNT(*) AS cnt
             FROM chat_session_messages
             GROUP BY session_id
         ) mc ON mc.session_id = s.session_id
         ORDER BY s.updated_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SessionSummary {
            agent: ID.into(),
            id: r.get(0)?,
            title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            cwd: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            created_ms: r.get(3)?,
            updated_ms: r.get(4)?,
            message_count: r.get(5)?,
            archived: r.get::<_, i64>(6)? != 0,
            deleted: r.get::<_, i64>(8)? != 0,
            workspace_deleted: r.get::<_, i64>(7)? != 0,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let db = open_rw(&paths.main_db)?;
    let tx = db.unchecked_transaction()?;
    let exists = tx
        .query_row(
            "SELECT deleted_at FROM chat_sessions WHERE session_id = ?1",
            [id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?;
    if exists.is_none() {
        anyhow::bail!("Qoder 会话不存在：{id}");
    }
    tx.execute(
        "UPDATE chat_sessions SET deleted_at = ?1 WHERE session_id = ?2",
        rusqlite::params![agent_core::now_ms(), id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 物理销毁：删除该会话的全部数据库记录与正文 jsonl，不可恢复。
/// 与软删除的区别：行被真正删除，而不是写 deleted_at。
pub fn destroy(paths: &Paths, id: &str) -> Result<()> {
    let db = open_rw(&paths.main_db)?;
    let tx = db.unchecked_transaction()?;
    // 先删子表再删会话行；会话行不存在则整体回滚并报错，与软删除的缺失检查一致。
    tx.execute(
        "DELETE FROM chat_session_messages WHERE session_id = ?1",
        [id],
    )?;
    tx.execute(
        "DELETE FROM chat_session_sidebar_placements WHERE session_id = ?1",
        [id],
    )?;
    let deleted = tx.execute("DELETE FROM chat_sessions WHERE session_id = ?1", [id])?;
    if deleted == 0 {
        anyhow::bail!("Qoder 会话不存在：{id}");
    }
    tx.commit()?;
    // 正文 jsonl：扫描 projects 定位；文件不存在则容忍（可能已被应用清理）。
    if let Ok(jsonl) = find_jsonl(&paths.projects, id) {
        fs::remove_file(&jsonl).with_context(|| format!("remove {}", jsonl.display()))?;
    }
    Ok(())
}

pub fn active(paths: &Paths, id: &str) -> bool {
    let Ok(db) = open_ro(&paths.main_db) else {
        return false;
    };
    let row = db.query_row(
        "SELECT COALESCE(s.archived, 0), s.deleted_at,
                COALESCE(w.archived, 0), w.deleted_at
         FROM chat_sessions s
         LEFT JOIN workspaces w ON w.workspace_id = s.workspace_id
         WHERE s.session_id = ?1",
        [id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        },
    );
    let Ok((archived, deleted_at, w_archived, w_deleted_at)) = row else {
        return false;
    };
    archived == 0
        && deleted_at.is_none()
        && w_archived == 0
        && w_deleted_at.is_none()
        && find_jsonl(&paths.projects, id).is_ok()
}

fn find_jsonl(projects: &Path, id: &str) -> Result<PathBuf> {
    for entry in fs::read_dir(projects)? {
        let p = entry?.path().join(format!("{id}.jsonl"));
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("session jsonl not found: {id}")
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let db = open_ro(&paths.main_db)?;
    let (title, cwd, created_ms, updated_ms) = db.query_row(
        "SELECT title, cwd, created_at, updated_at FROM chat_sessions WHERE session_id = ?1",
        [id],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )?;

    let jsonl = find_jsonl(&paths.projects, id)?;
    let text = fs::read_to_string(&jsonl)?;
    let (messages, compaction) = parse(&text)?;

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title,
        cwd,
        created_ms,
        updated_ms,
        messages,
        compaction,
    })
}

pub fn parse_jsonl(text: &str) -> Result<Vec<Message>> {
    Ok(parse(text)?.0)
}

fn parse(text: &str) -> Result<(Vec<Message>, Option<Compaction>)> {
    let mut messages: Vec<Message> = Vec::new();
    let mut tool_sites: HashMap<String, (usize, usize)> = HashMap::new();
    let mut compaction: Option<Compaction> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).context("parse jsonl line")?;
        let kind = v["type"].as_str().unwrap_or_default();
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if v["isSidechain"].as_bool().unwrap_or(false) {
            continue;
        }
        let time_ms = v["timestamp"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        // 压缩摘要行不进 messages，只记录摘要与模型窗口的起点。
        if kind == "user" && v["isCompactSummary"].as_bool().unwrap_or(false) {
            let summary = content_text(&v["message"]["content"]);
            if !summary.trim().is_empty() {
                compaction = Some(Compaction {
                    summary_text: summary,
                    tail_start_index: messages.len(),
                    time_ms,
                });
            }
            continue;
        }
        let role = match v["message"]["role"].as_str() {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };

        let mut blocks = Vec::new();
        if let Some(content) = v["message"]["content"].as_array() {
            for b in content {
                match b["type"].as_str().unwrap_or_default() {
                    "text" => {
                        blocks.push(Block::Text(b["text"].as_str().unwrap_or_default().into()))
                    }
                    "thinking" => blocks.push(Block::Thinking(
                        b["thinking"].as_str().unwrap_or_default().into(),
                    )),
                    "tool_use" => {
                        let id = b["id"].as_str().unwrap_or_default().to_string();
                        tool_sites.insert(id.clone(), (messages.len(), blocks.len()));
                        blocks.push(Block::ToolCall {
                            id,
                            name: b["name"].as_str().unwrap_or_default().into(),
                            input: b["input"].clone(),
                            output: None,
                            status: "completed".into(),
                        });
                    }
                    "tool_result" => {
                        let tid = b["tool_use_id"].as_str().unwrap_or_default();
                        if let Some(&(mi, bi)) = tool_sites.get(tid) {
                            if let Some(Block::ToolCall { output, status, .. }) =
                                messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                            {
                                *output = Some(content_text(&b["content"]));
                                *status = if b["is_error"].as_bool().unwrap_or(false) {
                                    "error".into()
                                } else {
                                    "completed".into()
                                };
                            }
                        }
                    }
                    "image" => blocks.push(Block::Image {
                        reference: image_ref(b),
                    }),
                    _ => {}
                }
            }
        } else if let Some(s) = v["message"]["content"].as_str() {
            blocks.push(Block::Text(s.into()));
        }

        if blocks.is_empty() {
            continue;
        }
        messages.push(Message {
            role,
            time_ms,
            blocks,
        });
    }
    Ok((messages, compaction))
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn image_ref(b: &Value) -> String {
    let src = &b["source"];
    src["url"]
        .as_str()
        .or_else(|| src["file_path"].as_str())
        .map(String::from)
        .unwrap_or_else(|| src.to_string())
}
