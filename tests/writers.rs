mod common;

use agent_plugin_claude as claude;
use agent_plugin_cursor as cursor;
use agent_plugin_dsh as dsh;
use agent_plugin_kiro as kiro;
use agent_plugin_qoder as qoder;
use agent_plugin_workbuddy as workbuddy;
use agent_plugin_zcode as zcode;
use agent_session_manager::ir::{Block, Compaction, Message, Role, Session};
use serde_json::{json, Value};

#[test]
fn qoder_to_zcode_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));

    let new_id = zcode::inject(&dst, &session, "D:\\test").unwrap();
    assert!(new_id.starts_with("sess_"));

    let back = zcode::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "zcode");
    assert_eq!(back.title, "测试会话");
    assert_eq!(back.messages.len(), 3);
    match &back.messages[1].blocks[1] {
        Block::ToolCall { id, output, .. } => {
            assert_eq!(id, "call_1");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        b => panic!("expected tool call, got {b:?}"),
    }

    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    let n: i64 = tasks
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let project_id: String = tasks
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    let root: String = tasks
        .query_row(
            "SELECT root_path FROM project_roots WHERE project_id = ?1 AND root_path = ?2",
            [&project_id, "D:\\test"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(root, "D:\\test");
    let admitted: i64 = tasks
        .query_row(
            "SELECT COUNT(*) FROM task_group_admissions WHERE task_id = ?1 AND user_organized = 1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(admitted, 1);
}

#[test]
fn zcode_overwrite_replaces_existing_session() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let mut session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));

    let new_id = zcode::inject(&dst, &session, "D:\\test").unwrap();
    session.title = "更新后的标题".into();
    let again = zcode::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    assert_eq!(again, new_id);

    let list = zcode::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
    let back = zcode::load(&dst, &new_id).unwrap();
    assert_eq!(back.messages.len(), 3);

    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    let rows: i64 = tasks
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    let title: String = tasks
        .query_row(
            "SELECT title FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "更新后的标题");
}

#[test]
fn qoder_overwrite_replaces_existing_session() {
    let tmp = common::TempDir::new();
    let (src, id) = common::zcode_fixture(&tmp.0.join("src"));
    let mut session = zcode::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));

    let new_id = qoder::inject(&dst, &session, "D:\\test").unwrap();
    session.title = "更新后的标题".into();
    let again = qoder::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    assert_eq!(again, new_id);

    let list = qoder::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
    let back = qoder::load(&dst, &new_id).unwrap();
    assert_eq!(back.messages.len(), 2);

    let mut jsonl = 0;
    for entry in std::fs::read_dir(&dst.projects).unwrap() {
        if entry
            .unwrap()
            .path()
            .join(format!("{new_id}.jsonl"))
            .exists()
        {
            jsonl += 1;
        }
    }
    assert_eq!(jsonl, 1);
}

#[test]
fn zcode_overwrite_reactivates_target() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));

    let new_id = zcode::inject(&dst, &session, "D:\\test").unwrap();
    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    tasks
        .execute(
            "UPDATE tasks SET archived = 1, deleted = 1, cli_deleted = 1 WHERE task_id = ?1",
            [&new_id],
        )
        .unwrap();
    tasks
        .execute(
            "UPDATE projects SET deleted = 1, deleted_at = 123
             WHERE project_id = (SELECT project_id FROM tasks WHERE task_id = ?1)",
            [&new_id],
        )
        .unwrap();

    zcode::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    let list = zcode::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert!(!list[0].archived);
    assert!(!list[0].deleted);
    assert!(!list[0].workspace_deleted);
}

#[test]
fn qoder_overwrite_reactivates_target() {
    let tmp = common::TempDir::new();
    let (src, id) = common::zcode_fixture(&tmp.0.join("src"));
    let session = zcode::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));

    let new_id = qoder::inject(&dst, &session, "D:\\test").unwrap();
    let db = rusqlite::Connection::open(&dst.main_db).unwrap();
    db.execute(
        "UPDATE chat_sessions SET archived = 1 WHERE session_id = ?1",
        [&new_id],
    )
    .unwrap();
    db.execute(
        "UPDATE workspaces SET archived = 1, deleted_at = 123
         WHERE workspace_id = (SELECT workspace_id FROM chat_sessions WHERE session_id = ?1)",
        [&new_id],
    )
    .unwrap();

    qoder::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    let list = qoder::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert!(!list[0].archived);
    assert!(!list[0].workspace_deleted);
    assert!(!list[0].deleted);
}

#[test]
fn zcode_to_qoder_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::zcode_fixture(&tmp.0.join("src"));
    let session = zcode::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));

    let new_id = qoder::inject(&dst, &session, "D:\\test").unwrap();

    let list = qoder::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, new_id);

    let back = qoder::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "qoder");
    assert_eq!(back.title, "ZCode 测试");
    assert_eq!(back.messages.len(), 2);
    match &back.messages[1].blocks[1] {
        Block::ToolCall { id, output, .. } => {
            assert_eq!(id, "call_9");
            assert_eq!(output.as_deref(), Some("content"));
        }
        b => panic!("expected tool call, got {b:?}"),
    }
}

#[test]
fn zcode_active_reflects_task_and_project_state() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));
    let new_id = zcode::inject(&dst, &session, r"D:\test").unwrap();

    assert!(zcode::active(&dst, &new_id));
    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    tasks
        .execute(
            "UPDATE tasks SET archived = 1 WHERE task_id = ?1",
            [&new_id],
        )
        .unwrap();
    assert!(!zcode::active(&dst, &new_id));
    tasks
        .execute(
            "UPDATE tasks SET archived = 0 WHERE task_id = ?1",
            [&new_id],
        )
        .unwrap();
    assert!(zcode::active(&dst, &new_id));
    tasks
        .execute(
            "UPDATE projects SET deleted = 1
             WHERE project_id = (SELECT project_id FROM tasks WHERE task_id = ?1)",
            [&new_id],
        )
        .unwrap();
    assert!(!zcode::active(&dst, &new_id));
}

