mod common;

use std::fs;

use agent_plugin_claude as claude;
use agent_plugin_cursor as cursor;
use agent_plugin_dsh as dsh;
use agent_plugin_kiro as kiro;
use agent_plugin_qoder as qoder;
use agent_plugin_workbuddy as workbuddy;
use agent_plugin_zcode as zcode;
use agent_session_manager::export;
use agent_session_manager::ir::{Block, Role};
use rusqlite::Connection;
use serde_json::Value;

#[test]
fn qoder_list_and_load() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::qoder_fixture(&tmp.0);
    let list = qoder::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "测试会话");
    assert_eq!(list[0].message_count, 2);
    assert!(list[0].archived);

    let s = qoder::load(&paths, &id).unwrap();
    assert_eq!(s.agent, "qoder");
    assert_eq!(s.messages.len(), 3);
    assert_eq!(s.messages[0].role, Role::User);
    let assistant = &s.messages[1];
    assert!(matches!(assistant.blocks[0], Block::Thinking(_)));
    match &assistant.blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "Bash");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        b => panic!("expected tool call, got {b:?}"),
    }
}

#[test]
fn zcode_list_and_load() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::zcode_fixture(&tmp.0);
    let list = zcode::list(&paths).unwrap();
    assert_eq!(list.len(), 1, "subagent sessions must be filtered");
    assert_eq!(list[0].message_count, 2);
    assert!(list[0].archived, "archive flag must come from tasks-index");

    let s = zcode::load(&paths, &id).unwrap();
    assert_eq!(s.agent, "zcode");
    assert_eq!(s.messages.len(), 2);
    match &s.messages[1].blocks[1] {
        Block::ToolCall {
            id,
            name,
            output,
            status,
            ..
        } => {
            assert_eq!(id, "call_9");
            assert_eq!(name, "Read");
            assert_eq!(output.as_deref(), Some("content"));
            assert_eq!(status, "completed");
        }
        b => panic!("expected tool call, got {b:?}"),
    }
}

#[test]
fn qoder_archived_workspace_marks_session() {
    let tmp = common::TempDir::new();
    let paths = common::empty_qoder(&tmp.0);
    let db = Connection::open(&paths.main_db).unwrap();
    db.execute_batch(
        r#"INSERT INTO workspaces (workspace_id, name, workspace_source, root_paths_json, created_at, updated_at, archived)
           VALUES ('ws_test', 'TEST', 'local', '[]', 1, 1, 1);
           INSERT INTO chat_sessions (session_id, title, cwd, workspace_id, created_at, updated_at)
           VALUES ('s1', '回滚代码并编译', 'C:\Work\TEST', 'ws_test', 1, 2);"#,
    )
    .unwrap();
    let list = qoder::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(
        list[0].workspace_deleted,
        "archived workspace must be flagged"
    );
    assert!(!list[0].deleted);
}

#[test]
fn qoder_deleted_session_is_listed_and_flagged() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::qoder_fixture(&tmp.0);
    let db = Connection::open(&paths.main_db).unwrap();
    db.execute(
        "UPDATE chat_sessions SET deleted_at = 123 WHERE session_id = ?1",
        [&id],
    )
    .unwrap();
    let list = qoder::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].deleted);
}

#[test]
fn zcode_deleted_and_workspace_deleted_flags() {
    let tmp = common::TempDir::new();
    let paths = common::empty_zcode(&tmp.0);
    let db = Connection::open(&paths.db).unwrap();
    db.execute_batch(
        r#"INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
           VALUES ('sess_gone_ws', 'proj_gone', 'a', 'D:\a', '工作区没了', '1', 1, 2);
           INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
           VALUES ('sess_tombstone', 'proj_alive', 'b', 'D:\b', '会话删了', '1', 3, 4);"#,
    )
    .unwrap();
    let tasks = Connection::open(&paths.tasks_db).unwrap();
    tasks
        .execute_batch(
            r#"INSERT INTO projects (project_id, host_id, name, position, deleted, deleted_at, created_at, updated_at)
               VALUES ('proj_gone', 'local', 'gone', 0, 1, 123, 1, 1),
                      ('proj_alive', 'local', 'alive', 1, 0, NULL, 1, 1);
               INSERT INTO tasks (workspace_key, workspace_path, task_id, title, created_at, updated_at, archived, deleted, project_id)
               VALUES ('ws', 'D:\a', 'sess_gone_ws', '工作区没了', 1, 2, 0, 0, 'proj_gone');
               INSERT INTO tasks (workspace_key, workspace_path, task_id, title, created_at, updated_at, archived, deleted, cli_deleted, project_id)
               VALUES ('ws', 'D:\b', 'sess_tombstone', '会话删了', 3, 4, 1, 1, 0, 'proj_alive');"#,
        )
        .unwrap();
    let list = zcode::list(&paths).unwrap();
    let gone = list.iter().find(|s| s.id == "sess_gone_ws").unwrap();
    assert!(gone.workspace_deleted);
    assert!(!gone.deleted);
    assert!(!gone.archived);
    let tomb = list.iter().find(|s| s.id == "sess_tombstone").unwrap();
    assert!(tomb.deleted);
    assert!(!tomb.workspace_deleted);
    assert!(tomb.archived);
}

