//! 导出测试：普通文档与交接文档。全部手写 Session 结构，不触碰真实 agent 存储，
//! 文件操作只落在 std::env::temp_dir 下的唯一目录并由 TempDir 自动清理。

mod common;

use std::fs;

use agent_session_manager::export::{self, Mode, Options};
use agent_session_manager::ir::{Block, Compaction, Message, Role, Session};
use serde_json::json;

const T: i64 = 1_793_527_200_000;

fn base_session(messages: Vec<Message>) -> Session {
    Session {
        agent: "qoder".into(),
        id: "handoff-test".into(),
        title: "交接测试".into(),
        cwd: r"D:\work\demo".into(),
        created_ms: T,
        updated_ms: T + 60_000,
        messages,
        compaction: None,
    }
}

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        time_ms: T,
        blocks: vec![Block::Text(text.into())],
    }
}

fn assistant(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        time_ms: T,
        blocks: vec![Block::Text(text.into())],
    }
}

/// 单工具调用的助手消息。
fn assistant_tool(
    name: &str,
    input: serde_json::Value,
    output: Option<&str>,
    status: &str,
) -> Message {
    Message {
        role: Role::Assistant,
        time_ms: T,
        blocks: vec![Block::ToolCall {
            id: "call_1".into(),
            name: name.into(),
            input,
            output: output.map(Into::into),
            status: status.into(),
        }],
    }
}

/// 截取某一节的内容（从节标题到下一个二级标题之间），便于对单节内容断言。
fn section<'a>(md: &'a str, title: &str) -> &'a str {
    let marker = format!("## {title}");
    let start = md
        .find(&marker)
        .unwrap_or_else(|| panic!("缺少节标题：{marker}"));
    let rest = &md[start + marker.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// 覆盖八节内容的完整会话：约束、成功/失败工具、待办行与压缩边界。
fn full_session() -> Session {
    let mut session = base_session(vec![
        user("第一阶段：必须先跑 cargo fmt，另外不要动依赖版本。"),
        assistant_tool(
            "Write",
            json!({ "file_path": "src/lib.rs" }),
            Some("写入完成"),
            "completed",
        ),
        assistant("第一阶段完成"),
        user("现在请继续完成剩余功能，务必保持向后兼容。"),
        assistant_tool(
            "Bash",
            json!({ "command": "cargo test" }),
            Some("test result: ok"),
            "completed",
        ),
        assistant("收尾中\n- 需要补充集成测试\n- 接下来更新文档\n- 无关行"),
        user("谢谢"),
    ]);
    session.compaction = Some(Compaction {
        summary_text: "<cb_summary>\n早期讨论了架构选型\n</cb_summary>".into(),
        tail_start_index: 4,
        time_ms: T,
    });
    session
}

// ---------- 普通文档 ----------

#[test]
fn normal_export_keeps_tool_output() {
    let session = base_session(vec![
        user("列出文件"),
        assistant_tool(
            "Bash",
            json!({ "command": "ls -la" }),
            Some("a.txt\nb.txt"),
            "completed",
        ),
    ]);
    let md = export::markdown(&session);
    assert!(md.contains("# 会话交接：交接测试"));
    assert!(md.contains("工具调用 `Bash`"));
    assert!(md.contains("\"command\": \"ls -la\""));
    assert!(md.contains("输出："));
    assert!(md.contains("a.txt"));
}

#[test]
fn normal_export_hides_output_when_disabled() {
    let tmp = common::TempDir::new();
    let session = base_session(vec![
        user("列出文件"),
        assistant_tool(
            "Bash",
            json!({ "command": "ls -la" }),
            Some("a.txt"),
            "completed",
        ),
    ]);
    let options = Options {
        normal_tool_results: false,
        ..Options::default()
    };
    let path = export::export_to(&tmp.0, &session, Mode::Normal, &options).unwrap();
    assert!(
        path.starts_with(tmp.0.join("normal")),
        "必须落盘到 normal/ 子目录"
    );
    let md = fs::read_to_string(&path).unwrap();
    assert!(!md.contains("输出："), "关闭开关后不得出现输出段");
    assert!(!md.contains("a.txt"), "关闭开关后不得泄露输出内容");
    assert!(md.contains("工具调用 `Bash`"), "工具名称仍保留");
    assert!(md.contains("\"command\": \"ls -la\""), "参数 JSON 仍保留");
}

#[test]
fn normal_compaction_marker_appears_once_at_boundary() {
    let mut session = base_session(vec![
        user("第一问"),
        assistant("第一答"),
        user("第二问"),
        assistant("压缩后回答"),
    ]);
    session.compaction = Some(Compaction {
        summary_text: "早期摘要".into(),
        tail_start_index: 3,
        time_ms: T,
    });
    let md = export::markdown(&session);
    assert_eq!(md.matches("> 上下文已压缩").count(), 1, "分隔只出现一次");
    let marker = md.find("> 上下文已压缩").unwrap();
    let boundary = md.find("压缩后回答").unwrap();
    assert!(marker < boundary, "分隔必须出现在压缩边界消息之前");
    assert!(!md.contains("早期摘要"), "摘要正文不进普通文档");
}

#[test]
fn normal_export_matches_markdown_reference() {
    let tmp = common::TempDir::new();
    let session = full_session();
    let path = export::export_to(&tmp.0, &session, Mode::Normal, &Options::default()).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        export::markdown(&session),
        "默认开关下的普通导出必须与 markdown() 逐字节一致"
    );
}

