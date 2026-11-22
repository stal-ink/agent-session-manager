use std::cmp::Ordering;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use agent_core::ir::{Block, Message, Role, Session};

use super::parse;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const CHUNK_ITEMS: usize = 200;

pub fn write(
    paths: &parse::Paths,
    session: &Session,
    target_cwd: &str,
    overwrite: Option<&str>,
) -> Result<String> {
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let target_cwd = agent_core::path::normalize(target_cwd);
    if target_cwd.trim().is_empty() {
        bail!("目标工作区为空，无法写入 Codex 会话");
    }

    let mut server = Server::connect(&paths.home)?;
    let mut created: Option<String> = None;
    let result = (|| -> Result<String> {
        let started = server.request(
            "thread/start",
            json!({ "cwd": target_cwd, "historyMode": "legacy" }),
            Duration::from_secs(60),
        )?;
        let id = started
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("Codex app-server 未返回新线程 id")?
            .to_string();
        created = Some(id.clone());

        let items = items_for(session);
        for chunk in items.chunks(CHUNK_ITEMS) {
            server.request(
                "thread/inject_items",
                json!({ "threadId": id, "items": chunk }),
                Duration::from_secs(120),
            )?;
        }

        let title = session.title.trim();
        if !title.is_empty() {
            server.request(
                "thread/name/set",
                json!({ "threadId": id, "name": agent_core::text::truncate(title, 120) }),
                Duration::from_secs(30),
            )?;
        }

        if let Some(old) = overwrite.map(str::trim).filter(|old| !old.is_empty()) {
            if old != id && parse::active(paths, old) {
                server.request(
                    "thread/delete",
                    json!({ "threadId": old }),
                    Duration::from_secs(60),
                )?;
            }
        }
        Ok(id)
    })();

    match result {
        Ok(id) => {
            server.finish();
            let _ = augment_history(paths, &id);
            Ok(id)
        }
        Err(error) => {
            if let Some(id) = created {
                let _ = server.request(
                    "thread/delete",
                    json!({ "threadId": id }),
                    Duration::from_secs(15),
                );
            }
            server.finish();
            Err(error)
        }
    }
}

pub fn delete(paths: &parse::Paths, id: &str) -> Result<()> {
    let mut server = Server::connect(&paths.home)?;
    let result = server.request(
        "thread/delete",
        json!({ "threadId": id }),
        Duration::from_secs(60),
    );
    server.finish();
    result.map(|_| ())
}

/// Codex 的 legacy 历史构建只读 `event_msg`；`inject_items` 只写模型可见的
/// response_item，因此这里把注入内容补成 UI 事件，插在首个原生 turn 之前。
/// 已补过（首个 turn 前已有 user_message）则返回 0。
pub(super) fn augment_history(paths: &parse::Paths, id: &str) -> Result<usize> {
    let file = parse::find_rollout(paths, id).context("Codex rollout 不存在")?;
    let text = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let values: Vec<Option<Value>> = lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();

    let insert_at = values
        .iter()
        .position(|value| {
            value.as_ref().is_some_and(|value| {
                value["type"].as_str() == Some("event_msg")
                    && value["payload"]["type"].as_str() == Some("task_started")
            })
        })
        .unwrap_or(lines.len());

    let already = values[..insert_at].iter().flatten().any(|value| {
        value["type"].as_str() == Some("event_msg")
            && value["payload"]["type"].as_str() == Some("user_message")
    });
    if already {
        return Ok(0);
    }

    let mut events = Vec::new();
    for value in values[..insert_at].iter().flatten() {
        if value["type"].as_str() != Some("response_item") {
            continue;
        }
        let payload = &value["payload"];
        let timestamp = value["timestamp"].as_str().unwrap_or_default();
        let timestamp = if timestamp.is_empty() {
            agent_core::time::ms_to_iso(agent_core::ir::now_ms())
        } else {
            timestamp.to_string()
        };
        match payload["type"].as_str().unwrap_or_default() {
            "message" => {
                let role = payload["role"].as_str().unwrap_or_default();
                if role != "user" && role != "assistant" {
                    continue;
                }
                let text = content_text(payload);
                if text.trim().is_empty() || (role == "user" && parse::is_injection(&text)) {
                    continue;
                }
                let body = if role == "user" {
                    json!({
                        "type": "user_message",
                        "client_id": uuid::Uuid::new_v4().to_string(),
                        "message": text,
                        "images": [],
                        "local_images": [],
                        "audio": [],
                        "local_audio": [],
                        "text_elements": []
                    })
                } else {
                    json!({
                        "type": "agent_message",
                        "message": text,
                        "phase": null,
                        "memory_citation": null
                    })
                };
                events.push(
                    json!({
                        "timestamp": timestamp,
                        "type": "event_msg",
                        "payload": body
                    })
                    .to_string(),
                );
            }
            "reasoning" => {
                let text = reasoning_text(payload);
                if text.trim().is_empty() {
                    continue;
                }
                events.push(
                    json!({
                        "timestamp": timestamp,
                        "type": "event_msg",
                        "payload": { "type": "agent_reasoning", "text": text }
                    })
                    .to_string(),
                );
            }
            _ => {}
        }
    }

    if events.is_empty() {
        return Ok(0);
    }
    let count = events.len();
    for (offset, event) in events.into_iter().enumerate() {
        lines.insert(insert_at + offset, event);
    }
    let mut output = lines.join("\n");
    output.push('\n');
    agent_core::jsonl::write_atomic(&file, &output)?;
    Ok(count)
}

