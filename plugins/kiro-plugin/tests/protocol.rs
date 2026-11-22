use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-kiro-{name}-{}", std::process::id()));
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

fn seed_kiro(home: &Path) -> String {
    let id = "sess_plugin".to_string();
    let dir = home
        .join(".kiro")
        .join("sessions")
        .join("workspace-hash")
        .join(&id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        r#"{
  "schemaVersion": "1.0.0",
  "dataModelVersion": 1,
  "id": "sess_plugin",
  "title": "插件会话",
  "workspacePaths": ["D:\\plugin"],
  "rootPaths": ["D:\\plugin"],
  "createdAt": "2026-11-01T10:00:00.000Z",
  "lastModifiedAt": "2026-11-01T10:05:00.000Z",
  "modelId": "auto",
  "status": "idle"
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("messages.jsonl"),
        r#"{"id":"user-1","timestamp":"2026-11-01T10:00:00.000Z","payload":{"type":"user","content":"你好","documents":[]}}
"#,
    )
    .unwrap();
    id
}

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-kiro"))
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
    let id = seed_kiro(&home);

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "kiro");
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
    assert_eq!(described["result"]["id"], "kiro");
    assert_eq!(described["result"]["label"], "Kiro");
    assert_eq!(described["result"]["process"], "Kiro.exe");

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
    assert_eq!(sessions[0]["cwd"], r"D:\plugin");
    assert_eq!(sessions[0]["message_count"], 1);

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
