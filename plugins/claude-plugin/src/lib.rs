mod writer;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use agent_core::text::truncate;
use agent_core::{Block, Compaction, Message, Role, Session, SessionSummary};
use anyhow::{Context, Result};
use serde_json::Value;

pub use writer::{inject, overwrite};

pub const ID: &str = "claude";

pub struct Paths {
    pub home: PathBuf,
    pub projects: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude")
            });
        Self {
            projects: home.join("projects"),
            home,
        }
    }
}

struct Parsed {
    title: String,
    cwd: String,
    created_ms: i64,
    updated_ms: i64,
    count: i64,
    messages: Vec<Message>,
    compaction: Option<Compaction>,
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(&paths.projects) else {
        return Ok(out);
    };
    for project in projects.flatten() {
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(parsed) = parse_file(&path, id, "") else {
                continue;
            };
            out.push(SessionSummary {
                agent: ID.into(),
                id: id.to_string(),
                title: parsed.title,
                cwd: parsed.cwd,
                created_ms: parsed.created_ms,
                updated_ms: parsed.updated_ms,
                message_count: parsed.count,
                archived: false,
                deleted: false,
                workspace_deleted: false,
            });
        }
    }
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let file = session_file(paths, id).with_context(|| format!("Claude Code 会话不存在：{id}"))?;
    fs::remove_file(&file).with_context(|| format!("remove {}", file.display()))?;
    Ok(())
}

pub fn active(paths: &Paths, id: &str) -> bool {
    session_file(paths, id).is_some()
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let file = session_file(paths, id).with_context(|| format!("Claude Code 会话不存在：{id}"))?;
    let parsed = parse_file(&file, id, "")?;
    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title: parsed.title,
        cwd: parsed.cwd,
        created_ms: parsed.created_ms,
        updated_ms: parsed.updated_ms,
        messages: parsed.messages,
        compaction: parsed.compaction,
    })
}