#[test]
fn export_to_writes_mode_subdirectories() {
    let tmp = common::TempDir::new();
    let session = base_session(vec![user("列出文件"), assistant("有 a.txt")]);

    let normal = export::export_to(&tmp.0, &session, Mode::Normal, &Options::default()).unwrap();
    assert!(normal.starts_with(tmp.0.join("normal")));
    assert!(fs::read_to_string(&normal)
        .unwrap()
        .contains("# 会话交接："));

    let handoff = export::export_to(&tmp.0, &session, Mode::Handoff, &Options::default()).unwrap();
    assert!(handoff.starts_with(tmp.0.join("handoff")));
    assert!(fs::read_to_string(&handoff)
        .unwrap()
        .contains("# 交接文档："));

    // 没有注册插件时，标签回退为 agent id。
    assert_eq!(
        normal.file_name().and_then(|n| n.to_str()).unwrap(),
        format!("qoder-{}.md", session.id)
    );
}

// ---------- 交接文档：结构与头部 ----------

#[test]
fn handoff_sections_in_fixed_order() {
    let md = export::handoff_markdown(&full_session(), true);
    let headers = [
        "# 交接文档：",
        "## 当前目标",
        "## 关键约束",
        "## 已完成",
        "## 未完成",
        "## 关键路径",
        "## 最近语境",
        "## 前情",
    ];
    let mut pos = 0usize;
    for header in headers {
        let found = md[pos..]
            .find(header)
            .unwrap_or_else(|| panic!("缺少标题：{header}"))
            + pos;
        assert!(found >= pos, "标题必须按固定顺序出现：{header}");
        pos = found + header.len();
    }
}

#[test]
fn handoff_header_lists_context_facts() {
    let session = base_session(vec![
        user("这个问题需要仔细分析，请注意边界情况。"),
        assistant("好的，我来分析。"),
    ]);
    let md = export::handoff_markdown(&session, true);
    assert!(md.contains("# 交接文档：交接测试"));
    assert!(md.contains("- 来源 agent：qoder"));
    assert!(md.contains("- 工作区：D:\\work\\demo"));
    assert!(md.contains("- 时间范围："));
    assert!(md.contains("- 消息数：2"));
    assert!(md.contains("- 压缩边界：否"));
    assert!(md.contains(
        "本文档按固定规则从会话原文裁剪重排而成，未调用任何模型；用于新会话续聊上下文。"
    ));
    assert!(!md.contains("## 前情"), "无压缩时前情整节省略");
}