#[test]
fn workbuddy_list_and_load() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::workbuddy_fixture(&tmp.0);
    let list = workbuddy::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "WorkBuddy 测试");
    assert_eq!(list[0].message_count, 2);
    assert!(!list[0].archived);
    assert!(!list[0].deleted);

    let session = workbuddy::load(&paths, &id).unwrap();
    assert_eq!(session.agent, "workbuddy");
    assert_eq!(session.cwd, "D:/test");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, Role::User);
    match &session.messages[0].blocks[0] {
        Block::Text(text) => assert_eq!(text, "列出文件"),
        block => panic!("expected user text, got {block:?}"),
    }
    assert!(matches!(session.messages[1].blocks[0], Block::Thinking(_)));
    match &session.messages[1].blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(id, "call_9");
            assert_eq!(name, "Bash");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        block => panic!("expected tool call, got {block:?}"),
    }
    assert!(workbuddy::active(&paths, &id));
}

#[test]
fn workbuddy_archived_session_is_flagged() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::workbuddy_fixture(&tmp.0);
    let db = Connection::open(&paths.db).unwrap();
    db.execute(
        "UPDATE sessions SET status = 'archived' WHERE id = ?1",
        [&id],
    )
    .unwrap();
    let list = workbuddy::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].archived);
    assert!(!list[0].deleted);
    assert!(!workbuddy::active(&paths, &id));
}

#[test]
fn workbuddy_deleted_session_is_flagged() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::workbuddy_fixture(&tmp.0);
    let db = Connection::open(&paths.db).unwrap();
    db.execute("UPDATE sessions SET deleted_at = 123 WHERE id = ?1", [&id])
        .unwrap();
    let list = workbuddy::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].deleted);
    assert!(!workbuddy::active(&paths, &id));
}

#[test]
fn kiro_list_and_load() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::kiro_fixture(&tmp.0);
    let list = kiro::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "Kiro 测试");
    assert_eq!(list[0].cwd, r"d:\test");
    assert_eq!(list[0].message_count, 2);
    assert!(!list[0].archived);
    assert!(!list[0].deleted);
    assert!(kiro::active(&paths, &id));

    let session = kiro::load(&paths, &id).unwrap();
    assert_eq!(session.agent, "kiro");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, Role::User);
    assert_eq!(session.messages[0].blocks.len(), 2);
    match &session.messages[0].blocks[0] {
        Block::Text(text) => assert_eq!(text, "列出文件"),
        block => panic!("expected user text, got {block:?}"),
    }
    match &session.messages[0].blocks[1] {
        Block::Image { reference } => {
            assert_eq!(reference, "data:image/jpeg;base64,AAAA");
        }
        block => panic!("expected user image, got {block:?}"),
    }
    let assistant = &session.messages[1];
    assert_eq!(assistant.blocks.len(), 3);
    assert!(matches!(assistant.blocks[0], Block::Thinking(_)));
    match &assistant.blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(id, "toolu_1");
            assert_eq!(name, "execute_pwsh");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        block => panic!("expected tool call, got {block:?}"),
    }
    assert!(matches!(assistant.blocks[2], Block::Text(_)));
}

#[test]
fn kiro_open_sessions_are_read_from_workspace_state() {
    let tmp = common::TempDir::new();
    let paths = common::empty_kiro(&tmp.0);
    assert!(kiro::open_session_ids(&paths).is_none());

    common::kiro_panel_state(
        &paths,
        "workspace-a",
        &["sess_11111111-2222-3333-4444-555555555555"],
    );
    common::kiro_panel_state(
        &paths,
        "workspace-b",
        &["sess_99999999-8888-7777-6666-555555555555"],
    );
    let open = kiro::open_session_ids(&paths).unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.contains("sess_11111111-2222-3333-4444-555555555555"));
    assert!(open.contains("sess_99999999-8888-7777-6666-555555555555"));
}