fn content_text(payload: &Value) -> String {
    let mut text = String::new();
    if let Some(content) = payload["content"].as_array() {
        for item in content {
            match item["type"].as_str().unwrap_or_default() {
                "input_text" | "output_text" => {
                    agent_core::text::push_text(
                        &mut text,
                        item["text"].as_str().unwrap_or_default(),
                    );
                }
                "input_image" => agent_core::text::push_text(&mut text, "[图片]"),
                _ => {}
            }
        }
    }
    text
}

fn reasoning_text(payload: &Value) -> String {
    let mut text = String::new();
    if let Some(summary) = payload["summary"].as_array() {
        for item in summary {
            agent_core::text::push_text(
                &mut text,
                item["summary_text"]
                    .as_str()
                    .or_else(|| item["text"].as_str())
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(content) = payload["content"].as_array() {
        for item in content {
            agent_core::text::push_text(&mut text, item["text"].as_str().unwrap_or_default());
        }
    }
    text
}

fn items_for(session: &Session) -> Vec<Value> {
    let mut items = Vec::new();
    if let Some(compaction) = &session.compaction {
        if compaction.tail_start_index == 0 && !compaction.summary_text.trim().is_empty() {
            items.push(json!({
                "type": "message", "role": "developer",
                "content": [{
                    "type": "input_text",
                    "text": format!("[上下文压缩摘要]\n{}", compaction.summary_text)
                }]
            }));
        }
    }
    for message in &session.messages {
        match message.role {
            Role::User => user_items(&mut items, message),
            Role::Assistant => assistant_items(&mut items, message),
        }
    }
    items
}

fn user_items(items: &mut Vec<Value>, message: &Message) {
    let mut content = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(text) if !text.trim().is_empty() => {
                content.push(json!({ "type": "input_text", "text": text }));
            }
            Block::Image { reference } => {
                if reference.starts_with("data:image/") {
                    content.push(json!({ "type": "input_image", "image_url": reference }));
                } else {
                    content.push(
                        json!({ "type": "input_text", "text": format!("[图片：{reference}]") }),
                    );
                }
            }
            Block::File { path } => {
                content.push(json!({ "type": "input_text", "text": format!("[文件：{path}]") }));
            }
            _ => {}
        }
    }
    if !content.is_empty() {
        items.push(json!({ "type": "message", "role": "user", "content": content }));
    }
}

fn assistant_items(items: &mut Vec<Value>, message: &Message) {
    let mut text = Vec::new();
    for block in &message.blocks {
        match block {
            Block::Text(value) if !value.trim().is_empty() => text.push(value.to_string()),
            Block::Thinking(value) if !value.trim().is_empty() => {
                flush_text(items, &mut text);
                items.push(json!({
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": value }]
                }));
            }
            Block::ToolCall {
                id,
                name,
                input,
                output,
                ..
            } => {
                flush_text(items, &mut text);
                let call_id = if id.trim().is_empty() {
                    format!("call_{}", uuid::Uuid::new_v4().simple())
                } else {
                    id.clone()
                };
                let arguments = match input {
                    Value::String(raw) if serde_json::from_str::<Value>(raw).is_ok() => raw.clone(),
                    Value::Null => "{}".to_string(),
                    other => serde_json::to_string(other).unwrap_or_else(|_| "{}".into()),
                };
                let name = if name.trim().is_empty() { "tool" } else { name };
                items.push(json!({
                    "type": "function_call",
                    "name": name,
                    "arguments": arguments,
                    "call_id": call_id
                }));
                if let Some(output) = output {
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output
                    }));
                }
            }
            Block::Image { reference } => {
                text.push(format!("[图片：{reference}]"));
            }
            Block::File { path } => {
                text.push(format!("[文件：{path}]"));
            }
            _ => {}
        }
    }
    flush_text(items, &mut text);
}