#[test]
fn qoder_to_cursor_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_cursor(&tmp.0.join("dst"));

    let new_id = cursor::inject(&dst, &session, "D:\\test").unwrap();
    assert_eq!(new_id.len(), 36);
    assert!(cursor::active(&dst, &new_id));

    let list = cursor::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "测试会话");
    assert_eq!(list[0].cwd, "d:\\test");

    let back = cursor::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "cursor");
    assert_eq!(back.messages.len(), 4);
    let tool = back
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolCall {
                id, name, output, ..
            } => Some((id.clone(), name.clone(), output.clone())),
            _ => None,
        })
        .expect("tool call bubble");
    assert_eq!(tool.0, "call_1");
    assert_eq!(tool.1, "Bash");
    assert_eq!(tool.2.as_deref(), Some("a.txt"));

    let db = rusqlite::Connection::open(&dst.state_db).unwrap();
    let data: String = db
        .query_row(
            "SELECT value FROM cursorDiskKV WHERE key = ?1",
            [format!("composerData:{new_id}")],
            |r| r.get(0),
        )
        .unwrap();
    let composer: Value = serde_json::from_str(&data).unwrap();
    let headers = composer["fullConversationHeadersOnly"].as_array().unwrap();
    let bubbles: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM cursorDiskKV WHERE key LIKE ?1",
            [format!("bubbleId:{new_id}:%")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(headers.len() as i64, bubbles);

    let workspace_id: String = db
        .query_row(
            "SELECT workspaceId FROM composerHeaders WHERE composerId = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    let workspace: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dst.workspace_storage
                .join(&workspace_id)
                .join("workspace.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(workspace["folder"], "file:///d%3A/test");
}

#[test]
fn cursor_write_reuses_matching_workspace_and_reactivates() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let mut session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_cursor(&tmp.0.join("dst"));

    let workspace_id = "existing-workspace";
    let dir = dst.workspace_storage.join(workspace_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("workspace.json"),
        r#"{"folder":"file:///d%3A/test"}"#,
    )
    .unwrap();

    let new_id = cursor::inject(&dst, &session, "D:\\test").unwrap();
    let db = rusqlite::Connection::open(&dst.state_db).unwrap();
    let stored: String = db
        .query_row(
            "SELECT workspaceId FROM composerHeaders WHERE composerId = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, workspace_id);

    db.execute(
        "UPDATE composerHeaders SET isArchived = 1 WHERE composerId = ?1",
        [&new_id],
    )
    .unwrap();
    assert!(!cursor::active(&dst, &new_id));

    session.title = "更新后的标题".into();
    let again = cursor::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    assert_eq!(again, new_id);
    assert!(cursor::active(&dst, &new_id));

    let list = cursor::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
    let back = cursor::load(&dst, &new_id).unwrap();
    assert_eq!(back.messages.len(), 4);
}

#[test]
fn qoder_to_workbuddy_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_workbuddy(&tmp.0.join("dst"));

    let new_id = workbuddy::inject(&dst, &session, "D:\\test").unwrap();
    assert!(workbuddy::active(&dst, &new_id));

    let list = workbuddy::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "测试会话");
    assert_eq!(list[0].cwd, "D:/test");
    assert_eq!(list[0].message_count, 2);

    let back = workbuddy::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "workbuddy");
    assert_eq!(back.messages.len(), 2);
    assert!(matches!(back.messages[1].blocks[0], Block::Thinking(_)));
    match &back.messages[1].blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "Bash");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        block => panic!("expected tool call, got {block:?}"),
    }

    let db = rusqlite::Connection::open(&dst.db).unwrap();
    let row: (String, String, Option<i64>) = db
        .query_row(
            "SELECT title, user_id, deleted_at FROM sessions WHERE id = ?1",
            [&new_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, "测试会话");
    assert_eq!(row.1, "test-user");
    assert!(row.2.is_none());

    let jsonl =
        std::fs::read_to_string(dst.projects.join("d-test").join(format!("{new_id}.jsonl")))
            .unwrap();
    assert!(jsonl.contains("<user_query>列出文件</user_query>"));
    assert!(jsonl.contains(r#""type":"function_call""#));
    assert!(jsonl.contains(r#""type":"function_call_result""#));
}

#[test]
fn workbuddy_overwrite_reactivates_and_rewrites() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let mut session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_workbuddy(&tmp.0.join("dst"));

    let new_id = workbuddy::inject(&dst, &session, "D:\\test").unwrap();
    let db = rusqlite::Connection::open(&dst.db).unwrap();
    db.execute(
        "UPDATE sessions SET deleted_at = 123, status = 'archived', transport = 'cloud', title = '旧标题' WHERE id = ?1",
        [&new_id],
    )
    .unwrap();
    assert!(!workbuddy::active(&dst, &new_id));

    session.title = "更新后的标题".into();
    let again = workbuddy::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    assert_eq!(again, new_id);

    let list = workbuddy::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
    assert!(!list[0].deleted);
    assert!(!list[0].archived);
    assert!(workbuddy::active(&dst, &new_id));

    let back = workbuddy::load(&dst, &new_id).unwrap();
    assert_eq!(back.messages.len(), 2);
}

#[test]
fn workbuddy_to_zcode_reuses_project_with_different_separators() {
    let tmp = common::TempDir::new();
    let (src, id) = common::workbuddy_fixture(&tmp.0.join("src"));
    let session = workbuddy::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));

    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    tasks
        .execute_batch(
            r#"INSERT INTO projects (project_id, host_id, name, position, created_at, updated_at)
               VALUES ('proj_test', 'local', 'test', 0, 1, 1);
               INSERT INTO project_roots (project_id, root_path, position, is_primary)
               VALUES ('proj_test', 'D:\test', 0, 1);"#,
        )
        .unwrap();

    let new_id = zcode::inject(&dst, &session, "D:/test").unwrap();
    let project: String = tasks
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(project, "proj_test");
    let count: i64 = tasks
        .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let db = rusqlite::Connection::open(&dst.db).unwrap();
    let directory: String = db
        .query_row(
            "SELECT directory FROM session WHERE id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(directory, "D:\\test");
}

#[test]
fn workbuddy_to_zcode_adds_root_to_same_name_project() {
    let tmp = common::TempDir::new();
    let (src, id) = common::workbuddy_fixture(&tmp.0.join("src"));
    let session = workbuddy::load(&src, &id).unwrap();
    let dst = common::empty_zcode(&tmp.0.join("dst"));

    let tasks = rusqlite::Connection::open(&dst.tasks_db).unwrap();
    tasks
        .execute_batch(
            r#"INSERT INTO projects (project_id, host_id, name, position, created_at, updated_at)
               VALUES ('proj_test', 'local', 'test', 0, 1, 1);
               INSERT INTO project_roots (project_id, root_path, position, is_primary)
               VALUES ('proj_test', 'E:\other\test', 0, 1);"#,
        )
        .unwrap();

    let new_id = zcode::inject(&dst, &session, "D:/test").unwrap();
    let project: String = tasks
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(project, "proj_test");
    let roots: i64 = tasks
        .query_row(
            "SELECT COUNT(*) FROM project_roots WHERE project_id = 'proj_test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(roots, 2);
}

#[test]
fn workbuddy_to_qoder_reuses_workspace_across_separators() {
    let tmp = common::TempDir::new();
    let (src, id) = common::workbuddy_fixture(&tmp.0.join("src"));
    let session = workbuddy::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));
    let db = rusqlite::Connection::open(&dst.main_db).unwrap();
    db.execute(
        "INSERT INTO workspaces (workspace_id, name, workspace_source, root_paths_json, created_at, updated_at, archived)
         VALUES ('ws_active', 'Ink', 'local', ?1, 1, 1, 0)",
        [r#"["D:\\Work\\Ink"]"#],
    )
    .unwrap();
    db.execute(
        "INSERT INTO workspaces (workspace_id, name, workspace_source, root_paths_json, created_at, updated_at, archived)
         VALUES ('ws_duplicate', 'Ink', 'local', ?1, 2, 2, 1)",
        [r#"["D:/Work/Ink"]"#],
    )
    .unwrap();

    let new_id = qoder::inject(&dst, &session, "D:/Work/Ink").unwrap();
    let workspace: String = db
        .query_row(
            "SELECT workspace_id FROM chat_sessions WHERE session_id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(workspace, "ws_active");
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "transfer must not create another workspace");
    let archived: i64 = db
        .query_row(
            "SELECT archived FROM workspaces WHERE workspace_id = 'ws_active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 0);
}

#[test]
fn qoder_new_workspace_stores_normalized_root() {
    let tmp = common::TempDir::new();
    let (src, id) = common::zcode_fixture(&tmp.0.join("src"));
    let session = zcode::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));

    qoder::inject(&dst, &session, "D:/Work/Ink").unwrap();
    let db = rusqlite::Connection::open(&dst.main_db).unwrap();
    let roots: String = db
        .query_row("SELECT root_paths_json FROM workspaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(roots, r#"["D:\\Work\\Ink"]"#);
}

#[test]
fn delete_marks_soft_deleted_sessions() {
    let tmp = common::TempDir::new();
    let (qoder_paths, qoder_id) = common::qoder_fixture(&tmp.0.join("qoder"));
    qoder::delete(&qoder_paths, &qoder_id).unwrap();
    let list = qoder::list(&qoder_paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].deleted);
    assert!(!qoder::active(&qoder_paths, &qoder_id));

    let (wb_paths, wb_id) = common::workbuddy_fixture(&tmp.0.join("workbuddy"));
    workbuddy::delete(&wb_paths, &wb_id).unwrap();
    let list = workbuddy::list(&wb_paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].deleted);
    assert!(!workbuddy::active(&wb_paths, &wb_id));

    let (zc_paths, zc_id) = common::zcode_fixture(&tmp.0.join("zcode"));
    zcode::delete(&zc_paths, &zc_id).unwrap();
    let list = zcode::list(&zc_paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].deleted);
    assert!(!zcode::active(&zc_paths, &zc_id));
}

