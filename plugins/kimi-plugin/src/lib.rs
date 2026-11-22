//! Kimi Code 适配器：会话为 `sessions/<bucket>/<session_id>/`，
//! 元数据在 `state.json`（旧版 `session-meta/state.json`），正文是
//! `agents/main/wire.jsonl` 事件流。

mod lock;
mod writer;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use agent_core::ir::{Block, Compaction, Message, Role, Session, SessionSummary};
use agent_core::jsonl::read_events;
use anyhow::{Context, Result};
use serde_json::Value;

pub use writer::{inject, overwrite};

pub const ID: &str = "kimi";

pub struct Paths {
    pub home: PathBuf,
    pub sessions: PathBuf,
    pub index: PathBuf,
    pub projects: PathBuf,
    pub locks: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = std::env::var("KIMI_CODE_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".kimi-code")
            });
        Self {
            sessions: home.join("sessions"),
            index: home.join("session_index.jsonl"),
            projects: home.join("projects.json"),
            locks: home.join("locks").join("sessions"),
            home,
        }
    }
}

struct State {
    id: String,
    cwd: String,
    title: String,
    created_ms: i64,
    updated_ms: i64,
    archived: bool,
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let mut out = Vec::new();
    for dir in session_dirs(paths) {
        let Some(state) = read_state(&dir) else {
            continue;
        };
        let wire = parse_wire(&dir).unwrap_or_default();
        out.push(SessionSummary {
            agent: ID.into(),
            title: resolve_title(&state.title, &wire.first_user, &state.id),
            id: state.id,
            cwd: agent_core::path::normalize(&state.cwd),
            created_ms: state.created_ms,
            updated_ms: state.updated_ms,
            message_count: wire.messages.len() as i64,
            archived: state.archived,
            deleted: false,
            workspace_deleted: false,
        });
    }
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let dir = session_dir(paths, id).with_context(|| format!("Kimi 会话不存在：{id}"))?;
    let state = read_state(&dir).with_context(|| format!("Kimi 会话元数据缺失：{id}"))?;
    let wire = if dir.join("agents").join("main").join("wire.jsonl").is_file() {
        parse_wire(&dir)?
    } else {
        Wire::default()
    };
    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title: resolve_title(&state.title, &wire.first_user, id),
        cwd: agent_core::path::normalize(&state.cwd),
        created_ms: state.created_ms,
        updated_ms: state.updated_ms,
        messages: wire.messages,
        compaction: wire.compaction,
    })
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let dir = session_dir(paths, id).with_context(|| format!("Kimi 会话不存在：{id}"))?;
    lock::ensure_unlocked(paths, "删除")?;
    fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    append_index(
        paths,
        serde_json::json!({ "sessionId": id, "deleted": true }),
    )
}

pub fn active(paths: &Paths, id: &str) -> bool {
    session_dir(paths, id).is_some()
}

/// 会话 id 与目录名一致，只允许 `session_` 加安全字符。
fn valid_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("session_") else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(crate) fn session_dir(paths: &Paths, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    session_dirs(paths)
        .into_iter()
        .find(|dir| dir.file_name().and_then(|name| name.to_str()) == Some(id))
}

fn session_dirs(paths: &Paths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(buckets) = fs::read_dir(&paths.sessions) else {
        return out;
    };
    for bucket in buckets.flatten() {
        if !bucket
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        // .index-cache 与 .index-dirty 不是工作区 bucket。
        if bucket.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("session_") {
                continue;
            }
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir = entry.path();
            if state_path(&dir).is_some() {
                out.push(dir);
            }
        }
    }
    out
}

pub(crate) fn state_path(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join("state.json");
    if primary.is_file() {
        return Some(primary);
    }
    let legacy = dir.join("session-meta").join("state.json");
    if legacy.is_file() {
        return Some(legacy);
    }
    None
}

