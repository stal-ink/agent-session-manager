use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("asm-plugin-kimi-{name}-{}", std::process::id()));
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

fn seed_kimi(home: &Path) -> String {
    let id = "session_01234567-89ab-cdef-0123-456789abcdef".to_string();
    let dir = home
        .join(".kimi-code")
        .join("sessions")
        .join("wd_plugin_000000000000")
        .join(&id);
    fs::create_dir_all(dir.join("agents").join("main")).unwrap();
    fs::write(
        dir.join("state.json"),
        serde_json::json!({
            "id": id,
            "version": 2,
            "cwd": "D:/plugin",
            "archived": false,
            "title": "插件会话",
            "titleKind": "custom",
            "isCustomTitle": true,
            "lastPrompt": "插件",
            "lastTurnReason": "completed",
            "createdAt": 1000,
            "updatedAt": 2000
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("agents").join("main").join("wire.jsonl"),
        r#"{"type":"metadata","protocol_version":"1.5","min_protocol_version":"1.5","created_at":1000}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"插件"}],"id":"m1","origin":{"kind":"user"}},"time":1100}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.begin","uuid":"s1","turnId":"0","step":1},"time":1200}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"p1","turnId":"0","step":1,"stepUuid":"s1","block":0,"part":{"type":"text","text":"回答"},"startedAt":1200,"durationMs":0},"time":1200}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","uuid":"s1","turnId":"0","step":1,"finishReason":"end_turn"},"time":1300}"#,
    )
    .unwrap();
    id
}

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-kimi"))
        .env("USERPROFILE", home)
        .env("APPDATA", appdata)
        .env_remove("KIMI_CODE_HOME")
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
    let id = seed_kimi(&home);

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);
    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "kimi");
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
    assert_eq!(described["result"]["id"], "kimi");
    assert_eq!(described["result"]["label"], "Kimi Code");

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