#[test]
fn delete_removes_cursor_composer() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_cursor(&tmp.0.join("dst"));
    let new_id = cursor::inject(&dst, &session, "D:\\test").unwrap();
    assert_eq!(cursor::list(&dst).unwrap().len(), 1);

    cursor::delete(&dst, &new_id).unwrap();
    assert!(cursor::list(&dst).unwrap().is_empty());
    assert!(!cursor::active(&dst, &new_id));
}

#[test]
fn cursor_delete_removes_bubbles_checkpoints_and_composer_data() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_cursor(&tmp.0.join("dst"));
    let new_id = cursor::inject(&dst, &session, "D:\\test").unwrap();

    // 手工造一条 checkpoint 键，覆盖现有测试从未命中的删除分支。
    let db = rusqlite::Connection::open(&dst.state_db).unwrap();
    db.execute(
        "INSERT INTO cursorDiskKV (key, value) VALUES (?1, '{}')",
        [format!("checkpointId:{new_id}:abc")],
    )
    .unwrap();
    let count = |pattern: String| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM cursorDiskKV WHERE key LIKE ?1",
            [pattern],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(count(format!("bubbleId:{new_id}:%")) > 0);
    assert_eq!(count(format!("checkpointId:{new_id}:%")), 1);
    assert!(count(format!("composerData:{new_id}")) >= 0);

    cursor::delete(&dst, &new_id).unwrap();
    assert_eq!(count(format!("bubbleId:{new_id}:%")), 0);
    assert_eq!(count(format!("checkpointId:{new_id}:%")), 0);
    let composer_data: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM cursorDiskKV WHERE key = ?1",
            [format!("composerData:{new_id}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(composer_data, 0);
    let headers: i64 = db
        .query_row("SELECT COUNT(*) FROM composerHeaders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(headers, 0);
}

#[test]
fn delete_missing_session_errors_or_pins_cursor_silent_ok() {
    let tmp = common::TempDir::new();
    let q = common::empty_qoder(&tmp.0.join("qoder"));
    assert!(qoder::delete(&q, "nonexistent").is_err());
    let z = common::empty_zcode(&tmp.0.join("zcode"));
    assert!(zcode::delete(&z, "nonexistent").is_err());
    let w = common::empty_workbuddy(&tmp.0.join("workbuddy"));
    assert!(workbuddy::delete(&w, "nonexistent").is_err());
    let k = common::empty_kiro(&tmp.0.join("kiro"));
    assert!(kiro::delete(&k, "nonexistent").is_err());
    // Cursor 对不存在的 id 静默成功，这是它的既有语义，用测试钉住。
    let c = common::empty_cursor(&tmp.0.join("cursor"));
    cursor::delete(&c, "nonexistent").unwrap();
}

#[test]
fn overwrite_missing_target_acts_as_inject_with_given_id() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();

    let q = common::empty_qoder(&tmp.0.join("qoder"));
    assert_eq!(
        qoder::overwrite(&q, &session, "D:\\test", "fixed-qoder-id").unwrap(),
        "fixed-qoder-id"
    );
    assert_eq!(qoder::list(&q).unwrap()[0].id, "fixed-qoder-id");
    assert_eq!(qoder::load(&q, "fixed-qoder-id").unwrap().messages.len(), 3);

    let z = common::empty_zcode(&tmp.0.join("zcode"));
    assert_eq!(
        zcode::overwrite(&z, &session, "D:\\test", "fixed-zcode-id").unwrap(),
        "fixed-zcode-id"
    );
    assert_eq!(zcode::list(&z).unwrap()[0].id, "fixed-zcode-id");

    let c = common::empty_cursor(&tmp.0.join("cursor"));
    assert_eq!(
        cursor::overwrite(&c, &session, "D:\\test", "fixed-cursor-id").unwrap(),
        "fixed-cursor-id"
    );
    assert_eq!(cursor::list(&c).unwrap()[0].id, "fixed-cursor-id");

    let w = common::empty_workbuddy(&tmp.0.join("workbuddy"));
    assert_eq!(
        workbuddy::overwrite(&w, &session, "D:\\test", "fixed-wb-id").unwrap(),
        "fixed-wb-id"
    );
    assert_eq!(workbuddy::list(&w).unwrap()[0].id, "fixed-wb-id");

    let k = common::empty_kiro(&tmp.0.join("kiro"));
    assert_eq!(
        kiro::overwrite(&k, &session, "D:\\test", "fixed-kiro-id").unwrap(),
        "fixed-kiro-id"
    );
    assert_eq!(kiro::list(&k).unwrap()[0].id, "fixed-kiro-id");
}

#[test]
fn empty_session_roundtrip_per_target() {
    let tmp = common::TempDir::new();
    let session = common::empty_session(r"D:\test");

    let q = common::empty_qoder(&tmp.0.join("qoder"));
    let qid = qoder::inject(&q, &session, "D:\\test").unwrap();
    assert_eq!(qoder::list(&q).unwrap()[0].message_count, 0);
    assert!(qoder::load(&q, &qid).unwrap().messages.is_empty());
    qoder::delete(&q, &qid).unwrap();

    let z = common::empty_zcode(&tmp.0.join("zcode"));
    let zid = zcode::inject(&z, &session, "D:\\test").unwrap();
    assert_eq!(zcode::list(&z).unwrap()[0].message_count, 0);
    assert!(zcode::load(&z, &zid).unwrap().messages.is_empty());
    zcode::delete(&z, &zid).unwrap();

    let c = common::empty_cursor(&tmp.0.join("cursor"));
    let cid = cursor::inject(&c, &session, "D:\\test").unwrap();
    assert_eq!(cursor::list(&c).unwrap()[0].message_count, 0);
    assert!(cursor::load(&c, &cid).unwrap().messages.is_empty());
    cursor::delete(&c, &cid).unwrap();

    let w = common::empty_workbuddy(&tmp.0.join("workbuddy"));
    let wid = workbuddy::inject(&w, &session, "D:\\test").unwrap();
    assert_eq!(workbuddy::list(&w).unwrap()[0].message_count, 0);
    assert!(workbuddy::load(&w, &wid).unwrap().messages.is_empty());
    workbuddy::delete(&w, &wid).unwrap();

    let k = common::empty_kiro(&tmp.0.join("kiro"));
    let kid = kiro::inject(&k, &session, "D:\\test").unwrap();
    assert_eq!(kiro::list(&k).unwrap()[0].message_count, 0);
    assert!(kiro::load(&k, &kid).unwrap().messages.is_empty());
    kiro::delete(&k, &kid).unwrap();
}

#[test]
fn empty_target_cwd_falls_back_to_session_cwd_or_bails() {
    let tmp = common::TempDir::new();
    let mut session = common::empty_session(r"D:\test");

    // 目标 cwd 为空时回落到会话自身的 cwd。
    let q = common::empty_qoder(&tmp.0.join("qoder"));
    let qid = qoder::inject(&q, &session, "").unwrap();
    assert_eq!(qoder::load(&q, &qid).unwrap().cwd, r"D:\test");
    let z = common::empty_zcode(&tmp.0.join("zcode"));
    let zid = zcode::inject(&z, &session, "").unwrap();
    assert_eq!(zcode::load(&z, &zid).unwrap().cwd, r"D:\test");
    let c = common::empty_cursor(&tmp.0.join("cursor"));
    let cid = cursor::inject(&c, &session, "").unwrap();
    assert_eq!(cursor::load(&c, &cid).unwrap().cwd, "d:\\test");
    let w = common::empty_workbuddy(&tmp.0.join("workbuddy"));
    let wid = workbuddy::inject(&w, &session, "").unwrap();
    // WorkBuddy 库内 cwd 以正斜杠存储。
    assert_eq!(workbuddy::load(&w, &wid).unwrap().cwd, "D:/test");
    let k = common::empty_kiro(&tmp.0.join("kiro"));
    let kid = kiro::inject(&k, &session, "").unwrap();
    // Kiro 写入时盘符转小写。
    assert_eq!(kiro::load(&k, &kid).unwrap().cwd, r"d:\test");

    // 会话与目标 cwd 都为空：qoder/zcode/workbuddy/kiro 拒绝写入；
    // Cursor 落到原生 empty-window 工作区。
    session.cwd = String::new();
    assert!(qoder::inject(&q, &session, "").is_err());
    assert!(zcode::inject(&z, &session, "").is_err());
    assert!(workbuddy::inject(&w, &session, "").is_err());
    assert!(kiro::inject(&k, &session, "").is_err());
    let c2 = common::empty_cursor(&tmp.0.join("cursor2"));
    let cid2 = cursor::inject(&c2, &session, "").unwrap();
    let db = rusqlite::Connection::open(&c2.state_db).unwrap();
    let workspace_id: String = db
        .query_row(
            "SELECT workspaceId FROM composerHeaders WHERE composerId = ?1",
            [&cid2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(workspace_id, "empty-window");
}

#[test]
fn qoder_active_reflects_session_workspace_and_jsonl() {
    let tmp = common::TempDir::new();
    let (src, id) = common::zcode_fixture(&tmp.0.join("src"));
    let session = zcode::load(&src, &id).unwrap();
    let dst = common::empty_qoder(&tmp.0.join("dst"));
    let new_id = qoder::inject(&dst, &session, r"D:\test").unwrap();

    assert!(qoder::active(&dst, &new_id));
    let db = rusqlite::Connection::open(&dst.main_db).unwrap();
    db.execute(
        "UPDATE chat_sessions SET archived = 1 WHERE session_id = ?1",
        [&new_id],
    )
    .unwrap();
    assert!(!qoder::active(&dst, &new_id));
    db.execute(
        "UPDATE chat_sessions SET archived = 0 WHERE session_id = ?1",
        [&new_id],
    )
    .unwrap();
    assert!(qoder::active(&dst, &new_id));
    db.execute(
        "UPDATE workspaces SET archived = 1
         WHERE workspace_id = (SELECT workspace_id FROM chat_sessions WHERE session_id = ?1)",
        [&new_id],
    )
    .unwrap();
    assert!(!qoder::active(&dst, &new_id));
    db.execute(
        "UPDATE workspaces SET archived = 0
         WHERE workspace_id = (SELECT workspace_id FROM chat_sessions WHERE session_id = ?1)",
        [&new_id],
    )
    .unwrap();
    assert!(qoder::active(&dst, &new_id));
    for entry in std::fs::read_dir(&dst.projects).unwrap() {
        let file = entry.unwrap().path().join(format!("{new_id}.jsonl"));
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
    assert!(!qoder::active(&dst, &new_id));
}

#[test]
fn qoder_to_kiro_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_kiro(&tmp.0.join("dst"));

    let new_id = kiro::inject(&dst, &session, "D:\\test").unwrap();
    assert!(new_id.starts_with("sess_"));
    assert!(kiro::active(&dst, &new_id));

    let list = kiro::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "测试会话");
    assert_eq!(list[0].cwd, r"d:\test");
    assert_eq!(list[0].message_count, 2);

    let back = kiro::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "kiro");
    assert_eq!(back.messages.len(), 2);
    match &back.messages[1].blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(name, "Bash");
            assert_eq!(output.as_deref(), Some("a.txt"));
            let _ = id;
        }
        block => panic!("expected tool call, got {block:?}"),
    }

    let hash = kiro::workspace_hash("D:\\test");
    let oplog = std::fs::read_to_string(dst.session_index.join(format!("{hash}.jsonl"))).unwrap();
    let line: Value = serde_json::from_str(oplog.lines().last().unwrap()).unwrap();
    assert_eq!(line["op"], "add");
    assert_eq!(line["sessionPath"], format!("{hash}/{new_id}"));

    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(dst.sessions.join(&hash).join(&new_id).join("session.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["schemaVersion"], "1.0.0");
    assert_eq!(meta["agentMode"], "vibe");
    assert_eq!(meta["workspacePaths"][0], r"d:\test");

    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(dst.sessions.join(&hash).join(".index/index.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(index["schemaVersion"], 2);
    let entries = index["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], new_id);
    assert_eq!(entries[0]["title"], "测试会话");
    assert_eq!(entries[0]["workspacePaths"][0], r"d:\test");
}

#[test]
fn kiro_write_uses_native_turn_order_and_ids() {
    let tmp = common::TempDir::new();
    let dst = common::empty_kiro(&tmp.0);
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "结构测试".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("你好".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("你好，有什么可以帮你".into())],
            },
        ],
        compaction: None,
    };
    let id = kiro::inject(&dst, &session, r"D:\test").unwrap();
    let path = dst
        .sessions
        .join(kiro::workspace_hash(r"D:\test"))
        .join(&id)
        .join("messages.jsonl");
    let events: Vec<Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["payload"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["user", "turn_start", "assistant", "turn_end"]);

    let execution = events[1]["payload"]["executionId"].as_str().unwrap();
    assert_eq!(events[2]["payload"]["executionId"], execution);
    assert_eq!(events[3]["payload"]["executionId"], execution);
    assert_eq!(
        events[1]["payload"]["userMessageTag"],
        events[0]["payload"]["_meta"]["kiro"]["userMessageTag"]
    );
    assert!(events[3]["payload"]["turnId"].as_str() == Some(execution));
}

#[test]
fn kiro_write_matches_event_schema_for_images_and_failed_tools() {
    let tmp = common::TempDir::new();
    let dst = common::empty_kiro(&tmp.0);
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "Schema 测试".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![
                    Block::Text("看图".into()),
                    Block::Image {
                        reference: "data:image/jpeg;base64,AAAA".into(),
                    },
                ],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::ToolCall {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: json!("raw"),
                    output: Some("failed".into()),
                    status: "error".into(),
                }],
            },
        ],
        compaction: None,
    };
    let id = kiro::inject(&dst, &session, r"D:\test").unwrap();
    let path = dst
        .sessions
        .join(kiro::workspace_hash(r"D:\test"))
        .join(&id)
        .join("messages.jsonl");
    let events: Vec<Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let user = events
        .iter()
        .find(|event| event["payload"]["type"] == "user")
        .unwrap();
    assert_eq!(user["payload"]["images"][0]["mimeType"], "image/jpeg");
    let call = events
        .iter()
        .find(|event| event["payload"]["type"] == "tool_call")
        .unwrap();
    assert_eq!(call["payload"]["status"], "failed");
    assert_eq!(call["payload"]["args"], json!({ "value": "raw" }));
}

