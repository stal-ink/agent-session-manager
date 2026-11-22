use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use agent_core::ir::{Block, Role, Session};
use agent_core::time::ms_to_iso;

use super::{append_oplog, update_index, workspace_hash, Paths};

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

fn inject_with(
    paths: &Paths,
    session: &Session,
    target_cwd: &str,
    existing: Option<&str>,
) -> Result<String> {
    if !paths.sessions.is_dir() {
        fs::create_dir_all(&paths.sessions)
            .with_context(|| format!("create {}", paths.sessions.display()))?;
    }
    let cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    if cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 Kiro 会话");
    }
    let hash = workspace_hash(cwd);
    let workspace_dir = paths.sessions.join(&hash);
    fs::create_dir_all(&workspace_dir)
        .with_context(|| format!("create {}", workspace_dir.display()))?;

    let id = existing
        .map(str::to_string)
        .unwrap_or_else(|| format!("sess_{}", uuid::Uuid::new_v4()));
    let title = if session.title.trim().is_empty() {
        "转接会话".to_string()
    } else {
        session.title.trim().to_string()
    };
    let now = agent_core::ir::now_ms();
    let created = if session.created_ms > 0 {
        session.created_ms
    } else {
        now
    };
    let created = match existing {
        Some(id) => read_created_at(&workspace_dir.join(id)).unwrap_or(created),
        None => created,
    };
    let cwd_out = agent_core::path::lower_drive(&agent_core::path::normalize(cwd));

    let staging = workspace_dir.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    let result = build_session_dir(&staging, &id, &title, &cwd_out, created, now, session);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let target = workspace_dir.join(&id);
    if target.exists() {
        let retired = workspace_dir.join(format!(".retired-{}", uuid::Uuid::new_v4()));
        fs::rename(&target, &retired).with_context(|| format!("rename {}", target.display()))?;
        fs::rename(&staging, &target).with_context(|| format!("rename {}", staging.display()))?;
        let _ = fs::remove_dir_all(&retired);
    } else {
        fs::rename(&staging, &target).with_context(|| format!("rename {}", staging.display()))?;
    }

    append_oplog(paths, &format!("{hash}/{id}"), "add", now)?;
    update_index(paths, &hash, Some(&id))?;
    Ok(id)
}

fn read_created_at(dir: &Path) -> Option<i64> {
    let raw: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("session.json")).ok()?).ok()?;
    Some(super::iso_to_ms(raw["createdAt"].as_str()?))
}

fn build_session_dir(
    staging: &Path,
    id: &str,
    title: &str,
    cwd: &str,
    created_ms: i64,
    updated_ms: i64,
    session: &Session,
) -> Result<()> {
    fs::create_dir_all(staging)?;
    let session_json = json!({
        "schemaVersion": "1.0.0",
        "dataModelVersion": 1,
        "id": id,
        "title": title,
        "agentMode": "vibe",
        "workspacePaths": [cwd],
        "rootPaths": [cwd],
        "createdAt": ms_to_iso(created_ms),
        "lastModifiedAt": ms_to_iso(updated_ms),
        "modelId": "auto",
        "status": "idle",
    });
    fs::write(
        staging.join("session.json"),
        serde_json::to_string_pretty(&session_json)?,
    )?;
    fs::write(
        staging.join("messages.jsonl"),
        build_messages(session, created_ms),
    )?;
    Ok(())
}

