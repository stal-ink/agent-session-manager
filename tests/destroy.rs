//! 会话销毁集成测试：各端物理删除落地、缺失表容忍、失败隔离与进程闸门。
//! 全部走 EnvGuard 重定向的临时目录，不触碰真实 agent 存储。
//! 进程探测桩是进程级全局，与 EnvGuard 共用一把锁串行化。

mod common;

use std::path::Path;

use agent_plugin_claude as claude;
use agent_plugin_dsh as dsh;
use agent_plugin_qoder as qoder;
use agent_plugin_zcode as zcode;
use agent_session_manager::transfer;
use rusqlite::Connection;

/// 持有期间：环境变量已重定向、进程探测桩已注入；
/// Drop 时先恢复探测桩、再解锁环境变量，保证下一个测试拿到干净的桩。
struct DestroyEnv {
    probe_set: bool,
    _env: Option<common::EnvGuard>,
}

impl Drop for DestroyEnv {
    fn drop(&mut self) {
        if self.probe_set {
            transfer::set_process_probe(None);
        }
        self._env = None;
    }
}

/// 环境重定向 + 「所有 agent 未运行」探测桩；用于走 destroy_groups 的测试。
fn env_with_closed_probe(vars: &[(&str, &str)]) -> DestroyEnv {
    let env = DestroyEnv {
        probe_set: true,
        _env: Some(common::EnvGuard::new(vars)),
    };
    transfer::set_process_probe(Some(|_| Ok(false)));
    env
}

/// 按 `qoder::Paths::default()` 的真实布局落位 Qoder 存储：main_db 在 APPDATA
/// 下，projects 在 USERPROFILE 下。empty_qoder 一次只能围绕一个根目录建库，
/// 分别调用一次，各自多出的伴生 projects/main.sqlite 不会被任何代码引用。
fn qoder_default_fixture(appdata: &Path, user_root: &Path, id: &str) -> qoder::Paths {
    let main_paths = common::empty_qoder(&appdata.join("com.qodercn.app.stable"));
    let home_paths = common::empty_qoder(&user_root.join(".qoder-cn"));
    let paths = qoder::Paths {
        projects: home_paths.projects,
        main_db: main_paths.main_db,
    };
    seed_qoder(&paths, id);
    // 正文 jsonl：与 qoder_fixture 相同的 slug 目录布局。
    let slug_dir = paths.projects.join("D--test");
    std::fs::create_dir_all(&slug_dir).unwrap();
    std::fs::write(
        slug_dir.join(format!("{id}.jsonl")),
        "{\"type\":\"user\"}\n",
    )
    .unwrap();
    paths
}

