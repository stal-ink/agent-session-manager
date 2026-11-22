mod writer;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use agent_core::ir::{Block, Compaction, Message, Role, Session, SessionSummary};
use agent_core::sqlite::{open_ro, open_rw};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

pub use writer::{inject, overwrite};

pub const ID: &str = "zcode";

pub struct Paths {
    pub db: PathBuf,
    pub tasks_db: PathBuf,
}

impl Paths {
    /// cli/exec 目录：db.sqlite 位于 cli/db/ 下，exec 与 db 同级（cli/ 之下）。
    fn exec_root(&self) -> Option<PathBuf> {
        self.db
            .parent()
            .and_then(Path::parent)
            .map(|cli| cli.join("exec"))
    }
}

impl Default for Paths {
    fn default() -> Self {
        let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
        Self {
            db: home.join(".zcode").join("cli").join("db").join("db.sqlite"),
            tasks_db: home.join(".zcode").join("v2").join("tasks-index.sqlite"),
        }
    }
}

struct TaskFlags {
    archived: bool,
    deleted: bool,
    project_id: Option<String>,
}

pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let tasks_db = open_ro(&paths.tasks_db)?;
    let mut task_stmt = tasks_db
        .prepare("SELECT task_id, archived, deleted, cli_deleted, project_id FROM tasks")?;
    let tasks: HashMap<String, TaskFlags> = task_stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TaskFlags {
                    archived: r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                    deleted: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0
                        || r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                    project_id: r.get(4)?,
                },
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut proj_stmt = tasks_db
        .prepare("SELECT project_id FROM projects WHERE deleted <> 0 OR deleted_at IS NOT NULL")?;
    let deleted_projects: HashSet<String> = proj_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;

    let db = open_ro(&paths.db)?;
    let mut stmt = db.prepare(
        "SELECT s.id, s.title, s.directory, s.time_created, s.time_updated,
                COALESCE(mc.cnt, 0)
         FROM session s
         LEFT JOIN (
             SELECT session_id, COUNT(*) AS cnt
             FROM message
             GROUP BY session_id
         ) mc ON mc.session_id = s.id
         WHERE s.id NOT LIKE 'sess_subagent%'
         ORDER BY s.time_updated DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: String = r.get(0)?;
        let flags = tasks.get(&id);
        Ok(SessionSummary {
            agent: ID.into(),
            archived: flags.map(|f| f.archived).unwrap_or(false),
            deleted: flags.map(|f| f.deleted).unwrap_or(false),
            workspace_deleted: flags
                .and_then(|f| f.project_id.as_deref())
                .is_some_and(|p| deleted_projects.contains(p)),
            id,
            title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            cwd: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            created_ms: r.get(3)?,
            updated_ms: r.get(4)?,
            message_count: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let now = agent_core::ir::now_ms();
    let tasks = open_rw(&paths.tasks_db)?;
    let ttx = tasks.unchecked_transaction()?;
    let count = ttx.execute(
        "UPDATE tasks SET deleted = 1, cli_deleted = 1 WHERE task_id = ?1",
        [id],
    )?;
    if count == 0 {
        anyhow::bail!("ZCode 任务不存在：{id}");
    }
    ttx.commit()?;

    let db = open_rw(&paths.db)?;
    let tx = db.unchecked_transaction()?;
    let count = tx.execute(
        "UPDATE session SET time_deleted = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    if count == 0 {
        anyhow::bail!("ZCode 会话不存在：{id}");
    }
    tx.commit()?;
    Ok(())
}

/// db.sqlite 中按会话清理的从表与匹配条件；不同版本可能缺表，删除前先查 sqlite_master。
/// `permission` 是按 `project_id` 存的项目级授权记录（真实 schema 无 session_id 列），
/// 不随会话销毁；`session_task_link` 以 parent/child 双列关联会话。
const SESSION_TABLES: &[(&str, &str)] = &[
    ("message", "session_id = ?1"),
    ("part", "session_id = ?1"),
    ("session_entry", "session_id = ?1"),
    ("todo", "session_id = ?1"),
    ("session_input", "session_id = ?1"),
    ("session_target", "session_id = ?1"),
    ("model_usage", "session_id = ?1"),
    ("turn_usage", "session_id = ?1"),
    ("tool_usage", "session_id = ?1"),
    (
        "session_task_link",
        "parent_session_id = ?1 OR child_session_id = ?1",
    ),
];

/// tasks-index.sqlite 中按 task_id 清理的索引表；同样容忍缺失。
const TASKS_TABLES: &[&str] = &["tasks", "task_group_admissions"];

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// 物理销毁：删除主库会话全部记录、任务索引行与 cli/exec 目录，不可恢复。
/// 与软删除的区别：行被真正删除，而不是写 deleted/cli_deleted/time_deleted。
pub fn destroy(paths: &Paths, id: &str) -> Result<()> {
    let db = open_rw(&paths.db)?;
    let tx = db.unchecked_transaction()?;
    for (table, condition) in SESSION_TABLES {
        if table_exists(&tx, table)? {
            tx.execute(&format!("DELETE FROM {table} WHERE {condition}"), [id])?;
        }
    }
    // 会话行不存在则整体回滚并报错，与软删除的缺失检查一致。
    let deleted = tx.execute("DELETE FROM session WHERE id = ?1", [id])?;
    if deleted == 0 {
        anyhow::bail!("ZCode 会话不存在：{id}");
    }
    tx.commit()?;

    // 任务索引：库或表缺失都容忍，索引本来就是辅助数据。
    if paths.tasks_db.exists() {
        let tasks = open_rw(&paths.tasks_db)?;
        let ttx = tasks.unchecked_transaction()?;
        for table in TASKS_TABLES {
            if table_exists(&ttx, table)? {
                ttx.execute(&format!("DELETE FROM {table} WHERE task_id = ?1"), [id])?;
            }
        }
        ttx.commit()?;
    }

    // cli/exec/<id>/：目录不存在则容忍。
    if let Some(dir) = paths.exec_root().map(|root| root.join(id)) {
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        }
    }
    Ok(())
}

pub fn active(paths: &Paths, id: &str) -> bool {
    let Ok(tasks_db) = open_ro(&paths.tasks_db) else {
        return false;
    };
    let row = tasks_db.query_row(
        "SELECT COALESCE(t.archived, 0), COALESCE(t.deleted, 0), COALESCE(t.cli_deleted, 0),
                COALESCE((SELECT deleted FROM projects p WHERE p.project_id = t.project_id), 0),
                (SELECT deleted_at FROM projects p WHERE p.project_id = t.project_id)
         FROM tasks t WHERE t.task_id = ?1",
        [id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        },
    );
    let Ok((archived, deleted, cli_deleted, project_deleted, project_deleted_at)) = row else {
        return false;
    };
    if archived != 0 || deleted != 0 || cli_deleted != 0 {
        return false;
    }
    if project_deleted != 0 || project_deleted_at.is_some() {
        return false;
    }
    let Ok(db) = open_ro(&paths.db) else {
        return false;
    };
    db.query_row("SELECT 1 FROM session WHERE id = ?1", [id], |_| Ok(()))
        .is_ok()
}

pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let db = open_ro(&paths.db)?;
    let (title, cwd, created_ms, updated_ms) = db.query_row(
        "SELECT title, directory, time_created, time_updated FROM session WHERE id = ?1",
        [id],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )?;

    let mut msg_stmt =
        db.prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY sequence")?;
    // 一次取齐会话内全部 part，按消息归组；避免每条消息一次查询的 N+1。
    let mut part_stmt = db.prepare(
        "SELECT p.message_id, p.data
         FROM part p JOIN message m ON m.id = p.message_id
         WHERE m.session_id = ?1
         ORDER BY m.sequence, p.sequence",
    )?;
    let mut parts_by_message: HashMap<String, Vec<Value>> = HashMap::new();
    for row in part_stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (mid, data) = row?;
        let part: Value = serde_json::from_str(&data).context("parse part data")?;
        parts_by_message.entry(mid).or_default().push(part);
    }

    let msg_rows = msg_stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;

    // 最后一次压缩的摘要与模型窗口起点；摘要消息本身不进入 messages。
    let mut summary_id = String::new();
    let mut summary_text = String::new();
    let mut summary_time = 0i64;
    let mut tail_start_index: Option<usize> = None;
    {
        let mut stmt = db.prepare(
            "SELECT p.data FROM part p JOIN message m ON p.message_id = m.id
             WHERE m.session_id = ?1 AND json_extract(p.data, '$.type') = 'compaction'
             ORDER BY m.sequence, p.sequence",
        )?;
        for data in stmt.query_map([id], |r| r.get::<_, String>(0))? {
            let data = data?;
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                let summary = v["summaryMessageId"].as_str().unwrap_or_default();
                // 同一轮压缩可能出现空壳 part，跳过没有摘要的条目。
                if !summary.is_empty() {
                    summary_id = summary.to_string();
                }
            }
        }
    }

    let mut messages = Vec::new();
    for row in msg_rows {
        let (mid, data) = row?;
        let data: Value = serde_json::from_str(&data).context("parse message data")?;
        if !summary_id.is_empty() && mid.as_str() == summary_id {
            let summary = &data["summary"];
            summary_text = summary["body"]
                .as_str()
                .or_else(|| summary["title"].as_str())
                .unwrap_or_default()
                .to_string();
            summary_time = data["time"]["created"].as_i64().unwrap_or(0);
            tail_start_index = Some(messages.len());
            continue;
        }
        let role = match data["role"].as_str() {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };
        let time_ms = data["time"]["created"].as_i64().unwrap_or(0);

        let mut blocks = Vec::new();
        for p in parts_by_message.remove(&mid).unwrap_or_default() {
            match p["type"].as_str().unwrap_or_default() {
                "text" => blocks.push(Block::Text(p["text"].as_str().unwrap_or_default().into())),
                "reasoning" => blocks.push(Block::Thinking(
                    p["text"].as_str().unwrap_or_default().into(),
                )),
                "tool" => blocks.push(Block::ToolCall {
                    id: p["callID"].as_str().unwrap_or_default().into(),
                    name: p["tool"].as_str().unwrap_or_default().into(),
                    input: p["state"]["input"].clone(),
                    output: Some(match &p["state"]["output"] {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    }),
                    status: p["state"]["status"].as_str().unwrap_or("completed").into(),
                }),
                "file" => {
                    let mime = p["mime"].as_str().unwrap_or_default();
                    if mime.starts_with("image/") {
                        blocks.push(Block::Image {
                            reference: p["url"].as_str().unwrap_or_default().into(),
                        });
                    } else {
                        blocks.push(Block::File {
                            path: p["source"]["path"]
                                .as_str()
                                .or_else(|| p["filename"].as_str())
                                .unwrap_or_default()
                                .into(),
                        });
                    }
                }
                _ => {}
            }
        }
        if blocks.is_empty() {
            continue;
        }
        messages.push(Message {
            role,
            time_ms,
            blocks,
        });
    }

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title,
        cwd,
        created_ms,
        updated_ms,
        compaction: if summary_text.is_empty() {
            None
        } else {
            Some(Compaction {
                summary_text,
                tail_start_index: tail_start_index.unwrap_or(messages.len()),
                time_ms: summary_time,
            })
        },
        messages,
    })
}
