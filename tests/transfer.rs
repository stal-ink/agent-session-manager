//! 转接执行层集成测试：台账判定、进程检查闸门、备份回滚与台账自愈。
//! 全部走 EnvGuard 重定向的临时目录，不触碰真实 agent 存储。
//! 进程探测桩是进程级全局，与 EnvGuard 共用一把锁串行化。

mod common;

use agent_session_manager::transfer::{self, Mode, Outcome, ProcessProbe, Request};
use rusqlite::Connection;

/// 持有期间：环境变量已重定向、进程探测桩已注入；
/// Drop 时先恢复探测桩、再解锁环境变量，保证下一个测试拿到干净的桩。
struct TransferEnv {
    probe_set: bool,
    _env: Option<common::EnvGuard>,
    wb_home: std::path::PathBuf,
    zc_db: std::path::PathBuf,
    zc_tasks: std::path::PathBuf,
}

impl Drop for TransferEnv {
    fn drop(&mut self) {
        if self.probe_set {
            transfer::set_process_probe(None);
        }
        self._env = None;
    }
}

fn request(source: &str, id: &str, target: &str, cwd: &str) -> Request {
    Request {
        source: source.into(),
        target: target.into(),
        source_id: id.into(),
        target_cwd: cwd.into(),
        mode: Mode::Native,
    }
}

/// WorkBuddy 源 + ZCode 目标（USERPROFILE 真实布局），返回目标库路径。
fn setup_env(root: &std::path::Path, probe: Option<ProcessProbe>) -> (TransferEnv, String) {
    common::register_transfer_adapters();
    let (wb_paths, id) = common::workbuddy_fixture(&root.join("wb"));
    let user_root = root.join("user");
    let (zc_paths, _) = common::zcode_default_fixture(&user_root);
    let data_root = root.join("appdata");
    let env = common::EnvGuard::new(&[
        ("WORKBUDDY_CONFIG_DIR", wb_paths.home.to_str().unwrap()),
        ("USERPROFILE", user_root.to_str().unwrap()),
        ("LOCALAPPDATA", data_root.to_str().unwrap()),
    ]);
    if probe.is_some() {
        transfer::set_process_probe(probe);
    }
    (
        TransferEnv {
            probe_set: probe.is_some(),
            _env: Some(env),
            wb_home: wb_paths.home,
            zc_db: zc_paths.db,
            zc_tasks: zc_paths.tasks_db,
        },
        id,
    )
}

#[test]
fn execute_injects_then_already_then_updates() {
    let tmp = common::TempDir::new();
    let (env, id) = setup_env(&tmp.0, Some(|_| Ok(false)));

    // 第一转：注入。
    let target_id = match transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")) {
        Ok(Outcome::Injected { target_id }) => target_id,
        other => panic!("expected Injected, got {other:?}"),
    };
    let target_id = target_id.as_str();

    // 台账命中且源未更新：已转接。
    match transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")) {
        Ok(Outcome::Already { target_id: again }) => assert_eq!(again, target_id),
        other => panic!("expected Already, got {other:?}"),
    }

    // 源更新后重转：覆盖，并清除目标端归档。
    let db = Connection::open(env.wb_home.join("workbuddy.db")).unwrap();
    db.execute(
        "UPDATE sessions SET updated_at = 99999, title = '更新标题' WHERE id = ?1",
        [&id],
    )
    .unwrap();
    let tasks = Connection::open(&env.zc_tasks).unwrap();
    tasks
        .execute(
            "UPDATE tasks SET archived = 1 WHERE task_id = ?1",
            [target_id],
        )
        .unwrap();

    match transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")) {
        Ok(Outcome::Updated { target_id: again }) => assert_eq!(again, target_id),
        other => panic!("expected Updated, got {other:?}"),
    }
    let archived: i64 = tasks
        .query_row(
            "SELECT archived FROM tasks WHERE task_id = ?1",
            [target_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 0, "覆盖后目标必须恢复活跃");
    let title: String = tasks
        .query_row(
            "SELECT title FROM tasks WHERE task_id = ?1",
            [target_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "更新标题");
}

#[test]
fn execute_rejects_write_when_target_process_running() {
    let tmp = common::TempDir::new();
    let (env, id) = setup_env(&tmp.0, Some(|_| Ok(true)));

    let before = std::fs::read(&env.zc_db).unwrap();
    let error = transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")).unwrap_err();
    assert!(
        error.downcast_ref::<transfer::TargetRunning>().is_some(),
        "必须是 TargetRunning：{error}"
    );
    assert_eq!(
        std::fs::read(&env.zc_db).unwrap(),
        before,
        "目标运行中时目标库必须字节级不变"
    );
    let backups = tmp
        .0
        .join("appdata")
        .join("agent-session-manager")
        .join("backups");
    assert!(
        !backups.exists() || std::fs::read_dir(&backups).unwrap().next().is_none(),
        "拒绝写入时不应产生备份"
    );
}

#[test]
fn execute_aborts_when_process_probe_fails() {
    let tmp = common::TempDir::new();
    let (_env, id) = setup_env(&tmp.0, Some(|_| Err(anyhow::anyhow!("tasklist 检测失败"))));

    let error = transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")).unwrap_err();
    assert!(
        error.to_string().contains("无法确认"),
        "探测失败必须中止转接：{error}"
    );
}

#[test]
fn execute_rolls_back_target_when_writer_fails() {
    let tmp = common::TempDir::new();
    let (env, id) = setup_env(&tmp.0, Some(|_| Ok(false)));

    let target_id = match transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")) {
        Ok(Outcome::Injected { target_id }) => target_id,
        other => panic!("expected Injected, got {other:?}"),
    };

    // 损坏任务索引库：ZCode 写入是两段提交，第二段打开 tasks 库时失败。
    std::fs::write(&env.zc_tasks, b"corrupted not a sqlite file").unwrap();
    let db = Connection::open(env.wb_home.join("workbuddy.db")).unwrap();
    db.execute(
        "UPDATE sessions SET updated_at = 99999 WHERE id = ?1",
        [&id],
    )
    .unwrap();

    let error = transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")).unwrap_err();
    assert!(
        error.to_string().contains("已回滚"),
        "写入失败必须自动回滚：{error}"
    );

    // 主库恢复到首次注入后的状态：会话还在，且没有第二次写入的痕迹。
    let session = agent_plugin_zcode::load(
        &agent_plugin_zcode::Paths {
            db: env.zc_db.clone(),
            tasks_db: env.zc_tasks.clone(),
        },
        &target_id,
    )
    .unwrap();
    assert_eq!(session.title, "WorkBuddy 测试");
    // tasks 库保持损坏（备份恢复的是损坏时刻的字节）。
    let raw = std::fs::read(&env.zc_tasks).unwrap();
    assert_eq!(raw, b"corrupted not a sqlite file");
}

