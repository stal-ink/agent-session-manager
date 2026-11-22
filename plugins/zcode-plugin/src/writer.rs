use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::json;

use agent_core::ir::{Block, Compaction, Role, Session};
use agent_core::path;
use agent_core::sqlite::open_rw;

/// 无历史消息可取模型时的兜底元数据：仅用于列表展示，跟随上游默认配置。
const FALLBACK_PROVIDER_ID: &str = "account:zai-start-plan";
const FALLBACK_MODEL_ID: &str = "GLM-5.3-Flash";
/// 目标库没有任何 session 行时的兜底版本号。
const FALLBACK_VERSION: &str = "0.17.1";

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
        .unwrap_or_else(|| format!("sess_{}", uuid::Uuid::new_v4()));
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = path::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 ZCode 会话");
    }
    let target_cwd = target_cwd.as_str();
    let now = agent_core::ir::now_ms();

    let db = open_rw(&paths.db)?;
    let version: String = db
        .query_row(
            "SELECT version FROM session ORDER BY version DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| FALLBACK_VERSION.into());
    let model_pair: Option<(String, String)> = db
        .query_row(
            "SELECT json_extract(data,'$.providerId'), json_extract(data,'$.modelId')
             FROM message WHERE json_extract(data,'$.modelId') IS NOT NULL
             ORDER BY time_created DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let model: Option<String> = model_pair.as_ref().map(|(p, m)| format!("{p}/{m}"));
    let (provider_id, model_id) =
        model_pair.unwrap_or_else(|| (FALLBACK_PROVIDER_ID.into(), FALLBACK_MODEL_ID.into()));
    let trace_id = uuid::Uuid::new_v4().to_string();

    let tx = db.unchecked_transaction()?;
    if existing.is_some() {
        tx.execute("DELETE FROM part WHERE session_id = ?1", [&new_id])?;
        tx.execute("DELETE FROM message WHERE session_id = ?1", [&new_id])?;
        tx.execute("DELETE FROM session_entry WHERE session_id = ?1", [&new_id])?;
        tx.execute("DELETE FROM session WHERE id = ?1", [&new_id])?;
    }
    tx.execute(
        "INSERT INTO session (id, project_id, slug, directory, path, title, version, permission, time_created, time_updated, task_type, title_source, trace_id, last_outcome, time_title_updated)
         VALUES (?1, ?2, ?1, ?3, ?3, ?4, ?5, ?6, ?7, ?7, 'interactive', 'first_input', ?8, 'completed', ?7)",
        rusqlite::params![
            new_id,
            session_project_key(target_cwd),
            target_cwd,
            session.title,
            version,
            r#"{"mode":"guarded"}"#,
            now,
            trace_id
        ],
    )?;
    tx.execute(
        "INSERT INTO session_entry (id, session_id, type, time_created, time_updated, data)
         VALUES (?1, ?2, 'runtime/execution_state', ?3, ?3, ?4)",
        rusqlite::params![
            format!("{new_id}:runtime-execution-state"),
            new_id,
            now,
            json!({ "mode": "guarded", "planEnabled": false }).to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO session_entry (id, session_id, type, time_created, time_updated, data)
         VALUES (?1, ?2, 'runtime/model_selection', ?3, ?3, ?4)",
        rusqlite::params![
            format!("{new_id}:runtime-model-selection"),
            new_id,
            now,
            json!({
                "modelSelection": {
                    "providerId": provider_id,
                    "modelId": model_id,
                    "options": { "reasoningLevel": "max" }
                }
            })
            .to_string()
        ],
    )?;

    let mut prev_msg: Option<String> = None;
    let mut next_seq = 1i64;
    let ctx = CompactionCtx {
        session_id: &new_id,
        now,
        model_id: &model_id,
        provider_id: &provider_id,
        target_cwd,
    };
    let compaction = session.compaction.as_ref();
    let total_messages = session.messages.len();
    for (index, m) in session.messages.iter().enumerate() {
        if let Some(c) = compaction.filter(|c| c.tail_start_index == index) {
            insert_compaction(&tx, &ctx, c, total_messages, &mut next_seq, &mut prev_msg)?;
        }
        let msg_id = format!("msg_{:x}_{}", now, uuid::Uuid::new_v4());
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let mut data = json!({
            "role": role,
            "time": { "created": m.time_ms.max(1), "completed": m.time_ms.max(1) },
            "agent": "zcode-agent",
            "path": { "cwd": target_cwd, "root": target_cwd },
            "mode": "guarded",
            "cost": 0,
            "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
            "finish": "completed",
        });
        if let Some(p) = &prev_msg {
            data["parentID"] = json!(p);
        }
        if m.role == Role::Assistant {
            data["semantics"] = json!({
                "origin": "agent_runtime",
                "kind": "assistant_response",
                "uiVisibility": "visible",
                "providerVisibility": "visible",
                "transcriptVisibility": "visible",
            });
            data["modelId"] = json!(model_id);
            data["providerId"] = json!(provider_id);
            data["planEnabled"] = json!(false);
            data["finish"] = json!("stop");
            data["time"]["completed"] = json!(m.time_ms.max(1) + 1000);
        } else {
            data["modelSelection"] = json!({
                "providerId": provider_id,
                "modelId": model_id,
                "options": { "reasoningLevel": "max" }
            });
        }
        tx.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data, sequence)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
            rusqlite::params![msg_id, new_id, now, data.to_string(), next_seq],
        )?;
        next_seq += 1;

        for (pseq, b) in m.blocks.iter().enumerate() {
            let part_id = format!("part_{:x}_{}", now, uuid::Uuid::new_v4());
            let pdata = match b {
                Block::Text(t) => {
                    json!({ "type": "text", "text": t, "time": { "start": m.time_ms, "end": m.time_ms } })
                }
                Block::Thinking(t) => json!({
                    "type": "reasoning",
                    "text": t,
                    "metadata": {},
                    "time": { "start": m.time_ms, "end": m.time_ms },
                }),
                Block::ToolCall {
                    id,
                    name,
                    input,
                    output,
                    status,
                } => {
                    let tool = if name == "Agent" {
                        "Task"
                    } else {
                        name.as_str()
                    };
                    let out = output.clone().unwrap_or_default();
                    let bytes = out.len();
                    let meta = json!({
                        "schemaVersion": 1,
                        "serialization": {
                            "truncated": false,
                            "originalBytes": bytes,
                            "returnedBytes": bytes,
                            "budgetStrategy": "truncate"
                        }
                    });
                    let state = if status == "error" {
                        json!({
                            "status": "error",
                            "input": input,
                            "error": out,
                            "metadata": meta,
                            "time": { "start": m.time_ms, "end": m.time_ms.max(1) + 1 },
                        })
                    } else {
                        json!({
                            "status": status,
                            "input": input,
                            "output": out,
                            "title": tool,
                            "metadata": meta,
                            "time": { "start": m.time_ms, "end": m.time_ms.max(1) + 1 },
                        })
                    };
                    json!({
                        "type": "tool",
                        "callID": id,
                        "declarationIndex": 0,
                        "tool": tool,
                        "metadata": meta,
                        "state": state,
                    })
                }
                Block::Image { reference } => json!({
                    "type": "text",
                    "text": format!("[图片：{reference}]"),
                    "time": { "start": m.time_ms, "end": m.time_ms },
                }),
                Block::File { path } => json!({
                    "type": "text",
                    "text": format!("[文件：{path}]"),
                    "time": { "start": m.time_ms, "end": m.time_ms },
                }),
            };
            tx.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                rusqlite::params![part_id, msg_id, new_id, now, pdata.to_string(), pseq as i64 + 1],
            )?;
        }
        prev_msg = Some(msg_id);
    }
    if let Some(c) = compaction.filter(|c| c.tail_start_index >= session.messages.len()) {
        insert_compaction(&tx, &ctx, c, total_messages, &mut next_seq, &mut prev_msg)?;
    }
    tx.commit()?;

    let tasks = open_rw(&paths.tasks_db)?;
    let tx_tasks = tasks.unchecked_transaction()?;
    if existing.is_some() {
        tx_tasks.execute("DELETE FROM tasks WHERE task_id = ?1", [&new_id])?;
        tx_tasks.execute(
            "DELETE FROM task_group_admissions WHERE task_id = ?1",
            [&new_id],
        )?;
    }
    let project_id = ensure_project(&tx_tasks, target_cwd, now)?;
    let meta = json!({
        "taskId": new_id,
        "traceId": trace_id,
        "title": session.title,
        "titleOverridden": false,
        "workspacePath": target_cwd,
        "createdAt": now,
        "updatedAt": now,
        "mode": "build",
        "model": model,
        "thoughtLevel": "max",
        "provider": "glm",
        "status": "completed",
        "target": null,
    });
    tx_tasks.execute(
        "INSERT INTO tasks (workspace_key, workspace_path, task_id, title, task_status, provider, mode, model, migration_source,
                            created_at, updated_at, cli_created_at, cli_updated_at, cli_title, cli_last_outcome, cli_deleted,
                            meta_json, project_id, root_path)
         VALUES (?1, ?1, ?2, ?3, 'completed', 'glm', 'build', ?4, ?5, ?6, ?6, ?6, ?6, ?3, 'completed', 0, ?7, ?8, ?1)",
        rusqlite::params![
            target_cwd,
            new_id,
            session.title,
            model,
            session.agent.clone(),
            now,
            meta.to_string(),
            project_id
        ],
    )?;
    tx_tasks.execute(
        "INSERT OR REPLACE INTO task_group_admissions (workspace_key, task_id, system_kind, user_organized, creation_id)
         VALUES (?1, ?2, NULL, 1, NULL)",
        rusqlite::params![target_cwd, new_id],
    )?;
    tx_tasks.commit()?;
    Ok(new_id)
}

