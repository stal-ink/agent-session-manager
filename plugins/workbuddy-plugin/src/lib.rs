mod writer;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use agent_core::ir::{Block, Compaction, Message, Role, Session, SessionSummary};
use agent_core::jsonl::read_events;
use agent_core::text::truncate;

pub use writer::{inject, overwrite};

pub const ID: &str = "workbuddy";

pub struct Paths {
    pub home: PathBuf,
    pub db: PathBuf,
    pub projects: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = std::env::var("WORKBUDDY_CONFIG_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".workbuddy")
            });
        Self {
            db: home.join("workbuddy.db"),
            projects: home.join("projects"),
            home,
        }
    }
}

fn open_ro(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let db = open_ro(&paths.db)?;
    let mut stmt = db.prepare(
        "SELECT id, COALESCE(NULLIF(custom_title, ''), title, ''), cwd, created_at, updated_at,
                COALESCE(last_activity_at, updated_at), deleted_at, status
         FROM sessions
         WHERE COALESCE(transport, 'local') = 'local'
         ORDER BY COALESCE(last_activity_at, updated_at) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: String = r.get(0)?;
        let cwd: String = r.get(2)?;
        let deleted_at: Option<i64> = r.get(6)?;
        let status: String = r.get(7)?;
        Ok((
            SessionSummary {
                agent: ID.into(),
                id: id.clone(),
                title: r.get(1)?,
                cwd: cwd.clone(),
                created_ms: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                updated_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                message_count: 0,
                archived: status.eq_ignore_ascii_case("archived"),
                deleted: deleted_at.is_some(),
                workspace_deleted: false,
            },
            cwd,
            id,
        ))
    })?;

    let mut out = Vec::new();
    // 直接路径命中时无需扫描；只有路径不匹配的会话才触发一次目录索引，
    // 后续会话复用同一索引，避免逐条 read_dir 整个 projects 目录。
    let mut index: Option<HashMap<String, PathBuf>> = None;
    for row in rows {
        let (mut summary, cwd, id) = row?;
        let direct = direct_session_file(paths, &cwd, &id);
        let file = if direct.is_file() {
            Some(direct)
        } else {
            index
                .get_or_insert_with(|| session_index(paths))
                .get(&id)
                .cloned()
        };
        if let Some(file) = file {
            summary.message_count = count_messages(&file);
        }
        out.push(summary);
    }
    Ok(out)
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let db = Connection::open(&paths.db).with_context(|| format!("open {}", paths.db.display()))?;
    let tx = db.unchecked_transaction()?;
    let exists = tx
        .query_row("SELECT deleted_at FROM sessions WHERE id = ?1", [id], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?;
    if exists.is_none() {
        anyhow::bail!("WorkBuddy 会话不存在：{id}");
    }
    tx.execute(
        "UPDATE sessions SET deleted_at = ?1 WHERE id = ?2",
        rusqlite::params![agent_core::ir::now_ms(), id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 物理销毁：删除会话与用量记录、正文 jsonl，不可恢复。
/// 与软删除的区别：行被真正删除，而不是写 deleted_at。
pub fn destroy(paths: &Paths, id: &str) -> Result<()> {
    let db = Connection::open(&paths.db).with_context(|| format!("open {}", paths.db.display()))?;
    let tx = db.unchecked_transaction()?;
    // 会话行不存在则整体回滚并报错，与软删除的缺失检查一致。
    let deleted = tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    if deleted == 0 {
        anyhow::bail!("WorkBuddy 会话不存在：{id}");
    }
    tx.execute("DELETE FROM session_usage WHERE session_id = ?1", [id])?;
    tx.commit()?;
    // 正文 jsonl：扫描 projects 各 slug 目录定位；不存在则容忍（可能已被应用清理）。
    remove_jsonl(&paths.projects, id)
}

/// 扫描 projects 下的各 slug 目录，删除该会话的 <id>.jsonl；目录不存在视为已清理。
fn remove_jsonl(projects: &Path, id: &str) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(projects) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join(format!("{id}.jsonl"));
        if candidate.is_file() {
            std::fs::remove_file(&candidate)
                .with_context(|| format!("remove {}", candidate.display()))?;
        }
    }
    Ok(())
}

pub fn active(paths: &Paths, id: &str) -> bool {
    let Ok(db) = open_ro(&paths.db) else {
        return false;
    };
    let row = db
        .query_row(
            "SELECT cwd, deleted_at, status FROM sessions WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional();
    let Ok(Some((cwd, deleted_at, status))) = row else {
        return false;
    };
    deleted_at.is_none()
        && !status.eq_ignore_ascii_case("archived")
        && session_file(paths, &cwd, id).is_some()
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let db = open_ro(&paths.db)?;
    let (title, cwd, created, updated) = db
        .query_row(
            "SELECT COALESCE(NULLIF(custom_title, ''), title, ''), cwd, created_at, updated_at
             FROM sessions WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            },
        )
        .optional()?
        .with_context(|| format!("WorkBuddy 会话不存在：{id}"))?;
    let file =
        session_file(paths, &cwd, id).with_context(|| format!("WorkBuddy 会话正文不存在：{id}"))?;

    let events = read_events(&file)?;
    let mut results: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut ai_title = String::new();
    for event in &events {
        match event["type"].as_str().unwrap_or_default() {
            "function_call_result" => {
                let call_id = event["callId"].as_str().unwrap_or_default();
                if call_id.is_empty() {
                    continue;
                }
                let status = match event["status"].as_str().unwrap_or("completed") {
                    "error" | "failed" | "failure" => "error",
                    "cancelled" | "canceled" => "cancelled",
                    "loading" | "running" | "pending" => "loading",
                    _ => "completed",
                };
                let output = event["output"]["text"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        event["providerData"]["toolResult"]["content"]
                            .as_str()
                            .map(str::to_string)
                    });
                results.insert(call_id.to_string(), (status.to_string(), output));
            }
            "ai-title" if ai_title.is_empty() => {
                ai_title = event["aiTitle"].as_str().unwrap_or_default().to_string();
            }
            _ => {}
        }
    }

    let mut messages: Vec<Message> = Vec::new();
    let mut current: Option<Message> = None;
    let mut compaction: Option<Compaction> = None;
    for event in &events {
        let ts = event["timestamp"].as_i64().unwrap_or(0);
        match event["type"].as_str().unwrap_or_default() {
            "message" => {
                let role = event["role"].as_str().unwrap_or_default();
                let text = content_text(&event["content"]);
                if role == "user" {
                    flush(&mut messages, &mut current);
                    // 压缩注入消息：摘要 + 重放历史，不进入消息列表。
                    // 必须在 clean_user_text 之前判断，否则 <cb_summary> 会被提取掉。
                    if text.trim_start().starts_with("<cb_summary>") {
                        compaction = Some(Compaction {
                            summary_text: text.clone(),
                            tail_start_index: messages.len(),
                            time_ms: ts,
                        });
                        continue;
                    }
                    let text = clean_user_text(&text);
                    if !text.trim().is_empty() {
                        messages.push(Message {
                            role: Role::User,
                            time_ms: ts,
                            blocks: vec![Block::Text(text)],
                        });
                    }
                } else if role == "assistant" && !text.trim().is_empty() {
                    let message = current.get_or_insert_with(|| Message {
                        role: Role::Assistant,
                        time_ms: ts,
                        blocks: Vec::new(),
                    });
                    message.blocks.push(Block::Text(text.trim().to_string()));
                }
            }
            "reasoning" => {
                let text = reasoning_text(event);
                if !text.trim().is_empty() {
                    let message = current.get_or_insert_with(|| Message {
                        role: Role::Assistant,
                        time_ms: ts,
                        blocks: Vec::new(),
                    });
                    message
                        .blocks
                        .push(Block::Thinking(text.trim().to_string()));
                }
            }
            "function_call" => {
                let name = event["name"].as_str().unwrap_or_default().to_string();
                if name.is_empty() {
                    continue;
                }
                let call_id = event["callId"]
                    .as_str()
                    .or_else(|| event["id"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let input = event["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                let (status, output) = results
                    .remove(&call_id)
                    .unwrap_or_else(|| ("completed".to_string(), None));
                let message = current.get_or_insert_with(|| Message {
                    role: Role::Assistant,
                    time_ms: ts,
                    blocks: Vec::new(),
                });
                message.blocks.push(Block::ToolCall {
                    id: call_id,
                    name,
                    input,
                    output,
                    status,
                });
            }
            _ => {}
        }
    }
    flush(&mut messages, &mut current);

    if title.trim().is_empty() {
        let fallback = messages
            .iter()
            .find_map(|m| match m.blocks.first() {
                Some(Block::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if !fallback.is_empty() {
            return Ok(Session {
                agent: ID.into(),
                id: id.to_string(),
                title: truncate(&fallback, 60),
                cwd,
                created_ms: created,
                updated_ms: updated,
                messages,
                compaction: compaction.clone(),
            });
        }
    }
    let title = if title.trim().is_empty() {
        ai_title
    } else {
        title
    };

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title: truncate(&title, 200),
        cwd,
        created_ms: created,
        updated_ms: updated,
        messages,
        compaction,
    })
}

fn flush(messages: &mut Vec<Message>, current: &mut Option<Message>) {
    if let Some(message) = current.take() {
        if !message.blocks.is_empty() {
            messages.push(message);
        }
    }
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let mut parts = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            match block["type"].as_str().unwrap_or_default() {
                "input_text" | "output_text" | "text" => {
                    if let Some(text) = block["text"].as_str() {
                        if !text.is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                }
                "reasoning_text" => {
                    if let Some(text) = block["text"].as_str() {
                        if !text.is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                }
                "input_image" | "output_image" | "image" => {
                    parts.push("[图片]".to_string());
                }
                _ => {}
            }
        }
    }
    parts.join("\n\n")
}

fn reasoning_text(event: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(blocks) = event["rawContent"].as_array() {
        for block in blocks {
            if let Some(text) = block["text"].as_str() {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        return content_text(&event["content"]);
    }
    parts.join("\n\n")
}

fn clean_user_text(text: &str) -> String {
    let mut queries = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<user_query>") {
        let after = &rest[start + "<user_query>".len()..];
        let Some(end) = after.find("</user_query>") else {
            break;
        };
        let query = after[..end].trim();
        if !query.is_empty() {
            queries.push(query.to_string());
        }
        rest = &after[end + "</user_query>".len()..];
    }
    if !queries.is_empty() {
        return queries.join("\n\n");
    }

    let mut cleaned = text.to_string();
    while let Some(start) = cleaned.find("<system-reminder") {
        let Some(close) = cleaned[start..].find("</system-reminder>") else {
            break;
        };
        let end = start + close + "</system-reminder>".len();
        cleaned.replace_range(start..end, "");
    }
    cleaned.trim().to_string()
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
                .map(|value| value["type"].as_str() == Some("message"))
                .unwrap_or(false)
        })
        .count() as i64
}

fn direct_session_file(paths: &Paths, cwd: &str, id: &str) -> PathBuf {
    paths
        .projects
        .join(compress_path(cwd))
        .join(format!("{id}.jsonl"))
}

pub(crate) fn session_file(paths: &Paths, cwd: &str, id: &str) -> Option<PathBuf> {
    let direct = direct_session_file(paths, cwd, id);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(&paths.projects).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(format!("{id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 一次扫描 projects 建立会话 id 到正文路径的索引，供列表批量补消息数。
fn session_index(paths: &Paths) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    let Ok(entries) = std::fs::read_dir(&paths.projects) else {
        return index;
    };
    for entry in entries.flatten() {
        let Ok(files) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                index.entry(stem.to_string()).or_insert(path);
            }
        }
    }
    index
}

pub(crate) fn compress_path(path: &str) -> String {
    let normalized = agent_core::path::lower_drive(path);
    let mut base: String = normalized
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '-',
            _ => ch,
        })
        .collect();
    base = base
        .trim_start_matches('-')
        .trim_end_matches('-')
        .to_string();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    if base.is_empty() {
        return "_".to_string();
    }
    if base.len() <= 255 {
        return base;
    }
    let mut truncated = String::new();
    for ch in base.chars() {
        if truncated.len() + ch.len_utf8() > 180 {
            break;
        }
        truncated.push(ch);
    }
    format!("{truncated}-{}", djb2_base36(&base))
}

fn djb2_base36(text: &str) -> String {
    let mut hash: u32 = 5381;
    for byte in text.as_bytes() {
        hash = hash.wrapping_mul(33) ^ (*byte as u32);
    }
    let mut n = hash;
    if n == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize] as char);
        n /= 36;
    }
    digits.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_path_matches_workbuddy() {
        assert_eq!(
            compress_path("D:/Work/Sample.Project"),
            "d-Work-Sample.Project"
        );
        assert_eq!(compress_path("D:\\test"), "d-test");
        assert_eq!(
            compress_path("C:\\Users\\Example\\Desktop\\APP"),
            "c-Users-Example-Desktop-APP"
        );
    }

    #[test]
    fn user_query_is_extracted() {
        let raw = "<system-reminder data-role=\"additional-data\">\n<current_time>\nnow\n</current_time>\n</system-reminder>\n<user_query>你好</user_query>";
        assert_eq!(clean_user_text(raw), "你好");
    }

    #[test]
    fn system_reminder_is_removed_without_query() {
        let raw = "<system-reminder>\ncontext\n</system-reminder>\nplain text";
        assert_eq!(clean_user_text(raw), "plain text");
    }
}
