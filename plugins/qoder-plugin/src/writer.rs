use std::fs;
use std::io::Write as _;

use agent_core::path as paths;
use agent_core::sqlite::open_rw;
use agent_core::time::ms_to_iso;
use agent_core::{Block, Compaction, Role, Session};
use anyhow::Result;
use serde_json::{json, Value};

pub fn inject(paths: &super::Paths, session: &Session, target_cwd: &str) -> Result<String> {
    inject_with(paths, session, target_cwd, None)
}

pub fn overwrite(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    target_id: &str,
) -> Result<String> {
    inject_with(paths, session, target_cwd, Some(target_id))
}

fn inject_with(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    existing: Option<&str>,
) -> Result<String> {
    let new_id = existing
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = agent_core::now_ms();
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = paths::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 Qoder 会话");
    }
    let target_cwd = target_cwd.as_str();

    let old_path = if existing.is_some() {
        super::find_jsonl(&paths.projects, &new_id).ok()
    } else {
        None
    };
    let slug: String = target_cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = paths.projects.join(&slug);
    fs::create_dir_all(&dir)?;
    let jsonl_path = dir.join(format!("{new_id}.jsonl"));
    write_jsonl(&jsonl_path, &new_id, target_cwd, session, now)?;

    let db = open_rw(&paths.main_db)?;
    let tx = db.unchecked_transaction()?;

    let workspace_id = ensure_workspace(&tx, target_cwd, now)?;
    if existing.is_some() {
        tx.execute(
            "DELETE FROM chat_session_messages WHERE session_id = ?1",
            [&new_id],
        )?;
        tx.execute(
            "DELETE FROM chat_session_sidebar_placements WHERE session_id = ?1",
            [&new_id],
        )?;
    }
    let updated = if existing.is_some() {
        tx.execute(
            "UPDATE chat_sessions SET title = ?1, cwd = ?2, workspace_id = ?3, updated_at = ?4,
                    archived = 0, archived_at = NULL, deleted_at = NULL
             WHERE session_id = ?5",
            rusqlite::params![session.title, target_cwd, &workspace_id, now, &new_id],
        )?
    } else {
        0
    };
    if updated == 0 {
        tx.execute(
            "INSERT INTO chat_sessions (session_id, title, cwd, workspace_id, extra_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                &new_id,
                session.title,
                target_cwd,
                &workspace_id,
                json!({ "source": "workspace", "titleSource": "custom", "titleResolved": true }).to_string(),
                now
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO chat_session_sidebar_placements (session_id, work_directory, sort_order, pinned, updated_at)
         VALUES (?1, ?2, 0, 0, ?3)",
        rusqlite::params![&new_id, target_cwd, now],
    )?;

    let mut sequence = 0i64;
    for m in &session.messages {
        let turn = uuid::Uuid::new_v4().to_string();
        let iso = ms_to_iso(m.time_ms.max(now));
        let (message_id, source, payload) = match m.role {
            Role::User => {
                let mut parts = Vec::new();
                for (i, b) in m.blocks.iter().enumerate() {
                    if let Block::Text(t) = b {
                        parts.push(
                            json!({ "id": format!("{turn}:text:{i}"), "type": "text", "text": t }),
                        );
                    }
                }
                (
                    turn.clone(),
                    "host-projection",
                    json!({
                        "issueId": null,
                        "attachments": [],
                        "selectedSkillNames": [],
                        "selectedPluginIds": [],
                        "selectedConnectorIds": [],
                        "selectedAgentNames": [],
                        "selectedCapabilityCommands": [],
                        "referencedChatSessions": [],
                        "parts": parts,
                    }),
                )
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                for (i, b) in m.blocks.iter().enumerate() {
                    match b {
                        Block::Text(t) => parts.push(json!({
                            "id": format!("{turn}:text:{i}"), "type": "text", "text": t, "parentToolUseId": null
                        })),
                        Block::Thinking(t) => parts.push(json!({
                            "id": format!("{turn}:thinking:{i}"), "type": "thinking", "text": t,
                            "status": "completed", "parentToolUseId": null
                        })),
                        Block::ToolCall { id, name, input, output, status } => parts.push(json!({
                            "id": format!("tool:{id}"), "type": "tool",
                            "tool": {
                                "id": id, "name": name, "input": input, "status": status,
                                "response": output.clone().unwrap_or_default(),
                                "completedAt": iso, "parentToolUseId": null
                            }
                        })),
                        Block::Image { reference } => parts.push(json!({
                            "id": format!("{turn}:image:{i}"), "type": "text",
                            "text": format!("[图片：{reference}]"), "parentToolUseId": null
                        })),
                        Block::File { path } => parts.push(json!({
                            "id": format!("{turn}:file:{i}"), "type": "text",
                            "text": format!("[文件：{path}]"), "parentToolUseId": null
                        })),
                    }
                }
                (
                    format!("assistant:{turn}"),
                    "sdk-projection",
                    json!({
                        "requestSetId": turn,
                        "resultUuid": uuid::Uuid::new_v4().to_string(),
                        "completedAt": iso,
                        "durationMs": 0,
                        "turnMetrics": { "durationMs": 0, "turnCount": 1 },
                        "parts": parts,
                    }),
                )
            }
        };
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        sequence += 1;
        tx.execute(
            "INSERT INTO chat_session_messages
             (session_id, message_id, turn_id, sequence, payload_json, status, source, created_at, updated_at, role, payload_schema_version, has_visible_content)
             VALUES (?1, ?2, ?3, ?4, ?5, 'completed', ?6, ?7, ?7, ?8, 2, 1)",
            rusqlite::params![new_id, message_id, turn, sequence, payload.to_string(), source, now, role],
        )?;
    }
    tx.commit()?;
    if let Some(old) = old_path {
        if old != jsonl_path && old.is_file() {
            let _ = fs::remove_file(old);
        }
    }
    Ok(new_id)
}