// ---------- 交接文档：当前目标 ----------

#[test]
fn handoff_goal_skips_confirmation_messages() {
    let session = base_session(vec![
        user("请把端口号改成 8080，并更新对应文档。"),
        assistant("已修改完成"),
        user("谢谢"),
    ]);
    let md = export::handoff_markdown(&session, true);
    let goal = section(&md, "当前目标");
    assert!(goal.contains("请把端口号改成 8080"), "应取上一条实质性消息");
    assert!(!goal.contains("谢谢"), "确认语必须被过滤");
}

#[test]
fn handoff_goal_falls_back_when_all_confirmations() {
    let session = base_session(vec![user("好的"), assistant("收到"), user("继续")]);
    let md = export::handoff_markdown(&session, true);
    assert!(section(&md, "当前目标").contains("（未记录）"));
}

#[test]
fn handoff_goal_truncated_to_2000_chars() {
    let long = "这是一个很长的目标描述。".repeat(400);
    let session = base_session(vec![user(&long)]);
    let md = export::handoff_markdown(&session, true);
    let goal = section(&md, "当前目标").trim().to_string();
    assert_eq!(goal.chars().count(), 2000, "正文按字符截断到 2000");
    assert!(!md.contains(&long), "全文不得原样出现");
}

// ---------- 交接文档：关键约束 ----------

#[test]
fn handoff_constraints_extracted_and_deduped() {
    let session = base_session(vec![
        user("所有改动必须通过测试。界面不要新增颜色。"),
        assistant("明白"),
        user("界面不要新增颜色。另外禁止直接写数据库。"),
    ]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "关键约束");
    assert!(sec.contains("- 所有改动必须通过测试。"));
    assert_eq!(
        sec.matches("界面不要新增颜色。").count(),
        1,
        "重复约束句应去重"
    );
    assert!(sec.contains("- 另外禁止直接写数据库。"), "整句原样保留");
    assert!(!sec.contains("明白"), "助手消息不参与约束提取");
    let must = sec.find("所有改动必须通过测试。").unwrap();
    let forbid = sec.find("禁止直接写数据库。").unwrap();
    assert!(must < forbid, "约束句按原文顺序排列");
}

#[test]
fn handoff_constraints_empty_placeholder() {
    let session = base_session(vec![user("随便聊聊天气吧"), assistant("好的")]);
    let md = export::handoff_markdown(&session, true);
    assert!(section(&md, "关键约束").contains("（无）"));
}

#[test]
fn handoff_constraints_keep_earliest_20() {
    let messages: Vec<Message> = (1..=25)
        .map(|i| user(&format!("事项{i}：必须记录第{i}项要求。")))
        .collect();
    let session = base_session(messages);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "关键约束");
    let lines = sec.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(lines, 20, "超过 20 条只保留最早的 20 条");
    assert!(sec.contains("事项1："));
    assert!(sec.contains("事项20："));
    assert!(!sec.contains("事项21："), "更晚的约束句应被丢弃");
}

// ---------- 交接文档：已完成 ----------