#[test]
fn kiro_overwrite_replaces_and_keeps_created_at() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let mut session = qoder::load(&src, &id).unwrap();
    let dst = common::empty_kiro(&tmp.0.join("dst"));

    let new_id = kiro::inject(&dst, &session, "D:\\test").unwrap();
    let created_before: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dst.sessions
                .join(kiro::workspace_hash("D:\\test"))
                .join(&new_id)
                .join("session.json"),
        )
        .unwrap(),
    )
    .unwrap();

    session.title = "更新后的标题".into();
    let again = kiro::overwrite(&dst, &session, "D:\\test", &new_id).unwrap();
    assert_eq!(again, new_id);

    let list = kiro::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
    let back = kiro::load(&dst, &new_id).unwrap();
    assert_eq!(back.messages.len(), 2);

    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dst.sessions
                .join(kiro::workspace_hash("D:\\test"))
                .join(".index/index.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let entries = index["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["title"], "更新后的标题");

    let created_after: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dst.sessions
                .join(kiro::workspace_hash("D:\\test"))
                .join(&new_id)
                .join("session.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(created_after["createdAt"], created_before["createdAt"]);

    let ws_dir = dst.sessions.join(kiro::workspace_hash("D:\\test"));
    let leftovers: Vec<String> = std::fs::read_dir(&ws_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".staging-") || name.starts_with(".retired-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no staging/retired leftovers: {leftovers:?}"
    );
}