/// 写入一个 Qoder 会话：会话行、消息行与侧栏放置行，覆盖销毁的完整删除范围。
fn seed_qoder(paths: &qoder::Paths, id: &str) {
    let db = Connection::open(&paths.main_db).unwrap();
    db.execute(
        "INSERT INTO chat_sessions (session_id, title, cwd, created_at, updated_at)
         VALUES (?1, 'Qoder 测试', 'D:\\test', 1000, 2000)",
        [&id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO chat_session_messages (session_id, message_id, sequence, payload_json, source, created_at, updated_at)
         VALUES (?1, 'm1', 1, '{}', 'host-projection', 1, 1)",
        [&id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO chat_session_sidebar_placements (session_id, work_directory, sort_order, pinned, updated_at)
         VALUES (?1, 'D:\\test', 1, 0, 2)",
        [&id],
    )
    .unwrap();
}

/// 在 zcode_default_fixture 之上补销毁涉及的各类关联行：
/// session_entry（按 session_id）、todo（按 session_id）、permission（项目级，必须保留）、
/// session_task_link（parent/child 双方向）与 task_group_admissions。
fn seed_zcode_extras(paths: &zcode::Paths, id: &str) {
    let db = Connection::open(&paths.db).unwrap();
    db.execute(
        "INSERT INTO session_entry (id, session_id, type, time_created, time_updated, data)
         VALUES ('e1', ?1, 'message', 1, 1, '{}')",
        [id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO todo (session_id, content, status, time_created, time_updated)
         VALUES (?1, '待办一', 'pending', 1, 1)",
        [id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO permission (project_id, time_created, time_updated, data)
         VALUES ('proj_d--test', 1, 1, '{}')",
        [],
    )
    .unwrap();
    db.execute_batch(&format!(
        "INSERT INTO session_task_link (id, parent_session_id, child_session_id, time_created, time_updated)
         VALUES ('l1', NULL, '{id}', 1, 1);
         INSERT INTO session_task_link (id, parent_session_id, child_session_id, time_created, time_updated)
         VALUES ('l2', '{id}', NULL, 1, 1);
         INSERT INTO session_task_link (id, parent_session_id, child_session_id, time_created, time_updated)
         VALUES ('l3', 'sess_subagent_x', NULL, 1, 1);"
    ))
    .unwrap();
    let tasks = Connection::open(&paths.tasks_db).unwrap();
    tasks
        .execute(
            "INSERT INTO task_group_admissions (workspace_key, task_id, system_kind)
             VALUES ('ws', ?1, 'manual')",
            [id],
        )
        .unwrap();
}

fn count_session_rows(db: &Connection, table: &str, column: &str, id: &str) -> i64 {
    db.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn destroy_qoder_removes_rows_and_jsonl() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let user_root = tmp.0.join("user");
    let appdata = tmp.0.join("appdata");
    let local = tmp.0.join("local");
    let id = "11111111-2222-3333-4444-555555555555".to_string();
    let paths = qoder_default_fixture(&appdata, &user_root, &id);
    let _env = env_with_closed_probe(&[
        ("USERPROFILE", user_root.to_str().unwrap()),
        ("APPDATA", appdata.to_str().unwrap()),
        ("LOCALAPPDATA", local.to_str().unwrap()),
    ]);

    let report = transfer::destroy_groups(&[("qoder".into(), vec![id.clone()])]).unwrap();
    assert!(!report.failed, "{:?}", report.lines);
    assert_eq!(report.lines, vec!["Qoder：1 条已销毁"]);

    let db = Connection::open(&paths.main_db).unwrap();
    for table in [
        "chat_sessions",
        "chat_session_messages",
        "chat_session_sidebar_placements",
    ] {
        assert_eq!(
            count_session_rows(&db, table, "session_id", &id),
            0,
            "{table} 的会话行必须已删除"
        );
    }
    let jsonl = paths.projects.join("D--test").join(format!("{id}.jsonl"));
    assert!(!jsonl.exists(), "正文 jsonl 必须已删除");
}

#[test]
fn destroy_zcode_removes_rows_tasks_and_exec_dir() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let user_root = tmp.0.join("user");
    let local = tmp.0.join("local");
    let (paths, id) = common::zcode_default_fixture(&user_root);
    seed_zcode_extras(&paths, &id);
    // cli/exec/<id>/ 按真实布局预先存在。
    let exec = user_root.join(".zcode").join("cli").join("exec").join(&id);
    std::fs::create_dir_all(&exec).unwrap();
    std::fs::write(exec.join("state.json"), "{}").unwrap();
    let _env = env_with_closed_probe(&[
        ("USERPROFILE", user_root.to_str().unwrap()),
        ("LOCALAPPDATA", local.to_str().unwrap()),
    ]);

    let report = transfer::destroy_groups(&[("zcode".into(), vec![id.clone()])]).unwrap();
    assert!(!report.failed, "{:?}", report.lines);
    assert_eq!(report.lines, vec!["ZCode：1 条已销毁"]);

    let db = Connection::open(&paths.db).unwrap();
    for table in ["message", "part", "session_entry", "todo"] {
        assert_eq!(
            count_session_rows(&db, table, "session_id", &id),
            0,
            "{table} 的会话行必须已删除"
        );
    }
    // parent 与 child 两个方向的任务链都已删除，无关链保留。
    let links: i64 = db
        .query_row("SELECT COUNT(*) FROM session_task_link", [], |r| r.get(0))
        .unwrap();
    assert_eq!(links, 1, "只应保留无关的任务链");
    let unrelated: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM session_task_link WHERE id = 'l3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unrelated, 1, "无关任务链不得被删除");
    // permission 是项目级授权（无 session_id），销毁会话不得删除它。
    let perms: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM permission WHERE project_id = 'proj_d--test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(perms, 1, "项目级 permission 必须保留");
    assert_eq!(
        count_session_rows(&db, "session", "id", &id),
        0,
        "session 行必须已删除"
    );
    // 同库其他会话不受影响（fixture 里还有一个子代理会话）。
    let others: i64 = db
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
        .unwrap();
    assert_eq!(others, 1, "其他会话必须保留");

    let tasks = Connection::open(&paths.tasks_db).unwrap();
    for table in ["tasks", "task_group_admissions"] {
        assert_eq!(
            count_session_rows(&tasks, table, "task_id", &id),
            0,
            "{table} 的索引行必须已删除"
        );
    }
    assert!(!exec.exists(), "cli/exec 目录必须已删除");
}

#[test]
fn destroy_workbuddy_removes_rows_and_jsonl() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let local = tmp.0.join("local");
    let (paths, id) = common::workbuddy_fixture(&tmp.0.join("wb"));
    let _env = env_with_closed_probe(&[
        ("WORKBUDDY_CONFIG_DIR", paths.home.to_str().unwrap()),
        ("LOCALAPPDATA", local.to_str().unwrap()),
    ]);

    let report = transfer::destroy_groups(&[("workbuddy".into(), vec![id.clone()])]).unwrap();
    assert!(!report.failed, "{:?}", report.lines);
    assert_eq!(report.lines, vec!["WorkBuddy：1 条已销毁"]);

    let db = Connection::open(&paths.db).unwrap();
    assert_eq!(
        count_session_rows(&db, "sessions", "id", &id),
        0,
        "sessions 行必须已删除"
    );
    assert_eq!(
        count_session_rows(&db, "session_usage", "session_id", &id),
        0,
        "session_usage 行必须已删除"
    );
    let jsonl = paths.projects.join("d-test").join(format!("{id}.jsonl"));
    assert!(!jsonl.exists(), "正文 jsonl 必须已删除");
}

#[test]
fn destroy_zcode_tolerates_missing_tables() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::zcode_fixture(&tmp.0.join("zcode"));
    // 模拟旧版本库：把建好的非核心表也删掉一张，销毁必须继续容忍。
    let db = Connection::open(&paths.db).unwrap();
    db.execute_batch("DROP TABLE session_entry;").unwrap();
    drop(db);

    zcode::destroy(&paths, &id).unwrap();

    let db = Connection::open(&paths.db).unwrap();
    assert_eq!(count_session_rows(&db, "session", "id", &id), 0);
}

#[test]
fn destroy_one_agent_failure_does_not_block_others() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let user_root = tmp.0.join("user");
    let appdata = tmp.0.join("appdata");
    let local = tmp.0.join("local");
    let qoder_id = "11111111-2222-3333-4444-555555555555".to_string();
    let qoder_paths = qoder_default_fixture(&appdata, &user_root, &qoder_id);
    let (wb_paths, wb_id) = common::workbuddy_fixture(&tmp.0.join("wb"));
    let _env = env_with_closed_probe(&[
        ("USERPROFILE", user_root.to_str().unwrap()),
        ("APPDATA", appdata.to_str().unwrap()),
        ("WORKBUDDY_CONFIG_DIR", wb_paths.home.to_str().unwrap()),
        ("LOCALAPPDATA", local.to_str().unwrap()),
    ]);

    // Qoder 第一个 id 销毁成功，第二个 id 不存在而失败：停止该 agent 剩余 id，
    // 已删的不回滚；另一个 agent 照常销毁。
    let report = transfer::destroy_groups(&[
        (
            "qoder".into(),
            vec![qoder_id.clone(), "sess_missing".to_string()],
        ),
        ("workbuddy".into(), vec![wb_id.clone()]),
    ])
    .unwrap();
    assert!(report.failed, "{:?}", report.lines);
    assert_eq!(report.lines.len(), 2);
    assert!(
        report.lines[0].starts_with("Qoder：1 条已销毁，1 条失败："),
        "{}",
        report.lines[0]
    );
    assert!(
        report.lines[0].contains("会话不存在"),
        "{}",
        report.lines[0]
    );
    assert_eq!(report.lines[1], "WorkBuddy：1 条已销毁");

    let db = Connection::open(&qoder_paths.main_db).unwrap();
    let n: i64 = db
        .query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "已销毁的 Qoder 会话不得回滚");
    let wb = Connection::open(&wb_paths.db).unwrap();
    assert_eq!(
        count_session_rows(&wb, "sessions", "id", &wb_id),
        0,
        "Qoder 失败不得阻断 WorkBuddy 销毁"
    );
    let jsonl = wb_paths
        .projects
        .join("d-test")
        .join(format!("{wb_id}.jsonl"));
    assert!(!jsonl.exists());
}

