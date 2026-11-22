use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use agent_core::ir::{Block, Message, Role, Session};
use agent_core::path as paths;

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
    super::lock::ensure_unlocked(paths, "转接")?;
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = paths::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 Kimi 会话");
    }
    let id = existing
        .map(str::to_string)
        .unwrap_or_else(|| format!("session_{}", uuid::Uuid::new_v4()));

    // Kimi 没有原地改写历史：覆盖即删除旧目录后按同 id 重建。
    if let Some(old) = super::session_dir(paths, &id) {
        fs::remove_dir_all(&old).with_context(|| format!("remove {}", old.display()))?;
    }

    let dir = paths.sessions.join(bucket_name(&target_cwd)).join(&id);
    let agent_dir = dir.join("agents").join("main");
    fs::create_dir_all(&agent_dir).with_context(|| format!("create {}", agent_dir.display()))?;

    let created = if session.created_ms > 0 {
        session.created_ms
    } else {
        agent_core::ir::now_ms()
    };
    let updated = if session.updated_ms > 0 {
        session.updated_ms
    } else {
        agent_core::ir::now_ms()
    };

    let built = write_wire(&agent_dir.join("wire.jsonl"), created, session).and_then(|()| {
        write_state(
            &dir,
            &id,
            &target_cwd,
            &agent_dir,
            created,
            updated,
            session,
        )
    });
    if let Err(error) = built {
        let _ = fs::remove_dir_all(&dir);
        return Err(error);
    }

    super::append_index(
        paths,
        json!({
            "sessionId": id,
            "sessionDir": forward(&dir),
            "workDir": target_cwd.replace('\\', "/")
        }),
    )?;
    Ok(id)
}

/// 工作区目录名：`wd_` + slug + `_` + sha256(正斜杠保留大小写路径) 前 12 位。
fn bucket_name(cwd: &str) -> String {
    let key = paths::normalize(cwd).replace('\\', "/");
    format!("wd_{}_{}", slug(cwd), &sha256_hex(key.as_bytes())[..12])
}

fn slug(cwd: &str) -> String {
    let lowered = paths::name(cwd).to_lowercase();
    let mapped: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed: String = mapped.trim_matches('-').chars().take(40).collect();
    let trimmed = trimmed.trim_matches('-');
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed.to_string()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn write_state(
    dir: &Path,
    id: &str,
    cwd: &str,
    agent_dir: &Path,
    created: i64,
    updated: i64,
    session: &Session,
) -> Result<()> {
    let title = session.title.trim();
    let title = if title.is_empty() {
        "转接会话"
    } else {
        title
    };
    let last_prompt = last_user_text(session).unwrap_or_else(|| title.to_string());
    let doc = json!({
        "id": id,
        "version": 2,
        "cwd": cwd.replace('\\', "/"),
        "archived": false,
        "agents": { "main": { "homedir": forward(agent_dir), "type": "main" } },
        "custom": {},
        "title": title,
        "titleKind": "custom",
        "isCustomTitle": true,
        "lastPrompt": last_prompt,
        "lastTurnReason": "completed",
        "createdAt": created,
        "updatedAt": updated,
    });
    agent_core::jsonl::write_atomic(&dir.join("state.json"), &doc.to_string())
}

fn last_user_text(session: &Session) -> Option<String> {
    session
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::User)
        .find_map(|message| {
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text(text) if !text.trim().is_empty() => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        })
}

