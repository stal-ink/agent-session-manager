//! 交接文档：按固定规则从会话原文裁剪重排，不调用模型、不联网。
//!
//! 规则要点：
//! - 只做过滤、截断与重排，不生成原文之外的正文内容；截断一律按字符数（chars）直接截取；
//! - 输出只依赖会话内容与开关：同一会话每次生成结果逐字节相同（顺序全部用 Vec 保序，集合仅用于判重）。

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::Value;

use super::fmt_time;
use crate::agents;
use crate::ir::{Block, Message, Role, Session};

/// 当前目标正文的字符上限。
const GOAL_LIMIT: usize = 2000;
/// 关键约束最多保留的条数（超限保留最早的）。
const CONSTRAINT_MAX: usize = 20;
/// 已完成条目中命令/文件摘要的字符上限。
const SUMMARY_LIMIT: usize = 160;
/// 已完成条目附带结果的字符上限。
const RESULT_LIMIT: usize = 200;
/// 未完成条目中错误摘要的字符上限。
const ERROR_LIMIT: usize = 200;
/// 关键路径中文件路径的条数上限。
const PATH_MAX: usize = 15;
/// 最近语境的轮数上限（一轮 = 一条用户消息 + 其后连续助手消息）。
const ROUNDS: usize = 6;
/// 最近语境单条正文的字符上限。
const ROUND_BODY_LIMIT: usize = 800;
/// 未完成小节中待办行的条数上限。
const TODO_MAX: usize = 10;

/// 纯确认语集合：整条消息去空白后完全命中即视为寒暄，跳过不作为当前目标。
const FILLERS: [&str; 9] = ["继续", "好的", "好", "谢谢", "ok", "OK", "嗯", "行", "可以"];

/// 约束词：句子包含任一即收录进「关键约束」（简化规则，直接子串匹配）。
const CONSTRAINT_WORDS: [&str; 8] = [
    "必须",
    "不要",
    "不能",
    "禁止",
    "默认",
    "务必",
    "一定要",
    "别",
];

/// 命令类工具名（小写包含匹配），输入摘要取 command 字段。
const COMMAND_TOOLS: [&str; 4] = ["bash", "powershell", "cmd", "shell"];

/// 收录进「已完成」的工具名：写文件类 + 命令类（小写包含匹配）。
const RECORDED_TOOLS: [&str; 7] = [
    "write",
    "edit",
    "notebookedit",
    "bash",
    "powershell",
    "cmd",
    "shell",
];

/// 待办线索词（与小写行文本比较，TODO 大小写均可命中）。
const TODO_WORDS: [&str; 5] = ["待", "未", "需要", "接下来", "todo"];

/// 交接文档主入口：`tool_results` 为 false 时「已完成」条目不附加结果摘要。
pub fn handoff_markdown(session: &Session, tool_results: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# 交接文档：{}", session.title);
    let _ = writeln!(out);
    let _ = writeln!(out, "- 来源 agent：{}", agents::label(&session.agent));
    let _ = writeln!(out, "- 工作区：{}", session.cwd);
    let _ = writeln!(
        out,
        "- 时间范围：{} → {}",
        fmt_time(session.created_ms),
        fmt_time(session.updated_ms)
    );
    let _ = writeln!(out, "- 消息数：{}", session.messages.len());
    let _ = writeln!(
        out,
        "- 压缩边界：{}",
        if session.compaction.is_some() {
            "是"
        } else {
            "否"
        }
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "本文档按固定规则从会话原文裁剪重排而成，未调用任何模型；用于新会话续聊上下文。"
    );

    // 一、当前目标
    let _ = writeln!(out, "\n## 当前目标\n");
    let _ = writeln!(out, "{}\n", current_goal(session));

    // 二、关键约束
    let _ = writeln!(out, "\n## 关键约束\n");
    let constraints = constraint_sentences(session);
    if constraints.is_empty() {
        let _ = writeln!(out, "（无）\n");
    } else {
        for sentence in &constraints {
            let _ = writeln!(out, "- {sentence}");
        }
        let _ = writeln!(out);
    }

    // 三、已完成
    let (completed_body, file_paths) = completed_section(session, tool_results);
    let _ = writeln!(out, "\n## 已完成\n");
    let _ = writeln!(out, "{completed_body}");

    // 四、未完成
    let _ = writeln!(out, "\n## 未完成\n");
    let unfinished = unfinished_items(session);
    if unfinished.is_empty() {
        let _ = writeln!(out, "（无）\n");
    } else {
        for item in &unfinished {
            let _ = writeln!(out, "{item}");
        }
        let _ = writeln!(out);
    }

    // 五、关键路径
    let _ = writeln!(out, "\n## 关键路径\n");
    for path in key_paths(session, &file_paths) {
        let _ = writeln!(out, "- {path}");
    }
    let _ = writeln!(out);

    // 六、最近语境
    let _ = writeln!(out, "\n## 最近语境\n");
    for (role_label, body) in recent_context(session) {
        let _ = writeln!(out, "**{role_label}**\n");
        let _ = writeln!(out, "{body}\n");
    }

    // 七、前情：只在存在压缩边界时输出，否则整节省略
    if let Some(compaction) = &session.compaction {
        let _ = writeln!(out, "\n## 前情\n");
        let _ = writeln!(out, "上下文在更早处已压缩，以下为压缩摘要原文：\n");
        let _ = writeln!(out, "{}\n", summary_body(&compaction.summary_text));
    }
    out
}

