use std::fs;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use agent_core::ir::{Block, Compaction, Role, Session};
use agent_core::jsonl::write_atomic;
use agent_core::text::{blocks_fallback, push_text, tool_status};

use super::Paths;

pub fn inject(paths: &Paths, session: &Session, target_cwd: &str) -> Result<String> {
    inject_with(paths, session, target_cwd, None)
}

pub fn overwrite(
    paths: &Paths,
    session: &Session,
    target_cwd: &str,
    target_id: &str,
) -> Result<String> {
    inject_with(paths, session, target_cwd, Some(target_id))
}

struct Template {
    user_id: Option<String>,
    source_mode: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    use_sandbox_cli: Option<i64>,
    context_window: Option<i64>,
    thought_level: Option<String>,
}

fn inject_with(
    paths: &Paths,
    session: &Session,
    target_cwd: &str,
    existing: Option<&str>,
) -> Result<String> {
    if !paths.db.is_file() {
        anyhow::bail!("WorkBuddy 会话库不存在：{}", paths.db.display());
    }
    let id = existing
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    if cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 WorkBuddy 会话");
    }
    let now = agent_core::ir::now_ms();
    let created = if session.created_ms > 0 {
        session.created_ms
    } else {
        now
    };
    let title = if session.title.trim().is_empty() {
        "转接会话".to_string()
    } else {
        session.title.trim().to_string()
    };

    let db = Connection::open(&paths.db)
        .with_context(|| format!("open {} for write", paths.db.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    let has_table: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
        [],
        |r| r.get(0),
    )?;
    if has_table == 0 {
        anyhow::bail!("{} 不是 WorkBuddy 会话库", paths.db.display());
    }

    let template = db
        .query_row(
            "SELECT user_id, source_mode, mode, model, permission_mode, use_sandbox_cli,
                    context_window, thought_level
             FROM sessions ORDER BY updated_at DESC LIMIT 1",
            [],
            |r| {
                Ok(Template {
                    user_id: r.get::<_, Option<String>>(0)?,
                    source_mode: r.get::<_, Option<String>>(1)?,
                    mode: r.get::<_, Option<String>>(2)?,
                    model: r.get::<_, Option<String>>(3)?,
                    permission_mode: r.get::<_, Option<String>>(4)?,
                    use_sandbox_cli: r.get::<_, Option<i64>>(5)?,
                    context_window: r.get::<_, Option<i64>>(6)?,
                    thought_level: r.get::<_, Option<String>>(7)?,
                })
            },
        )
        .optional()?;

    let old = if existing.is_some() {
        db.query_row(
            "SELECT cwd, created_at FROM sessions WHERE id = ?1",
            [&id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
    } else {
        None
    };
    let created = old.as_ref().map(|(_, created)| *created).unwrap_or(created);
    let user_id = template
        .as_ref()
        .and_then(|t| t.user_id.clone())
        .filter(|id| !id.is_empty())
        .or_else(|| storage_user(paths))
        .unwrap_or_else(|| "local".to_string());
    let source_mode = template
        .as_ref()
        .and_then(|t| t.source_mode.clone())
        .or_else(|| Some("craft".to_string()));
    let mode = template
        .as_ref()
        .and_then(|t| t.mode.clone())
        .or_else(|| Some("craft".to_string()));
    let model = template.as_ref().and_then(|t| t.model.clone());
    let permission_mode = template.as_ref().and_then(|t| t.permission_mode.clone());
    let use_sandbox_cli = template
        .as_ref()
        .and_then(|t| t.use_sandbox_cli)
        .or(Some(1));
    let context_window = template
        .as_ref()
        .and_then(|t| t.context_window)
        .or(Some(1_000_000));
    let thought_level = template
        .as_ref()
        .and_then(|t| t.thought_level.clone())
        .or_else(|| Some("high".to_string()));

    let jsonl_path = paths
        .projects
        .join(super::compress_path(cwd))
        .join(format!("{id}.jsonl"));
    let jsonl = build_jsonl(session, &id, cwd, created, &title);
    if let Some(dir) = jsonl_path.parent() {
        fs::create_dir_all(dir)?;
    }
    write_atomic(&jsonl_path, &jsonl)?;

    let db_cwd = cwd.replace('\\', "/");
    let tx = db.unchecked_transaction()?;
    let updated = if existing.is_some() {
        tx.execute(
            "UPDATE sessions
             SET title = ?1, custom_title = NULL, cwd = ?2, status = 'completed',
                 updated_at = ?3, last_activity_at = ?3, deleted_at = NULL,
                 transport = 'local', unread = 0
             WHERE id = ?4",
            rusqlite::params![title, db_cwd, now, id],
        )?
    } else {
        0
    };
    if updated == 0 {
        tx.execute(
            "INSERT INTO sessions
             (id, cwd, user_id, title, custom_title, status, created_at, updated_at,
              last_activity_at, deleted_at, is_playground, source_mode, mode, model,
              permission_mode, use_sandbox_cli, context_window, thought_level, transport, unread)
             VALUES (?1, ?2, ?3, ?4, NULL, 'completed', ?5, ?6, ?6, NULL, 0, ?7, ?8, ?9,
                     ?10, ?11, ?12, ?13, 'local', 0)",
            rusqlite::params![
                id,
                db_cwd,
                user_id,
                title,
                created,
                now,
                source_mode,
                mode,
                model,
                permission_mode,
                use_sandbox_cli,
                context_window,
                thought_level
            ],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO session_usage
         (session_id, used, size, updated_at, credit_json)
         VALUES (?1, 0, ?2, ?3, '{}')",
        rusqlite::params![id, context_window.unwrap_or(1_000_000), now],
    )?;
    tx.commit()?;

    if let Some((old_cwd, _)) = old {
        let old_path = paths
            .projects
            .join(super::compress_path(&old_cwd))
            .join(format!("{id}.jsonl"));
        if old_path != jsonl_path && old_path.is_file() {
            let _ = fs::remove_file(old_path);
        }
    }
    Ok(id)
}

fn storage_user(paths: &Paths) -> Option<String> {
    let entries = fs::read_dir(paths.home.join("storage")).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_prefix("user-") {
            if let Some(id) = rest.strip_suffix("-personal") {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

fn build_jsonl(session: &Session, id: &str, cwd: &str, created: i64, title: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let jsonl_cwd = agent_core::path::lower_drive(&agent_core::path::normalize(cwd));
    lines.push(
        json!({
            "type": "session-meta",
            "id": uuid::Uuid::new_v4().to_string(),
            "sessionId": id,
            "timestamp": created,
            "meta": { "codebuddy.ai/hostKind": "unopted" }
        })
        .to_string(),
    );
    lines.push(
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "timestamp": created + 1,
            "type": "ai-title",
            "aiTitle": title,
            "sessionId": id,
            "cwd": jsonl_cwd
        })
        .to_string(),
    );

    let mut previous: Option<String> = None;
    for (index, message) in session.messages.iter().enumerate() {
        if let Some(compaction) = session
            .compaction
            .as_ref()
            .filter(|c| c.tail_start_index == index)
        {
            emit_compaction(
                &mut lines,
                &mut previous,
                compaction,
                id,
                &jsonl_cwd,
                created + index as i64,
            );
        }
        let base = if message.time_ms > 0 {
            message.time_ms
        } else {
            created + index as i64
        };
        if message.role == Role::User {
            let text = blocks_fallback(&message.blocks);
            if text.trim().is_empty() {
                continue;
            }
            let text = if text.contains("<user_query>") {
                text
            } else {
                format!("<user_query>{}</user_query>", text.trim())
            };
            emit(
                &mut lines,
                &mut previous,
                json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "timestamp": base,
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": text }],
                    "sessionId": id,
                    "cwd": jsonl_cwd
                }),
            );
            continue;
        }

        let mut pending = String::new();
        for (block_index, block) in message.blocks.iter().enumerate() {
            let ts = base + block_index as i64;
            match block {
                Block::Text(text) => push_text(&mut pending, text),
                Block::Image { reference } => {
                    push_text(&mut pending, &format!("[图片] {reference}"))
                }
                Block::File { path } => push_text(&mut pending, &format!("[文件] {path}")),
                Block::Thinking(text) => {
                    flush_assistant_text(
                        &mut lines,
                        &mut previous,
                        id,
                        &jsonl_cwd,
                        ts,
                        &mut pending,
                    );
                    emit(
                        &mut lines,
                        &mut previous,
                        json!({
                            "id": uuid::Uuid::new_v4().to_string(),
                            "timestamp": ts,
                            "type": "reasoning",
                            "content": [],
                            "rawContent": [{ "type": "reasoning_text", "text": text }],
                            "sessionId": id,
                            "cwd": jsonl_cwd
                        }),
                    );
                }
                Block::ToolCall {
                    id: call_id,
                    name,
                    input,
                    output,
                    status,
                } => {
                    flush_assistant_text(
                        &mut lines,
                        &mut previous,
                        id,
                        &jsonl_cwd,
                        ts,
                        &mut pending,
                    );
                    let call_id = if call_id.is_empty() {
                        format!("call_{}", uuid::Uuid::new_v4())
                    } else {
                        call_id.clone()
                    };
                    let call_line = uuid::Uuid::new_v4().to_string();
                    emit(
                        &mut lines,
                        &mut previous,
                        json!({
                            "id": call_line,
                            "timestamp": ts,
                            "type": "function_call",
                            "callId": call_id,
                            "name": name,
                            "arguments": input.to_string(),
                            "sessionId": id,
                            "cwd": jsonl_cwd
                        }),
                    );
                    emit(
                        &mut lines,
                        &mut previous,
                        json!({
                            "id": uuid::Uuid::new_v4().to_string(),
                            "timestamp": ts + 1,
                            "type": "function_call_result",
                            "name": name,
                            "callId": call_id,
                            "status": tool_status(status),
                            "output": {
                                "type": "text",
                                "text": output.clone().unwrap_or_default()
                            },
                            "sessionId": id,
                            "cwd": jsonl_cwd
                        }),
                    );
                }
            }
        }
        flush_assistant_text(
            &mut lines,
            &mut previous,
            id,
            &jsonl_cwd,
            base + message.blocks.len() as i64,
            &mut pending,
        );
    }
    if let Some(compaction) = session
        .compaction
        .as_ref()
        .filter(|c| c.tail_start_index >= session.messages.len())
    {
        emit_compaction(
            &mut lines,
            &mut previous,
            compaction,
            id,
            &jsonl_cwd,
            created + session.messages.len() as i64,
        );
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn emit_compaction(
    lines: &mut Vec<String>,
    previous: &mut Option<String>,
    compaction: &Compaction,
    session_id: &str,
    cwd: &str,
    timestamp: i64,
) {
    let text = compaction.summary_text.trim();
    let text = if text.starts_with("<cb_summary>") {
        text.to_string()
    } else {
        format!("<cb_summary>\n{text}\n</cb_summary>")
    };
    emit(
        lines,
        previous,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "timestamp": timestamp,
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
            "sessionId": session_id,
            "cwd": cwd
        }),
    );
}

fn emit(lines: &mut Vec<String>, previous: &mut Option<String>, mut value: Value) {
    if let Some(parent) = previous.as_ref() {
        value["parentId"] = json!(parent);
    }
    if let Some(id) = value["id"].as_str() {
        *previous = Some(id.to_string());
    }
    lines.push(value.to_string());
}

fn flush_assistant_text(
    lines: &mut Vec<String>,
    previous: &mut Option<String>,
    session_id: &str,
    cwd: &str,
    ts: i64,
    pending: &mut String,
) {
    if pending.trim().is_empty() {
        pending.clear();
        return;
    }
    let text = pending.trim().to_string();
    pending.clear();
    emit(
        lines,
        previous,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "timestamp": ts,
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text }],
            "sessionId": session_id,
            "cwd": cwd
        }),
    );
}