fn write_wire(path: &Path, created: i64, session: &Session) -> Result<()> {
    let mut file = fs::File::create(path).with_context(|| format!("write {}", path.display()))?;
    emit(
        &mut file,
        json!({
            "type": "metadata", "protocol_version": "1.5",
            "min_protocol_version": "1.5", "created_at": created
        }),
    )?;

    let mut turn: i64 = -1;
    let mut step = 0i64;
    let mut open = false;
    let mut last_time = created.max(1);

    for message in &session.messages {
        let time = message.time_ms.max(1);
        match message.role {
            Role::User => {
                let content = user_content(message);
                if content.is_empty() {
                    continue;
                }
                if open {
                    emit_turn_end(&mut file, turn, last_time)?;
                }
                turn += 1;
                step = 0;
                open = true;
                let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                emit(
                    &mut file,
                    json!({
                        "type": "turn.prompt", "agentId": "main",
                        "input": content.clone(), "origin": { "kind": "user" },
                        "promptId": message_id, "turnId": turn,
                        "trigger": "normal", "time": time
                    }),
                )?;
                emit(
                    &mut file,
                    json!({
                        "type": "context.append_message", "agentId": "main",
                        "message": {
                            "role": "user", "content": content, "toolCalls": [],
                            "id": message_id, "origin": { "kind": "user" }
                        },
                        "time": time
                    }),
                )?;
                last_time = time;
            }
            Role::Assistant => {
                let blocks: Vec<&Block> =
                    message.blocks.iter().filter(|block| kept(block)).collect();
                if blocks.is_empty() {
                    continue;
                }
                if !open {
                    turn += 1;
                    step = 0;
                    open = true;
                }
                step += 1;
                write_step(&mut file, turn, step, time, &blocks)?;
                last_time = time;
            }
        }
    }
    if open {
        emit_turn_end(&mut file, turn, last_time)?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn emit(file: &mut fs::File, value: Value) -> Result<()> {
    writeln!(file, "{value}")?;
    Ok(())
}

fn emit_turn_end(file: &mut fs::File, turn: i64, time: i64) -> Result<()> {
    emit(
        file,
        json!({
            "type": "turn.ended", "agentId": "main", "turnId": turn,
            "reason": "completed", "durationMs": 0, "time": time
        }),
    )
}

fn kept(block: &Block) -> bool {
    match block {
        Block::Text(text) | Block::Thinking(text) => !text.trim().is_empty(),
        _ => true,
    }
}

fn user_content(message: &Message) -> Vec<Value> {
    let mut content = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(text) => {
                if !text.trim().is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
            }
            Block::Image { reference } => {
                content.push(json!({ "type": "text", "text": format!("[图片：{reference}]") }))
            }
            Block::File { path } => {
                content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
            }
            _ => {}
        }
    }
    content
}