/// 按字符数截断：超出部分直接丢弃，不添加原文之外的标记。
fn cut(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// 消息中所有 Text 块正文（按行拼接）；无 Text 块时为空串。
fn text_body(message: &Message) -> String {
    let parts: Vec<&str> = message
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    parts.join("\n")
}

/// 纯确认语：整条消息去空白后长度 ≤ 6，或完全命中确认语集合。
fn is_filler(text: &str) -> bool {
    let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if squeezed.chars().count() <= 6 {
        return true;
    }
    FILLERS.iter().any(|f| squeezed.eq_ignore_ascii_case(f))
}

/// 最近一条实质性用户消息正文（往前找、跳过确认语），截断到 2000 字符；全部被过滤时返回 None。
fn goal_text(session: &Session) -> Option<String> {
    session
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::User)
        .map(text_body)
        .find(|t| !is_filler(t))
        .map(|t| cut(t.trim(), GOAL_LIMIT))
}

fn current_goal(session: &Session) -> String {
    goal_text(session).unwrap_or_else(|| "（未记录）".into())
}

/// 按 。！？\n 分句：保留终止标点，换行不计入句尾，空句丢弃，顺序保持原文。
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '。' | '！' | '？' => {
                current.push(ch);
                push_sentence(&mut sentences, &mut current);
            }
            '\n' => push_sentence(&mut sentences, &mut current),
            _ => current.push(ch),
        }
    }
    push_sentence(&mut sentences, &mut current);
    sentences
}

/// 收束当前累积的句子：去首尾空白，非空才收录。
fn push_sentence(sentences: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    current.clear();
}

/// 全部用户消息中的约束句：命中约束词、去重保序，超过上限只保留最早的。
fn constraint_sentences(session: &Session) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in &session.messages {
        if m.role != Role::User {
            continue;
        }
        for b in &m.blocks {
            if let Block::Text(t) = b {
                for sentence in split_sentences(t) {
                    let hit = CONSTRAINT_WORDS.iter().any(|w| sentence.contains(w));
                    if hit && seen.insert(sentence.clone()) {
                        out.push(sentence);
                        if out.len() == CONSTRAINT_MAX {
                            return out;
                        }
                    }
                }
            }
        }
    }
    out
}

/// 是否命令类工具（输入摘要取 command 字段）。
fn is_command_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    COMMAND_TOOLS.iter().any(|n| lower.contains(n))
}

/// 是否收录进「已完成」：写文件类或命令类，小写包含匹配。
fn is_recorded_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    RECORDED_TOOLS.iter().any(|n| lower.contains(n))
}

