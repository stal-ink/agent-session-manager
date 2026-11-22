use std::fs;
use std::io::Write as _;

use anyhow::Result;
use serde_json::{json, Value};

use agent_core::ir::{Block, Role, Session};
use agent_core::path;

pub fn inject(paths: &super::Paths, session: &Session, target_cwd: &str) -> Result<String> {
    write_session(paths, session, target_cwd, None)
}

pub fn overwrite(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    target_id: &str,
) -> Result<String> {
    write_session(paths, session, target_cwd, Some(target_id))
}

fn write_session(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    existing: Option<&str>,
) -> Result<String> {
    let id = existing
        .map(str::to_string)
        .unwrap_or_else(|| format!("session-{}", uuid::Uuid::new_v4()));
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = path::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 DSH 会话");
    }
    if existing.is_some() && super::session_dir(paths, &id).is_some() {
        super::destroy(paths, &id)?;
    }

    let dir = paths
        .sessions
        .join(super::project_key(&target_cwd))
        .join(&id);
    fs::create_dir_all(&dir)?;
    let path = dir.join("session.v4.jsonl.zstd");
    let tmp = dir.join(format!(".session.{}.tmp", uuid::Uuid::new_v4().simple()));

    let created = if session.created_ms > 0 {
        session.created_ms
    } else {
        agent_core::ir::now_ms()
    };
    let header = json!({
        "type": "session", "version": 4, "id": id, "createdAt": created,
        "cwd": target_cwd, "isSeeded": false, "delegationDepth": 0,
        "agentPreset": "standard"
    });
    let mut log = EventLog::new(created);
    log.push(
        "session/title",
        created,
        json!({
            "title": session.title,
            "messageSeqs": [],
            "source": { "kind": "user" }
        }),
    );

    let mut surfaces: Vec<u64> = Vec::new();
    let mut compaction_done = session.compaction.is_none();
    let mut turn = 0i64;
    let mut step = 0i64;
    let mut open_turn = false;
    let mut open_step = false;

    for (index, message) in session.messages.iter().enumerate() {
        if !compaction_done {
            if let Some(compaction) = session
                .compaction
                .as_ref()
                .filter(|c| c.tail_start_index == index)
            {
                close_step(&mut log, &mut open_step, turn, step, message.time_ms);
                close_turn(&mut log, &mut open_turn, turn, message.time_ms);
                let seq = write_checkpoint(&mut log, compaction, &surfaces);
                surfaces = vec![seq];
                compaction_done = true;
            }
        }
        let time = message.time_ms.max(1);
        match message.role {
            Role::User => {
                close_step(&mut log, &mut open_step, turn, step, time);
                close_turn(&mut log, &mut open_turn, turn, time);
                turn += 1;
                step = 1;
                open_turn = true;
                open_step = true;
                log.push("turn/start", time, json!({ "turn": turn }));
                log.push("step/start", time, json!({ "turn": turn, "step": step }));
                let seq =
                    log.push_surface("user/message", time, user_data(message), json!("append"));
                surfaces.push(seq);
            }
            Role::Assistant => {
                if !open_turn {
                    turn += 1;
                    step = 1;
                    open_turn = true;
                    log.push("turn/start", time, json!({ "turn": turn }));
                    log.push("step/start", time, json!({ "turn": turn, "step": step }));
                } else if !open_step {
                    step += 1;
                    log.push("step/start", time, json!({ "turn": turn, "step": step }));
                }
                let seq = log.push_surface(
                    "assistant/message",
                    time,
                    assistant_data(message, turn, step),
                    json!("append"),
                );
                surfaces.push(seq);
                for block in &message.blocks {
                    if let Block::ToolCall {
                        id,
                        name,
                        input,
                        output,
                        status,
                    } = block
                    {
                        log.push(
                            "tool/call",
                            time,
                            json!({
                                "turn": turn, "step": step, "callId": id,
                                "name": name, "arguments": input.to_string()
                            }),
                        );
                        let result = json!({
                            "turn": turn, "step": step,
                            "message": {
                                "role": "tool",
                                "source": { "kind": "tool", "callId": id },
                                "toolCallId": id,
                                "content": [{
                                    "type": "text",
                                    "text": output.clone().unwrap_or_default()
                                }],
                                "isError": status == "error",
                                "id": uuid::Uuid::new_v4().to_string()
                            }
                        });
                        let seq = log.push_surface("tool/result", time, result, json!("append"));
                        surfaces.push(seq);
                    }
                }
                log.push("step/end", time, json!({ "turn": turn, "step": step }));
                open_step = false;
            }
        }
    }
    if !compaction_done {
        if let Some(compaction) = session.compaction.as_ref() {
            close_step(&mut log, &mut open_step, turn, step, compaction.time_ms);
            close_turn(&mut log, &mut open_turn, turn, compaction.time_ms);
            write_checkpoint(&mut log, compaction, &surfaces);
        }
    }
    let last = log.last_time;
    close_step(&mut log, &mut open_step, turn, step, last);
    close_turn(&mut log, &mut open_turn, turn, last);

    let mut file = fs::File::create(&tmp)?;
    file.write_all(&encode_frame(format!("{header}\n").as_bytes())?)?;
    let mut body = String::new();
    for event in &log.events {
        body.push_str(&event.to_string());
        body.push('\n');
    }
    file.write_all(&encode_frame(body.as_bytes())?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, &path)?;

    attach_workspace(paths, &id, &target_cwd)?;
    Ok(id)
}