/// 压缩边界写入所需的会话级上下文，避免写入函数参数过长。
struct CompactionCtx<'a> {
    session_id: &'a str,
    now: i64,
    model_id: &'a str,
    provider_id: &'a str,
    target_cwd: &'a str,
}

fn insert_compaction(
    tx: &rusqlite::Transaction<'_>,
    ctx: &CompactionCtx<'_>,
    compaction: &Compaction,
    total_messages: usize,
    next_seq: &mut i64,
    prev_msg: &mut Option<String>,
) -> Result<()> {
    let CompactionCtx {
        session_id,
        now,
        model_id,
        provider_id,
        target_cwd,
    } = *ctx;
    let boundary_id = format!("compact_{}", uuid::Uuid::new_v4());
    let operation_id = format!("cmp_{}", uuid::Uuid::new_v4());
    let timeline_id = format!("msg_{:x}_{}", now, uuid::Uuid::new_v4());
    let summary_id = format!("msg_{:x}_{}", now, uuid::Uuid::new_v4());
    let started = if compaction.time_ms > 0 {
        compaction.time_ms
    } else {
        now
    };
    // 原生语义：tail_start_id 指向压缩前最后一条消息，模型窗口从摘要消息开始。
    let tail_id = prev_msg.clone();
    let body = summary_body(&compaction.summary_text);
    let summarized = compaction.tail_start_index.min(total_messages);
    let kept = total_messages.saturating_sub(summarized);

    let mut timeline_data = json!({
        "role": "assistant",
        "time": { "created": started, "completed": started },
        "modelId": model_id,
        "providerId": provider_id,
        "agent": "zcode-agent",
        "path": { "cwd": target_cwd, "root": target_cwd },
        "mode": "guarded",
        "planEnabled": false,
        "cost": 0,
        "tokens": { "total": 0, "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
        "finish": "completed",
        "semantics": {
            "origin": "system",
            "kind": "timeline_event",
            "uiVisibility": "visible",
            "providerVisibility": "hidden",
            "transcriptVisibility": "visible"
        }
    });
    if let Some(tail) = &tail_id {
        timeline_data["parentID"] = json!(tail);
    }
    tx.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data, sequence)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        rusqlite::params![
            timeline_id,
            session_id,
            started,
            timeline_data.to_string(),
            *next_seq
        ],
    )?;
    *next_seq += 1;

    let timeline_part = json!({
        "type": "timeline",
        "timelineType": "context_compaction",
        "display": "separator",
        "status": "completed",
        "operationId": operation_id,
        "trigger": "manual",
        "phase": "standalone_turn",
        "compactReason": "user_requested",
        "boundaryId": boundary_id,
        "summaryMessageId": summary_id,
        "time": { "start": started, "end": started }
    });
    insert_part(tx, session_id, &timeline_id, 1, started, &timeline_part)?;

    let mut timeline_compaction = json!({
        "type": "compaction",
        "auto": false,
        "trigger": "manual",
        "phase": "standalone_turn",
        "compactReason": "user_requested",
        "operationId": operation_id,
        "timelineStatus": "completed",
        "timelineDisplay": "separator",
        "replace": true,
        "boundaryId": boundary_id,
        "summaryMessageId": summary_id,
        "time": { "start": started, "end": started }
    });
    if let Some(tail) = &tail_id {
        timeline_compaction["tail_start_id"] = json!(tail);
    }
    insert_part(
        tx,
        session_id,
        &timeline_id,
        2,
        started,
        &timeline_compaction,
    )?;

    let summary_data = json!({
        "role": "user",
        "time": { "created": started },
        "summary": { "title": "会话摘要", "body": body, "diffs": [] },
        "agent": "zcode-agent",
        "modelSelection": {
            "providerId": provider_id,
            "modelId": model_id,
            "options": { "reasoningLevel": "max" }
        },
        "semantics": {
            "origin": "agent_runtime",
            "kind": "compact_summary",
            "uiVisibility": "hidden",
            "providerVisibility": "visible",
            "transcriptVisibility": "hidden"
        }
    });
    tx.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data, sequence)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        rusqlite::params![
            summary_id,
            session_id,
            started,
            summary_data.to_string(),
            *next_seq
        ],
    )?;
    *next_seq += 1;

    let summary_text = json!({
        "type": "text",
        "text": body,
        "synthetic": true,
        "time": { "start": started, "end": started }
    });
    insert_part(tx, session_id, &summary_id, 1, started, &summary_text)?;

    let mut summary_compaction = json!({
        "type": "compaction",
        "auto": false,
        "trigger": "manual",
        "phase": "standalone_turn",
        "compactReason": "user_requested",
        "time": { "start": started, "end": started },
        "compactBoundary": {
            "boundaryId": boundary_id,
            "preCompactDiscoveredTools": [],
            "trigger": "manual",
            "phase": "standalone_turn",
            "compactReason": "user_requested",
            "summarySource": "model",
            "summarizedMessageCount": summarized,
            "keptMessageCount": kept,
            "summaryMessageIds": [summary_id]
        }
    });
    if let Some(tail) = &tail_id {
        summary_compaction["tail_start_id"] = json!(tail);
        summary_compaction["compactBoundary"]["lastSummarizedMessageId"] = json!(tail);
    }
    insert_part(tx, session_id, &summary_id, 2, started, &summary_compaction)?;
    // 摘要消息自身没有 parentID；压缩后的新消息重新起链，与原生记录一致。
    *prev_msg = None;
    Ok(())
}