#[test]
fn kiro_delete_removes_directory_and_appends_remove() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::kiro_fixture(&tmp.0);
    let hash = kiro::workspace_hash("D:\\test");
    let workspace = paths.sessions.join(&hash);
    let session_dir = workspace.join(&id);

    let staging = workspace.join(".staging-test");
    fs::create_dir_all(&staging).unwrap();
    fs::copy(
        session_dir.join("session.json"),
        staging.join("session.json"),
    )
    .unwrap();
    let list = kiro::list(&paths).unwrap();
    assert_eq!(list.len(), 1, "staging directories are not sessions");

    let missing = kiro::Paths {
        home: tmp.0.join("empty-home"),
        sessions: tmp.0.join("empty-home/sessions"),
        session_index: tmp.0.join("empty-home/session-index"),
        spec_sessions: tmp.0.join("empty-home/spec-sessions"),
        workspace_storage: tmp.0.join("empty-home/workspaceStorage"),
    };
    assert!(kiro::list(&missing).unwrap().is_empty());

    kiro::delete(&paths, &id).unwrap();
    assert!(!session_dir.exists());
    assert!(!kiro::active(&paths, &id));
    assert!(kiro::list(&paths).unwrap().is_empty());

    let oplog = fs::read_to_string(paths.session_index.join(format!("{hash}.jsonl"))).unwrap();
    let last: Value = serde_json::from_str(oplog.lines().last().unwrap()).unwrap();
    assert_eq!(last["op"], "remove");
    assert_eq!(last["sessionPath"], format!("{hash}/{id}"));

    let index: Value =
        serde_json::from_str(&fs::read_to_string(workspace.join(".index/index.json")).unwrap())
            .unwrap();
    assert!(index["entries"].as_array().unwrap().is_empty());
}

#[test]
fn export_markdown_roundtrip() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::qoder_fixture(&tmp.0);
    let s = qoder::load(&paths, &id).unwrap();
    let md = export::markdown(&s);
    assert!(md.contains("# 会话交接：测试会话"));
    assert!(md.contains("来源：qoder"));
    assert!(md.contains("call_1"));
    assert!(md.contains("a.txt"));

    let out = export::export_to(
        &tmp.0.join("exports"),
        &s,
        export::Mode::Normal,
        &export::Options::default(),
    )
    .unwrap();
    assert!(out.exists());
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        md,
        "落盘内容必须与 markdown() 一致"
    );
}

#[test]
fn export_markdown_covers_all_blocks_and_truncates() {
    let tmp = common::TempDir::new();
    let long_output = "x".repeat(5000);
    let session = agent_session_manager::ir::Session {
        agent: "qoder".into(),
        id: "export-all".into(),
        title: "导出全块型".into(),
        cwd: r"D:\work".into(),
        created_ms: 1_793_527_200_000,
        updated_ms: 1_793_527_200_000,
        messages: vec![
            agent_session_manager::ir::Message {
                role: Role::Assistant,
                time_ms: 1_793_527_200_000,
                blocks: vec![
                    Block::Thinking("内部推理".into()),
                    Block::ToolCall {
                        id: "call_1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({ "command": "ls" }),
                        output: Some(long_output),
                        status: "completed".into(),
                    },
                    Block::Image {
                        reference: "img.png".into(),
                    },
                    Block::File {
                        path: "a.txt".into(),
                    },
                ],
            },
            agent_session_manager::ir::Message {
                role: Role::User,
                time_ms: i64::MAX, // 超出可表示范围，时间应显示占位符
                blocks: vec![Block::Text("看这里".into())],
            },
        ],
        compaction: None,
    };
    let md = export::markdown(&session);
    assert!(md.contains("<thinking>\n内部推理\n</thinking>"));
    assert!(md.contains("…（截断，原 5000 字符）"));
    assert!(!md.contains(&"x".repeat(4500)), "输出必须被截断");
    assert!(md.contains("[图片：img.png]"));
    assert!(md.contains("[文件：a.txt]"));
    assert!(md.contains("· -"), "越界时间戳应显示占位符：{md}");

    let out = export::export_to(
        &tmp.0.join("exports"),
        &session,
        export::Mode::Normal,
        &export::Options::default(),
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), md);
}