fn build_messages(session: &Session, created_ms: i64) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut turn: Option<(String, i64)> = None;
    for (index, message) in session.messages.iter().enumerate() {
        let base = if message.time_ms > 0 {
            message.time_ms
        } else {
            created_ms + index as i64
        };
        match message.role {
            Role::User => {
                close_turn(&mut lines, &mut turn);
                let (text, images) = user_content(message);
                if text.is_empty() && images.is_empty() {
                    continue;
                }
                let tag = format!("prompt_{}", uuid::Uuid::new_v4());
                let mut user = json!({
                    "type": "user",
                    "content": text,
                    "documents": [],
                    "_meta": { "kiro": { "userMessageTag": tag } },
                });
                if !images.is_empty() {
                    user["images"] = json!(images);
                }
                push_event(&mut lines, base, uuid::Uuid::new_v4().to_string(), user);
                let execution = uuid::Uuid::new_v4().to_string();
                push_event(
                    &mut lines,
                    base + 1,
                    format!("{execution}-turn-start"),
                    json!({
                        "type": "turn_start",
                        "executionId": execution,
                        "turnId": execution.clone(),
                        "boundarySource": "observed",
                        "userMessageTag": tag,
                    }),
                );
                turn = Some((execution, base + 1));
            }
            Role::Assistant => {
                if message.blocks.is_empty() {
                    continue;
                }
                if turn.is_none() {
                    let execution = uuid::Uuid::new_v4().to_string();
                    push_event(
                        &mut lines,
                        base,
                        format!("{execution}-turn-start"),
                        json!({
                            "type": "turn_start",
                            "executionId": execution,
                            "turnId": execution.clone(),
                            "boundarySource": "observed",
                        }),
                    );
                    turn = Some((execution, base));
                }
                let execution = turn
                    .as_ref()
                    .map(|(execution, _)| execution.clone())
                    .unwrap_or_default();
                let mut last = base;
                for (offset, block) in message.blocks.iter().enumerate() {
                    let ts = base + offset as i64;
                    match block {
                        Block::Text(text) => {
                            push_event(
                                &mut lines,
                                ts,
                                format!("{}-say", uuid::Uuid::new_v4()),
                                assistant_say(text, &execution),
                            );
                        }
                        Block::Thinking(text) => {
                            push_event(
                                &mut lines,
                                ts,
                                format!("{}-reasoning", uuid::Uuid::new_v4()),
                                json!({
                                    "type": "assistant",
                                    "operationType": "Reasoning",
                                    "content": text.trim(),
                                    "executionId": execution,
                                    "turnId": execution,
                                    "_meta": { "kiro": { "agentMode": "vibe" } },
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
                            let call_id = if call_id.is_empty() {
                                format!("toolu_{}", uuid::Uuid::new_v4().simple())
                            } else {
                                call_id.clone()
                            };
                            let success = status == "completed";
                            let args = if input.is_object() {
                                input.clone()
                            } else {
                                json!({ "value": input })
                            };
                            push_event(
                                &mut lines,
                                ts,
                                format!("{call_id}-call"),
                                json!({
                                    "type": "tool_call",
                                    "toolCallId": call_id,
                                    "toolName": name,
                                    "args": args,
                                    "status": kiro_tool_status(status),
                                    "executionId": execution,
                                    "turnId": execution,
                                }),
                            );
                            push_event(
                                &mut lines,
                                ts + 1,
                                format!("{call_id}-result"),
                                json!({
                                    "type": "tool_result",
                                    "toolCallId": call_id,
                                    "content": output.clone().unwrap_or_default(),
                                    "success": success,
                                    "executionId": execution,
                                    "turnId": execution,
                                }),
                            );
                            last = ts + 1;
                        }
                        Block::Image { reference } => {
                            push_event(
                                &mut lines,
                                ts,
                                format!("{}-say", uuid::Uuid::new_v4()),
                                assistant_say(&format!("[图片] {reference}"), &execution),
                            );
                        }
                        Block::File { path } => {
                            push_event(
                                &mut lines,
                                ts,
                                format!("{}-say", uuid::Uuid::new_v4()),
                                assistant_say(&format!("[文件] {path}"), &execution),
                            );
                        }
                    }
                    last = last.max(ts);
                }
                if let Some((_, turn_last)) = turn.as_mut() {
                    *turn_last = (*turn_last).max(last);
                }
            }
        }
    }
    close_turn(&mut lines, &mut turn);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn user_content(message: &agent_core::ir::Message) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut images: Vec<Value> = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(value) => push_text(&mut text, value),
            Block::Image { reference } => {
                if let Some((mime, data)) = data_url_base64(reference) {
                    images.push(json!({ "data": data, "mimeType": mime }));
                } else {
                    push_text(&mut text, &format!("[图片] {reference}"));
                }
            }
            other => push_text(&mut text, &blocks_fallback(std::slice::from_ref(other))),
        }
    }
    (text, images)
}

fn assistant_say(text: &str, execution: &str) -> Value {
    json!({
        "type": "assistant",
        "operationType": "Say",
        "content": text.trim(),
        "executionId": execution,
        "turnId": execution,
        "_meta": { "kiro": { "agentMode": "vibe" } },
    })
}

fn close_turn(lines: &mut Vec<String>, turn: &mut Option<(String, i64)>) {
    if let Some((execution, last)) = turn.take() {
        push_event(
            lines,
            last + 1,
            format!("{execution}-turn-end"),
            json!({
                "type": "turn_end",
                "stopReason": "end_turn",
                "executionId": execution,
                "turnId": execution.clone(),
                "boundarySource": "observed",
            }),
        );
    }
}

fn push_event(lines: &mut Vec<String>, time_ms: i64, id: String, payload: Value) {
    let line = json!({
        "id": id,
        "timestamp": ms_to_iso(time_ms),
        "payload": payload,
    });
    lines.push(line.to_string());
}

fn data_url_base64(reference: &str) -> Option<(&str, &str)> {
    let rest = reference.strip_prefix("data:")?;
    let (mime, data) = rest.split_once("base64,")?;
    let mime = mime.trim_end_matches(';');
    if !mime.starts_with("image/") {
        return None;
    }
    Some((mime, data))
}

fn kiro_tool_status(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "pending" => "pending",
        "running" | "executing" => "executing",
        "awaiting_approval" => "awaiting_approval",
        "approved" => "approved",
        "denied" => "denied",
        "failed" | "error" => "failed",
        _ => "completed",
    }
}

fn blocks_fallback(blocks: &[Block]) -> String {
    let mut text = String::new();
    for block in blocks {
        match block {
            Block::Thinking(value) => push_text(&mut text, value),
            Block::File { path } => push_text(&mut text, &format!("[文件] {path}")),
            Block::ToolCall {
                name,
                input,
                output,
                ..
            } => {
                let mut line = format!("[工具调用] {name} {input}");
                if let Some(output) = output {
                    line.push_str(&format!("\n[结果] {output}"));
                }
                push_text(&mut text, &line);
            }
            Block::Image { reference } => push_text(&mut text, &format!("[图片] {reference}")),
            Block::Text(value) => push_text(&mut text, value),
        }
    }
    text
}

fn push_text(target: &mut String, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(value);
}