#[test]
fn qoder_write_preserves_compaction_boundary() {
    let tmp = common::TempDir::new();
    let dst = common::empty_qoder(&tmp.0);
    let session = Session {
        agent: "workbuddy".into(),
        id: "source".into(),
        title: "压缩测试".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("回答".into())],
            },
            Message {
                role: Role::User,
                time_ms: 1_200,
                blocks: vec![Block::Text("压缩后".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 2,
            time_ms: 1_150,
        }),
    };
    let id = qoder::inject(&dst, &session, r"D:\test").unwrap();
    let reloaded = qoder::load(&dst, &id).unwrap();
    assert_eq!(reloaded.messages.len(), 3);
    let compaction = reloaded.compaction.expect("重新读取应识别压缩边界");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, 2);
}

#[test]
fn workbuddy_write_preserves_compaction_boundary() {
    let tmp = common::TempDir::new();
    let dst = common::empty_workbuddy(&tmp.0);
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "压缩测试".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("回答".into())],
            },
            Message {
                role: Role::User,
                time_ms: 1_200,
                blocks: vec![Block::Text("压缩后".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 2,
            time_ms: 1_150,
        }),
    };
    let id = workbuddy::inject(&dst, &session, r"D:\test").unwrap();
    let reloaded = workbuddy::load(&dst, &id).unwrap();
    assert_eq!(reloaded.messages.len(), 3);
    let compaction = reloaded.compaction.expect("重新读取应识别压缩注入消息");
    assert!(compaction.summary_text.contains("<cb_summary>"));
    assert_eq!(compaction.tail_start_index, 2);
}