struct EventLog {
    events: Vec<Value>,
    seq: u64,
    last_time: i64,
}

impl EventLog {
    fn new(created: i64) -> Self {
        Self {
            events: Vec::new(),
            seq: 0,
            last_time: created,
        }
    }

    fn push(&mut self, kind: &str, time: i64, data: Value) -> u64 {
        self.last_time = self.last_time.max(time);
        let seq = self.seq;
        self.events.push(json!({
            "type": kind, "seq": seq, "time": self.last_time, "data": data
        }));
        self.seq += 1;
        seq
    }

    fn push_surface(&mut self, kind: &str, time: i64, data: Value, surface: Value) -> u64 {
        self.last_time = self.last_time.max(time);
        let seq = self.seq;
        self.events.push(json!({
            "type": kind, "seq": seq, "time": self.last_time,
            "data": data, "surfaceOp": surface
        }));
        self.seq += 1;
        seq
    }
}

fn close_step(log: &mut EventLog, open: &mut bool, turn: i64, step: i64, time: i64) {
    if *open {
        log.push("step/end", time, json!({ "turn": turn, "step": step }));
        *open = false;
    }
}

fn close_turn(log: &mut EventLog, open: &mut bool, turn: i64, time: i64) {
    if *open {
        log.push(
            "turn/end",
            time,
            json!({ "turn": turn, "reason": { "kind": "completed" } }),
        );
        *open = false;
    }
}

fn write_checkpoint(
    log: &mut EventLog,
    compaction: &agent_core::ir::Compaction,
    surfaces: &[u64],
) -> u64 {
    const PREAMBLE: &str = "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.";
    let time = compaction.time_ms.max(1);
    let content = json!([
        { "type": "text", "text": format!("{PREAMBLE}\n\n<compacted-summary>") },
        { "type": "text", "text": compaction.summary_text },
        { "type": "text", "text": "</compacted-summary>" }
    ]);
    let (Some(&start), Some(&end)) = (surfaces.first(), surfaces.last()) else {
        // 没有可遮蔽的历史（压缩后截断写入）：检查点作为普通追加消息。
        let data = json!({
            "content": content,
            "source": {
                "kind": "compact-checkpoint",
                "compactionId": uuid::Uuid::new_v4().to_string()
            },
            "role": "user",
            "id": uuid::Uuid::new_v4().to_string()
        });
        return log.push_surface("user/message", time, data, json!("append"));
    };
    let compaction_id = uuid::Uuid::new_v4().to_string();
    let start_seq = log.push(
        "compaction/start",
        time,
        json!({ "compactionId": compaction_id, "turn": null }),
    );
    let summary_seq = log.push(
        "compaction/summary",
        time,
        json!({
            "compactionId": compaction_id,
            "summary": [{ "type": "text", "text": compaction.summary_text }],
            "shadowedRange": { "start": start, "end": end },
            "shadowedSeqs": surfaces,
            "shadowedTokenCount": 0,
            "provider": "deepseek-official",
            "model": "deepseek-v4-flash"
        }),
    );
    let data = json!({
        "content": content,
        "source": { "kind": "compact-checkpoint", "compactionId": compaction_id },
        "role": "user",
        "id": uuid::Uuid::new_v4().to_string()
    });
    let mut sources = Vec::with_capacity(surfaces.len() + 2);
    sources.push(json!(start_seq));
    sources.push(json!(summary_seq));
    for seq in surfaces {
        sources.push(json!(seq));
    }
    let checkpoint = json!({
        "type": "user/message", "seq": log.seq, "time": log.last_time.max(time),
        "data": data,
        "surfaceOp": { "op": "replace", "startSeq": start, "endSeq": end },
        "sourceEventSeqs": sources
    });
    let seq = log.seq;
    log.seq += 1;
    log.events.push(checkpoint);
    log.push(
        "compaction/end",
        time,
        json!({ "compactionId": compaction_id, "turn": null }),
    );
    seq
}

