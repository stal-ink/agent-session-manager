use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "asm-plugin-workbuddy-{name}-{}",
            std::process::id()
        ));
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

fn spawn(home: &Path, appdata: &Path) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-plugin-workbuddy"))
        .env("USERPROFILE", home)
        .env("APPDATA", appdata)
        .env_remove("WORKBUDDY_CONFIG_DIR")
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
fn plugin_registers_describes_and_rejects_unknown_method() {
    let temp = TempDir::new("protocol");
    let home = temp.0.join("home");
    let appdata = temp.0.join("appdata");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&appdata).unwrap();

    let (mut child, mut stdin, mut stdout) = spawn(&home, &appdata);

    let registration = receive(&mut stdout);
    assert_eq!(registration["method"], "plugin.register");
    assert_eq!(registration["params"]["id"], "workbuddy");
    assert_eq!(registration["params"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(registration["params"]["protocol"], 1);
    assert_eq!(registration["params"]["ir_version"], 1);
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
    assert_eq!(described["result"]["id"], "workbuddy");
    assert_eq!(described["result"]["label"], "WorkBuddy");
    assert_eq!(described["result"]["process"], "WorkBuddy.exe");
    assert_eq!(described["result"]["launch"], "workbuddy");
    assert_eq!(described["result"]["can_write"], true);
    assert_eq!(described["result"]["version"], env!("CARGO_PKG_VERSION"));

    // 备份范围跟随重定向后的 USERPROFILE，不接触真实存储。
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "agent.backup_paths",
            "params": {}
        }),
    );
    let backups = receive(&mut stdout);
    let expected = serde_json::json!({
        "paths": [
            home.join(".workbuddy").join("workbuddy.db").to_string_lossy(),
            home.join(".workbuddy").join("projects").to_string_lossy(),
        ]
    });
    assert_eq!(backups["result"], expected);

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