fn flush_text(items: &mut Vec<Value>, text: &mut Vec<String>) {
    if text.is_empty() {
        return;
    }
    let content: Vec<Value> = text
        .drain(..)
        .filter(|value| !value.trim().is_empty())
        .map(|value| json!({ "type": "output_text", "text": value }))
        .collect();
    if !content.is_empty() {
        items.push(json!({ "type": "message", "role": "assistant", "content": content }));
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<std::result::Result<Value, String>>,
    next_id: i64,
    _reader: thread::JoinHandle<()>,
}

impl Server {
    fn connect(codex_home: &Path) -> Result<Self> {
        let mut errors = Vec::new();
        for mut command in candidates(codex_home) {
            match Self::start(&mut command) {
                Ok(mut server) => {
                    let init = server.request(
                        "initialize",
                        json!({
                            "clientInfo": {
                                "name": "agent-session-manager",
                                "version": env!("CARGO_PKG_VERSION")
                            },
                            "capabilities": { "experimentalApi": true }
                        }),
                        Duration::from_secs(20),
                    );
                    match init {
                        Ok(_) => {
                            if let Err(error) = server.notify("initialized") {
                                errors.push(format!("initialized 通知失败：{error:#}"));
                                server.finish();
                                continue;
                            }
                            return Ok(server);
                        }
                        Err(error) => {
                            errors.push(format!("{error:#}"));
                            server.finish();
                        }
                    }
                }
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        if errors.is_empty() {
            bail!("未找到可用的 Codex app-server，请确认 Codex CLI 或 VS Code 扩展已安装");
        }
        bail!("Codex app-server 启动失败：{}", errors.join("；"))
    }

    fn start(command: &mut Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        let mut child = command.spawn().context("启动 Codex app-server")?;
        let stdout = child.stdout.take().context("读取 Codex app-server 输出")?;
        let (sender, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = sender.send(Err("app-server 已退出".into()));
                        break;
                    }
                    Ok(_) => {
                        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                            let _ = sender.send(Ok(value));
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(format!("读取 app-server 输出失败：{error}")));
                        break;
                    }
                }
            }
        });
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
            _reader: reader,
        })
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.send(json!({ "method": method }))
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "id": id, "method": method, "params": params }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Codex app-server 响应超时：{method}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(Ok(value)) => {
                    if value["id"].as_i64() != Some(id) {
                        continue;
                    }
                    if let Some(error) = value.get("error") {
                        let message = error["message"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| error.to_string());
                        bail!("Codex app-server 拒绝 {method}：{message}");
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(Err(error)) => bail!("Codex app-server 已退出：{error}"),
                Err(RecvTimeoutError::Timeout) => {
                    bail!("Codex app-server 响应超时：{method}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("Codex app-server 连接断开")
                }
            }
        }
    }

    fn send(&mut self, value: Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("Codex app-server stdin 已关闭")?;
        writeln!(stdin, "{value}").context("写入 Codex app-server")?;
        stdin.flush().context("刷新 Codex app-server")?;
        Ok(())
    }

    fn finish(mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let pid = self.child.id().to_string();
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid, "/T", "/F"]);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        let _ = command.output();
        let _ = self.child.wait();
    }
}

fn candidates(codex_home: &Path) -> Vec<Command> {
    let mut out = Vec::new();
    if command_on_path("codex") {
        let mut command = Command::new("cmd");
        command.args(["/c", "codex", "app-server"]);
        out.push(command);
    }
    if let Some(exe) = latest_vsix_codex() {
        let mut command = Command::new(exe);
        command.arg("app-server");
        out.push(command);
    }
    let daemon = codex_home
        .join("packages")
        .join("app-server-daemon")
        .join("current")
        .join("bin")
        .join("codex.exe");
    if daemon.is_file() {
        let mut command = Command::new(daemon);
        command.arg("app-server");
        out.push(command);
    }
    if let Some(exe) = desktop_codex() {
        let mut command = Command::new(exe);
        command.arg("app-server");
        out.push(command);
    }
    out
}

fn command_on_path(name: &str) -> bool {
    let mut command = Command::new("where.exe");
    command.arg(name);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}

fn latest_vsix_codex() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let mut best: Option<(String, PathBuf)> = None;
    for root in [".vscode", ".vscode-cn"] {
        let dir = home.join(root).join("extensions");
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("openai.chatgpt-") {
                continue;
            }
            let exe = entry
                .path()
                .join("bin")
                .join("windows-x86_64")
                .join("codex.exe");
            if !exe.is_file() {
                continue;
            }
            let replace = match &best {
                Some((current, _)) => name.cmp(current) == Ordering::Greater,
                None => true,
            };
            if replace {
                best = Some((name, exe));
            }
        }
    }
    best.map(|(_, path)| path)
}