#[test]
fn corrupt_ledger_is_preserved_and_rebuilt() {
    let tmp = common::TempDir::new();
    let (env, id) = setup_env(&tmp.0, Some(|_| Ok(false)));

    let data_dir = tmp.0.join("appdata").join("agent-session-manager");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("ledger.json"), "{not json").unwrap();

    transfer::execute(&request("workbuddy", &id, "zcode", r"D:\test")).unwrap();
    drop(env);

    // 损坏台账保留现场，新台账只含本次转接，不会把旧映射带进未知状态。
    let corrupt: Vec<_> = std::fs::read_dir(&data_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("ledger.json.corrupt-"))
        .collect();
    assert_eq!(corrupt.len(), 1);
    let ledger: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data_dir.join("ledger.json")).unwrap())
            .unwrap();
    assert_eq!(ledger.as_array().unwrap().len(), 1);
}

/// 只需重定向环境与探测桩的测试守卫；Drop 时先清桩再放环境锁。
struct RunEnv {
    _env: Option<common::EnvGuard>,
}

impl Drop for RunEnv {
    fn drop(&mut self) {
        transfer::set_process_probe(None);
        self._env = None;
    }
}

#[test]
fn claude_to_dsh_native_transfer() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let (claude_paths, id) = common::claude_fixture(&tmp.0.join("claude-src"));
    let (dsh_paths, _, _) = common::dsh_fixture(&tmp.0.join("dsh-dst"));
    let data = tmp.0.join("appdata");
    let _env = RunEnv {
        _env: Some(common::EnvGuard::new(&[
            ("CLAUDE_CONFIG_DIR", claude_paths.home.to_str().unwrap()),
            ("DSH_HOME", dsh_paths.home.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
        ])),
    };
    transfer::set_process_probe(Some(|_| Ok(false)));

    let target_id = match transfer::execute(&request("claude", &id, "dsh", r"D:\test")) {
        Ok(Outcome::Injected { target_id }) => target_id,
        other => panic!("expected Injected, got {other:?}"),
    };
    let back = agent_plugin_dsh::load(&dsh_paths, &target_id).unwrap();
    assert_eq!(back.title, "列出文件");
    assert_eq!(back.messages.len(), 3);
    match &back.messages[1].blocks[1] {
        agent_session_manager::ir::Block::ToolCall { output, .. } => {
            assert_eq!(output.as_deref(), Some("a.txt"))
        }
        b => panic!("expected tool call, got {b:?}"),
    }

    // 新会话挂到同路径工作区的候选列表首位。
    let workspace: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dsh_paths.storages.join("workspace.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        workspace["tables"]["workspaces"]["ws-test"]["sessionIds"][0],
        serde_json::Value::String(target_id.clone())
    );

    match transfer::execute(&request("claude", &id, "dsh", r"D:\test")) {
        Ok(Outcome::Already { target_id: again }) => assert_eq!(again, target_id),
        other => panic!("expected Already, got {other:?}"),
    }
}