#[test]
fn qoder_load_rejects_corrupt_jsonl_but_wb_kiro_skip() {
    let tmp = common::TempDir::new();
    // Qoder：单行损坏使整个会话读取失败——这是它的既有语义，钉住。
    let (paths, id) = common::qoder_fixture(&tmp.0.join("qoder"));
    let slug = paths.projects.join("D--test");
    let file = fs::read_to_string(slug.join(format!("{id}.jsonl"))).unwrap();
    fs::write(
        slug.join(format!("{id}.jsonl")),
        format!("{file}not json\n"),
    )
    .unwrap();
    let error = qoder::load(&paths, &id).unwrap_err();
    assert!(
        error.to_string().contains("parse jsonl line"),
        "损坏行必须报错：{error}"
    );

    // WorkBuddy / Kiro：坏行静默跳过，其余内容照常读出——同样钉住。
    let (wb_paths, wb_id) = common::workbuddy_fixture(&tmp.0.join("wb"));
    let wb_file = wb_paths
        .projects
        .join("d-test")
        .join(format!("{wb_id}.jsonl"));
    let wb_text = fs::read_to_string(&wb_file).unwrap();
    fs::write(&wb_file, format!("{{bad line\n{wb_text}not json\n")).unwrap();
    let session = workbuddy::load(&wb_paths, &wb_id).unwrap();
    assert_eq!(session.messages.len(), 2);

    let (k_paths, k_id) = common::kiro_fixture(&tmp.0.join("kiro"));
    let k_file = k_paths
        .sessions
        .join(kiro::workspace_hash(r"D:\test"))
        .join(&k_id)
        .join("messages.jsonl");
    let k_text = fs::read_to_string(&k_file).unwrap();
    fs::write(&k_file, format!("not json\n{k_text}")).unwrap();
    let k_session = kiro::load(&k_paths, &k_id).unwrap();
    assert_eq!(k_session.messages.len(), 2);
}

#[test]
fn claude_list_and_load() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::claude_fixture(&tmp.0);
    let list = claude::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "列出文件");
    assert_eq!(list[0].message_count, 3);
    assert_eq!(list[0].cwd, r"D:\test");
    assert!(!list[0].archived);

    let s = claude::load(&paths, &id).unwrap();
    assert_eq!(s.agent, "claude");
    assert_eq!(s.messages.len(), 3);
    assert!(matches!(s.messages[1].blocks[0], Block::Thinking(_)));
    match &s.messages[1].blocks[1] {
        Block::ToolCall {
            id, name, output, ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "Bash");
            assert_eq!(output.as_deref(), Some("a.txt"));
        }
        b => panic!("expected tool call, got {b:?}"),
    }
}

#[test]
fn dsh_list_and_load_handles_multi_frame_log() {
    let tmp = common::TempDir::new();
    let (paths, id, _) = common::dsh_fixture(&tmp.0);
    let list = dsh::list(&paths).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "DSH 测试");
    assert_eq!(list[0].message_count, 4);
    assert_eq!(list[0].cwd, r"D:\test");

    let s = dsh::load(&paths, &id).unwrap();
    assert_eq!(s.agent, "dsh");
    assert_eq!(s.messages.len(), 4);
    assert!(matches!(s.messages[1].blocks[0], Block::Thinking(_)));
    match &s.messages[1].blocks[1] {
        Block::ToolCall { output, .. } => assert_eq!(output.as_deref(), Some("a.txt")),
        b => panic!("expected tool call, got {b:?}"),
    }
    let compaction = s.compaction.expect("compaction");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, 2);
}