fn ensure_workspace(tx: &rusqlite::Transaction<'_>, cwd: &str, now: i64) -> Result<String> {
    let target = paths::key(cwd);
    let mut stmt = tx.prepare(
        "SELECT workspace_id, root_paths_json FROM workspaces
         ORDER BY deleted_at IS NULL DESC, archived ASC, created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, roots) = row?;
        let matched = serde_json::from_str::<Vec<String>>(&roots)
            .map(|roots| roots.iter().any(|root| paths::key(root) == target))
            .unwrap_or(false);
        if matched {
            tx.execute(
                "UPDATE workspaces SET archived = 0, deleted_at = NULL, updated_at = ?2
                 WHERE workspace_id = ?1",
                rusqlite::params![id, now],
            )?;
            return Ok(id);
        }
    }

    let roots = json!([cwd]).to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let name = paths::name(cwd);
    tx.execute(
        "INSERT INTO workspaces (workspace_id, name, workspace_source, root_paths_json, created_at, updated_at)
         VALUES (?1, ?2, 'local', ?3, ?4, ?4)",
        rusqlite::params![id, name, roots, now],
    )?;
    Ok(id)
}

fn write_jsonl(
    path: &std::path::Path,
    sid: &str,
    cwd: &str,
    session: &Session,
    now: i64,
) -> Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    let mut f = fs::File::create(&tmp)?;
    writeln!(
        f,
        "{}",
        json!({ "type": "workspace-directories", "sessionId": sid, "directories": [cwd] })
    )?;
    writeln!(
        f,
        "{}",
        json!({
            "type": "runtime-config", "sessionId": sid, "model": "lmodel",
            "reasoningEffort": null, "contextWindow": null, "generation": null, "timestamp": now
        })
    )?;

    let mut parent: Option<String> = None;
    let mut leaf = String::new();
    for (index, m) in session.messages.iter().enumerate() {
        if let Some(compaction) = session
            .compaction
            .as_ref()
            .filter(|c| c.tail_start_index == index)
        {
            write_compaction(&mut f, sid, cwd, compaction, &mut parent, &mut leaf)?;
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let mut content: Vec<Value> = Vec::new();
        let mut results: Vec<Value> = Vec::new();
        for b in &m.blocks {
            match b {
                Block::Text(t) => content.push(json!({ "type": "text", "text": t })),
                Block::Thinking(t) => content.push(json!({ "type": "thinking", "thinking": t })),
                Block::ToolCall {
                    id,
                    name,
                    input,
                    output,
                    ..
                } => {
                    content.push(
                        json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                    );
                    results.push(json!({
                        "type": "tool_result", "tool_use_id": id,
                        "content": output.clone().unwrap_or_default()
                    }));
                }
                Block::Image { reference } => {
                    content.push(json!({ "type": "text", "text": format!("[图片：{reference}]") }))
                }
                Block::File { path } => {
                    content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
                }
            }
        }
        let line = json!({
            "type": role, "uuid": uuid, "parentUuid": parent, "sessionId": sid,
            "timestamp": ms_to_iso(m.time_ms.max(1)), "cwd": cwd,
            "message": { "role": role, "content": content }
        });
        writeln!(f, "{line}")?;
        parent = Some(uuid.clone());
        leaf = uuid;

        if !results.is_empty() {
            let ruuid = uuid::Uuid::new_v4().to_string();
            let line = json!({
                "type": "user", "uuid": ruuid, "parentUuid": parent, "sessionId": sid,
                "timestamp": ms_to_iso(m.time_ms.max(1)), "cwd": cwd,
                "message": { "role": "user", "content": results }
            });
            writeln!(f, "{line}")?;
            parent = Some(ruuid.clone());
            leaf = ruuid;
        }
    }
    if let Some(compaction) = session
        .compaction
        .as_ref()
        .filter(|c| c.tail_start_index >= session.messages.len())
    {
        write_compaction(&mut f, sid, cwd, compaction, &mut parent, &mut leaf)?;
    }
    writeln!(
        f,
        "{}",
        json!({ "type": "active-leaf", "sessionId": sid, "leafUuid": leaf })
    )?;
    f.flush()?;
    drop(f);
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_compaction(
    f: &mut fs::File,
    sid: &str,
    cwd: &str,
    compaction: &Compaction,
    parent: &mut Option<String>,
    leaf: &mut String,
) -> Result<()> {
    let boundary = uuid::Uuid::new_v4().to_string();
    let timestamp = ms_to_iso(compaction.time_ms.max(1));
    writeln!(
        f,
        "{}",
        json!({
            "type": "system", "subtype": "compact_boundary", "isCompactSummary": true,
            "uuid": boundary, "parentUuid": parent, "sessionId": sid,
            "timestamp": timestamp, "cwd": cwd,
            "content": "Conversation compacted",
            "compactMetadata": {
                "messagesSummarized": compaction.tail_start_index,
                "trigger": "manual"
            }
        })
    )?;
    let summary_uuid = uuid::Uuid::new_v4().to_string();
    writeln!(
        f,
        "{}",
        json!({
            "type": "user", "uuid": summary_uuid, "parentUuid": boundary,
            "isCompactSummary": true, "sessionId": sid,
            "timestamp": timestamp, "cwd": cwd,
            "message": {
                "role": "user",
                "content": [{ "type": "text", "text": compaction.summary_text }]
            }
        })
    )?;
    *parent = Some(summary_uuid.clone());
    *leaf = summary_uuid;
    Ok(())
}