fn read_state(dir: &Path) -> Option<State> {
    let text = fs::read_to_string(state_path(dir)?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let id = value["id"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    Some(State {
        id,
        cwd: value["cwd"].as_str().unwrap_or_default().to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        created_ms: value["createdAt"].as_i64().unwrap_or(0),
        updated_ms: value["updatedAt"].as_i64().unwrap_or(0),
        archived: value["archived"].as_bool().unwrap_or(false),
    })
}

pub(crate) fn append_index(paths: &Paths, value: Value) -> Result<()> {
    use std::io::Write as _;

    if let Some(parent) = paths.index.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.index)
        .with_context(|| format!("append {}", paths.index.display()))?;
    writeln!(file, "{value}")?;
    Ok(())
}

fn resolve_title(state_title: &str, first_user: &str, id: &str) -> String {
    let title = state_title.trim();
    if !title.is_empty() && !title.eq_ignore_ascii_case("New Session") && title != "新会话" {
        return title.to_string();
    }
    if !first_user.trim().is_empty() {
        return agent_core::text::truncate(first_user.trim(), 60);
    }
    let short: String = id.trim_start_matches("session_").chars().take(8).collect();
    format!("Kimi 会话 {short}")
}

#[derive(Default)]
struct Wire {
    messages: Vec<Message>,
    compaction: Option<Compaction>,
    first_user: String,
}

fn parse_wire(dir: &Path) -> Result<Wire> {
    let path = dir.join("agents").join("main").join("wire.jsonl");
    let events = read_events(&path)?;
    let mut reader = Reader::default();
    for value in events {
        reader.apply(&value);
    }
    reader.finish();
    let first_user = reader
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .find_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                Block::Text(text) => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default();
    Ok(Wire {
        messages: reader.messages,
        compaction: reader.compaction,
        first_user,
    })
}

#[derive(Default)]
struct Reader {
    messages: Vec<Message>,
    tool_sites: HashMap<String, (usize, usize)>,
    pending: Option<usize>,
    compaction: Option<Compaction>,
}

impl Reader {
    fn apply(&mut self, value: &Value) {
        match value["type"].as_str().unwrap_or_default() {
            "context.append_message" => self.user_message(value),
            "context.append_loop_event" => self.loop_event(value),
            "context.undo" => {
                self.flush();
                let count = value["count"].as_u64().unwrap_or(0) as usize;
                let keep = self.messages.len().saturating_sub(count);
                self.messages.truncate(keep);
                self.tool_sites.retain(|_, (index, _)| *index < keep);
            }
            "context.clear" => {
                self.messages.clear();
                self.tool_sites.clear();
                self.pending = None;
                self.compaction = None;
            }
            "context.apply_compaction" => {
                self.compaction = Some(Compaction {
                    summary_text: value["summary"].as_str().unwrap_or_default().to_string(),
                    tail_start_index: self.messages.len(),
                    time_ms: value["time"].as_i64().unwrap_or(0),
                });
            }
            _ => {}
        }
    }

    fn user_message(&mut self, value: &Value) {
        let message = &value["message"];
        if message["role"].as_str() != Some("user") {
            return;
        }
        // 注入、打断提醒等非用户来源不是对话内容。
        if message["origin"]["kind"].as_str() != Some("user") {
            return;
        }
        let mut blocks = Vec::new();
        for part in message["content"].as_array().into_iter().flatten() {
            match part["type"].as_str().unwrap_or_default() {
                "text" => {
                    let text = part["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() {
                        blocks.push(Block::Text(text.to_string()));
                    }
                }
                kind if is_media(kind) => blocks.push(Block::Text("[图片]".into())),
                _ => {}
            }
        }
        if blocks.is_empty() {
            return;
        }
        self.flush();
        self.messages.push(Message {
            role: Role::User,
            time_ms: value["time"].as_i64().unwrap_or(0),
            blocks,
        });
    }

    fn loop_event(&mut self, value: &Value) {
        let event = &value["event"];
        let time_ms = value["time"].as_i64().unwrap_or(0);
        match event["type"].as_str().unwrap_or_default() {
            "step.begin" => self.open_step(time_ms),
            "content.part" => {
                let part = &event["part"];
                match part["type"].as_str().unwrap_or_default() {
                    "text" => {
                        let text = part["text"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            self.push_block(Block::Text(text.to_string()), time_ms);
                        }
                    }
                    "think" => {
                        let text = part["think"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            self.push_block(Block::Thinking(text.to_string()), time_ms);
                        }
                    }
                    _ => {}
                }
            }
            "tool.call" => {
                let id = event["toolCallId"].as_str().unwrap_or_default();
                let name = event["name"].as_str().unwrap_or_default();
                self.push_tool_call(id.to_string(), name, event["args"].clone(), time_ms);
            }
            "tool.result" => {
                let id = event["toolCallId"].as_str().unwrap_or_default();
                let result = &event["result"];
                let output = result["output"].as_str().map(str::to_string).or_else(|| {
                    if result["output"].is_null() {
                        None
                    } else {
                        Some(result["output"].to_string())
                    }
                });
                self.fill_tool_result(id, output, result["isError"].as_bool().unwrap_or(false));
            }
            "step.end" => self.flush(),
            _ => {}
        }
    }

    fn finish(&mut self) {
        self.flush();
    }

    /// 结束当前 pending assistant step：无块时不产生空消息。
    fn flush(&mut self) {
        let Some(index) = self.pending.take() else {
            return;
        };
        let empty = self
            .messages
            .get(index)
            .map(|message| message.blocks.is_empty())
            .unwrap_or(true);
        if empty {
            if index + 1 == self.messages.len() {
                self.messages.remove(index);
            }
            self.tool_sites.retain(|_, (message, _)| *message != index);
        }
    }

    fn open_step(&mut self, time_ms: i64) {
        self.flush();
        self.messages.push(Message {
            role: Role::Assistant,
            time_ms,
            blocks: Vec::new(),
        });
        self.pending = Some(self.messages.len() - 1);
    }

    fn ensure_step(&mut self, time_ms: i64) {
        if self.pending.is_none() {
            self.open_step(time_ms);
        }
    }

    fn push_block(&mut self, block: Block, time_ms: i64) {
        self.ensure_step(time_ms);
        if let Some(index) = self.pending {
            self.messages[index].blocks.push(block);
        }
    }

    fn push_tool_call(&mut self, id: String, name: &str, input: Value, time_ms: i64) {
        self.ensure_step(time_ms);
        let Some(index) = self.pending else {
            return;
        };
        let slot = self.messages[index].blocks.len();
        self.messages[index].blocks.push(Block::ToolCall {
            id: id.clone(),
            name: name.to_string(),
            input,
            output: None,
            status: "completed".into(),
        });
        self.tool_sites.insert(id, (index, slot));
    }

    fn fill_tool_result(&mut self, id: &str, output: Option<String>, is_error: bool) {
        let Some(&(index, slot)) = self.tool_sites.get(id) else {
            return;
        };
        if let Some(Block::ToolCall {
            output: slot_output,
            status,
            ..
        }) = self
            .messages
            .get_mut(index)
            .and_then(|message| message.blocks.get_mut(slot))
        {
            if output.is_some() {
                *slot_output = output;
            }
            *status = String::from(if is_error { "error" } else { "completed" });
        }
    }
}

fn is_media(kind: &str) -> bool {
    kind.contains("image") || kind.contains("media") || kind.contains("blob")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_paths(tag: &str) -> Paths {
        let home = std::env::temp_dir().join(format!("asm-kimi-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        Paths {
            sessions: home.join("sessions"),
            index: home.join("session_index.jsonl"),
            projects: home.join("projects.json"),
            locks: home.join("locks").join("sessions"),
            home,
        }
    }

    fn sample_session() -> Session {
        Session {
            agent: ID.into(),
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

    fn state_value(id: &str, title: &str, archived: bool, created: i64, updated: i64) -> Value {
        json!({
            "id": id, "version": 2, "cwd": "D:/src", "archived": archived,
            "title": title, "titleKind": "replaceable", "isCustomTitle": false,
            "lastPrompt": "第一问", "lastTurnReason": "completed",
            "createdAt": created, "updatedAt": updated
        })
    }

    /// 在临时数据根写入一个会话目录（可指定 state 位置与 wire 正文）。
    fn write_fixture(
        paths: &Paths,
        id: &str,
        state: &Value,
        legacy_state: bool,
        wire: Option<&str>,
    ) -> PathBuf {
        let bucket = paths.sessions.join("wd_test_000000000000");
        let dir = bucket.join(id);
        std::fs::create_dir_all(dir.join("agents").join("main")).unwrap();
        let state_dir = if legacy_state {
            dir.join("session-meta")
        } else {
            dir.clone()
        };
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::to_string(state).unwrap(),
        )
        .unwrap();
        if let Some(wire) = wire {
            std::fs::write(dir.join("agents").join("main").join("wire.jsonl"), wire).unwrap();
        }
        dir
    }

    #[test]
    fn write_then_list_and_load_roundtrip() {
        let paths = temp_paths("roundtrip");
        let cwd = paths.home.join("work");
        let cwd = cwd.to_string_lossy().to_string();
        let id = writer::inject(&paths, &sample_session(), &cwd).unwrap();

        let summaries = list(&paths).unwrap();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.agent, "kimi");
        assert_eq!(summary.id, id);
        assert_eq!(summary.title, "测试会话");
        assert_eq!(summary.message_count, 4);
        assert_eq!(summary.created_ms, 1_700_000_000_000);
        assert_eq!(summary.updated_ms, 1_700_000_005_000);
        assert!(!summary.archived && !summary.deleted && !summary.workspace_deleted);
        assert_eq!(summary.cwd, agent_core::path::normalize(&cwd));

        let session = load(&paths, &id).unwrap();
        assert_eq!(session.messages.len(), 4);
        assert_eq!(session.title, "测试会话");
        assert!(session.compaction.is_none());
        match &session.messages[1].blocks[..] {
            [Block::Thinking(think), Block::Text(text), Block::ToolCall {
                id,
                name,
                output,
                status,
                ..
            }] => {
                assert_eq!(think, "想一想");
                assert_eq!(text, "回答");
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "Grep");
                assert_eq!(output.as_deref(), Some("命中"));
                assert_eq!(status, "completed");
            }
            other => panic!("unexpected blocks: {other:?}"),
        }
        assert_eq!(session.messages[1].time_ms, 1_700_000_001_000);

        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn compaction_undo_and_injection_are_applied() {
        let paths = temp_paths("controls");
        let id = "session_abcdef01-2345-6789-abcd-ef0123456789";
        let wire = r#"{"type":"metadata","protocol_version":"1.5","min_protocol_version":"1.5","created_at":1}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"问一"}],"toolCalls":[],"id":"m1","origin":{"kind":"user"}},"time":10}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.begin","uuid":"s1","turnId":"0","step":1},"time":11}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"p1","turnId":"0","step":1,"stepUuid":"s1","block":0,"part":{"type":"think","think":"想"},"startedAt":11,"durationMs":0},"time":12}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"p2","turnId":"0","step":1,"stepUuid":"s1","block":1,"part":{"type":"text","text":"答一"},"startedAt":12,"durationMs":0},"time":13}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","uuid":"s1","turnId":"0","step":1,"finishReason":"end_turn","messageId":"msg_1"},"time":14}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"问二"},{"type":"image","blob":"ref"}],"id":"m2","origin":{"kind":"user"}},"time":20}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"注入"}],"origin":{"kind":"injection"}},"time":21}
{"type":"context.apply_compaction","agentId":"main","summary":"摘要正文","compactedCount":2,"wireLines":{"start":1,"end":4},"time":30}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.begin","uuid":"s2","turnId":"1","step":1},"time":31}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"p3","turnId":"1","step":1,"stepUuid":"s2","block":0,"part":{"type":"text","text":"答二"},"startedAt":31,"durationMs":0},"time":32}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","uuid":"s2","turnId":"1","step":1,"finishReason":"end_turn"},"time":33}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"问三"}],"id":"m3","origin":{"kind":"user"}},"time":40}
{"type":"context.undo","agentId":"main","count":1,"time":50}"#;
        write_fixture(
            &paths,
            id,
            &state_value(id, "New Session", false, 1, 50),
            false,
            Some(wire),
        );

        let session = load(&paths, id).unwrap();
        assert_eq!(session.title, "问一");
        assert_eq!(session.messages.len(), 4);
        assert_eq!(session.messages[0].role, Role::User);
        assert_eq!(session.messages[1].blocks.len(), 2);
        match &session.messages[2].blocks[..] {
            [Block::Text(text), Block::Text(image)] => {
                assert_eq!(text, "问二");
                assert_eq!(image, "[图片]");
            }
            other => panic!("unexpected blocks: {other:?}"),
        }
        assert_eq!(session.messages[3].time_ms, 31);
        let compaction = session.compaction.expect("compaction");
        assert_eq!(compaction.summary_text, "摘要正文");
        assert_eq!(compaction.tail_start_index, 3);
        assert_eq!(compaction.time_ms, 30);

        let summaries = list(&paths).unwrap();
        assert_eq!(summaries[0].message_count, 4);
        assert_eq!(summaries[0].title, "问一");

        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn clear_drops_messages_and_compaction() {
        let paths = temp_paths("clear");
        let id = "session_11111111-2222-3333-4444-555555555555";
        let wire = r#"{"type":"metadata","protocol_version":"1.5","created_at":1}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"问"}],"id":"m1","origin":{"kind":"user"}},"time":10}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.begin","uuid":"s1","turnId":"0","step":1},"time":11}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"p1","turnId":"0","step":1,"stepUuid":"s1","block":0,"part":{"type":"text","text":"答"},"startedAt":11,"durationMs":0},"time":12}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","uuid":"s1","turnId":"0","step":1,"finishReason":"end_turn"},"time":13}
{"type":"context.apply_compaction","agentId":"main","summary":"旧摘要","time":20}
{"type":"context.clear","agentId":"main","time":30}"#;
        write_fixture(
            &paths,
            id,
            &state_value(id, "", false, 1, 30),
            false,
            Some(wire),
        );

        let session = load(&paths, id).unwrap();
        assert!(session.messages.is_empty());
        assert!(session.compaction.is_none());
        // 无 state 标题、无用户文本时回退固定标题。
        assert_eq!(session.title, "Kimi 会话 11111111");

        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn list_falls_back_to_truncated_first_input_and_legacy_state() {
        let paths = temp_paths("fallback");
        let id = "session_99999999-8888-7777-6666-555555555555";
        let long = "q".repeat(80);
        let wire = format!(
            "{{\"type\":\"context.append_message\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{long}\"}}],\"id\":\"m1\",\"origin\":{{\"kind\":\"user\"}}}},\"time\":5}}"
        );
        write_fixture(
            &paths,
            id,
            &state_value(id, "新会话", true, 100, 200),
            true,
            Some(&wire),
        );

        let summaries = list(&paths).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title.chars().count(), 60);
        assert!(summaries[0].archived);
        assert_eq!(summaries[0].cwd, "D:\\src");
        assert_eq!(summaries[0].message_count, 1);

        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn missing_wire_counts_zero_and_sorts_by_updated() {
        let paths = temp_paths("counts");
        let older = "session_aaaaaaaa-0000-0000-0000-000000000000";
        let newer = "session_bbbbbbbb-0000-0000-0000-000000000000";
        write_fixture(
            &paths,
            older,
            &state_value(older, "旧", false, 1, 100),
            false,
            None,
        );
        let wire = r#"{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"新"}],"id":"m1","origin":{"kind":"user"}},"time":5}"#;
        write_fixture(
            &paths,
            newer,
            &state_value(newer, "新", false, 1, 900),
            false,
            Some(wire),
        );

        let summaries = list(&paths).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, newer);
        assert_eq!(summaries[0].message_count, 1);
        assert_eq!(summaries[1].id, older);
        assert_eq!(summaries[1].message_count, 0);

        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn delete_removes_dir_and_appends_index() {
        let paths = temp_paths("delete");
        let id = writer::inject(&paths, &sample_session(), "D:/work").unwrap();
        assert!(active(&paths, &id));

        delete(&paths, &id).unwrap();
        assert!(!active(&paths, &id));
        assert!(session_dir(&paths, &id).is_none());

        let index = std::fs::read_to_string(&paths.index).unwrap();
        let last: Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
        assert_eq!(last["sessionId"], id);
        assert_eq!(last["deleted"], true);

        assert!(delete(&paths, &id).is_err());
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn delete_blocks_live_lock_with_delete_wording() {
        let paths = temp_paths("delete-lock");
        let id = writer::inject(&paths, &sample_session(), "D:/work").unwrap();
        write_live_lock(&paths, "session_other");
        let error = delete(&paths, &id).unwrap_err().to_string();
        assert!(error.contains("Kimi Code CLI"), "{error}");
        assert!(error.contains("再删除"), "{error}");
        assert!(active(&paths, &id));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn delete_ignores_crash_residue_locks() {
        let paths = temp_paths("delete-other-lock");
        let id = writer::inject(&paths, &sample_session(), "D:/work").unwrap();
        std::fs::create_dir_all(&paths.locks).unwrap();
        std::fs::write(paths.locks.join("session_other.lock"), "x").unwrap();
        std::fs::write(paths.locks.join(format!("{id}.lock")), "x").unwrap();
        delete(&paths, &id).unwrap();
        assert!(!active(&paths, &id));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[cfg(windows)]
    #[test]
    fn delete_ignores_lock_with_dead_pid() {
        let paths = temp_paths("delete-dead-lock");
        let id = writer::inject(&paths, &sample_session(), "D:/work").unwrap();
        std::fs::create_dir_all(&paths.locks).unwrap();
        let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let lock = json!({
            "version": 1,
            "token": "4000000:test",
            "pid": 4_000_000,
            "hostname": hostname,
            "startedAt": started,
            "clientIdentity": "kimi_code_cli",
            "clientDisplayName": "Kimi Code CLI"
        });
        std::fs::write(paths.locks.join("session_other.lock"), lock.to_string()).unwrap();
        delete(&paths, &id).unwrap();
        assert!(!active(&paths, &id));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    fn write_live_lock(paths: &Paths, id: &str) {
        std::fs::create_dir_all(&paths.locks).unwrap();
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
        std::fs::write(paths.locks.join(format!("{id}.lock")), lock.to_string()).unwrap();
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let paths = temp_paths("ids");
        assert!(!active(&paths, "session_../../x"));
        assert!(!active(&paths, "other_id"));
        assert!(load(&paths, "session_../../x").is_err());
        assert!(load(&paths, "session_").is_err());
        let _ = std::fs::remove_dir_all(&paths.home);
    }
}
