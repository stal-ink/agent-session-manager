//! Trae CN 会话读取。
//!
//! 数据库由 Trae 的 AI Agent 服务写入，使用 SQLCipher 4 加密；
//! 本插件只读，解密后从 `history_v2` 事件流还原会话。

mod key;
mod sqlcipher;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use agent_core::adapter::AgentListing;
use agent_core::protocol::PluginWarning;
use agent_core::sqlite::open_ro;
use agent_core::{Block, Compaction, Message, Role, Session, SessionSummary};
use anyhow::{bail, Context, Result};
use serde_json::Value;

use sqlcipher::DecryptedDb;

/// 插件 id。
pub const ID: &str = "trae";

/// Trae 数据目录与数据库文件。
pub struct Paths {
    /// 产品数据根目录，例如 `%APPDATA%\Trae CN`。
    pub root: PathBuf,
    /// 加密数据库。
    pub db: PathBuf,
    /// WAL 文件。
    pub wal: PathBuf,
    /// SHM 文件。
    pub shm: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        if let Ok(root) = std::env::var("TRAE_DATA_DIR") {
            let root = PathBuf::from(root);
            if !root.as_os_str().is_empty() {
                return Self::from_root(root);
            }
        }
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let base = PathBuf::from(appdata);
        for name in ["Trae CN", "Trae"] {
            let root = base.join(name);
            if root.join("ModularData").is_dir() {
                return Self::from_root(root);
            }
        }
        Self::from_root(base.join("Trae CN"))
    }
}

impl Paths {
    /// 由产品数据根目录构造路径。
    pub fn from_root(root: PathBuf) -> Self {
        let dir = root.join("ModularData").join("ai-agent");
        Self {
            db: dir.join("database.db"),
            wal: dir.join("database.db-wal"),
            shm: dir.join("database.db-shm"),
            root,
        }
    }
}

/// 列出 Trae 会话摘要；单条会话无法解析时跳过并附带警告。
pub fn list_with_warnings(paths: &Paths) -> Result<AgentListing> {
    if !paths.db.is_file() {
        bail!("Trae 数据库不存在：{}", paths.db.display());
    }
    let (decrypted, mut warnings) = open_database(paths)?;
    let sessions = {
        let conn = open_ro(decrypted.path())?;
        let mut stmt = conn.prepare(
            "SELECT s.session_id, s.session_title, s.created_at, s.updated_at,
                    s.deleted_at, COALESCE(p.absolute_path, ''),
                    COALESCE(p.workspace_folders, '[]'), COALESCE(p.is_archived, 0),
                    p.deleted_at, (p.project_id IS NULL) AS project_missing, s.extra,
                    (SELECT COUNT(*) FROM history_v2 h
                      WHERE h.session_id = s.session_id AND h.deleted_at = 0
                        AND h.content_source IN ('user_input', 'llm_default')) AS msg_count
             FROM chat_session s
             LEFT JOIN project p ON p.project_id = s.project_id
             ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let title: String = row.get(1)?;
            let created: i64 = row.get(2)?;
            let updated: i64 = row.get(3)?;
            let session_deleted: i64 = row.get(4)?;
            let path: String = row.get(5)?;
            let folders: String = row.get(6)?;
            let project_archived: i64 = row.get(7)?;
            let project_deleted: Option<i64> = row.get(8)?;
            let project_missing: i64 = row.get(9)?;
            let extra: String = row.get(10)?;
            let count: i64 = row.get(11)?;
            Ok((
                id,
                title,
                created,
                updated,
                session_deleted,
                path,
                folders,
                project_archived,
                project_deleted,
                project_missing,
                extra,
                count,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                title,
                created,
                updated,
                session_deleted,
                path,
                folders,
                project_archived,
                project_deleted,
                project_missing,
                extra,
                count,
            ) = match row {
                Ok(row) => row,
                Err(error) => {
                    warnings.push(PluginWarning {
                        kind: "parse_skipped".into(),
                        message: format!("会话记录无法解析：{error}"),
                        session_id: None,
                    });
                    continue;
                }
            };
            let (deleted, archived, workspace_deleted) = status_flags(
                session_deleted != 0,
                extra_archived(&extra),
                project_archived != 0,
                project_deleted.is_some(),
                project_missing != 0,
            );
            out.push(SessionSummary {
                agent: ID.into(),
                title: if title.trim().is_empty() {
                    fallback_title(&id)
                } else {
                    title
                },
                cwd: pick_cwd(path, &folders),
                id,
                created_ms: created.saturating_mul(1000),
                updated_ms: updated.saturating_mul(1000),
                message_count: count,
                archived,
                deleted,
                workspace_deleted,
            });
        }
        out
    };
    Ok(AgentListing { sessions, warnings })
}

/// 列出 Trae 会话摘要。
pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    Ok(list_with_warnings(paths)?.sessions)
}