#[test]
fn zcode_write_preserves_compaction_boundary() {
    let tmp = common::TempDir::new();
    let dst = common::empty_zcode(&tmp.0);
    let session = Session {
        agent: "workbuddy".into(),
        id: "source".into(),
        title: "压缩测试".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("回答".into())],
            },
            Message {
                role: Role::User,
                time_ms: 1_200,
                blocks: vec![Block::Text("压缩后".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 2,
            time_ms: 1_150,
        }),
    };
    let id = zcode::inject(&dst, &session, r"D:\test").unwrap();
    let reloaded = zcode::load(&dst, &id).unwrap();
    assert_eq!(reloaded.messages.len(), 3);
    let compaction = reloaded.compaction.expect("重新读取应识别 compaction part");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, 2);

    let db = rusqlite::Connection::open(&dst.db).unwrap();
    let old_id: String = db
        .query_row(
            "SELECT message_id FROM part
             WHERE session_id = ?1 AND json_extract(data,'$.type') = 'text'
               AND json_extract(data,'$.text') = '回答'",
            [&id],
            |r| r.get(0),
        )
        .expect("应找到压缩前最后一条消息");
    let parts: Vec<String> = db
        .prepare(
            "SELECT data FROM part
             WHERE session_id = ?1 AND json_extract(data,'$.type') = 'compaction'",
        )
        .unwrap()
        .query_map([&id], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(parts.len(), 2, "时间线事件与摘要消息各一条 compaction part");
    for part in &parts {
        let value: Value = serde_json::from_str(part).unwrap();
        assert_eq!(
            value["tail_start_id"].as_str(),
            Some(old_id.as_str()),
            "tail_start_id 应指向压缩前最后一条消息"
        );
    }
    let (kind, provider, ui): (String, String, String) = db
        .query_row(
            "SELECT json_extract(data,'$.semantics.kind'),
                    json_extract(data,'$.semantics.providerVisibility'),
                    json_extract(data,'$.semantics.uiVisibility')
             FROM message
             WHERE session_id = ?1 AND json_extract(data,'$.semantics.kind') = 'compact_summary'",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("应写入 compact_summary 摘要消息");
    assert_eq!(kind, "compact_summary");
    assert_eq!(provider, "visible");
    assert_eq!(ui, "hidden");
    let summary_text: String = db
        .query_row(
            "SELECT json_extract(p.data,'$.text')
             FROM part p JOIN message m ON m.id = p.message_id
             WHERE m.session_id = ?1 AND json_extract(p.data,'$.type') = 'text'
               AND json_extract(m.data,'$.semantics.kind') = 'compact_summary'",
            [&id],
            |r| r.get(0),
        )
        .expect("摘要消息应带正文 part");
    assert_eq!(summary_text, "摘要正文");
}

#[test]
fn zcode_write_compaction_at_end_points_to_last_message() {
    let tmp = common::TempDir::new();
    let dst = common::empty_zcode(&tmp.0);
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "末端压缩".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("结束".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 2,
            time_ms: 1_150,
        }),
    };
    let id = zcode::inject(&dst, &session, r"D:\test").unwrap();
    let reloaded = zcode::load(&dst, &id).unwrap();
    assert_eq!(reloaded.messages.len(), 2);
    let compaction = reloaded.compaction.expect("重新读取应识别 compaction part");
    assert_eq!(compaction.tail_start_index, 2);

    let db = rusqlite::Connection::open(&dst.db).unwrap();
    let tail_id: String = db
        .query_row(
            "SELECT json_extract(data,'$.tail_start_id') FROM part
             WHERE session_id = ?1 AND json_extract(data,'$.type') = 'compaction'
             ORDER BY sequence LIMIT 1",
            [&id],
            |r| r.get(0),
        )
        .expect("应写入 compaction part");
    let (tail_seq, summary_seq, summary_id): (i64, i64, String) = db
        .query_row(
            "SELECT
               (SELECT sequence FROM message WHERE id = ?1),
               (SELECT sequence FROM message WHERE session_id = ?2
                 AND json_extract(data,'$.semantics.kind') = 'compact_summary'),
               (SELECT id FROM message WHERE session_id = ?2
                 AND json_extract(data,'$.semantics.kind') = 'compact_summary')",
            rusqlite::params![&tail_id, &id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(tail_seq, 2, "tail_start_id 应指向最后一条历史消息");
    assert!(summary_seq > tail_seq, "摘要必须位于 tail_start_id 之后");
    assert_ne!(summary_id, tail_id);
}

#[test]
fn qoder_to_claude_roundtrip() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let session = qoder::load(&src, &id).unwrap();
    let home = tmp.0.join("claude");
    let dst = claude::Paths {
        projects: home.join("projects"),
        home,
    };

    let new_id = claude::inject(&dst, &session, r"D:\test").unwrap();
    assert!(!new_id.is_empty());
    let back = claude::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "claude");
    assert_eq!(back.title, "列出文件");
    assert_eq!(back.messages.len(), 3);
    match &back.messages[1].blocks[1] {
        Block::ToolCall { id, output, .. } => {
            assert_eq!(id, "call_1");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        b => panic!("expected tool call, got {b:?}"),
    }
    let list = claude::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message_count, 3);
}