#[test]
fn destroy_aborts_entirely_when_process_running() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let local = tmp.0.join("local");
    let (paths, id) = common::workbuddy_fixture(&tmp.0.join("wb"));
    let _env = DestroyEnv {
        probe_set: true,
        _env: Some(common::EnvGuard::new(&[
            ("WORKBUDDY_CONFIG_DIR", paths.home.to_str().unwrap()),
            ("LOCALAPPDATA", local.to_str().unwrap()),
        ])),
    };
    transfer::set_process_probe(Some(|_| Ok(true)));

    let error = transfer::destroy_groups(&[("workbuddy".into(), vec![id.clone()])]).unwrap_err();
    assert!(error.to_string().contains("正在运行"), "{error}");

    // 整体不删：数据库行与正文都保持原样，且不产生备份。
    let db = Connection::open(&paths.db).unwrap();
    assert_eq!(count_session_rows(&db, "sessions", "id", &id), 1);
    assert!(paths
        .projects
        .join("d-test")
        .join(format!("{id}.jsonl"))
        .exists());
    let backups = local.join("agent-session-manager").join("backups");
    assert!(
        !backups.exists() || std::fs::read_dir(&backups).unwrap().next().is_none(),
        "拒绝销毁时不应产生备份"
    );
}

#[test]
fn destroy_aborts_when_probe_fails() {
    common::register_transfer_adapters();
    let tmp = common::TempDir::new();
    let local = tmp.0.join("local");
    let (paths, id) = common::workbuddy_fixture(&tmp.0.join("wb"));
    let _env = DestroyEnv {
        probe_set: true,
        _env: Some(common::EnvGuard::new(&[
            ("WORKBUDDY_CONFIG_DIR", paths.home.to_str().unwrap()),
            ("LOCALAPPDATA", local.to_str().unwrap()),
        ])),
    };
    transfer::set_process_probe(Some(|_| Err(anyhow::anyhow!("tasklist 检测失败"))));

    let error = transfer::destroy_groups(&[("workbuddy".into(), vec![id.clone()])]).unwrap_err();
    assert!(error.to_string().contains("无法确认"), "{error}");
    let db = Connection::open(&paths.db).unwrap();
    assert_eq!(
        count_session_rows(&db, "sessions", "id", &id),
        1,
        "探测失败必须整体不删"
    );
}