fn insert_part(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    message_id: &str,
    sequence: i64,
    now: i64,
    data: &serde_json::Value,
) -> Result<()> {
    tx.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data, sequence)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
        rusqlite::params![
            format!("part_{:x}_{}", now, uuid::Uuid::new_v4()),
            message_id,
            session_id,
            now,
            data.to_string(),
            sequence
        ],
    )?;
    Ok(())
}

fn summary_body(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix("<cb_summary>").unwrap_or(text);
    text.strip_suffix("</cb_summary>")
        .unwrap_or(text)
        .trim()
        .to_string()
}

fn ensure_project(tasks: &Connection, cwd: &str, now: i64) -> Result<String> {
    let cwd = path::normalize(cwd);
    let mut roots = tasks.prepare("SELECT project_id, root_path FROM project_roots")?;
    let rows = roots.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (pid, root) = row?;
        if path::same(&root, &cwd) {
            tasks.execute(
                "UPDATE projects SET deleted = 0, deleted_at = NULL, updated_at = ?2 WHERE project_id = ?1",
                rusqlite::params![pid, now],
            )?;
            return Ok(pid);
        }
    }

    let name = path::name(&cwd);
    if let Some(pid) = tasks
        .query_row(
            "SELECT project_id FROM projects
             WHERE host_id = 'local' AND name = ?1 AND deleted = 0
             LIMIT 1",
            [&name],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        let position: i64 = tasks.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM project_roots WHERE project_id = ?1",
            [&pid],
            |r| r.get(0),
        )?;
        tasks.execute(
            "INSERT OR IGNORE INTO project_roots (project_id, root_path, position, is_primary)
             VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![pid, cwd, position],
        )?;
        tasks.execute(
            "UPDATE projects SET deleted = 0, deleted_at = NULL, updated_at = ?2 WHERE project_id = ?1",
            rusqlite::params![pid, now],
        )?;
        return Ok(pid);
    }

    let project_id = uuid::Uuid::new_v4().to_string();
    let position: i64 = tasks
        .query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM projects",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    tasks.execute(
        "INSERT INTO projects (project_id, host_id, name, position, created_at, updated_at)
         VALUES (?1, 'local', ?2, ?3, ?4, ?4)",
        rusqlite::params![project_id, name, position, now],
    )?;
    tasks.execute(
        "INSERT INTO project_roots (project_id, root_path, position, is_primary) VALUES (?1, ?2, 0, 1)",
        rusqlite::params![project_id, cwd],
    )?;
    Ok(project_id)
}

fn session_project_key(cwd: &str) -> String {
    let mut mapped = String::from("proj_");
    let mut last_dash = true;
    for c in cwd.chars() {
        let l = c.to_ascii_lowercase();
        if l.is_ascii_alphanumeric() || l == '.' {
            mapped.push(l);
            last_dash = false;
        } else if !last_dash {
            mapped.push('-');
            last_dash = true;
        }
    }
    while mapped.ends_with('-') {
        mapped.pop();
    }
    mapped
}
