use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-trae-{name}-{}", std::process::id()));
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

fn spawn(root: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-trae"))
        .env("TRAE_DATA_DIR", root)
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

fn call(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    send(
        stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }),
    );
    receive(stdout)
}

#[test]
fn plugin_process_registers_and_reports_missing_database() {
    let temp = TempDir::new("protocol");
    let (mut child, mut stdin, mut stdout) = spawn(&temp.0);

    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "trae");
    assert_eq!(registration["params"]["sha256"].as_str().unwrap().len(), 64);
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": { "accepted": true, "generation": 1 }
        }),
    );

    let described = call(
        &mut stdin,
        &mut stdout,
        1,
        "agent.describe",
        serde_json::json!({}),
    );
    assert_eq!(described["result"]["id"], "trae");
    assert_eq!(described["result"]["can_write"], false);

    let listed = call(
        &mut stdin,
        &mut stdout,
        2,
        "agent.list",
        serde_json::json!({}),
    );
    assert!(listed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("数据库不存在"));

    let deleted = call(
        &mut stdin,
        &mut stdout,
        3,
        "agent.delete",
        serde_json::json!({ "id": "missing" }),
    );
    assert!(deleted["error"]["message"]
        .as_str()
        .unwrap()
        .contains("只读"));

    let written = call(
        &mut stdin,
        &mut stdout,
        4,
        "agent.write",
        serde_json::json!({
            "session": {
                "agent": "trae",
                "id": "missing",
                "title": "",
                "cwd": "",
                "created_ms": 0,
                "updated_ms": 0,
                "messages": [],
                "compaction": null
            },
            "cwd": "",
            "overwrite": null
        }),
    );
    assert!(written["error"]["message"]
        .as_str()
        .unwrap()
        .contains("只读"));

    let active = call(
        &mut stdin,
        &mut stdout,
        5,
        "agent.active",
        serde_json::json!({ "id": "missing" }),
    );
    assert_eq!(active["result"]["active"], false);

    let missing = call(
        &mut stdin,
        &mut stdout,
        6,
        "agent.load",
        serde_json::json!({ "id": "missing" }),
    );
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("数据库不存在"));

    let unknown = call(
        &mut stdin,
        &mut stdout,
        7,
        "agent.unknown",
        serde_json::json!({}),
    );
    assert_eq!(unknown["error"]["code"], -32601);

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
