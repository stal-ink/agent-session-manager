use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-claude-{name}-{}", std::process::id()));
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

fn seed_claude(home: &Path) -> String {
    let project = home.join("projects").join("d--plugin");
    fs::create_dir_all(&project).unwrap();
    let id = "plugin-session".to_string();
    fs::write(
        project.join(format!("{id}.jsonl")),
        r#"{"type":"user","uuid":"u1","timestamp":"2026-11-01T10:00:00.000Z","cwd":"D:\\plugin","message":{"role":"user","content":[{"type":"text","text":"插件会话"}]}}
"#,
    )
    .unwrap();
    id
}

fn spawn(home: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-claude"))
        .env("CLAUDE_CONFIG_DIR", home)
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
    let home = temp.0.join("claude");
    fs::create_dir_all(&home).unwrap();
    let id = seed_claude(&home);

    let (mut child, mut stdin, mut stdout) = spawn(&home);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "claude");
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
            "id": 2,
            "method": "agent.active",
            "params": { "id": id }
        }),
    );
    assert_eq!(receive(&mut stdout)["result"]["active"], true);

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