#[test]
fn handoff_completed_groups_and_dedupes() {
    let session = base_session(vec![
        assistant_tool(
            "Write",
            json!({ "file_path": "src/a.rs" }),
            Some("ok"),
            "completed",
        ),
        assistant_tool(
            "Write",
            json!({ "file_path": "src/a.rs" }),
            Some("ok again"),
            "completed",
        ),
        assistant_tool(
            "Write",
            json!({ "file_path": "src/b.rs" }),
            Some("done"),
            "completed",
        ),
        assistant_tool(
            "Bash",
            json!({ "command": "cargo build" }),
            Some("built"),
            "completed",
        ),
        assistant_tool(
            "Bash",
            json!({ "command": "cargo build" }),
            Some("built twice"),
            "completed",
        ),
        assistant_tool(
            "Read",
            json!({ "file_path": "src/c.rs" }),
            Some("content"),
            "completed",
        ),
    ]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "已完成");
    assert!(sec.contains("### Write"));
    assert!(sec.contains("### Bash"));
    assert_eq!(sec.matches("src/a.rs").count(), 1, "同文件应去重");
    assert_eq!(sec.matches("cargo build").count(), 1, "同命令应去重");
    assert!(sec.contains("src/b.rs"));
    assert!(!sec.contains("Read"), "非收录类工具不进已完成");
    assert!(!sec.contains("src/c.rs"));
    assert!(sec.contains("（结果：ok）"));
    assert!(!sec.contains("ok again"), "去重后只保留首次结果");
    assert!(!sec.contains("built twice"));
    let write = sec.find("### Write").unwrap();
    let bash = sec.find("### Bash").unwrap();
    assert!(write < bash, "工具小节按首次出现顺序排列");
}

#[test]
fn handoff_completed_omits_results_when_disabled() {
    let session = base_session(vec![
        assistant_tool(
            "Write",
            json!({ "file_path": "src/a.rs" }),
            Some("写入完成"),
            "completed",
        ),
        assistant_tool(
            "Bash",
            json!({ "command": "cargo test" }),
            Some("测试通过"),
            "completed",
        ),
    ]);
    let md = export::handoff_markdown(&session, false);
    let sec = section(&md, "已完成");
    assert!(sec.contains("- src/a.rs"));
    assert!(sec.contains("- cargo test"));
    assert!(!md.contains("（结果："), "关闭开关后不附加结果");
}

#[test]
fn handoff_completed_empty_placeholder() {
    let session = base_session(vec![user("帮忙看看"), assistant("看完了")]);
    let md = export::handoff_markdown(&session, true);
    assert!(section(&md, "已完成").contains("（无）"));
}

// ---------- 交接文档：未完成 ----------

#[test]
fn handoff_unfinished_collects_errors_and_todos() {
    let session = base_session(vec![
        user("运行测试"),
        assistant_tool(
            "Bash",
            json!({ "command": "cargo test" }),
            Some("编译失败：缺少字段"),
            "error",
        ),
        assistant_tool("Bash", json!({ "command": "ls" }), Some("a.txt"), "completed"),
        assistant("后续安排\n- 需要修复编译错误\n* 接下来跑冒烟验证\n3. TODO 补文档\n- 普通备注行\n没有标记的行"),
    ]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "未完成");
    assert!(sec.contains("- 工具 Bash 失败：编译失败：缺少字段"));
    assert!(sec.contains("- 需要修复编译错误"), "- 标记待办行应收录");
    assert!(sec.contains("- 接下来跑冒烟验证"), "* 标记待办行应收录");
    assert!(sec.contains("- TODO 补文档"), "数字标记待办行应收录");
    assert!(!sec.contains("普通备注行"), "无待办线索词的行应排除");
    assert!(!sec.contains("没有标记的行"), "无列表标记的行应排除");
    let done = section(&md, "已完成");
    assert!(!done.contains("cargo test"), "失败工具不得进已完成");
    assert!(done.contains("- ls"), "成功工具照常收录");
}

#[test]
fn handoff_todos_capped_at_ten() {
    let todo_lines: Vec<String> = (1..=12).map(|i| format!("- 待办第{i}条")).collect();
    let session = base_session(vec![
        user("开始"),
        assistant(&format!("安排如下\n{}", todo_lines.join("\n"))),
    ]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "未完成");
    let lines = sec.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(lines, 10, "待办行最多 10 条");
    assert!(sec.contains("待办第10条"));
    assert!(!sec.contains("待办第11条"), "超出的待办行应被丢弃");
}

// ---------- 交接文档：关键路径 ----------