fn open_database(paths: &Paths) -> Result<(DecryptedDb, Vec<PluginWarning>)> {
    let mut file = File::open(&paths.db)
        .with_context(|| format!("打开 Trae 数据库失败：{}", paths.db.display()))?;
    let mut page1 = vec![0u8; 4096];
    file.read_exact(&mut page1)
        .with_context(|| format!("读取 Trae 数据库首页失败：{}", paths.db.display()))?;

    let data_dir = key::data_dir();
    let settings = key::Settings::load(&data_dir);
    let build = trae_build(&paths.root);
    let scan: Option<&dyn Fn(&[u8], std::time::Duration) -> Result<Option<[u8; 32]>>> =
        if settings.memory_scan {
            Some(&key::scan_processes)
        } else {
            None
        };
    let request = key::KeyRequest {
        db_path: &paths.db,
        data_dir: &data_dir,
        page1: &page1,
        build: &build,
        settings: &settings,
        builtin: key::BUILTIN_KEYS,
        scan,
    };
    let resolved = key::resolve(&request)?;
    let db =
        DecryptedDb::open(&paths.db, &paths.wal, &resolved.key).context("Trae 数据库解密失败")?;
    Ok((db, resolved.warnings))
}

fn trae_build(root: &std::path::Path) -> String {
    let path = root.join("User").join("globalStorage").join("storage.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return "unknown".into();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return "unknown".into();
    };
    value
        .get("iCubeLastVersion")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// 读取单个会话并转换为统一 IR。
pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    if !paths.db.is_file() {
        bail!("Trae 数据库不存在：{}", paths.db.display());
    }
    let (decrypted, _warnings) = open_database(paths)?;
    let conn = open_ro(decrypted.path())?;
    let (title, created, updated, path, folders) = conn
        .query_row(
            "SELECT s.session_title, s.created_at, s.updated_at,
                    COALESCE(p.absolute_path, ''), COALESCE(p.workspace_folders, '[]')
             FROM chat_session s
             LEFT JOIN project p ON p.project_id = s.project_id
             WHERE s.session_id = ?1 AND s.deleted_at = 0",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .with_context(|| format!("Trae 会话不存在：{id}"))?;

    let mut messages: Vec<Message> = Vec::new();
    let mut tool_sites: HashMap<String, (usize, usize)> = HashMap::new();
    let mut compaction: Option<Compaction> = None;
    let mut parse_failures = 0usize;
    let mut first_failure = String::new();

    let mut stmt = conn.prepare(
        "SELECT id, messages, created_at, summary, summarized_above
         FROM history_v2
         WHERE session_id = ?1 AND deleted_at = 0
         ORDER BY id",
    )?;
    let rows = stmt.query_map([id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (row_id, raw_json, created_at, summary, summarized_above) = row?;
        let time_ms = created_at.saturating_mul(1000);
        if let Some(summary) = summary {
            if summarized_above != 0 && !summary.trim().is_empty() {
                compaction = Some(Compaction {
                    summary_text: summary,
                    tail_start_index: messages.len(),
                    time_ms,
                });
            }
        }
        let value = match serde_json::from_str::<Value>(&raw_json) {
            Ok(value) => value,
            Err(error) => {
                parse_failures += 1;
                if first_failure.is_empty() {
                    first_failure = format!("history_v2 id={row_id} 消息 JSON 解析失败：{error}");
                }
                continue;
            }
        };
        let Some(raw_messages) = value.get("raw_messages").and_then(Value::as_array) else {
            continue;
        };
        for message in raw_messages {
            match message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "user" => {
                    let text = user_text(message);
                    if !text.trim().is_empty() {
                        messages.push(Message {
                            role: Role::User,
                            time_ms,
                            blocks: vec![Block::Text(text)],
                        });
                    }
                }
                "assistant" => {
                    let blocks = assistant_blocks(message, messages.len(), &mut tool_sites);
                    if !blocks.is_empty() {
                        messages.push(Message {
                            role: Role::Assistant,
                            time_ms,
                            blocks,
                        });
                    }
                }
                "tool" => {
                    let Some(call_id) = message.get("tool_call_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(&(message_index, block_index)) = tool_sites.get(call_id) else {
                        continue;
                    };
                    let Some(text) = message_text(message) else {
                        continue;
                    };
                    let (output, status) = tool_output_and_status(&text);
                    if let Some(Block::ToolCall {
                        output: slot,
                        status: slot_status,
                        ..
                    }) = messages
                        .get_mut(message_index)
                        .and_then(|message| message.blocks.get_mut(block_index))
                    {
                        *slot = Some(output);
                        *slot_status = status;
                    }
                }
                _ => {}
            }
        }
    }
    drop(stmt);
    drop(conn);

    if parse_failures > 0 {
        bail!("会话 {id} 有 {parse_failures} 条消息无法解析，首条原因：{first_failure}");
    }

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title: if title.trim().is_empty() {
            fallback_title(id)
        } else {
            title
        },
        cwd: pick_cwd(path, &folders),
        created_ms: created.saturating_mul(1000),
        updated_ms: updated.saturating_mul(1000),
        messages,
        compaction,
    })
}

