use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use agent_core::ir::{Block, Message, Role, Session, SessionSummary};
use agent_core::protocol::{
    PluginRegistration, IR_VERSION, JSONRPC, PROTOCOL_VERSION, REGISTER_ID,
};
use agent_session_manager::agents::AgentAdapter;
use agent_session_manager::plugins::adapter::ProcessPluginAdapter;
use agent_session_manager::plugins::manifest::{sha256_file, PluginManifest};
use agent_session_manager::plugins::process::PluginManager;
use serde_json::{json, Value};

const FAKE_ID: &str = "fake";
const FAKE_VERSION: &str = "0.1.0";

fn main() {
    if std::env::var_os("ASM_FAKE_PLUGIN").is_some() {
        run_fake_plugin();
        return;
    }
    run_test();
}

fn run_fake_plugin() {
    let executable = std::env::current_exe().unwrap();
    let sha256 = sha256_file(&executable).unwrap();
    let registration = PluginRegistration {
        schema: 2,
        id: FAKE_ID.into(),
        version: FAKE_VERSION.into(),
        protocol: PROTOCOL_VERSION,
        ir_version: IR_VERSION,
        sha256,
    };
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    write_line(
        &mut output,
        &json!({
            "jsonrpc": JSONRPC,
            "id": REGISTER_ID,
            "method": "plugin.register",
            "params": registration,
        }),
    );

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let accepted = lines
        .next()
        .and_then(std::result::Result::ok)
        .and_then(|line| serde_json::from_str::<Value>(&line).ok())
        .and_then(|value| value["result"]["accepted"].as_bool())
        .unwrap_or(false);
    if !accepted {
        std::process::exit(1);
    }

    for line in lines.map_while(std::result::Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = request["id"].as_u64().unwrap_or_default();
        let method = request["method"].as_str().unwrap_or_default();
        let response = match method {
            "agent.describe" => json!({
                "jsonrpc": JSONRPC,
                "id": id,
                "result": {
                    "schema": 2,
                    "id": FAKE_ID,
                    "label": "Fake Agent",
                    "version": FAKE_VERSION,
                    "process": "fake.exe",
                    "launch": "fake",
                    "can_write": true,
                    "requires_exclusive": true,
                    "restore_on_error": true,
                },
            }),
            "agent.list" => success(
                id,
                json!({
                    "sessions": [summary()],
                    "warnings": [{
                        "kind": "parse_skipped",
                        "message": "跳过损坏会话 fake-bad",
                        "session_id": "fake-bad",
                    }],
                }),
            ),
            "agent.load" => {
                let id_value = request["params"]["id"].as_str().unwrap_or("fake-session");
                if id_value == "broken" {
                    json!({
                        "jsonrpc": JSONRPC,
                        "id": id,
                        "error": {
                            "code": -32603,
                            "message": "密钥获取失败：网络不可用",
                            "data": { "kind": "internal" },
                        },
                    })
                } else {
                    let session = session(id_value);
                    success(id, json!({ "session": session }))
                }
            }
            "agent.write" => success(id, json!({ "target_id": "fake-target" })),
            "agent.active" => success(id, json!({ "active": true })),
            "agent.backup_paths" => success(id, json!({ "paths": [] })),
            "agent.needs_rewrite" => success(id, json!({ "needs_rewrite": false })),
            "agent.open_session_ids" => success(id, json!({ "ids": null })),
            "agent.delete" | "agent.destroy" | "agent.prepare_open" | "agent.ensure_closable" => {
                success(id, json!({}))
            }
            _ => json!({
                "jsonrpc": JSONRPC,
                "id": id,
                "error": {
                    "code": -32601,
                    "message": "unsupported",
                    "data": { "kind": "unsupported" },
                },
            }),
        };
        write_line(&mut output, &response);
    }
}

fn success(id: u64, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result })
}

fn summary() -> SessionSummary {
    SessionSummary {
        agent: FAKE_ID.into(),
        id: "fake-session".into(),
        title: "Fake Session".into(),
        cwd: "C:\\fake".into(),
        created_ms: 1000,
        updated_ms: 2000,
        message_count: 1,
        archived: false,
        deleted: false,
        workspace_deleted: false,
    }
}

fn session(id: &str) -> Session {
    Session {
        agent: FAKE_ID.into(),
        id: id.into(),
        title: "Fake Session".into(),
        cwd: "C:\\fake".into(),
        created_ms: 1000,
        updated_ms: 2000,
        messages: vec![Message {
            role: Role::User,
            time_ms: 2000,
            blocks: vec![Block::Text("hello".into())],
        }],
        compaction: None,
    }
}

fn write_line(output: &mut impl Write, value: &Value) {
    writeln!(output, "{value}").unwrap();
    output.flush().unwrap();
}

fn run_test() {
    let temp = temp_dir();
    let manager = PluginManager::new(temp.join("plugins")).unwrap();
    let package = manager.root().join(FAKE_ID).join(FAKE_VERSION);
    fs::create_dir_all(package.join("bin")).unwrap();
    let executable = package.join("bin/fake.exe");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let icon = image::RgbaImage::from_pixel(512, 512, image::Rgba([17, 24, 39, 255]));
    icon.save(package.join("icon.png")).unwrap();
    let sha256 = sha256_file(&executable).unwrap();
    fs::write(
        package.join("plugin.toml"),
        format!(
            r#"schema = 2
id = "{FAKE_ID}"
label = "Fake Agent"
version = "{FAKE_VERSION}"
protocol = 1
ir_version = 1
min_host = "0.17.0"
process = "fake.exe"
launch = "fake"
icon = "icon.png"
can_write = true
sha256 = "{sha256}"

[execution]
mode = "ephemeral"
command = "bin/fake.exe"
timeout_ms = 5000

[capabilities]
list = true
load = true
write = true
delete = true
destroy = true
active = true
backup_paths = true

[safety]
requires_exclusive = true
restore_on_error = true

[[variants]]
id = "fake-desktop"
label = "Fake Agent"
kind = "desktop"
default_open = true

[variants.open]
strategy = "command"
command = "fake"
args = []
"#
        ),
    )
    .unwrap();

    let manifest = PluginManifest::load(&package).unwrap();
    let adapter = ProcessPluginAdapter::new(
        manager.clone(),
        package,
        manifest,
        vec![("ASM_FAKE_PLUGIN".into(), "1".into())],
    );

    let listing = adapter.list_with_warnings().unwrap();
    assert_eq!(listing.sessions.len(), 1);
    assert_eq!(listing.sessions[0].id, "fake-session");
    assert_eq!(listing.warnings.len(), 1);
    assert_eq!(listing.warnings[0].kind, "parse_skipped");
    assert_eq!(listing.warnings[0].session_id.as_deref(), Some("fake-bad"));
    assert_eq!(adapter.list().unwrap().len(), 1);
    let loaded = adapter.load("fake-session").unwrap();
    assert_eq!(loaded.title, "Fake Session");
    // 适配器保留完整错误链，UI 负责展示；不能只留下最外层调用上下文。
    let error = format!("{:#}", adapter.load("broken").unwrap_err());
    assert!(error.contains("插件 fake 调用 agent.load 失败"));
    assert!(error.contains("密钥获取失败：网络不可用"));
    assert!(adapter.active("fake-session").unwrap());
    assert!(adapter.backup_paths().unwrap().is_empty());

    assert_eq!(manager.active_count(), 0);
    manager.shutdown().unwrap();
    let _ = fs::remove_dir_all(temp);
}

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("asm-fake-plugin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}
