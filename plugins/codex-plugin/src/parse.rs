//! Codex 只读解析层：state 数据库索引 + rollout JSONL 正文。
//!
//! 读取以 `response_item` 为准（`event_msg` 是同一内容的双写，只在标题
//! 兜底时使用），developer 注入整行丢弃，用户文本中的环境/规则注入按
//! 固定前缀剔除。数据库列名全部动态读取，未来版本增删列不影响读取。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use agent_core::ir::{Block, Compaction, Message, Role, Session, SessionSummary};

pub const ID: &str = "codex";

const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
const TITLE_MAX: usize = 60;

pub struct Paths {
    pub home: PathBuf,
    pub sessions: PathBuf,
    pub archived: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = std::env::var("CODEX_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".codex")
            });
        Self {
            sessions: home.join("sessions"),
            archived: home.join("archived_sessions"),
            home,
        }
    }
}

/// 选 `state_<数字>.sqlite` 中版本号最大的一份。
pub fn state_db(paths: &Paths) -> Option<PathBuf> {
    let entries = fs::read_dir(&paths.home).ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(digits) = name
            .strip_prefix("state_")
            .and_then(|rest| rest.strip_suffix(".sqlite"))
        else {
            continue;
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(number) = digits.parse::<u64>() else {
            continue;
        };
        let replace = match &best {
            Some((current, _)) => number > *current,
            None => true,
        };
        if replace {
            best = Some((number, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// 列出会话：优先 state 数据库，缺失/打开失败/无 threads 表时回退扫描 rollout。
pub fn list(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let mut out = match list_db(paths) {
        Ok(rows) => rows,
        Err(_) => fallback_list(paths),
    };
    out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(out)
}

/// 加载单个会话：正文来自 rollout，标题优先 state 数据库的 name/title。
pub fn load(paths: &Paths, id: &str) -> Result<Session> {
    let file = find_rollout(paths, id).with_context(|| format!("Codex 会话不存在：{id}"))?;
    let db = db_thread(paths, id);
    let parsed = parse_rollout(&file)?;
    let mtime = file_mtime(&file).unwrap_or(0);

    let db_created = db.as_ref().map(|row| row.created_ms).unwrap_or(0);
    let db_updated = db.as_ref().map(|row| row.updated_ms).unwrap_or(0);
    let db_cwd = db
        .as_ref()
        .map(|row| normalize_cwd(&row.cwd))
        .unwrap_or_default();
    let db_title = db
        .as_ref()
        .and_then(|row| row.name_or_title())
        .map(|title| agent_core::text::truncate(title.trim(), TITLE_MAX))
        .filter(|title| !title.is_empty());

    let created_ms = if parsed.created_ms > 0 {
        parsed.created_ms
    } else if db_created > 0 {
        db_created
    } else {
        mtime
    };
    let updated_ms = if parsed.updated_ms > 0 {
        parsed.updated_ms
    } else if db_updated > 0 {
        db_updated
    } else {
        mtime
    };
    let title_id = if parsed.id.is_empty() {
        id
    } else {
        parsed.id.as_str()
    };
    let title = db_title.unwrap_or_else(|| parsed.title(title_id));
    let cwd = if parsed.cwd.is_empty() {
        db_cwd
    } else {
        parsed.cwd
    };

    Ok(Session {
        agent: ID.into(),
        id: id.to_string(),
        title,
        cwd,
        created_ms,
        updated_ms,
        messages: parsed.messages,
        compaction: parsed.compaction,
    })
}

/// 按 id 定位 rollout：先查 state 数据库的 rollout_path，再递归扫描文件名。
pub fn find_rollout(paths: &Paths, id: &str) -> Option<PathBuf> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    if let Some(path) = db_rollout_path(paths, id) {
        if is_zst(&path) {
            eprintln!("codex: 跳过压缩 rollout：{}", path.display());
        } else if path.is_file() {
            return Some(path);
        }
    }
    let mut found = None;
    scan_for_rollout(&paths.sessions, id, &mut found);
    if found.is_none() {
        scan_for_rollout(&paths.archived, id, &mut found);
    }
    found
}

/// 文件存在即活跃；数据库有行但 rollout 缺失时为 false。
pub fn active(paths: &Paths, id: &str) -> bool {
    find_rollout(paths, id).is_some()
}

/// 早期版本由 app-server 默认 `paginated` 写出的线程在客户端里看不到注入历史；
/// 这类线程由本工具创建，再次转接时强制重建为 legacy。
pub fn needs_rewrite(paths: &Paths, id: &str) -> bool {
    let Some(db) = state_db(paths) else {
        return false;
    };
    let Ok(conn) = open_ro_db(&db) else {
        return false;
    };
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT originator, history_mode FROM threads WHERE id = ?1",
            [id],
            |row| Ok((row.get(0).ok(), row.get(1).ok())),
        )
        .ok();
    matches!(
        row,
        Some((Some(originator), Some(mode)))
            if originator == "agent-session-manager" && mode == "paginated"
    )
}

/// 该线程是否由本工具通过 app-server 创建。
pub fn is_ours(paths: &Paths, id: &str) -> bool {
    let Some(db) = state_db(paths) else {
        return false;
    };
    let Ok(conn) = open_ro_db(&db) else {
        return false;
    };
    conn.query_row(
        "SELECT originator FROM threads WHERE id = ?1",
        [id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .is_some_and(|originator| originator == "agent-session-manager")
}

/// 打开会话前把本工具早期写出的 paginated 线程切成 legacy，让客户端直接读 rollout。
/// 只改 originator 匹配的行；数据库忙或没有该列时返回 false，不阻断打开。
pub fn ensure_legacy(paths: &Paths, id: &str) -> Result<bool> {
    let Some(db) = state_db(paths) else {
        return Ok(false);
    };
    let conn = agent_core::sqlite::open_rw(&db)?;
    let changed = conn.execute(
        "UPDATE threads SET history_mode = 'legacy' \
         WHERE id = ?1 AND originator = 'agent-session-manager' AND history_mode = 'paginated'",
        [id],
    )?;
    Ok(changed > 0)
}

/// 备份清单只包含存在的路径。
pub fn backup_paths(paths: &Paths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [&paths.sessions, &paths.archived] {
        if dir.exists() {
            out.push(dir.clone());
        }
    }
    if let Some(db) = state_db(paths) {
        if db.exists() {
            out.push(db);
        }
    }
    let index = paths.home.join("session_index.jsonl");
    if index.exists() {
        out.push(index);
    }
    let mut histories = Vec::new();
    if let Ok(entries) = fs::read_dir(&paths.home) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with("thread_history_") && name.ends_with(".sqlite") {
                let path = entry.path();
                if path.is_file() {
                    histories.push(path);
                }
            }
        }
    }
    histories.sort();
    out.extend(histories);
    let legacy = paths.home.join("sqlite").join("codex.db");
    if legacy.exists() {
        out.push(legacy);
    }
    out
}

// ---------------------------------------------------------------------------
// state 数据库
// ---------------------------------------------------------------------------

struct DbThread {
    id: String,
    rollout: String,
    source: String,
    parent: String,
    cwd: String,
    name: String,
    title: String,
    first_user: String,
    preview: String,
    created_ms: i64,
    updated_ms: i64,
    archived: bool,
}

impl DbThread {
    fn listing_title(&self) -> Option<String> {
        first_nonempty(&[
            self.name.as_str(),
            self.title.as_str(),
            self.first_user.as_str(),
            self.preview.as_str(),
        ])
    }

    fn name_or_title(&self) -> Option<String> {
        first_nonempty(&[self.name.as_str(), self.title.as_str()])
    }
}

enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Cell {
    fn text(&self) -> String {
        match self {
            Cell::Text(value) => value.clone(),
            Cell::Int(value) => value.to_string(),
            Cell::Real(value) => value.to_string(),
            Cell::Null => String::new(),
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Cell::Int(value) => Some(*value),
            Cell::Real(value) => Some(*value as i64),
            Cell::Text(value) => value
                .trim()
                .parse::<i64>()
                .ok()
                .or_else(|| value.trim().parse::<f64>().ok().map(|number| number as i64)),
            Cell::Null => None,
        }
    }
}

fn cell(row: &rusqlite::Row<'_>, columns: &HashMap<String, usize>, name: &str) -> Cell {
    let Some(&index) = columns.get(name) else {
        return Cell::Null;
    };
    match row.get_ref(index) {
        Ok(ValueRef::Integer(value)) => Cell::Int(value),
        Ok(ValueRef::Real(value)) => Cell::Real(value),
        Ok(ValueRef::Text(value)) => Cell::Text(String::from_utf8_lossy(value).into_owned()),
        Ok(ValueRef::Blob(_)) | Ok(ValueRef::Null) | Err(_) => Cell::Null,
    }
}

fn open_ro_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

fn read_threads(conn: &Connection, id_filter: Option<&str>) -> Result<Vec<DbThread>> {
    let sql = if id_filter.is_some() {
        "SELECT * FROM threads WHERE id = ?1"
    } else {
        "SELECT * FROM threads"
    };
    let mut stmt = conn.prepare(sql)?;
    let columns: HashMap<String, usize> = stmt
        .column_names()
        .enumerate()
        .map(|(index, name)| (name.to_ascii_lowercase(), index))
        .collect();
    let mut rows = match id_filter {
        Some(id) => stmt.query([id])?,
        None => stmt.query([])?,
    };
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let get = |name: &str| cell(row, &columns, name);
        out.push(DbThread {
            id: get("id").text().trim().to_string(),
            rollout: get("rollout_path").text(),
            source: get("source").text(),
            parent: get("attached_thread_parent_id").text(),
            cwd: get("cwd").text(),
            name: get("name").text(),
            title: get("title").text(),
            first_user: get("first_user_message").text(),
            preview: get("preview").text(),
            created_ms: ms_or_seconds(get("created_at_ms"), get("created_at")),
            updated_ms: ms_or_seconds(get("updated_at_ms"), get("updated_at")),
            archived: get("archived")
                .int()
                .map(|value| value != 0)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

fn ms_or_seconds(ms: Cell, seconds: Cell) -> i64 {
    if let Some(value) = ms.int() {
        if value > 0 {
            return value;
        }
    }
    if let Some(value) = seconds.int() {
        if value > 0 {
            return value.saturating_mul(1000);
        }
    }
    0
}

fn db_thread(paths: &Paths, id: &str) -> Option<DbThread> {
    let db_path = state_db(paths)?;
    let conn = open_ro_db(&db_path).ok()?;
    let rows = read_threads(&conn, Some(id)).ok()?;
    rows.into_iter().next()
}

fn db_rollout_path(paths: &Paths, id: &str) -> Option<PathBuf> {
    let db_path = state_db(paths)?;
    let conn = open_ro_db(&db_path).ok()?;
    let raw: String = conn
        .query_row(
            "SELECT rollout_path FROM threads WHERE id = ?1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .ok()?;
    let clean = strip_verbatim(raw.trim());
    if clean.is_empty() {
        None
    } else {
        Some(PathBuf::from(clean))
    }
}

fn is_subagent(thread: &DbThread) -> bool {
    thread
        .source
        .trim()
        .to_ascii_lowercase()
        .starts_with("subagent")
        || !thread.parent.trim().is_empty()
}

fn list_db(paths: &Paths) -> Result<Vec<SessionSummary>> {
    let db_path = state_db(paths).context("Codex state 数据库不存在")?;
    let conn = open_ro_db(&db_path)?;
    let threads = read_threads(&conn, None)?;
    let mut out = Vec::with_capacity(threads.len());
    for thread in threads {
        if thread.id.is_empty() || is_subagent(&thread) {
            continue;
        }
        let rollout = resolve_rollout(paths, &thread);
        let mtime = rollout.as_deref().and_then(file_mtime).unwrap_or(0);
        let created_ms = if thread.created_ms > 0 {
            thread.created_ms
        } else {
            mtime
        };
        let updated_ms = if thread.updated_ms > 0 {
            thread.updated_ms
        } else {
            mtime
        };
        let message_count = rollout.as_deref().map(count_messages).unwrap_or(0);
        let title = thread
            .listing_title()
            .map(|value| agent_core::text::truncate(&value, TITLE_MAX))
            .unwrap_or_else(|| default_title(&thread.id));
        out.push(SessionSummary {
            agent: ID.into(),
            id: thread.id,
            title,
            cwd: normalize_cwd(&thread.cwd),
            created_ms,
            updated_ms,
            message_count,
            archived: thread.archived,
            deleted: false,
            workspace_deleted: false,
        });
    }
    Ok(out)
}

fn resolve_rollout(paths: &Paths, thread: &DbThread) -> Option<PathBuf> {
    let clean = strip_verbatim(thread.rollout.trim());
    if !clean.is_empty() {
        let path = PathBuf::from(clean);
        if !is_zst(&path) && path.is_file() {
            return Some(path);
        }
    }
    find_rollout(paths, &thread.id)
}

// ---------------------------------------------------------------------------
// 回退扫描（state 数据库不可用时）
// ---------------------------------------------------------------------------

fn fallback_list(paths: &Paths) -> Vec<SessionSummary> {
    let mut files = Vec::new();
    collect_jsonl(&paths.sessions, false, &mut files);
    collect_jsonl(&paths.archived, true, &mut files);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (path, archived) in files {
        let Some(summary) = scan_summary(&path, archived) else {
            continue;
        };
        if seen.insert(summary.id.clone()) {
            out.push(summary);
        }
    }
    out
}

fn collect_jsonl(dir: &Path, archived: bool, out: &mut Vec<(PathBuf, bool)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_jsonl(&path, archived, out);
        } else if kind.is_file() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.ends_with(".jsonl") {
                out.push((path, archived));
            } else if name.ends_with(".jsonl.zst") {
                eprintln!("codex: 跳过压缩 rollout：{}", path.display());
            }
        }
    }
}

fn scan_summary(path: &Path, archived: bool) -> Option<SessionSummary> {
    let events = agent_core::jsonl::read_events(path).ok()?;
    let mtime = file_mtime(path).unwrap_or(0);
    let mut meta: Option<&Value> = None;
    let mut title_hint = String::new();
    let mut message_count = 0i64;
    for value in &events {
        let kind = value["type"].as_str().unwrap_or_default();
        if kind == "session_meta" && meta.is_none() {
            meta = Some(value);
            continue;
        }
        if kind != "response_item" {
            continue;
        }
        let payload = &value["payload"];
        if payload["type"].as_str() != Some("message") {
            continue;
        }
        let role = payload["role"].as_str().unwrap_or_default();
        if role == "user" || role == "assistant" {
            message_count += 1;
        }
        if role == "user" && title_hint.is_empty() {
            if let Some(content) = payload["content"].as_array() {
                for item in content {
                    if !matches!(
                        item["type"].as_str(),
                        Some("input_text") | Some("output_text")
                    ) {
                        continue;
                    }
                    let text = item["text"].as_str().unwrap_or_default();
                    if !text.trim().is_empty() && !is_injection(text) {
                        title_hint = text.trim().to_string();
                        break;
                    }
                }
            }
        }
    }

    let meta = meta?;
    let payload = &meta["payload"];
    let id = payload["id"]
        .as_str()
        .or_else(|| payload["session_id"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() {
        return None;
    }
    let source = payload["source"].as_str().unwrap_or_default();
    if source.to_ascii_lowercase().starts_with("subagent") {
        return None;
    }
    let created = envelope_ms(meta);
    let created_ms = if created > 0 { created } else { mtime };
    let updated_ms = if mtime > 0 { mtime } else { created_ms };
    let title = if title_hint.is_empty() {
        default_title(&id)
    } else {
        agent_core::text::truncate(&title_hint, TITLE_MAX)
    };
    Some(SessionSummary {
        agent: ID.into(),
        id,
        title,
        cwd: normalize_cwd(payload["cwd"].as_str().unwrap_or_default()),
        created_ms,
        updated_ms,
        message_count,
        archived,
        deleted: false,
        workspace_deleted: false,
    })
}

fn scan_for_rollout(dir: &Path, id: &str, found: &mut Option<PathBuf>) {
    if found.is_some() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if found.is_some() {
            return;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            scan_for_rollout(&path, id, found);
        } else if kind.is_file() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.contains(id) {
                continue;
            }
            if name.ends_with(".jsonl.zst") {
                eprintln!("codex: 跳过压缩 rollout：{}", path.display());
            } else if name.ends_with(".jsonl") {
                *found = Some(path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// rollout 正文
// ---------------------------------------------------------------------------

struct Parsed {
    id: String,
    cwd: String,
    created_ms: i64,
    updated_ms: i64,
    messages: Vec<Message>,
    compaction: Option<Compaction>,
    title_hint: String,
}

impl Parsed {
    fn title(&self, id: &str) -> String {
        let hint = self.title_hint.trim();
        if hint.is_empty() {
            default_title(id)
        } else {
            agent_core::text::truncate(hint, TITLE_MAX)
        }
    }
}

fn parse_rollout(path: &Path) -> Result<Parsed> {
    let events = agent_core::jsonl::read_events(path)?;
    let fallback_time = file_mtime(path).unwrap_or(0);
    let mut meta_seen = false;
    let mut id = String::new();
    let mut cwd = String::new();
    let mut first_ms = i64::MAX;
    let mut last_ms = 0i64;
    let mut messages: Vec<Message> = Vec::new();
    let mut tool_sites: HashMap<String, (usize, usize)> = HashMap::new();
    let mut compaction: Option<Compaction> = None;
    let mut title_hint = String::new();

    for value in events {
        let time_ms = envelope_ms(&value);
        if time_ms > 0 {
            first_ms = first_ms.min(time_ms);
            last_ms = last_ms.max(time_ms);
        }
        let kind = value["type"].as_str().unwrap_or_default();
        match kind {
            "session_meta" => {
                if meta_seen {
                    continue;
                }
                meta_seen = true;
                let payload = &value["payload"];
                id = payload["id"]
                    .as_str()
                    .or_else(|| payload["session_id"].as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                cwd = normalize_cwd(payload["cwd"].as_str().unwrap_or_default());
            }
            "response_item" => {
                let payload = &value["payload"];
                match payload["type"].as_str().unwrap_or_default() {
                    "message" => {
                        let role = match payload["role"].as_str() {
                            Some("user") => Role::User,
                            Some("assistant") => Role::Assistant,
                            _ => continue,
                        };
                        let mut blocks = Vec::new();
                        if let Some(content) = payload["content"].as_array() {
                            for item in content {
                                match item["type"].as_str().unwrap_or_default() {
                                    "input_text" | "output_text" => {
                                        let text = item["text"].as_str().unwrap_or_default();
                                        if text.trim().is_empty() {
                                            continue;
                                        }
                                        if role == Role::User && is_injection(text) {
                                            continue;
                                        }
                                        if role == Role::User && title_hint.is_empty() {
                                            title_hint = text.trim().to_string();
                                        }
                                        blocks.push(Block::Text(text.to_string()));
                                    }
                                    "input_image" => blocks.push(Block::Image {
                                        reference: image_reference(item),
                                    }),
                                    _ => {}
                                }
                            }
                        }
                        push_message(role, time_ms, fallback_time, blocks, &mut messages);
                    }
                    "reasoning" => {
                        let mut text = String::new();
                        if let Some(summary) = payload["summary"].as_array() {
                            for item in summary {
                                let part = item["summary_text"]
                                    .as_str()
                                    .or_else(|| item["text"].as_str())
                                    .unwrap_or_default();
                                agent_core::text::push_text(&mut text, part);
                            }
                        }
                        if let Some(content) = payload["content"].as_array() {
                            for item in content {
                                agent_core::text::push_text(
                                    &mut text,
                                    item["text"].as_str().unwrap_or_default(),
                                );
                            }
                        }
                        if !text.trim().is_empty() {
                            push_message(
                                Role::Assistant,
                                time_ms,
                                fallback_time,
                                vec![Block::Thinking(text)],
                                &mut messages,
                            );
                        }
                    }
                    "function_call" | "custom_tool_call" | "tool_search_call" => {
                        let tool_kind = payload["type"].as_str().unwrap_or_default();
                        let call_id = payload["call_id"]
                            .as_str()
                            .or_else(|| payload["id"].as_str())
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        let name =
                            payload["name"]
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|| {
                                    if tool_kind == "tool_search_call" {
                                        "tool_search".to_string()
                                    } else {
                                        String::new()
                                    }
                                });
                        // custom_tool_call 的 input 原样保留；其余把 JSON 字符串解析成值。
                        let input = if tool_kind == "custom_tool_call" {
                            payload.get("input").cloned().unwrap_or(Value::Null)
                        } else if let Some(arguments) = payload.get("arguments") {
                            json_or_string(arguments)
                        } else if let Some(input) = payload.get("input") {
                            json_or_string(input)
                        } else {
                            Value::Null
                        };
                        let message_index = messages.len();
                        let block_index = 0;
                        let blocks = vec![Block::ToolCall {
                            id: call_id.clone(),
                            name,
                            input,
                            output: None,
                            status: "completed".into(),
                        }];
                        push_message(
                            Role::Assistant,
                            time_ms,
                            fallback_time,
                            blocks,
                            &mut messages,
                        );
                        if !call_id.is_empty() {
                            tool_sites.insert(call_id, (message_index, block_index));
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
                        let call_id = payload["call_id"].as_str().unwrap_or_default();
                        if let Some(&(message_index, block_index)) = tool_sites.get(call_id) {
                            if let Some(output) = tool_output_text(payload) {
                                if let Some(Block::ToolCall {
                                    output: slot,
                                    status,
                                    ..
                                }) = messages
                                    .get_mut(message_index)
                                    .and_then(|message| message.blocks.get_mut(block_index))
                                {
                                    *slot = Some(output.clone());
                                    *status = if has_nonzero_exit(&output) {
                                        "error".into()
                                    } else {
                                        "completed".into()
                                    };
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "compacted" => {
                let summary = value_text(&value["payload"]["message"]);
                if !summary.trim().is_empty() {
                    compaction = Some(Compaction {
                        summary_text: summary,
                        tail_start_index: messages.len(),
                        time_ms: if time_ms > 0 { time_ms } else { fallback_time },
                    });
                }
            }
            "event_msg" => {
                if title_hint.is_empty()
                    && value["payload"]["type"].as_str() == Some("user_message")
                {
                    let text = value["payload"]["message"]
                        .as_str()
                        .or_else(|| value["payload"]["text"].as_str())
                        .unwrap_or_default()
                        .trim();
                    if !text.is_empty() && !is_injection(text) {
                        title_hint = text.to_string();
                    }
                }
            }
            _ => {}
        }
    }

    if first_ms == i64::MAX {
        first_ms = 0;
    }
    Ok(Parsed {
        id,
        cwd,
        created_ms: first_ms,
        updated_ms: last_ms,
        messages,
        compaction,
        title_hint,
    })
}

fn push_message(
    role: Role,
    time_ms: i64,
    fallback: i64,
    blocks: Vec<Block>,
    messages: &mut Vec<Message>,
) {
    if blocks.is_empty() {
        return;
    }
    messages.push(Message {
        role,
        time_ms: if time_ms > 0 { time_ms } else { fallback },
        blocks,
    });
}

fn envelope_ms(value: &Value) -> i64 {
    value["timestamp"]
        .as_str()
        .map(agent_core::time::iso_to_ms)
        .unwrap_or(0)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(value_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(content) = map.get("content") {
                let text = value_text(content);
                if !text.trim().is_empty() {
                    return text;
                }
            }
            if let Some(text) = map.get("text") {
                let text = value_text(text);
                if !text.trim().is_empty() {
                    return text;
                }
            }
            value.to_string()
        }
        other => other.to_string(),
    }
}

fn tool_output_text(payload: &Value) -> Option<String> {
    if let Some(output) = payload.get("output") {
        return match output {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            other => Some(value_text(other)),
        };
    }
    payload.get("tools").map(value_text)
}

fn json_or_string(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        other => other.clone(),
    }
}

fn image_reference(item: &Value) -> String {
    if let Some(url) = item["image_url"].as_str() {
        if !url.trim().is_empty() {
            return url.to_string();
        }
    }
    for key in ["localImage", "local_image"] {
        if let Some(path) = item[key]["path"].as_str() {
            if !path.trim().is_empty() {
                return path.to_string();
            }
        }
    }
    "[图片]".to_string()
}

/// 非零退出码只作为工具失败的尽力判断，不改变 Codex 的 completed 语义。
fn has_nonzero_exit(text: &str) -> bool {
    for line in text.lines() {
        let line = line.trim();
        for prefix in ["Process exited with code ", "Exit code: "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                if rest
                    .trim()
                    .parse::<i32>()
                    .map(|code| code != 0)
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn is_injection(text: &str) -> bool {
    let text = text.trim_start();
    [
        "<environment_context>",
        "<turn_aborted>",
        "# AGENTS.md instructions for",
        "# Files mentioned by the user",
    ]
    .iter()
    .any(|prefix| text.starts_with(*prefix))
}

fn first_nonempty(values: &[&str]) -> Option<String> {
    for value in values {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn default_title(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("Codex 会话 {short}")
}

fn strip_verbatim(path: &str) -> String {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return format!(r"\\{unc}");
        }
        return rest.to_string();
    }
    if let Some(rest) = path.strip_prefix("//?/") {
        if let Some(unc) = rest.strip_prefix("UNC/") {
            return format!("//{unc}");
        }
        return rest.to_string();
    }
    path.to_string()
}

fn normalize_cwd(raw: &str) -> String {
    let cleaned = strip_verbatim(raw);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        String::new()
    } else {
        agent_core::path::normalize(cleaned)
    }
}

fn is_zst(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("zst"))
        .unwrap_or(false)
}

fn file_mtime(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
}

/// 只统计 response_item.message 中 user/assistant 的条数，避免与 event_msg 重复。
fn count_messages(path: &Path) -> i64 {
    let Ok(file) = fs::File::open(path) else {
        return 0;
    };
    let mut count = 0i64;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            return 0;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"].as_str() != Some("response_item") {
            continue;
        }
        let payload = &value["payload"];
        if payload["type"].as_str() != Some("message") {
            continue;
        }
        if matches!(payload["role"].as_str(), Some("user") | Some("assistant")) {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use rusqlite::Connection;
    use serde_json::json;
    use std::time::{Duration, UNIX_EPOCH};

    fn temp_paths() -> (PathBuf, Paths) {
        let home = std::env::temp_dir().join(format!("asm-codex-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            sessions: home.join("sessions"),
            archived: home.join("archived_sessions"),
            home: home.clone(),
        };
        fs::create_dir_all(&paths.sessions).unwrap();
        fs::create_dir_all(&paths.archived).unwrap();
        (home, paths)
    }

    fn write_lines(path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut text = lines.join("\n");
        text.push('\n');
        fs::write(path, text).unwrap();
    }

    fn set_mtime(path: &Path, seconds: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    fn simple_rollout(id: &str, user: &str, assistant: bool) -> Vec<String> {
        let mut lines = vec![
            json!({
                "timestamp": "2026-02-01T00:00:00Z",
                "type": "session_meta",
                "payload": {"id": id, "session_id": id, "cwd": r"\\?\D:\Work"}
            })
            .to_string(),
            json!({
                "timestamp": "2026-02-01T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": user}]
                }
            })
            .to_string(),
        ];
        if assistant {
            lines.push(
                json!({
                    "timestamp": "2026-02-01T00:00:02Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "回复"}]
                    }
                })
                .to_string(),
            );
        }
        lines
    }

    #[test]
    fn parses_rollout_dedupes_and_pairs_tools() {
        let (home, paths) = temp_paths();
        let id = "01a1aa00-0000-7000-8000-000000000001";
        let file = paths
            .sessions
            .join("2026/01/01")
            .join(format!("rollout-2026-01-01T00-00-00-{id}.jsonl"));
        let lines = vec![
            json!({
                "timestamp": "2026-01-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "session_id": id,
                    "timestamp": "2026-01-01T00:00:00.000Z",
                    "cwd": r"\\?\D:\Work",
                    "source": "cli"
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "developer",
                    "content": [{"type": "input_text", "text": "开发说明"}]
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "第一问"}]
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:02.500Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "回答一"}]
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "response_item",
                "payload": {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "想一下"}],
                    "content": null,
                    "encrypted_content": ""
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:04.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "arguments": r#"{"cmd":"pwd"}"#,
                    "call_id": "c1",
                    "status": "completed"
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:05.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "c1",
                    "output": "ok"
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:06.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "重复文本"}
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:07.000Z",
                "type": "compacted",
                "payload": {"message": "摘要正文"}
            })
            .to_string(),
            json!({
                "timestamp": "2026-01-01T00:00:08.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "第二问"}]
                }
            })
            .to_string(),
        ];
        write_lines(&file, &lines);

        let session = load(&paths, id).unwrap();
        assert_eq!(session.agent, "codex");
        assert_eq!(session.id, id);
        assert_eq!(session.title, "第一问");
        assert_eq!(session.cwd, r"D:\Work");
        assert_eq!(
            session.created_ms,
            agent_core::time::iso_to_ms("2026-01-01T00:00:00.000Z")
        );
        assert_eq!(
            session.updated_ms,
            agent_core::time::iso_to_ms("2026-01-01T00:00:08.000Z")
        );
        assert_eq!(session.messages.len(), 5);

        let first_question = session
            .messages
            .iter()
            .flat_map(|message| message.blocks.iter())
            .filter(|block| matches!(block, Block::Text(text) if text == "第一问"))
            .count();
        assert_eq!(first_question, 1, "event_msg 与 developer 不应重复进入 IR");
        assert!(session.messages.iter().all(|message| {
            message.blocks.iter().all(
            |block| !matches!(block, Block::Text(text) if text == "开发说明" || text == "重复文本")
        )
        }));

        assert!(matches!(
            &session.messages[2].blocks[0],
            Block::Thinking(text) if text == "想一下"
        ));
        match &session.messages[3].blocks[0] {
            Block::ToolCall {
                id,
                name,
                input,
                output,
                status,
            } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "shell");
                assert_eq!(input["cmd"], "pwd");
                assert_eq!(output.as_deref(), Some("ok"));
                assert_eq!(status, "completed");
            }
            other => panic!("expected tool call, got {other:?}"),
        }

        let compaction = session.compaction.expect("compaction");
        assert_eq!(compaction.summary_text, "摘要正文");
        assert_eq!(compaction.tail_start_index, 4);
        assert_eq!(
            compaction.time_ms,
            agent_core::time::iso_to_ms("2026-01-01T00:00:07.000Z")
        );

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn fallback_list_scans_files_sorts_and_marks_archived() {
        let (home, paths) = temp_paths();
        let a = "01a1cc00-0000-7000-8000-00000000000a";
        let b = "01a1cc00-0000-7000-8000-00000000000b";
        let c = "01a1cc00-0000-7000-8000-00000000000c";

        let a_file = paths
            .sessions
            .join("2026/03/01")
            .join(format!("rollout-a-{a}.jsonl"));
        write_lines(&a_file, &simple_rollout(a, "A 问", true));
        set_mtime(&a_file, 1_700_000_001);

        let b_file = paths
            .sessions
            .join("2026/03/02")
            .join(format!("rollout-b-{b}.jsonl"));
        write_lines(&b_file, &simple_rollout(b, "B 问", false));
        set_mtime(&b_file, 1_700_000_003);

        let c_file = paths.archived.join(format!("rollout-c-{c}.jsonl"));
        write_lines(&c_file, &simple_rollout(c, "C 问", true));
        set_mtime(&c_file, 1_700_000_002);

        let rows = list(&paths).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec![b, c, a]
        );
        assert_eq!(rows[0].message_count, 1);
        assert_eq!(rows[1].message_count, 2);
        assert_eq!(rows[2].message_count, 2);
        assert!(!rows[0].archived);
        assert!(rows[1].archived);
        assert!(!rows[2].archived);
        assert_eq!(rows[2].title, "A 问");
        assert_eq!(rows[2].cwd, r"D:\Work");
        assert_eq!(rows[2].updated_ms, 1_700_000_001_000);
        assert_eq!(
            rows[2].created_ms,
            agent_core::time::iso_to_ms("2026-02-01T00:00:00Z")
        );

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn lists_from_state_db_and_filters_subagent_threads() {
        let (home, paths) = temp_paths();
        let main_id = "01a1bb00-0000-7000-8000-000000000001";
        let sub_id = "01a1bb00-0000-7000-8000-000000000002";
        let child_id = "01a1bb00-0000-7000-8000-000000000003";

        // 文件名故意不含会话 id，验证 DB rollout_path 优先于文件名扫描。
        let rollout = paths
            .sessions
            .join("2026/04/01")
            .join("rollout-odd-name.jsonl");
        write_lines(&rollout, &simple_rollout(main_id, "数据库兜底", true));

        fs::write(home.join("state_2.sqlite"), b"").unwrap();
        let db_path = home.join("state_5.sqlite");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
                source TEXT NOT NULL DEFAULT '',
                cwd TEXT NOT NULL DEFAULT '',
                title TEXT NOT NULL DEFAULT '',
                first_user_message TEXT NOT NULL DEFAULT '',
                preview TEXT NOT NULL DEFAULT '',
                archived INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER,
                updated_at_ms INTEGER,
                name TEXT,
                attached_thread_parent_id TEXT
            );",
        )
        .unwrap();
        let insert = "INSERT INTO threads (
            id, rollout_path, source, cwd, title, name, first_user_message,
            preview, archived, created_at_ms, updated_at_ms, attached_thread_parent_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";
        let rollout_text = rollout.to_string_lossy().to_string();
        conn.execute(
            insert,
            params![
                main_id,
                rollout_text,
                "vscode",
                r"\\?\D:\Work",
                "",
                "数据库名称",
                "",
                "",
                0i64,
                1_700_000_100_000i64,
                1_700_000_200_000i64,
                Option::<String>::None
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            params![
                sub_id,
                rollout_text,
                "subAgentV1",
                r"\\?\D:\Work",
                "",
                Option::<String>::None,
                "",
                "",
                0i64,
                1_700_000_000_000i64,
                1_700_000_000_000i64,
                Option::<String>::None
            ],
        )
        .unwrap();
        conn.execute(
            insert,
            params![
                child_id,
                rollout_text,
                "vscode",
                r"\\?\D:\Work",
                "",
                Option::<String>::None,
                "",
                "",
                0i64,
                1_700_000_000_000i64,
                1_700_000_000_000i64,
                Some(main_id.to_string())
            ],
        )
        .unwrap();
        drop(conn);

        assert_eq!(state_db(&paths).unwrap(), db_path);
        let rows = list(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, main_id);
        assert_eq!(row.title, "数据库名称");
        assert_eq!(row.cwd, r"D:\Work");
        assert_eq!(row.message_count, 2);
        assert_eq!(row.created_ms, 1_700_000_100_000);
        assert_eq!(row.updated_ms, 1_700_000_200_000);
        assert!(!row.archived);
        assert_eq!(
            find_rollout(&paths, main_id).as_deref(),
            Some(rollout.as_path())
        );
        assert!(active(&paths, main_id));

        let _ = fs::remove_dir_all(home);
    }
}