#[test]
fn handoff_key_paths_lists_cwd_and_written_files() {
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(assistant_tool(
            "Write",
            json!({ "file_path": format!("src/file_{i}.rs") }),
            None,
            "completed",
        ));
    }
    messages.push(assistant_tool(
        "Write",
        json!({ "file_path": "src/file_0.rs" }),
        None,
        "completed",
    ));
    let session = base_session(messages);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "关键路径");
    let lines: Vec<&str> = sec.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(lines[0], "- D:\\work\\demo", "工作区排第一条");
    assert_eq!(lines.len(), 16, "cwd + 最多 15 条文件路径");
    assert!(sec.contains("src/file_14.rs"));
    assert!(!sec.contains("src/file_15.rs"), "超出 15 条的路径应被丢弃");
}

// ---------- 交接文档：最近语境 ----------

#[test]
fn handoff_recent_context_keeps_last_six_rounds() {
    let mut messages = Vec::new();
    for i in 1..=8 {
        messages.push(user(&format!("用户问题{i}")));
        messages.push(assistant(&format!("助手回答{i}")));
    }
    let md = export::handoff_markdown(&base_session(messages), true);
    let sec = section(&md, "最近语境");
    assert_eq!(sec.matches("**用户**").count(), 6, "只保留最近 6 轮");
    assert_eq!(sec.matches("**助手**").count(), 6);
    assert!(sec.contains("用户问题3"), "第 3 轮是最近 6 轮的第一轮");
    assert!(!sec.contains("用户问题2"));
    assert!(!sec.contains("助手回答1"));
}

#[test]
fn handoff_recent_context_only_text_blocks() {
    let session = base_session(vec![
        user("真正的问题描述，需要完整保留。"),
        Message {
            role: Role::Assistant,
            time_ms: T,
            blocks: vec![
                Block::Thinking("内部推理不该出现".into()),
                Block::ToolCall {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: json!({ "command": "ls" }),
                    output: Some("ok".into()),
                    status: "completed".into(),
                },
                Block::Text("可见回答".into()),
            ],
        },
    ]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "最近语境");
    assert!(sec.contains("**用户**"));
    assert!(sec.contains("真正的问题描述"));
    assert!(sec.contains("**助手**"));
    assert!(sec.contains("可见回答"));
    assert!(!sec.contains("内部推理不该出现"), "thinking 块不进最近语境");
    assert!(!sec.contains("<thinking>"));
    assert!(!sec.contains("Bash"), "工具调用不进最近语境");
}

#[test]
fn handoff_recent_context_truncates_to_800_chars() {
    let long = "很长的回答内容".repeat(200);
    let session = base_session(vec![user("请展开说明"), assistant(&long)]);
    let md = export::handoff_markdown(&session, true);
    let sec = section(&md, "最近语境");
    let prefix: String = long.chars().take(800).collect();
    assert!(sec.contains(&prefix), "应保留前 800 字符");
    assert!(!sec.contains(&long), "超出部分必须截断");
}

// ---------- 交接文档：前情与确定性 ----------

#[test]
fn handoff_compaction_summary_strips_cb_tags() {
    let mut session = base_session(vec![user("早前的问题"), assistant("早前的回答")]);
    session.compaction = Some(Compaction {
        summary_text: "<cb_summary>\n早期讨论了数据模型\n</cb_summary>".into(),
        tail_start_index: 0,
        time_ms: T,
    });
    let md = export::handoff_markdown(&session, true);
    assert!(md.contains("- 压缩边界：是"));
    let sec = section(&md, "前情");
    assert!(sec.contains("上下文在更早处已压缩，以下为压缩摘要原文："));
    assert!(sec.contains("早期讨论了数据模型"));
    assert!(!md.contains("<cb_summary>"), "cb_summary 标签必须剥掉");
}

#[test]
fn handoff_is_deterministic() {
    let session = full_session();
    let first = export::handoff_markdown(&session, true);
    let second = export::handoff_markdown(&session, true);
    assert_eq!(first, second, "同一会话两次生成必须逐字节一致");
    assert_ne!(
        first,
        export::handoff_markdown(&session, false),
        "开关不同时结果应不同"
    );
}
