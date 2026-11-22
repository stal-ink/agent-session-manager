use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rusqlite::Connection;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-qoder-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seed_qoder(appdata: &Path) -> String {
    let package = appdata.join("com.qodercn.app.stable");
    fs::create_dir_all(&package).unwrap();
    let db = Connection::open(package.join("main.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE chat_sessions (
            session_id TEXT PRIMARY KEY, title TEXT, cwd TEXT,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER,
            deleted_at INTEGER, workspace_id TEXT,
            extra_json TEXT NOT NULL DEFAULT '{}'
         );
         CREATE TABLE workspaces (
            workspace_id TEXT PRIMARY KEY, name TEXT NOT NULL,
            workspace_source TEXT NOT NULL, root_paths_json TEXT NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            archived INTEGER NOT NULL DEFAULT 0, deleted_at INTEGER
         );
         CREATE TABLE chat_session_sidebar_placements (
            session_id TEXT PRIMARY KEY, work_directory TEXT,
            sort_order INTEGER NOT NULL, pinned INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
         );
         CREATE TABLE chat_session_messages (
            session_id TEXT NOT NULL, message_id TEXT NOT NULL, turn_id TEXT,
            sequence INTEGER NOT NULL, payload_json TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'completed', source TEXT NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            role TEXT, payload_schema_version INTEGER,
            has_visible_content INTEGER
         );",
    )
    .unwrap();
    let id = "plugin-session".to_string();
    db.execute(
        "INSERT INTO chat_sessions
         (session_id, title, cwd, created_at, updated_at, archived)
         VALUES (?1, '插件会话', 'D:\\plugin', 1000, 2000, 0)",
        [&id],
    )
    .unwrap();
    id
}

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-qoder"))
        .env("USERPROFILE", home)
        .env("APPDATA", appdata)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, stdout)
}

fn send(stdin: &mut ChildStdin, value: serde_json::Value) {
    writeln!(stdin, "{value}").unwrap();
    stdin.flush().unwrap();
}

fn receive(stdout: &mut BufReader<ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn plugin_process_registers_and_lists_without_host_library() {
    let temp = TempDir::new("protocol");
    let home = temp.0.join("home");
    let appdata = temp.0.join("appdata");
    fs::create_dir_all(home.join(".qoder-cn").join("projects")).unwrap();
    fs::create_dir_all(&appdata).unwrap();
    let id = seed_qoder(&appdata);

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "qoder");
    assert_eq!(registration["params"]["sha256"].as_str().unwrap().len(), 64);
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": { "accepted": true, "generation": 1 }
        }),
    );

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "agent.describe",
            "params": {}
        }),
    );
    let described = receive(&mut stdout);
    assert_eq!(described["result"]["id"], "qoder");
    assert_eq!(described["result"]["label"], "Qoder");

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "agent.list",
            "params": {}
        }),
    );
    let listed = receive(&mut stdout);
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], id);
    assert_eq!(sessions[0]["title"], "插件会话");

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "agent.unknown",
            "params": {}
        }),
    );
    let unknown = receive(&mut stdout);
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(unknown["error"]["data"]["kind"], "unsupported");

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