#[test]
fn qoder_to_dsh_roundtrip_and_overwrite() {
    let tmp = common::TempDir::new();
    let (src, id) = common::qoder_fixture(&tmp.0.join("src"));
    let mut session = qoder::load(&src, &id).unwrap();
    let home = tmp.0.join("dsh");
    let dst = dsh::Paths {
        sessions: home.join("sessions"),
        storages: home.join("storages"),
        home,
    };

    let new_id = dsh::inject(&dst, &session, r"D:\test").unwrap();
    assert!(new_id.starts_with("session-"));
    let back = dsh::load(&dst, &new_id).unwrap();
    assert_eq!(back.agent, "dsh");
    assert_eq!(back.title, "测试会话");
    assert_eq!(back.messages.len(), 3);
    match &back.messages[1].blocks[1] {
        Block::ToolCall { output, .. } => assert_eq!(output.as_deref(), Some("a.txt")),
        b => panic!("expected tool call, got {b:?}"),
    }
    assert_eq!(back.cwd, r"D:\test");

    session.title = "更新后的标题".into();
    let again = dsh::overwrite(&dst, &session, r"D:\test", &new_id).unwrap();
    assert_eq!(again, new_id);
    let list = dsh::list(&dst).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "更新后的标题");
}

#[test]
fn claude_and_dsh_preserve_compaction_boundary() {
    let tmp = common::TempDir::new();
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "压缩会话".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("回答".into())],
            },
            Message {
                role: Role::User,
                time_ms: 1_200,
                blocks: vec![Block::Text("尾部".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 1,
            time_ms: 1_150,
        }),
    };

    let claude_home = tmp.0.join("claude");
    let claude_paths = claude::Paths {
        projects: claude_home.join("projects"),
        home: claude_home,
    };
    let cid = claude::inject(&claude_paths, &session, r"D:\test").unwrap();
    let back = claude::load(&claude_paths, &cid).unwrap();
    assert_eq!(back.messages.len(), 3);
    let compaction = back.compaction.expect("claude compaction");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, 1);

    let dsh_home = tmp.0.join("dsh");
    let dsh_paths = dsh::Paths {
        sessions: dsh_home.join("sessions"),
        storages: dsh_home.join("storages"),
        home: dsh_home,
    };
    let did = dsh::inject(&dsh_paths, &session, r"D:\test").unwrap();
    let back = dsh::load(&dsh_paths, &did).unwrap();
    assert_eq!(back.messages.len(), 3);
    let compaction = back.compaction.expect("dsh compaction");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, 1);
}