/// 只读插件不支持删除。
pub fn delete(_paths: &Paths, id: &str) -> Result<()> {
    bail!("Trae 插件为只读，不支持删除会话：{id}")
}

/// 会话不会通过插件被视为活跃；活跃判断由界面自行处理。
pub fn active(_paths: &Paths, _id: &str) -> Result<bool> {
    Ok(false)
}

/// 需要备份的目录：整个 ai-agent 数据目录。
pub fn backup_paths(paths: &Paths) -> Result<Vec<PathBuf>> {
    Ok(vec![paths
        .db
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.root.clone())])
}

fn fallback_title(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("Trae 会话 {short}")
}

fn pick_cwd(path: String, folders: &str) -> String {
    if !path.trim().is_empty() {
        return path;
    }
    serde_json::from_str::<Vec<String>>(folders)
        .ok()
        .and_then(|items| items.into_iter().next())
        .unwrap_or_default()
}

/// 会话级归档：Trae 当前构建未开放，预留 `extra` 中的布尔字段。
fn extra_archived(extra: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(extra) else {
        return false;
    };
    ["archived", "is_archived", "session_archived"]
        .iter()
        .any(|key| value.get(*key).and_then(Value::as_bool).unwrap_or(false))
}

/// 映射宿主的四态：会话删除优先，其次工作区归档/删除，最后会话归档。
fn status_flags(
    session_deleted: bool,
    session_archived: bool,
    project_archived: bool,
    project_deleted: bool,
    project_missing: bool,
) -> (bool, bool, bool) {
    let workspace_deleted = project_archived || project_deleted || project_missing;
    (session_deleted, session_archived, workspace_deleted)
}