fn desktop_codex() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let root = Path::new(&local).join("OpenAI").join("Codex").join("bin");
    let entries = fs::read_dir(root).ok()?;
    let mut best: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let exe = entry.path().join("codex.exe");
        if !exe.is_file() {
            continue;
        }
        let replace = match &best {
            Some((current, _)) => name.cmp(current) == Ordering::Greater,
            None => true,
        };
        if replace {
            best = Some((name, exe));
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_text_reasoning_and_tools_keep_order() {
        let session = Session {
            agent: "codex".into(),
            id: "s".into(),
            title: "t".into(),
            cwd: "C:\\work".into(),
            created_ms: 1,
            updated_ms: 2,
            messages: vec![
                Message {
                    role: Role::User,
                    time_ms: 1,
                    blocks: vec![Block::Text("hi".into())],
                },
                Message {
                    role: Role::Assistant,
                    time_ms: 2,
                    blocks: vec![
                        Block::Thinking("think".into()),
                        Block::Text("answer".into()),
                        Block::ToolCall {
                            id: "c1".into(),
                            name: "shell".into(),
                            input: json!({"command": ["ls"]}),
                            output: Some("ok".into()),
                            status: "completed".into(),
                        },
                    ],
                },
            ],
            compaction: None,
        };
        let items = items_for(&session);
        let kinds: Vec<&str> = items
            .iter()
            .filter_map(|item| item["type"].as_str())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message",
                "reasoning",
                "message",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[2]["role"], "assistant");
        assert_eq!(items[4]["call_id"], "c1");
    }

    #[test]
    fn remote_image_urls_degrade_to_text_but_data_urls_inline() {
        let session = Session {
            agent: "codex".into(),
            id: "s".into(),
            title: "t".into(),
            cwd: "C:\\work".into(),
            created_ms: 1,
            updated_ms: 2,
            messages: vec![Message {
                role: Role::User,
                time_ms: 1,
                blocks: vec![
                    Block::Image {
                        reference: "https://example.com/a.png".into(),
                    },
                    Block::Image {
                        reference: "data:image/png;base64,AAAA".into(),
                    },
                ],
            }],
            compaction: None,
        };
        let items = items_for(&session);
        let content = items[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_text");
        assert!(content[0]["text"]
            .as_str()
            .unwrap()
            .contains("https://example.com/a.png"));
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn augment_history_inserts_legacy_events_before_native_turn() {
        let home = std::env::temp_dir().join(format!("asm-codex-aug-{}", uuid::Uuid::new_v4()));
        let sessions = home.join("sessions").join("2026/01/01");
        fs::create_dir_all(&sessions).unwrap();
        let id = "01a1aug0000-0000-7000-8000-000000000001";
        let file = sessions.join(format!("rollout-test-{id}.jsonl"));
        let lines = [
            json!({"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":id}}),
            json!({"timestamp":"2026-01-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}),
            json!({"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}),
            json!({"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"think"}]}}),
            json!({"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"task_started"}}),
            json!({"timestamp":"2026-01-01T00:00:05Z","type":"event_msg","payload":{"type":"user_message","message":"native"}}),
        ];
        fs::write(
            &file,
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let paths = parse::Paths {
            home: home.clone(),
            sessions,
            archived: home.join("archived_sessions"),
        };
        assert_eq!(augment_history(&paths, id).unwrap(), 3);
        let text = fs::read_to_string(&file).unwrap();
        let task = text.find("\"task_started\"").unwrap();
        let user = text.find("\"hi\"").unwrap();
        let assistant = text.find("\"hello\"").unwrap();
        let reasoning = text.find("\"think\"").unwrap();
        assert!(user < task && assistant < task && reasoning < task);
        assert!(text.find("\"native\"").unwrap() > task);
        assert_eq!(augment_history(&paths, id).unwrap(), 0);
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn compaction_at_tail_start_becomes_developer_summary() {
        let session = Session {
            agent: "codex".into(),
            id: "s".into(),
            title: "t".into(),
            cwd: "C:\\work".into(),
            created_ms: 1,
            updated_ms: 2,
            messages: vec![Message {
                role: Role::User,
                time_ms: 1,
                blocks: vec![Block::Text("tail".into())],
            }],
            compaction: Some(agent_core::ir::Compaction {
                summary_text: "summary".into(),
                tail_start_index: 0,
                time_ms: 1,
            }),
        };
        let items = items_for(&session);
        assert_eq!(items[0]["role"], "developer");
        assert!(items[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("summary"));
        assert_eq!(items[1]["role"], "user");
    }
}
