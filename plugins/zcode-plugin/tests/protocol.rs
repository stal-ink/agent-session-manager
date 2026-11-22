use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rusqlite::Connection;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-zcode-{name}-{}", std::process::id()));
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

/// 只保留 `list` 需要的列，按真实布局放在 `USERPROFILE/.zcode` 下。
fn seed_zcode(home: &Path) -> String {
    let cli = home.join(".zcode").join("cli").join("db");
    let v2 = home.join(".zcode").join("v2");
    fs::create_dir_all(&cli).unwrap();
    fs::create_dir_all(&v2).unwrap();
    let db = Connection::open(cli.join("db.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY, title TEXT NOT NULL, directory TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
         );
         CREATE TABLE message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
            data TEXT NOT NULL, sequence INTEGER
         );",
    )
    .unwrap();
    let id = "sess_01234567-89ab-cdef-0123-456789abcdef".to_string();
    db.execute(
        "INSERT INTO session (id, title, directory, time_created, time_updated)
         VALUES (?1, '插件会话', 'D:\\plugin', 1000, 2000)",
        [&id],
    )
    .unwrap();
    for (mid, seq) in [("msg_1", 1), ("msg_2", 2)] {
        db.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data, sequence)
             VALUES (?1, ?2, 1, 1, '{}', ?3)",
            rusqlite::params![mid, id, seq],
        )
        .unwrap();
    }

    let tasks = Connection::open(v2.join("tasks-index.sqlite")).unwrap();
    tasks
        .execute_batch(
            "CREATE TABLE tasks (
                task_id TEXT PRIMARY KEY, archived INTEGER NOT NULL DEFAULT 0,
                deleted INTEGER NOT NULL DEFAULT 0, cli_deleted INTEGER,
                project_id TEXT
             );
             CREATE TABLE projects (
                project_id TEXT PRIMARY KEY, deleted INTEGER NOT NULL DEFAULT 0,
                deleted_at INTEGER
             );",
        )
        .unwrap();
    tasks
        .execute(
            "INSERT INTO tasks (task_id, archived, deleted, cli_deleted, project_id)
             VALUES (?1, 0, 0, 0, NULL)",
            [&id],
        )
        .unwrap();
    id
}

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-zcode"))
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
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&appdata).unwrap();
    let id = seed_zcode(&home);

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "zcode");
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
    assert_eq!(described["result"]["id"], "zcode");
    assert_eq!(described["result"]["label"], "ZCode");

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
    assert_eq!(sessions[0]["message_count"], 2);

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
