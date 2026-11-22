use std::fs;
use std::io::Write as _;

use agent_core::path as paths;
use agent_core::time::ms_to_iso;
use agent_core::{Block, Compaction, Role, Session};
use anyhow::Result;
use serde_json::{json, Value};

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
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = paths::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        anyhow::bail!("目标工作区为空，无法写入 Claude Code 会话");
    }
    let slug: String = paths::lower_drive(&target_cwd)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = paths.projects.join(&slug);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.jsonl"));
    let tmp = dir.join(format!(".{id}.{}.tmp", uuid::Uuid::new_v4().simple()));
    write_jsonl(&tmp, &id, &target_cwd, session)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&tmp, &path)?;
    if let Some(old_id) = existing {
        if let Some(old) = super::session_file(paths, old_id) {
            if old != path && old.is_file() {
                let _ = fs::remove_file(&old);
            }
        }
    }
    Ok(id)
}

fn write_jsonl(path: &std::path::Path, sid: &str, cwd: &str, session: &Session) -> Result<()> {
    let mut f = fs::File::create(path)?;
    let mut parent: Option<String> = None;
    let mut compaction_written = false;

    for (index, m) in session.messages.iter().enumerate() {
        if !compaction_written {
            if let Some(compaction) = session
                .compaction
                .as_ref()
                .filter(|c| c.tail_start_index == index)
            {
                write_compaction(&mut f, sid, cwd, compaction, &mut parent)?;
                compaction_written = true;
            }
        }
        write_message(&mut f, sid, cwd, m, &mut parent)?;
    }
    if !compaction_written {
        if let Some(compaction) = session.compaction.as_ref() {
            write_compaction(&mut f, sid, cwd, compaction, &mut parent)?;
        }
    }
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

fn write_message(
    f: &mut fs::File,
    sid: &str,
    cwd: &str,
    m: &agent_core::Message,
    parent: &mut Option<String>,
) -> Result<()> {
    let uuid = uuid::Uuid::new_v4().to_string();
    let iso = ms_to_iso(m.time_ms.max(1));
    match m.role {
        Role::User => {
            let mut content: Vec<Value> = Vec::new();
            for b in &m.blocks {
                match b {
                    Block::Text(t) => content.push(json!({ "type": "text", "text": t })),
                    Block::Image { reference } => content
                        .push(json!({ "type": "text", "text": format!("[图片：{reference}]") })),
                    Block::File { path } => {
                        content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
                    }
                    _ => {}
                }
            }
            writeln!(
                f,
                "{}",
                json!({
                    "type": "user", "uuid": uuid, "parentUuid": parent, "promptId": uuid,
                    "sessionId": sid, "timestamp": iso, "cwd": cwd, "userType": "external",
                    "entrypoint": "cli",
                    "message": { "role": "user", "content": content }
                })
            )?;
            *parent = Some(uuid);
        }
        Role::Assistant => {
            let mut content: Vec<Value> = Vec::new();
            let mut results: Vec<Value> = Vec::new();
            for b in &m.blocks {
                match b {
                    Block::Text(t) => content.push(json!({ "type": "text", "text": t })),
                    Block::Thinking(t) => {
                        content.push(json!({ "type": "thinking", "thinking": t }))
                    }
                    Block::ToolCall {
                        id,
                        name,
                        input,
                        output,
                        status,
                    } => {
                        content.push(
                            json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                        );
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": output.clone().unwrap_or_default(),
                            "is_error": status == "error"
                        }));
                    }
                    Block::Image { reference } => content
                        .push(json!({ "type": "text", "text": format!("[图片：{reference}]") })),
                    Block::File { path } => {
                        content.push(json!({ "type": "text", "text": format!("[文件：{path}]") }))
                    }
                }
            }
            writeln!(
                f,
                "{}",
                json!({
                    "type": "assistant", "uuid": uuid, "parentUuid": parent,
                    "sessionId": sid, "timestamp": iso, "cwd": cwd, "userType": "external",
                    "entrypoint": "cli",
                    "message": {
                        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        "type": "message", "role": "assistant", "model": "transferred",
                        "content": content
                    }
                })
            )?;
            *parent = Some(uuid);

            if !results.is_empty() {
                let ruuid = uuid::Uuid::new_v4().to_string();
                writeln!(
                    f,
                    "{}",
                    json!({
                        "type": "user", "uuid": ruuid, "parentUuid": parent,
                        "sessionId": sid, "timestamp": iso, "cwd": cwd, "userType": "external",
                        "entrypoint": "cli",
                        "message": { "role": "user", "content": results }
                    })
                )?;
                *parent = Some(ruuid);
            }
        }
    }
    Ok(())
}

fn write_compaction(
    f: &mut fs::File,
    sid: &str,
    cwd: &str,
    compaction: &Compaction,
    parent: &mut Option<String>,
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
    *parent = Some(summary_uuid);
    Ok(())
}