pub(crate) fn session_file(paths: &Paths, id: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(&paths.projects).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(format!("{id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn parse_file(path: &Path, id: &str, cwd_hint: &str) -> Result<Parsed> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let fallback_time = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut messages: Vec<Message> = Vec::new();
    let mut tool_sites: HashMap<String, (usize, usize)> = HashMap::new();
    let mut compaction: Option<Compaction> = None;
    let mut cwd = cwd_hint.to_string();
    let mut first_ms = i64::MAX;
    let mut last_ms = 0i64;
    let mut count = 0i64;
    let mut first_user_text = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let time_ms = v["timestamp"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        if time_ms > 0 {
            first_ms = first_ms.min(time_ms);
            last_ms = last_ms.max(time_ms);
        }
        if cwd.is_empty() {
            if let Some(value) = v["cwd"].as_str() {
                if !value.trim().is_empty() {
                    cwd = value.to_string();
                }
            }
        }
        let kind = v["type"].as_str().unwrap_or_default();
        // 旧版压缩行：摘要行出现在被总结消息之后，后续消息即尾部。
        if kind == "summary" {
            let summary = v["summary"].as_str().unwrap_or_default();
            if !summary.trim().is_empty() {
                compaction = Some(Compaction {
                    summary_text: summary.to_string(),
                    tail_start_index: messages.len(),
                    time_ms,
                });
            }
            continue;
        }
        if kind == "system" && v["subtype"].as_str() == Some("compact_boundary") {
            compaction = Some(Compaction {
                summary_text: compaction
                    .take()
                    .map(|c| c.summary_text)
                    .unwrap_or_default(),
                tail_start_index: messages.len(),
                time_ms,
            });
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if v["isSidechain"].as_bool().unwrap_or(false) || v["isMeta"].as_bool().unwrap_or(false) {
            continue;
        }
        if kind == "user" && v["isCompactSummary"].as_bool().unwrap_or(false) {
            let summary = content_text(&v["message"]["content"]);
            if !summary.trim().is_empty() {
                let tail = compaction
                    .as_ref()
                    .map(|c| c.tail_start_index)
                    .unwrap_or(messages.len());
                compaction = Some(Compaction {
                    summary_text: summary,
                    tail_start_index: tail,
                    time_ms,
                });
            }
            continue;
        }
        let role = match v["message"]["role"].as_str() {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };

        let mut blocks = Vec::new();
        if let Some(content) = v["message"]["content"].as_array() {
            for b in content {
                match b["type"].as_str().unwrap_or_default() {
                    "text" => {
                        let text = b["text"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() && !is_command_text(text) {
                            if role == Role::User && first_user_text.is_empty() {
                                first_user_text = text.to_string();
                            }
                            blocks.push(Block::Text(text.into()));
                        }
                    }
                    "thinking" => {
                        let text = b["thinking"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            blocks.push(Block::Thinking(text.into()));
                        }
                    }
                    "tool_use" => {
                        let id = b["id"].as_str().unwrap_or_default().to_string();
                        tool_sites.insert(id.clone(), (messages.len(), blocks.len()));
                        blocks.push(Block::ToolCall {
                            id,
                            name: b["name"].as_str().unwrap_or_default().into(),
                            input: b["input"].clone(),
                            output: None,
                            status: "completed".into(),
                        });
                    }
                    "tool_result" => {
                        let tid = b["tool_use_id"].as_str().unwrap_or_default();
                        if let Some(&(mi, bi)) = tool_sites.get(tid) {
                            if let Some(Block::ToolCall { output, status, .. }) =
                                messages.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                            {
                                *output = Some(content_text(&b["content"]));
                                *status = if b["is_error"].as_bool().unwrap_or(false) {
                                    "error".into()
                                } else {
                                    "completed".into()
                                };
                            }
                        }
                    }
                    "image" => blocks.push(Block::Image {
                        reference: image_ref(b),
                    }),
                    _ => {}
                }
            }
        } else if let Some(text) = v["message"]["content"].as_str() {
            if !text.trim().is_empty() && !is_command_text(text) {
                if role == Role::User && first_user_text.is_empty() {
                    first_user_text = text.to_string();
                }
                blocks.push(Block::Text(text.into()));
            }
        }

        if blocks.is_empty() {
            continue;
        }
        count += 1;
        messages.push(Message {
            role,
            time_ms,
            blocks,
        });
    }

    if first_ms == i64::MAX {
        first_ms = fallback_time;
    }
    if last_ms == 0 {
        last_ms = fallback_time;
    }
    let title = if first_user_text.trim().is_empty() {
        let short: String = id.chars().take(8).collect();
        format!("Claude Code 会话 {short}")
    } else {
        truncate(first_user_text.trim(), 60)
    };
    Ok(Parsed {
        title,
        cwd,
        created_ms: first_ms,
        updated_ms: last_ms,
        count,
        messages,
        compaction,
    })
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// 斜杠命令与本地命令输出是 CLI 自身的管道文本，不属于对话内容。
fn is_command_text(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
    ];
    let text = text.trim_start();
    PREFIXES.iter().any(|prefix| text.starts_with(prefix))
}

fn image_ref(b: &Value) -> String {
    let src = &b["source"];
    src["url"]
        .as_str()
        .or_else(|| src["file_path"].as_str())
        .map(String::from)
        .unwrap_or_else(|| "图片".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_sidechain_meta_and_reads_compaction() {
        let text = r#"
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"第一问"}]},"uuid":"u1","timestamp":"2026-01-01T00:00:00.000Z","cwd":"D:\\work"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"回答一"}]},"uuid":"a1","timestamp":"2026-01-01T00:00:01.000Z","cwd":"D:\\work"}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"},"uuid":"m1","timestamp":"2026-01-01T00:00:02.000Z"}
{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"},"uuid":"cmd1","timestamp":"2026-01-01T00:00:02.500Z"}
{"type":"user","isSidechain":true,"message":{"role":"user","content":"子代理"},"uuid":"s1","timestamp":"2026-01-01T00:00:03.000Z"}
{"type":"system","subtype":"compact_boundary","uuid":"c1","timestamp":"2026-01-01T00:00:04.000Z"}
{"type":"user","isCompactSummary":true,"message":{"role":"user","content":[{"type":"text","text":"摘要正文"}]},"uuid":"c2","timestamp":"2026-01-01T00:00:04.100Z"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"第二问"}]},"uuid":"u2","timestamp":"2026-01-01T00:00:05.000Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file":"a"}}]},"uuid":"a2","timestamp":"2026-01-01T00:00:06.000Z"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"文件内容"}]}]},"uuid":"r1","timestamp":"2026-01-01T00:00:07.000Z"}
"#;
        let parsed = parse_text(text);
        assert_eq!(parsed.messages.len(), 4);
        assert_eq!(parsed.title, "第一问");
        assert_eq!(parsed.count, 4);
        let compaction = parsed.compaction.expect("compaction");
        assert_eq!(compaction.summary_text, "摘要正文");
        assert_eq!(compaction.tail_start_index, 2);
        match &parsed.messages[3].blocks[0] {
            Block::ToolCall { output, .. } => assert_eq!(output.as_deref(), Some("文件内容")),
            _ => panic!("expected tool call"),
        }
    }

    fn parse_text(text: &str) -> Parsed {
        let dir = std::env::temp_dir().join(format!("asm-claude-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(&path, text).unwrap();
        let parsed = parse_file(&path, "s", "").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        parsed
    }
}
