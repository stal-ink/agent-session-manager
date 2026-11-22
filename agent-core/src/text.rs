//! 文本截断、拼接与工具状态归一。

use crate::ir::Block;

/// 按字符数截断，不切断多字节字符。
///
/// ```
/// assert_eq!(agent_core::text::truncate("你好世界", 2), "你好");
/// ```
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// 追加一段文本，空文本跳过，段与段之间用空行分隔。
pub fn push_text(target: &mut String, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(value);
}

/// 工具状态别名归一：failed/failure → error，canceled → cancelled，running/pending → loading。
pub fn tool_status(status: &str) -> &'static str {
    match status {
        "error" | "failed" | "failure" => "error",
        "cancelled" | "canceled" => "cancelled",
        "loading" | "running" | "pending" => "loading",
        _ => "completed",
    }
}

/// 不支持写入的块降级为纯文本引用。
pub fn blocks_fallback(blocks: &[Block]) -> String {
    let mut text = String::new();
    for block in blocks {
        match block {
            Block::Text(value) => push_text(&mut text, value),
            Block::Thinking(value) => push_text(&mut text, value),
            Block::Image { reference } => push_text(&mut text, &format!("[图片] {reference}")),
            Block::File { path } => push_text(&mut text, &format!("[文件] {path}")),
            Block::ToolCall {
                name,
                input,
                output,
                ..
            } => {
                let mut line = format!("[工具调用] {name} {input}");
                if let Some(output) = output {
                    line.push_str(&format!("\n[结果] {output}"));
                }
                push_text(&mut text, &line);
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helpers_are_stable() {
        assert_eq!(truncate("a中b", 2), "a中");
        let mut text = String::new();
        push_text(&mut text, " ");
        push_text(&mut text, "first");
        push_text(&mut text, "second");
        assert_eq!(text, "first\n\nsecond");
        assert_eq!(tool_status("failed"), "error");
        assert_eq!(tool_status("running"), "loading");
        assert_eq!(tool_status("other"), "completed");
    }
}