/// 工具输入摘要：命令类取 command，其余取 file_path；字段缺失时回退到紧凑 JSON。统一截断到 160 字符。
fn input_summary(name: &str, input: &Value) -> String {
    let field = if is_command_tool(name) {
        "command"
    } else {
        "file_path"
    };
    let raw = match input.get(field).and_then(Value::as_str) {
        Some(s) => s.trim().to_string(),
        None => input.to_string(),
    };
    cut(&raw, SUMMARY_LIMIT)
}

/// 已完成小节：非 error 的写文件/命令类工具按工具名聚合，同文件/同命令去重。
/// 返回（小节正文，其中出现过的文件路径列表）。
/// 单个工具的分节条目：(输入摘要, 结果摘要)。
type ToolEntries = Vec<(String, Option<String>)>;

fn completed_section(session: &Session, tool_results: bool) -> (String, Vec<String>) {
    // (工具名, 条目列表)，工具按首次出现顺序分节。
    let mut groups: Vec<(String, ToolEntries)> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();

    for m in &session.messages {
        for b in &m.blocks {
            let Block::ToolCall {
                name,
                input,
                output,
                status,
                ..
            } = b
            else {
                continue;
            };
            if status == "error" || !is_recorded_tool(name) {
                continue;
            }
            let summary = input_summary(name, input);
            // 附带结果：开关关闭或输出缺失（含纯空白）时不附。
            let result = if tool_results {
                output
                    .as_deref()
                    .map(str::trim)
                    .filter(|o| !o.is_empty())
                    .map(|o| cut(o, RESULT_LIMIT))
            } else {
                None
            };
            let group = match groups.iter().position(|(n, _)| n == name) {
                Some(i) => &mut groups[i].1,
                None => {
                    groups.push((name.clone(), Vec::new()));
                    let last = groups.len() - 1;
                    &mut groups[last].1
                }
            };
            if group.iter().all(|(s, _)| s != &summary) {
                group.push((summary, result));
            }
            // 关键路径复用：收集写文件类的真实文件路径。
            if !is_command_tool(name) {
                if let Some(path) = input.get("file_path").and_then(Value::as_str) {
                    if !path.trim().is_empty() && !file_paths.iter().any(|p| p == path) {
                        file_paths.push(path.to_string());
                    }
                }
            }
        }
    }

    if groups.is_empty() {
        return ("（无）".to_string(), file_paths);
    }
    let mut out = String::new();
    for (name, items) in &groups {
        let _ = writeln!(out, "### {name}\n");
        for (summary, result) in items {
            match result {
                Some(r) => {
                    let _ = writeln!(out, "- {summary}（结果：{r}）");
                }
                None => {
                    let _ = writeln!(out, "- {summary}");
                }
            }
        }
        let _ = writeln!(out);
    }
    (out, file_paths)
}

/// 待办行判定：以 `- `、`* ` 或「数字. 」开头，且包含待办线索词；返回去掉列表标记后的正文。
fn todo_item(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let body = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        rest
    } else {
        let digits = trimmed.len()
            - trimmed
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        if digits == 0 {
            return None;
        }
        trimmed[digits..].strip_prefix(". ")?
    };
    let lowered = trimmed.to_lowercase();
    if TODO_WORDS.iter().any(|w| lowered.contains(w)) {
        Some(body.trim().to_string())
    } else {
        None
    }
}

/// 未完成条目：status == "error" 的工具调用在前，随后是最后一条助手消息中的待办行（最多 10 条）。
fn unfinished_items(session: &Session) -> Vec<String> {
    let mut items = Vec::new();
    for m in &session.messages {
        for b in &m.blocks {
            if let Block::ToolCall {
                name,
                output,
                status,
                ..
            } = b
            {
                if status == "error" {
                    let detail = cut(
                        output.as_deref().map(str::trim).unwrap_or_default(),
                        ERROR_LIMIT,
                    );
                    items.push(format!("- 工具 {name} 失败：{detail}"));
                }
            }
        }
    }
    let Some(last) = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
    else {
        return items;
    };
    let mut todos = 0usize;
    for b in &last.blocks {
        if let Block::Text(t) = b {
            for line in t.lines() {
                if let Some(todo) = todo_item(line) {
                    items.push(format!("- {todo}"));
                    todos += 1;
                    if todos == TODO_MAX {
                        return items;
                    }
                }
            }
        }
    }
    items
}