fn message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    let mut parts = Vec::new();
    if let Some(items) = content.as_array() {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
        }
    } else if let Some(text) = content.as_str() {
        if !text.trim().is_empty() {
            parts.push(text.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn user_text(message: &Value) -> String {
    let Some(text) = message_text(message) else {
        return String::new();
    };
    let mut inputs = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<user_input>") {
        let after = &rest[start + "<user_input>".len()..];
        let Some(end) = after.find("</user_input>") else {
            break;
        };
        let inner = after[..end].trim();
        if !inner.is_empty() {
            inputs.push(inner.to_string());
        }
        rest = &after[end + "</user_input>".len()..];
    }
    if !inputs.is_empty() {
        return inputs.join("\n");
    }
    let mut stripped = text;
    for tag in [
        "system-reminder",
        "rules",
        "environment",
        "mcp_file_system",
        "mcp_server",
        "always_applied_workspace_rules",
    ] {
        stripped = strip_tag_block(&stripped, tag);
    }
    stripped.trim().to_string()
}

fn strip_tag_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        match after.find(&close) {
            Some(end) => rest = &after[end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn assistant_blocks(
    message: &Value,
    current_index: usize,
    tool_sites: &mut HashMap<String, (usize, usize)>,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    if let Some(reasoning) = reasoning_text(message) {
        blocks.push(Block::Thinking(reasoning));
    }
    if let Some(text) = message_text(message) {
        blocks.push(Block::Text(text));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let Some(function) = call.get("function_call").or_else(|| call.get("function")) else {
                continue;
            };
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .unwrap_or_else(|| Value::String(String::new()));
            tool_sites.insert(id.clone(), (current_index, blocks.len()));
            blocks.push(Block::ToolCall {
                id,
                name,
                input,
                output: None,
                status: "completed".into(),
            });
        }
    }
    blocks
}

fn reasoning_text(message: &Value) -> Option<String> {
    let raw = message
        .get("extra_info")
        .and_then(|info| info.get("content"))
        .and_then(Value::as_str)?;
    let value: Value = serde_json::from_str(raw).ok()?;
    let text = value.get("reasoning_content").and_then(Value::as_str)?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn tool_output_and_status(text: &str) -> (String, String) {
    let status = if let Some(value) = between(text, "<toolcall_status>", "</toolcall_status>") {
        if value.trim().eq_ignore_ascii_case("done") {
            "completed"
        } else {
            "error"
        }
    } else if let Some(value) = between(text, "<shell_status>", "</shell_status>") {
        if value.trim().eq_ignore_ascii_case("completed") {
            "completed"
        } else {
            "error"
        }
    } else {
        "completed"
    };
    let body = between(text, "<toolcall_result>", "</toolcall_result>")
        .or_else(|| between(text, "<shell_output>", "</shell_output>"))
        .map(str::to_string)
        .unwrap_or_else(|| text.to_string());
    (body.trim().to_string(), status.to_string())
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> Value {
        serde_json::json!({
            "role": role,
            "content": [{"type": "text", "text": content}],
            "tool_calls": null,
            "tool_call_id": null,
            "extra_info": null,
        })
    }

    #[test]
    fn extracts_user_input_only() {
        let raw = message(
            "user",
            "<system-reminder>注入内容</system-reminder>\n<user_input>你好</user_input>",
        );
        assert_eq!(user_text(&raw), "你好");
    }

    #[test]
    fn strips_system_blocks_without_user_input() {
        let raw = message("user", "<system-reminder>环境</system-reminder>正文");
        assert_eq!(user_text(&raw), "正文");
    }

    #[test]
    fn pairs_tool_result_by_call_id() {
        let assistant = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": ""}],
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function_call": {"name": "Read", "arguments": "{\"file_path\":\"a\"}"}
            }],
            "extra_info": null,
        });
        let mut sites = HashMap::new();
        let blocks = assistant_blocks(&assistant, 0, &mut sites);
        assert!(matches!(blocks[0], Block::ToolCall { .. }));
        assert_eq!(sites.get("call_1"), Some(&(0usize, 0usize)));

        let tool = message(
            "tool",
            "<toolcall_status>done</toolcall_status>\n<toolcall_result>内容</toolcall_result>",
        );
        let tool = serde_json::json!({
            "role": "tool",
            "content": tool["content"].clone(),
            "tool_call_id": "call_1",
            "tool_calls": null,
            "extra_info": null,
        });
        let text = message_text(&tool).unwrap();
        assert_eq!(
            tool_output_and_status(&text),
            ("内容".to_string(), "completed".to_string())
        );
    }

    #[test]
    fn reads_shell_status_and_output() {
        let text = "\n<shell_command_id>x</shell_command_id>\n<shell_status>failed</shell_status>\n<shell_exit_code>1</shell_exit_code>\n<shell_output>错误</shell_output>";
        assert_eq!(
            tool_output_and_status(text),
            ("错误".to_string(), "error".to_string())
        );
    }

    #[test]
    fn maps_sessions_to_host_status_flags() {
        assert_eq!(
            status_flags(true, false, false, false, false),
            (true, false, false)
        );
        assert_eq!(
            status_flags(false, false, true, false, false),
            (false, false, true)
        );
        assert_eq!(
            status_flags(false, false, false, true, false),
            (false, false, true)
        );
        assert_eq!(
            status_flags(false, false, false, false, true),
            (false, false, true)
        );
        assert_eq!(
            status_flags(false, true, false, false, false),
            (false, true, false)
        );
    }

    #[test]
    fn reads_session_archived_from_extra() {
        assert!(extra_archived(r#"{"archived":true}"#));
        assert!(extra_archived(r#"{"is_archived":true}"#));
        assert!(!extra_archived(r#"{"archived":false}"#));
        assert!(!extra_archived("not json"));
    }
}