#[test]
fn load_missing_session_errors_for_every_agent() {
    let tmp = common::TempDir::new();
    let q = common::empty_qoder(&tmp.0.join("qoder"));
    assert!(qoder::load(&q, "nonexistent").is_err());
    // 库里有一行但 jsonl 缺失：同样报错。
    let db = Connection::open(&q.main_db).unwrap();
    db.execute(
        "INSERT INTO chat_sessions (session_id, title, cwd, created_at, updated_at)
         VALUES ('orphan', '孤儿', 'D:\\x', 1, 2)",
        [],
    )
    .unwrap();
    let error = qoder::load(&q, "orphan").unwrap_err();
    assert!(error.to_string().contains("session jsonl not found"));

    let z = common::empty_zcode(&tmp.0.join("zcode"));
    assert!(zcode::load(&z, "nonexistent").is_err());
    let c = common::empty_cursor(&tmp.0.join("cursor"));
    assert!(cursor::load(&c, "nonexistent").is_err());
    let w = common::empty_workbuddy(&tmp.0.join("workbuddy"));
    assert!(workbuddy::load(&w, "nonexistent").is_err());
    let k = common::empty_kiro(&tmp.0.join("kiro"));
    assert!(kiro::load(&k, "nonexistent").is_err());
    let empty = tmp.0.join("claude");
    let cl = claude::Paths {
        projects: empty.join("projects"),
        home: empty,
    };
    assert!(claude::load(&cl, "nonexistent").is_err());
    let empty = tmp.0.join("dsh");
    let d = dsh::Paths {
        sessions: empty.join("sessions"),
        storages: empty.join("storages"),
        home: empty,
    };
    assert!(dsh::load(&d, "nonexistent").is_err());
}

#[test]
fn kiro_list_skips_corrupt_and_retired_dirs() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::kiro_fixture(&tmp.0);
    let workspace = paths.sessions.join(kiro::workspace_hash(r"D:\test"));

    // 损坏的 session.json：跳过该会话，不影响其他会话列出。
    let corrupt = workspace.join("sess_corrupt");
    fs::create_dir_all(&corrupt).unwrap();
    fs::write(corrupt.join("session.json"), "{not json").unwrap();

    // .retired- 前缀目录不算会话（与 .staging- 同一容错族）。
    let retired = workspace.join(".retired-xyz");
    fs::create_dir_all(&retired).unwrap();
    fs::write(
        retired.join("session.json"),
        r#"{"id":"sess_retired","title":"retired"}"#,
    )
    .unwrap();

    let list = kiro::list(&paths).unwrap();
    assert_eq!(list.len(), 1, "只应列出有效的 fixture 会话");
    assert_eq!(list[0].id, id);
}

#[test]
fn qoder_compaction_is_hidden_and_bounded() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::qoder_fixture(&tmp.0);
    let file = paths.projects.join("D--test").join(format!("{id}.jsonl"));
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(
        r#"{"type":"system","subtype":"compact_boundary","isCompactSummary":true,"timestamp":"2026-11-01T10:00:20Z","content":"Conversation compacted"}
{"type":"user","uuid":"c1","parentUuid":"a2","isCompactSummary":true,"timestamp":"2026-11-01T10:00:21Z","message":{"role":"user","content":[{"type":"text","text":"摘要正文"}]}}
{"type":"assistant","uuid":"a3","parentUuid":"c1","timestamp":"2026-11-01T10:00:30Z","message":{"role":"assistant","content":[{"type":"text","text":"压缩后回答"}]}}
"#,
    );
    fs::write(&file, text).unwrap();

    let session = qoder::load(&paths, &id).unwrap();
    let compaction = session.compaction.expect("应识别压缩边界");
    assert_eq!(compaction.summary_text, "摘要正文");
    assert_eq!(compaction.tail_start_index, session.messages.len() - 1);
    assert!(
        !session.messages.iter().any(|m| m
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text(t) if t.contains("摘要正文")))),
        "摘要不应进入消息列表"
    );
}

#[test]
fn workbuddy_compaction_is_hidden_and_bounded() {
    let tmp = common::TempDir::new();
    let (paths, id) = common::workbuddy_fixture(&tmp.0);
    let file = paths.projects.join("d-test").join(format!("{id}.jsonl"));
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(
        r#"{"id":"cmp1","timestamp":1500,"type":"message","role":"user","content":[{"type":"input_text","text":"<cb_summary>\nSummary of the conversation so far\n</cb_summary>"}],"sessionId":"wb_session_1","cwd":"d:\\test"}
{"id":"t1","timestamp":1600,"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"压缩后回答"}],"sessionId":"wb_session_1","cwd":"d:\\test"}
"#,
    );
    fs::write(&file, text).unwrap();

    let session = workbuddy::load(&paths, &id).unwrap();
    let compaction = session.compaction.expect("应识别压缩注入消息");
    assert!(compaction.summary_text.starts_with("<cb_summary>"));
    assert_eq!(compaction.tail_start_index, session.messages.len() - 1);
    assert!(
        !session.messages.iter().any(|m| m
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text(t) if t.contains("cb_summary")))),
        "压缩注入消息不应进入消息列表"
    );
}