/// 关键路径：工作区 cwd 一条 + 已完成小节出现过的文件路径（去重、最多 15 条）。
fn key_paths(session: &Session, file_paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if !session.cwd.trim().is_empty() {
        out.push(session.cwd.clone());
    }
    let mut count = 0usize;
    for path in file_paths {
        if path.trim().is_empty() || out.iter().any(|p| p == path) {
            continue;
        }
        out.push(path.clone());
        count += 1;
        if count == PATH_MAX {
            break;
        }
    }
    out
}

/// 最近语境：最近 6 轮（一条用户消息 + 其后连续助手消息）中的 Text 正文，单条截断到 800 字符。
fn recent_context(session: &Session) -> Vec<(&'static str, String)> {
    let user_indices: Vec<usize> = session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    let start = user_indices.len().saturating_sub(ROUNDS);
    let mut entries = Vec::new();
    for (n, &ui) in user_indices[start..].iter().enumerate() {
        let end = user_indices
            .get(start + n + 1)
            .copied()
            .unwrap_or(session.messages.len());
        for m in &session.messages[ui..end] {
            let body = text_body(m).trim().to_string();
            if body.is_empty() {
                continue;
            }
            let label = match m.role {
                Role::User => "用户",
                Role::Assistant => "助手",
            };
            entries.push((label, cut(&body, ROUND_BODY_LIMIT)));
        }
    }
    entries
}

/// 剥掉压缩摘要外层的 <cb_summary> 包裹（与 zcode writer 的 summary_body 同规则）。
fn summary_body(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix("<cb_summary>").unwrap_or(text);
    text.strip_suffix("</cb_summary>")
        .unwrap_or(text)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filler_rules_match_spec() {
        assert!(is_filler("谢谢"));
        assert!(is_filler("OK"));
        assert!(is_filler(" 好 "));
        assert!(is_filler("继续"), "确认语集合内一律跳过");
        assert!(!is_filler("把端口改成 8080 并重启"));
        assert!(!is_filler("这个改动比较大，需要你确认"));
    }

    #[test]
    fn sentences_keep_punctuation_and_split_on_newline() {
        let sentences = split_sentences("必须先编译。然后测试！可以提交吗\n最后一行");
        assert_eq!(
            sentences,
            vec!["必须先编译。", "然后测试！", "可以提交吗", "最后一行",]
        );
    }

    #[test]
    fn todo_item_requires_marker_and_keyword() {
        assert_eq!(
            todo_item("- 需要回归测试"),
            Some("需要回归测试".to_string())
        );
        assert_eq!(todo_item("* 接下来发版"), Some("接下来发版".to_string()));
        assert_eq!(todo_item("3. TODO 补文档"), Some("TODO 补文档".to_string()));
        assert_eq!(todo_item("- 普通备注"), None);
        assert_eq!(todo_item("没有标记但含需要"), None);
        assert_eq!(todo_item("2.没有空格的序号行"), None);
    }

    #[test]
    fn input_summary_prefers_field_and_truncates() {
        assert_eq!(
            input_summary("Bash", &json!({ "command": "cargo test" })),
            "cargo test"
        );
        assert_eq!(
            input_summary("Write", &json!({ "file_path": "src/a.rs" })),
            "src/a.rs"
        );
        // 字段缺失时回退到紧凑 JSON，避免凭空生成内容。
        assert_eq!(
            input_summary("Write", &json!({ "path": "x" })),
            r#"{"path":"x"}"#
        );
        let long = "长".repeat(300);
        assert_eq!(
            input_summary("Bash", &json!({ "command": long }))
                .chars()
                .count(),
            160
        );
    }

    #[test]
    fn summary_body_strips_wrapper() {
        assert_eq!(summary_body("<cb_summary>\n正文\n</cb_summary>"), "正文");
        assert_eq!(summary_body("无包裹原文"), "无包裹原文");
    }

    #[test]
    fn cut_counts_chars_not_bytes() {
        assert_eq!(cut("中文abc", 3), "中文a");
        assert_eq!(cut("中文", 10), "中文");
    }
}