#[test]
fn dsh_writer_emits_v4_compaction_envelope() {
    use std::io::Read as _;

    let tmp = common::TempDir::new();
    let session = Session {
        agent: "qoder".into(),
        id: "source".into(),
        title: "压缩".into(),
        cwd: r"D:\test".into(),
        created_ms: 1_000,
        updated_ms: 2_000,
        messages: vec![
            Message {
                role: Role::User,
                time_ms: 1_000,
                blocks: vec![Block::Text("历史".into())],
            },
            Message {
                role: Role::Assistant,
                time_ms: 1_100,
                blocks: vec![Block::Text("回答".into())],
            },
            Message {
                role: Role::User,
                time_ms: 1_200,
                blocks: vec![Block::Text("尾部".into())],
            },
        ],
        compaction: Some(Compaction {
            summary_text: "摘要正文".into(),
            tail_start_index: 2,
            time_ms: 1_150,
        }),
    };
    let home = tmp.0.join("dsh");
    let paths = dsh::Paths {
        sessions: home.join("sessions"),
        storages: home.join("storages"),
        home,
    };
    let id = dsh::inject(&paths, &session, r"D:\test").unwrap();
    let log = paths
        .sessions
        .join("--D-test--")
        .join(&id)
        .join("session.v4.jsonl.zstd");
    let file = std::fs::File::open(log).unwrap();
    let mut decoder = zstd::stream::read::Decoder::new(file).unwrap();
    let mut text = String::new();
    decoder.read_to_string(&mut text).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["type"], "session");
    assert_eq!(rows[0]["version"], 4);
    for (index, row) in rows[1..].iter().enumerate() {
        assert_eq!(row["seq"], index as u64, "seq 必须连续");
        // DSH 会话层还原要求每个消息事件带非空 id；assistant 还要带 stream 结算字段。
        match row["type"].as_str().unwrap_or_default() {
            "user/message" => {
                assert!(
                    row["data"]["id"].as_str().is_some_and(|id| !id.is_empty()),
                    "seq {index}: user/message 缺少 id"
                );
            }
            "assistant/message" => {
                assert!(
                    row["data"]["message"]["id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "seq {index}: assistant/message 缺少 id"
                );
                assert!(
                    row["data"]["stream"].as_array().is_some(),
                    "seq {index}: assistant/message 缺少 stream"
                );
                assert_eq!(row["data"]["message"]["role"], "assistant");
                assert_eq!(row["data"]["message"]["source"]["kind"], "model");
            }
            "tool/result" => {
                assert!(
                    row["data"]["message"]["id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "seq {index}: tool/result 缺少 id"
                );
                assert_eq!(row["data"]["message"]["role"], "tool");
                assert_eq!(
                    row["data"]["message"]["toolCallId"],
                    row["data"]["message"]["source"]["callId"]
                );
            }
            _ => {}
        }
        if row["type"] == "user/message" && row["surfaceOp"].is_object() {
            assert_eq!(row["data"]["source"]["kind"], "compact-checkpoint");
            assert!(row["surfaceOp"]["startSeq"].is_u64());
            assert!(row["surfaceOp"]["endSeq"].is_u64());
            assert!(row["sourceEventSeqs"]
                .as_array()
                .is_some_and(|a| a.len() >= 3));
        }
    }
    let kinds: Vec<&str> = rows[1..]
        .iter()
        .filter_map(|row| row["type"].as_str())
        .collect();
    for expected in ["compaction/start", "compaction/summary", "compaction/end"] {
        assert!(kinds.contains(&expected), "缺少 {expected}");
    }
}