fn write_step(
    file: &mut fs::File,
    turn: i64,
    step: i64,
    time: i64,
    blocks: &[&Block],
) -> Result<()> {
    let turn_id = turn.to_string();
    let step_uuid = uuid::Uuid::new_v4().to_string();
    let event = |value: Value| {
        json!({
            "type": "context.append_loop_event", "agentId": "main",
            "event": value, "time": time
        })
    };

    emit(
        file,
        event(json!({
            "type": "step.begin", "uuid": step_uuid, "turnId": turn_id, "step": step
        })),
    )?;

    let mut block_index = 0usize;
    for block in blocks {
        match block {
            Block::Thinking(think) => {
                emit(
                    file,
                    event(json!({
                        "type": "content.part", "uuid": uuid::Uuid::new_v4().to_string(),
                        "turnId": turn_id, "step": step, "stepUuid": step_uuid,
                        "block": block_index,
                        "part": { "type": "think", "think": think },
                        "startedAt": time, "durationMs": 0
                    })),
                )?;
                block_index += 1;
            }
            Block::Text(text) => {
                emit(
                    file,
                    event(json!({
                        "type": "content.part", "uuid": uuid::Uuid::new_v4().to_string(),
                        "turnId": turn_id, "step": step, "stepUuid": step_uuid,
                        "block": block_index,
                        "part": { "type": "text", "text": text },
                        "startedAt": time, "durationMs": 0
                    })),
                )?;
                block_index += 1;
            }
            Block::Image { reference } => {
                emit(
                    file,
                    event(json!({
                        "type": "content.part", "uuid": uuid::Uuid::new_v4().to_string(),
                        "turnId": turn_id, "step": step, "stepUuid": step_uuid,
                        "block": block_index,
                        "part": { "type": "text", "text": format!("[图片：{reference}]") },
                        "startedAt": time, "durationMs": 0
                    })),
                )?;
                block_index += 1;
            }
            Block::File { path } => {
                emit(
                    file,
                    event(json!({
                        "type": "content.part", "uuid": uuid::Uuid::new_v4().to_string(),
                        "turnId": turn_id, "step": step, "stepUuid": step_uuid,
                        "block": block_index,
                        "part": { "type": "text", "text": format!("[文件：{path}]") },
                        "startedAt": time, "durationMs": 0
                    })),
                )?;
                block_index += 1;
            }
            Block::ToolCall {
                id,
                name,
                input,
                output,
                status,
            } => {
                let call_id = if id.trim().is_empty() {
                    format!("toolu_{}", uuid::Uuid::new_v4().simple())
                } else {
                    id.clone()
                };
                let call_uuid = uuid::Uuid::new_v4().to_string();
                let args = if input.is_null() {
                    json!({})
                } else {
                    input.clone()
                };
                emit(
                    file,
                    event(json!({
                        "type": "tool.call", "uuid": call_uuid, "turnId": turn_id,
                        "step": step, "stepUuid": step_uuid,
                        "toolCallId": call_id, "name": name, "args": args
                    })),
                )?;
                emit(
                    file,
                    event(json!({
                        "type": "tool.result", "parentUuid": call_uuid,
                        "toolCallId": call_id,
                        "result": {
                            "output": output.clone().unwrap_or_default(),
                            "isError": status == "error",
                            "durationMs": 0
                        }
                    })),
                )?;
            }
        }
    }

    emit(
        file,
        event(json!({
            "type": "step.end", "uuid": step_uuid, "turnId": turn_id, "step": step,
            "finishReason": "end_turn",
            "messageId": format!("msg_{}", uuid::Uuid::new_v4()),
            "usage": { "inputOther": 0, "output": 0, "inputCacheRead": 0, "inputCacheCreation": 0 },
            "providerFinishReason": "completed", "rawFinishReason": "end_turn"
        })),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ir::Message;
    use serde_json::json;

    fn temp_paths(tag: &str) -> super::super::Paths {
        let home = std::env::temp_dir().join(format!("asm-kimi-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        super::super::Paths {
            sessions: home.join("sessions"),
            index: home.join("session_index.jsonl"),
            projects: home.join("projects.json"),
            locks: home.join("locks").join("sessions"),
            home,
        }
    }

    fn sample_session() -> Session {
        Session {
            agent: "kimi".into(),
            id: "source".into(),
            title: "测试会话".into(),
            cwd: "D:/src".into(),
            created_ms: 1_700_000_000_000,
            updated_ms: 1_700_000_005_000,
            messages: vec![
                Message {
                    role: Role::User,
                    time_ms: 1_700_000_000_100,
                    blocks: vec![Block::Text("第一问".into())],
                },
                Message {
                    role: Role::Assistant,
                    time_ms: 1_700_000_001_000,
                    blocks: vec![
                        Block::Thinking("想一想".into()),
                        Block::Text("回答".into()),
                        Block::ToolCall {
                            id: "toolu_1".into(),
                            name: "Grep".into(),
                            input: json!({ "pattern": "x" }),
                            output: Some("命中".into()),
                            status: "completed".into(),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    time_ms: 1_700_000_002_000,
                    blocks: vec![Block::Text("第二问".into())],
                },
                Message {
                    role: Role::Assistant,
                    time_ms: 1_700_000_003_000,
                    blocks: vec![Block::Text("回答二".into())],
                },
            ],
            compaction: None,
        }
    }

    #[test]
    fn bucket_and_slug_follow_kimi_layout() {
        assert_eq!(bucket_name("D:/work/example"), "wd_example_04be385f1028");
        assert_eq!(bucket_name(r"D:\work\example"), "wd_example_04be385f1028");
        assert_eq!(slug("C:/Users/Me/My Project!"), "my-project");
        assert_eq!(slug("C:/work/My_App.v2"), "my_app.v2");
        assert_eq!(slug("C:/"), "c");
        assert_eq!(slug(""), "workspace");
        let long = format!("C:/x/{}", "a".repeat(60));
        assert_eq!(slug(&long).chars().count(), 40);
        let trailing = format!("C:/x/{}-b", "a".repeat(39));
        assert_eq!(slug(&trailing), "a".repeat(39));
    }

    #[test]
    fn inject_writes_state_wire_and_index() {
        let paths = temp_paths("inject");
        let cwd = paths.home.join("work").to_string_lossy().to_string();
        let id = inject(&paths, &sample_session(), &cwd).unwrap();
        assert!(id.starts_with("session_"));
        let normalized = paths::normalize(&cwd);
        let dir = paths.sessions.join(bucket_name(&normalized)).join(&id);

        let state: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(state["id"], id);
        assert_eq!(state["version"], 2);
        assert_eq!(state["cwd"], normalized.replace('\\', "/"));
        assert_eq!(state["archived"], false);
        assert_eq!(state["title"], "测试会话");
        assert_eq!(state["titleKind"], "custom");
        assert_eq!(state["isCustomTitle"], true);
        assert_eq!(state["lastPrompt"], "第二问");
        assert_eq!(state["lastTurnReason"], "completed");
        assert_eq!(state["createdAt"], 1_700_000_000_000i64);
        assert_eq!(state["updatedAt"], 1_700_000_005_000i64);
        assert_eq!(state["custom"], json!({}));
        assert_eq!(state["agents"]["main"]["type"], "main");
        let homedir = state["agents"]["main"]["homedir"].as_str().unwrap();
        assert!(homedir.contains('/') && !homedir.contains('\\'));
        assert!(homedir.ends_with(&format!("agents/main")));

        let wire = fs::read_to_string(dir.join("agents").join("main").join("wire.jsonl")).unwrap();
        let rows: Vec<Value> = wire
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows[0]["type"], "metadata");
        assert_eq!(rows[0]["protocol_version"], "1.5");
        let count = |kind: &str| {
            rows.iter()
                .filter(|row| row["event"]["type"] == kind)
                .count()
        };
        assert_eq!(
            rows.iter()
                .filter(|row| row["type"] == "turn.prompt")
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["type"] == "turn.ended")
                .count(),
            2
        );
        assert_eq!(count("step.begin"), 2);
        assert_eq!(count("step.end"), 2);
        assert_eq!(count("content.part"), 3);
        assert_eq!(count("tool.call"), 1);
        assert_eq!(count("tool.result"), 1);
        let think = rows
            .iter()
            .find(|row| row["event"]["part"]["type"] == "think")
            .unwrap();
        assert_eq!(think["event"]["part"]["think"], "想一想");
        assert_eq!(think["event"]["block"], 0);
        let call = rows
            .iter()
            .find(|row| row["event"]["type"] == "tool.call")
            .unwrap();
        assert_eq!(call["event"]["args"], json!({ "pattern": "x" }));
        assert_eq!(call["event"]["toolCallId"], "toolu_1");
        let result = rows
            .iter()
            .find(|row| row["event"]["type"] == "tool.result")
            .unwrap();
        assert_eq!(result["event"]["result"]["output"], "命中");
        assert_eq!(result["event"]["result"]["isError"], false);
        assert_eq!(result["event"]["parentUuid"], call["event"]["uuid"]);

        let index = fs::read_to_string(&paths.index).unwrap();
        let line: Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
        assert_eq!(line["sessionId"], id);
        assert_eq!(line["sessionDir"], forward(&dir));
        assert_eq!(line["workDir"], normalized.replace('\\', "/"));

        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn live_lock_blocks_inject_with_transfer_wording() {
        let paths = temp_paths("lock-other");
        write_live_lock(&paths, "session_other");
        let error = inject(&paths, &sample_session(), "D:/work")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Kimi Code CLI"), "{error}");
        assert!(error.contains("再转接"), "{error}");
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn crash_residue_lock_does_not_block_inject() {
        let paths = temp_paths("lock-residue");
        fs::create_dir_all(&paths.locks).unwrap();
        fs::write(paths.locks.join("session_other.lock"), "x").unwrap();
        let id = inject(&paths, &sample_session(), "D:/work").unwrap();
        assert!(super::super::session_dir(&paths, &id).is_some());
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn live_target_lock_blocks_overwrite_with_transfer_wording() {
        let paths = temp_paths("lock-overwrite");
        let id = inject(&paths, &sample_session(), "D:/work").unwrap();
        write_live_lock(&paths, &id);
        let error = overwrite(&paths, &sample_session(), "D:/other", &id)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Kimi Code CLI"), "{error}");
        assert!(error.contains("再转接"), "{error}");
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn unreadable_target_lock_is_treated_as_crash_residue() {
        let paths = temp_paths("lock-malformed");
        let id = inject(&paths, &sample_session(), "D:/work").unwrap();
        fs::create_dir_all(&paths.locks).unwrap();
        fs::write(paths.locks.join(format!("{id}.lock")), "x").unwrap();
        assert!(overwrite(&paths, &sample_session(), "D:/other", &id).is_ok());
        let _ = fs::remove_dir_all(&paths.home);
    }

    fn write_live_lock(paths: &super::super::Paths, id: &str) {
        fs::create_dir_all(&paths.locks).unwrap();
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_default();
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let lock = json!({
            "version": 1,
            "token": format!("{}:test", std::process::id()),
            "pid": std::process::id(),
            "hostname": hostname,
            "startedAt": started,
            "clientIdentity": "kimi_code_cli",
            "clientDisplayName": "Kimi Code CLI"
        });
        fs::write(paths.locks.join(format!("{id}.lock")), lock.to_string()).unwrap();
    }

    #[test]
    fn overwrite_rebuilds_same_id_under_target_workspace() {
        let paths = temp_paths("overwrite");
        let first = paths.home.join("one").to_string_lossy().to_string();
        let second = paths.home.join("two").to_string_lossy().to_string();
        let id = inject(&paths, &sample_session(), &first).unwrap();
        let again = overwrite(&paths, &sample_session(), &second, &id).unwrap();
        assert_eq!(again, id);
        assert!(!paths
            .sessions
            .join(bucket_name(&paths::normalize(&first)))
            .join(&id)
            .exists());
        assert!(paths
            .sessions
            .join(bucket_name(&paths::normalize(&second)))
            .join(&id)
            .join("state.json")
            .is_file());

        let index = fs::read_to_string(&paths.index).unwrap();
        assert_eq!(index.lines().count(), 2);
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn null_tool_input_becomes_empty_object() {
        let paths = temp_paths("nullargs");
        let mut session = sample_session();
        session.messages = vec![
            Message {
                role: Role::User,
                time_ms: 10,
                blocks: vec![Block::Text("问".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 20,
                blocks: vec![Block::ToolCall {
                    id: String::new(),
                    name: "Bash".into(),
                    input: Value::Null,
                    output: None,
                    status: "error".into(),
                }],
            },
        ];
        let id = inject(&paths, &session, "D:/work").unwrap();
        let wire = fs::read_to_string(
            paths
                .sessions
                .join(bucket_name("D:/work"))
                .join(&id)
                .join("agents")
                .join("main")
                .join("wire.jsonl"),
        )
        .unwrap();
        let rows: Vec<Value> = wire
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let call = rows
            .iter()
            .find(|row| row["event"]["type"] == "tool.call")
            .unwrap();
        assert_eq!(call["event"]["args"], json!({}));
        assert!(call["event"]["toolCallId"]
            .as_str()
            .unwrap()
            .starts_with("toolu_"));
        let result = rows
            .iter()
            .find(|row| row["event"]["type"] == "tool.result")
            .unwrap();
        assert_eq!(result["event"]["result"]["isError"], true);
        assert_eq!(result["event"]["result"]["output"], "");
        let _ = fs::remove_dir_all(&paths.home);
    }
}
