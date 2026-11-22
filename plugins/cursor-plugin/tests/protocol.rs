use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rusqlite::Connection;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-cursor-{name}-{}", std::process::id()));
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

fn seed_cursor(appdata: &Path) -> String {
    let user = appdata.join("Cursor").join("User");
    let workspace = user.join("workspaceStorage").join("ws1");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///d%3A/plugin"}"#,
    )
    .unwrap();

    let global = user.join("globalStorage");
    fs::create_dir_all(&global).unwrap();
    let db = Connection::open(global.join("state.vscdb")).unwrap();
    db.execute_batch(
        "CREATE TABLE composerHeaders (
            composerId TEXT PRIMARY KEY, workspaceId TEXT,
            createdAt INTEGER, lastUpdatedAt INTEGER,
            isArchived INTEGER NOT NULL DEFAULT 0,
            isSubagent INTEGER NOT NULL DEFAULT 0,
            recency INTEGER, checkpointAt INTEGER,
            value TEXT, subagentTypeName TEXT
         );
         CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    let id = "plugin-session".to_string();
    db.execute(
        "INSERT INTO composerHeaders
         (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, value)
         VALUES (?1, 'ws1', 1000, 2000, 0, 0, ?2)",
        rusqlite::params![
            id,
            r#"{"type":"head","name":"插件会话","subtitle":"插件会话"}"#
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
        rusqlite::params![
            format!("bubbleId:{id}:bubble-1"),
            r#"{"type":1,"text":"你好","createdAt":"2026-11-01T10:00:00.000Z"}"#
        ],
    )
    .unwrap();
    id
}

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-cursor"))
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
fn plugin_process_registers_and_describes_without_host_library() {
    let temp = TempDir::new("protocol");
    let home = temp.0.join("home");
    let appdata = temp.0.join("appdata");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&appdata).unwrap();
    let id = seed_cursor(&appdata);

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "cursor");
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
    assert_eq!(described["result"]["id"], "cursor");
    assert_eq!(described["result"]["label"], "Cursor");
    assert_eq!(described["result"]["process"], "Cursor.exe");

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
    assert_eq!(sessions[0]["cwd"], "d:\\plugin");

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