#[test]
fn destroy_groups_rejects_empty_selection() {
    common::register_transfer_adapters();
    let error = transfer::destroy_groups(&[]).unwrap_err();
    assert!(error.to_string().contains("没有选择会话"), "{error}");
}

#[test]
fn destroy_claude_removes_jsonl() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::claude_fixture(&tmp.0);
    let file = paths.projects.join("d--test").join(format!("{id}.jsonl"));
    assert!(file.exists());

    claude::delete(&paths, &id).unwrap();
    assert!(!file.exists(), "Claude Code 正文 jsonl 必须已删除");
    assert!(claude::list(&paths).unwrap().is_empty());
    assert!(!claude::active(&paths, &id));
}

#[test]
fn destroy_dsh_removes_session_dir_cache_and_indexes() {
    let tmp = common::TempDir::new();
    let (paths, id, dir) = common::dsh_fixture(&tmp.0);

    // 软删除只写归档集合，日志保留。
    dsh::delete(&paths, &id).unwrap();
    assert!(dir.exists());
    assert!(!dsh::active(&paths, &id));
    assert!(dsh::list(&paths).unwrap()[0].archived);

    dsh::destroy(&paths, &id).unwrap();
    assert!(!dir.exists(), "DSH 会话目录必须已删除");
    let cache = paths
        .storages
        .join("session_projcache")
        .join("sessions")
        .join(format!("{id}.json"));
    assert!(!cache.exists(), "投影缓存文件必须已删除");

    let aggregate: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(paths.storages.join("session_projcache.json")).unwrap(),
    )
    .unwrap();
    assert!(aggregate["tables"]["sessions"].get(&id).is_none());

    let workspace: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(paths.storages.join("workspace.json")).unwrap(),
    )
    .unwrap();
    assert!(!workspace["global"]["archivedSessionIds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some(&id)));
    assert!(!workspace["tables"]["workspaces"]["ws-test"]["sessionIds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some(&id)));

    let feedback: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(paths.storages.join("message_feedback.json")).unwrap(),
    )
    .unwrap();
    assert!(feedback["tables"]["sessions"].get(&id).is_none());
}