fn user_data(message: &agent_core::ir::Message) -> Value {
    let mut content = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(text) => content.push(json!({ "type": "text", "text": text })),
            Block::Image { reference } => {
                content.push(json!({ "type": "text", "text": format!("[图片：{reference}]") }))
            }
            Block::File { path } => {
                content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
            }
            _ => {}
        }
    }
    json!({
        "content": content,
        "source": {
            "kind": "user",
            "rpcId": uuid::Uuid::new_v4().to_string(),
            "clientTimeZone": "Asia/Shanghai"
        },
        "role": "user",
        "id": uuid::Uuid::new_v4().to_string()
    })
}

fn assistant_data(message: &agent_core::ir::Message, turn: i64, step: i64) -> Value {
    let mut content = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(text) => content.push(json!({ "type": "text", "text": text })),
            Block::Thinking(text) => content.push(json!({ "type": "reasoning", "text": text })),
            Block::ToolCall {
                id, name, input, ..
            } => content.push(json!({
                "type": "tool-call", "id": id, "name": name,
                "arguments": input.to_string()
            })),
            Block::Image { reference } => {
                content.push(json!({ "type": "text", "text": format!("[图片：{reference}]") }))
            }
            Block::File { path } => {
                content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
            }
        }
    }
    json!({
        "turn": turn,
        "step": step,
        "message": {
            "role": "assistant",
            "content": content,
            "source": {
                "kind": "model",
                "provider": "deepseek-official",
                "model": "deepseek-v4-flash"
            },
            "id": uuid::Uuid::new_v4().to_string()
        },
        // 还原路径要求 assistant/message 携带结算字段 stream（可为空数组）。
        "stream": []
    })
}

fn encode_frame(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
    encoder.include_checksum(true)?;
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

fn attach_workspace(paths: &super::Paths, id: &str, cwd: &str) -> Result<()> {
    let path = paths.storages.join("workspace.json");
    let Some(mut doc) = super::read_json(&path)? else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut attached = false;
    if let Some(tables) = doc
        .pointer_mut("/tables/workspaces")
        .and_then(Value::as_object_mut)
    {
        for workspace in tables.values_mut() {
            let matches = workspace["path"]
                .as_str()
                .map(|existing| path::same(existing, cwd))
                .unwrap_or(false);
            if !matches {
                continue;
            }
            let list = workspace
                .as_object_mut()
                .expect("workspace record")
                .entry("sessionIds")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(list) = list.as_array_mut() {
                if !list.iter().any(|item| item.as_str() == Some(id)) {
                    list.insert(0, Value::String(id.to_string()));
                }
                workspace["updatedAt"] = Value::String(now.clone());
            }
            attached = true;
            break;
        }
        if !attached {
            let workspace_id = uuid::Uuid::new_v4().to_string();
            tables.insert(
                workspace_id.clone(),
                json!({
                    "path": cwd,
                    "title": path::name(cwd),
                    "sessionIds": [id],
                    "createdAt": now,
                    "updatedAt": now
                }),
            );
            if let Some(global) = doc.pointer_mut("/global").and_then(Value::as_object_mut) {
                let ids = global
                    .entry("workspaceIds")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(ids) = ids.as_array_mut() {
                    ids.insert(0, Value::String(workspace_id));
                }
            }
        }
    }
    if let Some(global) = doc.pointer_mut("/global").and_then(Value::as_object_mut) {
        for key in ["archivedSessionIds", "pinnedSessionIds"] {
            if let Some(list) = global.get_mut(key).and_then(Value::as_array_mut) {
                list.retain(|item| item.as_str() != Some(id));
            }
        }
    }
    super::write_json(&path, &doc)
}
