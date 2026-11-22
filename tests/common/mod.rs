// 本文件是共享 fixture：每个测试二进制只用到其中一部分，未用项属正常。
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use agent_plugin_claude as claude;
use agent_plugin_cursor as cursor;
use agent_plugin_dsh as dsh;
use agent_plugin_kiro as kiro;
use agent_plugin_qoder as qoder;
use agent_plugin_workbuddy as workbuddy;
use agent_plugin_zcode as zcode;
use agent_session_manager::agents::{self, AgentAdapter, AgentInfo};
use agent_session_manager::ir::{Session, SessionSummary};
use rusqlite::Connection;

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let p = std::env::temp_dir().join(format!("asm-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 进程级环境变量重定向，Drop 时恢复；持有期间同进程其他依赖 env 的测试被串行化。
pub struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn new(vars: &[(&str, &str)]) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut saved = Vec::new();
        for (name, value) in vars {
            saved.push((name.to_string(), std::env::var(name).ok()));
            std::env::set_var(name, value);
        }
        Self { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// messages 为空的会话，用于空会话链路与空工作区守卫测试。
pub fn empty_session(cwd: &str) -> Session {
    Session {
        agent: "qoder".into(),
        id: "source-empty".into(),
        title: "空会话".into(),
        cwd: cwd.into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: Vec::new(),
        compaction: None,
    }
}

const QODER_DDL: &str = "
CREATE TABLE chat_sessions (
    session_id TEXT PRIMARY KEY, sidebar_custom_order INTEGER, origin_session_id TEXT,
    session_kind TEXT NOT NULL DEFAULT 'standard', owner_session_id TEXT,
    side_chat_label_ordinal INTEGER, side_chat_lifecycle_state TEXT, side_chat_cleanup_state TEXT,
    side_chat_last_activity_at TIMESTAMP, side_chat_expires_at TIMESTAMP, side_chat_expired_at TIMESTAMP,
    side_chat_closed_at TIMESTAMP, side_chat_close_reason TEXT, side_chat_cleanup_failure_code TEXT,
    conversation_mode TEXT NOT NULL DEFAULT 'normal', product_mode TEXT NOT NULL DEFAULT 'coding',
    title TEXT NOT NULL, cwd TEXT NOT NULL, execution_kind TEXT, workspace_id TEXT, git_branch TEXT,
    model TEXT, permission_mode TEXT, additional_direcotries TEXT NOT NULL DEFAULT '{}',
    extra_json TEXT NOT NULL DEFAULT '{}', created_at TIMESTAMP NOT NULL, updated_at TIMESTAMP NOT NULL,
    execution_target_json TEXT NOT NULL DEFAULT '{}', registration_json TEXT,
    archived INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0,
    archived_at TIMESTAMP, deleted_at TIMESTAMP, message_envelope_version INTEGER, initiator_peer_device_id TEXT
);
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY, name TEXT NOT NULL, workspace_source TEXT NOT NULL,
    root_paths_json TEXT NOT NULL, product_mode TEXT NOT NULL DEFAULT 'coding',
    extra_json TEXT NOT NULL DEFAULT '{}', created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0, deleted_at INTEGER
);
CREATE TABLE chat_session_sidebar_placements (
    session_id TEXT PRIMARY KEY, work_directory TEXT, sort_order INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES chat_sessions(session_id) ON DELETE CASCADE
);
CREATE TABLE chat_session_messages (
    session_id TEXT NOT NULL, message_id TEXT NOT NULL, turn_id TEXT, sequence INTEGER NOT NULL,
    payload_json TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'completed', feedback TEXT,
    source TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, role TEXT,
    message_stream TEXT, voice_session_id TEXT, subtype TEXT, runtime_input_state TEXT,
    fork_availability TEXT, turn_started_at INTEGER, is_compaction_boundary INTEGER,
    has_visible_content INTEGER, payload_bytes INTEGER, payload_hash TEXT, payload_schema_version INTEGER,
    feedback_revision INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, message_id),
    FOREIGN KEY (session_id) REFERENCES chat_sessions(session_id) ON DELETE CASCADE
);
";

const ZCODE_DDL: &str = "
CREATE TABLE session (
    id text primary key, project_id text not null, workspace_id text, parent_id text,
    slug text not null, directory text not null, path text, title text not null, version text not null,
    share_url text, summary_additions integer, summary_deletions integer, summary_files integer,
    summary_diffs text, revert text, permission text, time_created integer not null,
    time_updated integer not null, time_compacting integer, time_archived integer,
    task_type text not null default 'interactive',
    title_source text not null default 'first_input', title_message_id text,
    time_title_updated integer, trace_id text, time_deleted integer, last_outcome text
);
CREATE TABLE message (
    id text primary key, session_id text not null references session(id) on delete cascade,
    time_created integer not null, time_updated integer not null, data text not null, sequence integer
);
CREATE TABLE part (
    id text primary key, message_id text not null references message(id) on delete cascade,
    session_id text not null, time_created integer not null, time_updated integer not null,
    data text not null, sequence integer
);
CREATE TABLE session_entry (
    id text primary key, session_id text not null references session(id) on delete cascade,
    type text not null, time_created integer not null, time_updated integer not null, data text not null
);
CREATE TABLE todo (
    session_id text not null, content text not null, status text not null,
    priority integer not null default 0, position integer not null default 0,
    time_created integer not null, time_updated integer not null
);
-- 项目级授权：真实 schema 没有 session_id 列，销毁会话不得触达此表。
CREATE TABLE permission (
    project_id text not null, time_created integer not null, time_updated integer not null,
    data text not null, PRIMARY KEY (project_id)
);
-- 任务链：parent/child 双列关联会话。
CREATE TABLE session_task_link (
    id text primary key, parent_session_id text, child_session_id text, role text,
    status text, time_created integer not null, time_updated integer not null
);
";

const TASKS_DDL: &str = "
CREATE TABLE tasks (
    workspace_key TEXT NOT NULL, workspace_path TEXT NOT NULL, workspace_identity TEXT,
    task_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', task_status TEXT, provider TEXT,
    mode TEXT NOT NULL DEFAULT 'build', model TEXT, migration_source TEXT, forked_from_task_id TEXT,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, unread_at INTEGER,
    pinned INTEGER NOT NULL DEFAULT 0, archived INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0,
    title_overridden INTEGER NOT NULL DEFAULT 0, meta_json TEXT NOT NULL DEFAULT '{}',
    searchable_text TEXT NOT NULL DEFAULT '', last_unread_at INTEGER NOT NULL DEFAULT 0,
    cron_automation_id TEXT, off_peak_task_id TEXT, host_id TEXT, root_path TEXT, project_id TEXT,
    execution_directories TEXT, sort_order INTEGER, cli_title TEXT, handoff_pending INTEGER NOT NULL DEFAULT 0,
    cli_title_source TEXT, cli_created_at INTEGER, cli_updated_at INTEGER, cli_last_outcome TEXT, cli_deleted INTEGER,
    PRIMARY KEY (workspace_key, task_id)
);
CREATE TABLE hosts (
    host_id TEXT PRIMARY KEY, kind TEXT NOT NULL, target_json TEXT, membership_id TEXT,
    position INTEGER NOT NULL DEFAULT 0, last_connection_status TEXT, last_connection_error TEXT, last_opened_at INTEGER
);
INSERT INTO hosts (host_id, kind) VALUES ('local', 'local');
CREATE TABLE projects (
    project_id TEXT PRIMARY KEY, host_id TEXT NOT NULL REFERENCES hosts(host_id),
    name TEXT NOT NULL, name_overridden INTEGER NOT NULL DEFAULT 0, position INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0, pinned_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0,
    deleted_at INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE project_roots (
    project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
    root_path TEXT NOT NULL, position INTEGER NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(project_id, root_path)
);
CREATE TABLE task_group_admissions (
    workspace_key TEXT NOT NULL, task_id TEXT NOT NULL, system_kind TEXT,
    user_organized INTEGER NOT NULL DEFAULT 0, creation_id TEXT,
    PRIMARY KEY (workspace_key, task_id)
);
";

#[allow(dead_code)]
const CURSOR_DDL: &str = "
CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
CREATE TABLE composerHeaders (
    composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER,
    isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, value TEXT,
    subagentTypeName TEXT
);
";

#[allow(dead_code)]
pub fn empty_cursor(root: &Path) -> cursor::Paths {
    let user = root.join("Cursor").join("User");
    let state_db = user.join("globalStorage").join("state.vscdb");
    let workspace_storage = user.join("workspaceStorage");
    fs::create_dir_all(state_db.parent().unwrap()).unwrap();
    fs::create_dir_all(&workspace_storage).unwrap();
    let db = Connection::open(&state_db).unwrap();
    db.execute_batch(CURSOR_DDL).unwrap();
    cursor::Paths {
        state_db,
        workspace_storage,
    }
}

const WORKBUDDY_DDL: &str = "
CREATE TABLE sessions (
    id TEXT PRIMARY KEY, cwd TEXT NOT NULL, user_id TEXT NOT NULL, title TEXT,
    custom_title TEXT, status TEXT NOT NULL DEFAULT 'Pending', created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL, last_activity_at INTEGER, deleted_at INTEGER,
    is_playground INTEGER NOT NULL DEFAULT 0, source_mode TEXT, mode TEXT, model TEXT,
    permission_mode TEXT, use_sandbox_cli INTEGER, context_window INTEGER, thought_level TEXT,
    transport TEXT NOT NULL DEFAULT 'local', unread INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE session_usage (
    session_id TEXT PRIMARY KEY, used INTEGER NOT NULL, size INTEGER NOT NULL,
    updated_at INTEGER NOT NULL, credit_json TEXT
);
";

pub fn empty_workbuddy(root: &Path) -> workbuddy::Paths {
    let home = root.join("workbuddy");
    let db = home.join("workbuddy.db");
    std::fs::create_dir_all(&home).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(WORKBUDDY_DDL).unwrap();
    let storage = home.join("storage").join("user-test-user-personal");
    std::fs::create_dir_all(&storage).unwrap();
    workbuddy::Paths {
        projects: home.join("projects"),
        db,
        home,
    }
}

pub fn workbuddy_fixture(root: &Path) -> (workbuddy::Paths, String) {
    let paths = empty_workbuddy(root);
    let id = "wb_session_1".to_string();
    let db = Connection::open(&paths.db).unwrap();
    db.execute(
        "INSERT INTO sessions
         (id, cwd, user_id, title, status, created_at, updated_at, last_activity_at, transport)
         VALUES (?1, 'D:/test', 'test-user', 'WorkBuddy 测试', 'completed', 1000, 2000, 2000, 'local')",
        [&id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_usage (session_id, used, size, updated_at, credit_json)
         VALUES (?1, 0, 1000000, 2000, '{}')",
        [&id],
    )
    .unwrap();

    let dir = paths.projects.join("d-test");
    std::fs::create_dir_all(&dir).unwrap();
    let jsonl = [
        r#"{"type":"session-meta","id":"meta1","sessionId":"wb_session_1","timestamp":1000,"meta":{}}"#,
        r#"{"id":"u1","timestamp":1000,"type":"message","role":"user","content":[{"type":"input_text","text":"<system-reminder data-role=\"additional-data\">\n<current_time>now</current_time>\n</system-reminder>\n<user_query>列出文件</user_query>"}],"sessionId":"wb_session_1","cwd":"d:\\test"}"#,
        r#"{"id":"r1","parentId":"u1","timestamp":1100,"type":"reasoning","content":[],"rawContent":[{"type":"reasoning_text","text":"用 ls"}],"sessionId":"wb_session_1","cwd":"d:\\test"}"#,
        r#"{"id":"c1","parentId":"r1","timestamp":1200,"type":"function_call","callId":"call_9","name":"Bash","arguments":"{\"command\":\"ls\"}","sessionId":"wb_session_1","cwd":"d:\\test"}"#,
        r#"{"id":"cr1","parentId":"c1","timestamp":1300,"type":"function_call_result","name":"Bash","callId":"call_9","status":"completed","output":{"type":"text","text":"a.txt"},"sessionId":"wb_session_1","cwd":"d:\\test"}"#,
        r#"{"id":"a1","parentId":"cr1","timestamp":1400,"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"有 a.txt"}],"sessionId":"wb_session_1","cwd":"d:\\test"}"#,
    ]
    .join("\n")
        + "\n";
    std::fs::write(dir.join(format!("{id}.jsonl")), jsonl).unwrap();
    (paths, id)
}

pub fn empty_qoder(root: &Path) -> qoder::Paths {
    fs::create_dir_all(root).unwrap();
    let projects = root.join("projects");
    fs::create_dir_all(&projects).unwrap();
    let main_db = root.join("main.sqlite");
    Connection::open(&main_db)
        .unwrap()
        .execute_batch(QODER_DDL)
        .unwrap();
    qoder::Paths { projects, main_db }
}

pub fn empty_zcode(root: &Path) -> zcode::Paths {
    let paths = zcode::Paths {
        db: root.join("db.sqlite"),
        tasks_db: root.join("tasks-index.sqlite"),
    };
    create_zcode_dbs(&paths);
    paths
}

fn create_zcode_dbs(paths: &zcode::Paths) {
    fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
    fs::create_dir_all(paths.tasks_db.parent().unwrap()).unwrap();
    Connection::open(&paths.db)
        .unwrap()
        .execute_batch(ZCODE_DDL)
        .unwrap();
    Connection::open(&paths.tasks_db)
        .unwrap()
        .execute_batch(TASKS_DDL)
        .unwrap();
}

/// 按 `zcode::Paths::default()` 的真实布局（USERPROFILE 下）建库并填充会话，
/// 供走 `Paths::default()` 的转接执行层测试使用。
pub fn zcode_default_fixture(user_root: &Path) -> (zcode::Paths, String) {
    let paths = zcode::Paths {
        db: user_root
            .join(".zcode")
            .join("cli")
            .join("db")
            .join("db.sqlite"),
        tasks_db: user_root
            .join(".zcode")
            .join("v2")
            .join("tasks-index.sqlite"),
    };
    create_zcode_dbs(&paths);
    let id = "sess_99999999-8888-7777-6666-555555555555".to_string();
    seed_zcode(&paths, &id);
    (paths, id)
}

pub fn qoder_fixture(root: &Path) -> (qoder::Paths, String) {
    let paths = empty_qoder(root);
    let id = "11111111-2222-3333-4444-555555555555".to_string();
    let slug_dir = paths.projects.join("D--test");
    fs::create_dir_all(&slug_dir).unwrap();
    let jsonl = r#"{"type":"workspace-directories","directories":["D:\\test"]}
{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-11-01T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"列出文件"}]}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-11-01T10:00:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"用 ls"},{"type":"tool_use","id":"call_1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-11-01T10:00:06Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"a.txt"}]}}
{"type":"assistant","uuid":"a2","parentUuid":"u2","timestamp":"2026-11-01T10:00:10Z","message":{"role":"assistant","content":[{"type":"text","text":"有 a.txt"}]}}
{"type":"active-leaf","leafUuid":"a2"}
"#;
    fs::write(slug_dir.join(format!("{id}.jsonl")), jsonl).unwrap();
    let db = Connection::open(&paths.main_db).unwrap();
    db.execute_batch(
        "INSERT INTO chat_sessions (session_id, title, cwd, created_at, updated_at, archived) VALUES ('11111111-2222-3333-4444-555555555555', '测试会话', 'D:\\test', 1000, 2000, 1);
         INSERT INTO chat_session_messages (session_id, message_id, sequence, payload_json, source, created_at, updated_at) VALUES ('11111111-2222-3333-4444-555555555555', 'm1', 1, '{}', 'host-projection', 1, 1);
         INSERT INTO chat_session_messages (session_id, message_id, sequence, payload_json, source, created_at, updated_at) VALUES ('11111111-2222-3333-4444-555555555555', 'm2', 2, '{}', 'sdk-projection', 2, 2);",
    )
    .unwrap();
    (paths, id)
}

pub fn zcode_fixture(root: &Path) -> (zcode::Paths, String) {
    let paths = empty_zcode(root);
    let id = "sess_99999999-8888-7777-6666-555555555555".to_string();
    seed_zcode(&paths, &id);
    (paths, id)
}

fn seed_zcode(paths: &zcode::Paths, id: &str) {
    let db = Connection::open(&paths.db).unwrap();
    db.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?1, 'proj_d--test', ?1, 'D:\\test', 'ZCode 测试', '0.17.1', 1000, 2000)",
        [&id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES ('sess_subagent_x', 'proj_d--test', 'sess_subagent_x', 'D:\\test', '子代理', '0.17.1', 1, 2)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO message VALUES ('msg_1', ?1, 1, 1, ?2, 1)",
        rusqlite::params![id, r#"{"role":"user","time":{"created":1000}}"#],
    )
    .unwrap();
    db.execute(
        "INSERT INTO message VALUES ('msg_2', ?1, 2, 2, ?2, 2)",
        rusqlite::params![id, r#"{"role":"assistant","time":{"created":1500}}"#],
    )
    .unwrap();
    db.execute(
        "INSERT INTO part VALUES ('p1', 'msg_1', ?1, 1, 1, ?2, 1)",
        rusqlite::params![id, r#"{"type":"text","text":"读文件"}"#],
    )
    .unwrap();
    db.execute_batch(&format!(
        "INSERT INTO part VALUES ('p2', 'msg_2', '{id}', 2, 2, '{{\"type\":\"reasoning\",\"text\":\"思考\"}}', 1);
         INSERT INTO part VALUES ('p3', 'msg_2', '{id}', 2, 2, '{{\"type\":\"tool\",\"callID\":\"call_9\",\"tool\":\"Read\",\"state\":{{\"status\":\"completed\",\"input\":{{\"file_path\":\"a.txt\"}},\"output\":\"content\"}}}}', 2);
         INSERT INTO part VALUES ('p4', 'msg_2', '{id}', 2, 2, '{{\"type\":\"step-finish\"}}', 3);"
    ))
    .unwrap();
    let tasks = Connection::open(&paths.tasks_db).unwrap();
    tasks
        .execute(
            "INSERT INTO tasks (workspace_key, workspace_path, task_id, title, created_at, updated_at, archived) VALUES ('ws', 'D:\\test', ?1, 'ZCode 测试', 1000, 2000, 1)",
            [id],
        )
        .unwrap();
}

pub fn empty_kiro(root: &Path) -> kiro::Paths {
    let home = root.join("kiro");
    let paths = kiro::Paths {
        sessions: home.join("sessions"),
        session_index: home.join("session-index"),
        spec_sessions: home.join("spec-sessions"),
        workspace_storage: home.join("workspaceStorage"),
        home: home.clone(),
    };
    std::fs::create_dir_all(&paths.sessions).unwrap();
    paths
}

#[allow(dead_code)]
pub fn kiro_panel_state(paths: &kiro::Paths, workspace: &str, ids: &[&str]) {
    let dir = paths.workspace_storage.join(workspace);
    fs::create_dir_all(&dir).unwrap();
    let db = Connection::open(dir.join("state.vscdb")).unwrap();
    db.execute_batch("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);")
        .unwrap();
    let entries: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!({ "id": id, "source": "local" }))
        .collect();
    let value = serde_json::json!({ "sessionPanels": { "entries": entries } }).to_string();
    db.execute(
        "INSERT INTO ItemTable (key, value) VALUES ('kiro.kiroAgent', ?1)",
        [value],
    )
    .unwrap();
}

#[allow(dead_code)]
pub fn kiro_fixture(root: &Path) -> (kiro::Paths, String) {
    let paths = empty_kiro(root);
    let id = "sess_11111111-2222-3333-4444-555555555555".to_string();
    let hash = kiro::workspace_hash("D:\\test");
    let dir = paths.sessions.join(&hash).join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    let session_json = r#"{
  "schemaVersion": "1.0.0",
  "dataModelVersion": 1,
  "id": "sess_11111111-2222-3333-4444-555555555555",
  "title": "Kiro 测试",
  "agentMode": "vibe",
  "workspacePaths": ["d:\\test"],
  "rootPaths": ["d:\\test"],
  "createdAt": "2026-11-01T10:00:00.000Z",
  "lastModifiedAt": "2026-11-01T10:00:10.000Z",
  "modelId": "auto",
  "status": "idle"
}"#;
    std::fs::write(dir.join("session.json"), session_json).unwrap();
    let jsonl = [
        r#"{"id":"e0","timestamp":"2026-11-01T10:00:00.000Z","payload":{"type":"turn_start","executionId":"turn_1"}}"#,
        r#"{"id":"e1","timestamp":"2026-11-01T10:00:01.000Z","payload":{"type":"user","content":"列出文件","documents":[],"images":[{"data":"AAAA","mimeType":"image/jpeg"}]}}"#,
        r#"{"id":"e2","timestamp":"2026-11-01T10:00:02.000Z","payload":{"type":"assistant","operationType":"Reasoning","content":"用 ls"}}"#,
        r#"{"id":"e3","timestamp":"2026-11-01T10:00:03.000Z","payload":{"type":"tool_call","toolCallId":"toolu_1","toolName":"execute_pwsh","args":{"command":"ls"},"status":"completed"}}"#,
        r#"{"id":"e4","timestamp":"2026-11-01T10:00:04.000Z","payload":{"type":"tool_result","toolCallId":"toolu_1","content":"a.txt","success":true}}"#,
        r#"{"id":"e5","timestamp":"2026-11-01T10:00:05.000Z","payload":{"type":"assistant","operationType":"Say","content":"有 a.txt"}}"#,
        r#"{"id":"e6","timestamp":"2026-11-01T10:00:06.000Z","payload":{"type":"session_metadata","key":"contextUsage","value":{"usagePercentage":12}}}"#,
        r#"{"id":"e7","timestamp":"2026-11-01T10:00:07.000Z","payload":{"type":"turn_end","executionId":"turn_1"}}"#,
    ]
    .join("\n")
        + "\n";
    std::fs::write(dir.join("messages.jsonl"), jsonl).unwrap();
    (paths, id)
}

pub fn claude_fixture(root: &Path) -> (claude::Paths, String) {
    let home = root.join("claude");
    let paths = claude::Paths {
        projects: home.join("projects"),
        home,
    };
    let id = "11111111-2222-3333-4444-555555555555".to_string();
    let dir = paths.projects.join("d--test");
    fs::create_dir_all(&dir).unwrap();
    let jsonl = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-11-01T10:00:00.000Z","sessionId":"11111111-2222-3333-4444-555555555555"}
{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-11-01T10:00:00.000Z","cwd":"D:\\test","message":{"role":"user","content":[{"type":"text","text":"列出文件"}]}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-11-01T10:00:05.000Z","cwd":"D:\\test","message":{"role":"assistant","content":[{"type":"thinking","thinking":"用 ls"},{"type":"tool_use","id":"call_1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-11-01T10:00:06.000Z","cwd":"D:\\test","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"a.txt"}]}}
{"type":"assistant","uuid":"a2","parentUuid":"u2","timestamp":"2026-11-01T10:00:10.000Z","cwd":"D:\\test","message":{"role":"assistant","content":[{"type":"text","text":"有 a.txt"}]}}
"#;
    fs::write(dir.join(format!("{id}.jsonl")), jsonl).unwrap();
    (paths, id)
}

fn zstd_frame(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

pub fn dsh_fixture(root: &Path) -> (dsh::Paths, String, PathBuf) {
    let home = root.join("dsh");
    let paths = dsh::Paths {
        sessions: home.join("sessions"),
        storages: home.join("storages"),
        home,
    };
    let id = "session-11111111-2222-3333-4444-555555555555".to_string();
    let dir = paths.sessions.join("--D-test--").join(&id);
    fs::create_dir_all(&dir).unwrap();

    let header = r#"{"type":"session","version":4,"id":"session-11111111-2222-3333-4444-555555555555","createdAt":1000,"cwd":"D:\\test","isSeeded":false,"delegationDepth":0,"agentPreset":"standard"}"#;
    let body = [
        r#"{"type":"session/title","seq":0,"time":1000,"data":{"title":"DSH 测试","messageSeqs":[],"source":{"kind":"user"}}}"#,
        r#"{"type":"turn/start","seq":1,"time":1000,"data":{"turn":1}}"#,
        r#"{"type":"step/start","seq":2,"time":1001,"data":{"turn":1,"step":1}}"#,
        r#"{"type":"user/message","seq":3,"time":1002,"data":{"content":[{"type":"text","text":"列出文件"}],"source":{"kind":"user"},"role":"user","id":"u1"},"surfaceOp":"append"}"#,
        r#"{"type":"assistant/message","seq":4,"time":1003,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"用 ls"},{"type":"tool-call","id":"call_1","name":"Bash","arguments":"{\"command\":\"ls\"}"}],"source":{"kind":"model","provider":"x","model":"y"}}}}"#,
        r#"{"type":"tool/call","seq":5,"time":1004,"data":{"turn":1,"step":1,"callId":"call_1","name":"Bash","arguments":"{\"command\":\"ls\"}"}}"#,
        r#"{"type":"tool/result","seq":6,"time":1004,"data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"call_1"},"toolCallId":"call_1","content":[{"type":"text","text":"a.txt"}],"isError":false}}}"#,
        r#"{"type":"step/end","seq":7,"time":1005,"data":{"turn":1,"step":1}}"#,
        r#"{"type":"turn/end","seq":8,"time":1005,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
        r#"{"type":"user/message","seq":9,"time":1006,"data":{"content":[{"type":"text","text":"摘要正文"}],"source":{"kind":"plugin","plugin":"compact","compactionId":"c1"},"role":"user","id":"c1"},"surfaceOp":{"op":"replace","start":3,"end":6}}"#,
        r#"{"type":"turn/start","seq":10,"time":1007,"data":{"turn":2}}"#,
        r#"{"type":"step/start","seq":11,"time":1008,"data":{"turn":2,"step":1}}"#,
        r#"{"type":"user/message","seq":12,"time":1009,"data":{"content":[{"type":"text","text":"继续"}],"source":{"kind":"user"},"role":"user","id":"u2"},"surfaceOp":"append"}"#,
        r#"{"type":"assistant/message","seq":13,"time":1010,"data":{"turn":2,"step":1,"message":{"role":"assistant","content":[{"type":"text","text":"好的"}],"source":{"kind":"model","provider":"x","model":"y"}}}}"#,
        r#"{"type":"step/end","seq":14,"time":1011,"data":{"turn":2,"step":1}}"#,
        r#"{"type":"turn/end","seq":15,"time":1012,"data":{"turn":2,"reason":{"kind":"completed"}}}"#,
    ]
    .join("\n")
        + "\n";
    let mut file = zstd_frame(format!("{header}\n").as_bytes());
    let lines: Vec<&str> = body.lines().collect();
    let mid = lines.len() / 2;
    file.extend(zstd_frame((lines[..mid].join("\n") + "\n").as_bytes()));
    file.extend(zstd_frame((lines[mid..].join("\n") + "\n").as_bytes()));
    fs::write(dir.join("session.v4.jsonl.zstd"), file).unwrap();

    fs::create_dir_all(&paths.storages).unwrap();
    let workspace = serde_json::json!({
        "unit": { "name": "workspace", "version": 2 },
        "global": {
            "initialized": true,
            "workspaceIds": ["ws-test"],
            "archivedSessionIds": [],
            "pinnedSessionIds": []
        },
        "tables": {
            "workspaces": {
                "ws-test": {
                    "path": "D:\\test",
                    "title": "test",
                    "sessionIds": [id.clone()],
                    "createdAt": "2026-11-01T10:00:00.000Z",
                    "updatedAt": "2026-11-01T10:00:00.000Z"
                }
            }
        }
    });
    fs::write(
        paths.storages.join("workspace.json"),
        serde_json::to_string_pretty(&workspace).unwrap(),
    )
    .unwrap();
    let cache = serde_json::json!({
        "version": 7,
        "record": {
            "identity": {
                "formatVersion": 4,
                "createdAt": 1000,
                "cwd": "D:\\test",
                "isSeeded": false,
                "inheritedEventCount": 0
            },
            "rows": {
                "title": { "ver": 1, "seq": 15, "val": "DSH 测试" }
            }
        }
    });
    let cache_dir = paths.storages.join("session_projcache").join("sessions");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(
        cache_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&cache).unwrap(),
    )
    .unwrap();
    let mut cache_sessions = serde_json::Map::new();
    cache_sessions.insert(id.clone(), cache);
    let aggregate = serde_json::json!({
        "unit": { "name": "session_projcache", "version": 3 },
        "global": null,
        "tables": { "sessions": cache_sessions }
    });
    fs::write(
        paths.storages.join("session_projcache.json"),
        serde_json::to_string_pretty(&aggregate).unwrap(),
    )
    .unwrap();
    let mut feedback_sessions = serde_json::Map::new();
    feedback_sessions.insert(
        id.clone(),
        serde_json::json!({ "session": { "createdAt": 1000 }, "items": [] }),
    );
    let feedback = serde_json::json!({
        "unit": { "name": "message_feedback", "version": 0 },
        "global": null,
        "tables": { "sessions": feedback_sessions }
    });
    fs::write(
        paths.storages.join("message_feedback.json"),
        serde_json::to_string_pretty(&feedback).unwrap(),
    )
    .unwrap();
    (paths, id, dir)
}

struct WorkBuddyAdapter;
struct ZCodeAdapter;
struct ClaudeAdapter;
struct DshAdapter;
struct QoderAdapter;

macro_rules! adapter_boilerplate {
    ($name:ident, $paths:expr, $list:path, $load:path, $inject:path, $overwrite:path, $delete:path, $destroy:path, $active:path, $backup:expr) => {
        impl AgentAdapter for $name {
            fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
                $list(&$paths())
            }

            fn load(&self, id: &str) -> anyhow::Result<Session> {
                $load(&$paths(), id)
            }

            fn write(
                &self,
                session: &Session,
                cwd: &str,
                overwrite: Option<&str>,
            ) -> anyhow::Result<String> {
                match overwrite {
                    Some(id) => $overwrite(&$paths(), session, cwd, id),
                    None => $inject(&$paths(), session, cwd),
                }
            }

            fn delete(&self, id: &str) -> anyhow::Result<()> {
                $delete(&$paths(), id)
            }

            fn destroy(&self, id: &str) -> anyhow::Result<()> {
                $destroy(&$paths(), id)
            }

            fn active(&self, id: &str) -> anyhow::Result<bool> {
                Ok($active(&$paths(), id))
            }

            fn backup_paths(&self) -> anyhow::Result<Vec<std::path::PathBuf>> {
                let paths = $paths();
                Ok($backup(&paths))
            }
        }
    };
}

adapter_boilerplate!(
    WorkBuddyAdapter,
    workbuddy::Paths::default,
    workbuddy::list,
    workbuddy::load,
    workbuddy::inject,
    workbuddy::overwrite,
    workbuddy::delete,
    workbuddy::destroy,
    workbuddy::active,
    |paths: &workbuddy::Paths| vec![paths.db.clone(), paths.projects.clone()]
);
adapter_boilerplate!(
    ZCodeAdapter,
    zcode::Paths::default,
    zcode::list,
    zcode::load,
    zcode::inject,
    zcode::overwrite,
    zcode::delete,
    zcode::destroy,
    zcode::active,
    |paths: &zcode::Paths| vec![paths.db.clone(), paths.tasks_db.clone()]
);
adapter_boilerplate!(
    ClaudeAdapter,
    claude::Paths::default,
    claude::list,
    claude::load,
    claude::inject,
    claude::overwrite,
    claude::delete,
    claude::delete,
    claude::active,
    |paths: &claude::Paths| vec![paths.projects.clone()]
);
adapter_boilerplate!(
    DshAdapter,
    dsh::Paths::default,
    dsh::list,
    dsh::load,
    dsh::inject,
    dsh::overwrite,
    dsh::delete,
    dsh::destroy,
    dsh::active,
    |paths: &dsh::Paths| vec![paths.sessions.clone(), paths.storages.clone()]
);
adapter_boilerplate!(
    QoderAdapter,
    qoder::Paths::default,
    qoder::list,
    qoder::load,
    qoder::inject,
    qoder::overwrite,
    qoder::delete,
    qoder::destroy,
    qoder::active,
    |paths: &qoder::Paths| vec![paths.main_db.clone(), paths.projects.clone()]
);

/// 转接执行层测试用的进程内注册表。生产构建没有内置 agent，测试直接
/// 注册插件 lib 的薄适配器；存储实现仍来自对应插件包。
pub fn register_transfer_adapters() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let entries = [
            AgentInfo::external(
                "workbuddy",
                "WorkBuddy",
                "WorkBuddy.exe",
                "workbuddy",
                Vec::<u8>::new(),
                true,
                Arc::new(WorkBuddyAdapter),
            ),
            AgentInfo::external(
                "zcode",
                "ZCode",
                "ZCode.exe",
                "zcode",
                Vec::<u8>::new(),
                true,
                Arc::new(ZCodeAdapter),
            ),
            AgentInfo::external(
                "claude",
                "Claude Code",
                "claude.exe",
                "claude",
                Vec::<u8>::new(),
                true,
                Arc::new(ClaudeAdapter),
            ),
            AgentInfo::external(
                "dsh",
                "DeepSeek Harness",
                "DeepSeek Harness.exe",
                "deepseek-harness",
                Vec::<u8>::new(),
                true,
                Arc::new(DshAdapter),
            ),
            AgentInfo::external(
                "qoder",
                "Qoder",
                "Qoder CN.exe",
                "qoder-cn",
                Vec::<u8>::new(),
                true,
                Arc::new(QoderAdapter),
            ),
        ];
        for info in entries {
            let _ = agents::register_external(info);
        }
    });
}
