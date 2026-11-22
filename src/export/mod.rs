//! 会话导出：普通全量文档（`Mode::Normal`）与规则裁剪的交接文档（`Mode::Handoff`）。
//! 交接文档的裁剪规则见 [`handoff`]，全程不调用模型、不联网。

mod handoff;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::agents;
use crate::ir::{Block, Role, Session};
use crate::time::local_time;

pub use handoff::handoff_markdown;

const OUTPUT_LIMIT: usize = 4000;

/// 导出模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 普通文档：现有全量结构原样保留。
    Normal,
    /// 交接文档：按固定规则裁剪重排的优化上下文。
    Handoff,
}

/// 导出开关。默认全部保留工具输出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// 普通文档是否写入「输出：」段；false 时仍写工具名称与参数 JSON。
    pub normal_tool_results: bool,
    /// 交接文档是否在已完成条目后附加结果摘要。
    pub handoff_tool_results: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            normal_tool_results: true,
            handoff_tool_results: true,
        }
    }
}

impl Mode {
    /// 导出文件落盘的子目录名。
    fn sub_dir(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Handoff => "handoff",
        }
    }
}

fn fmt_time(ms: i64) -> String {
    local_time(ms, "%Y-%m-%d %H:%M")
}

/// 普通全量文档（工具输出完整保留），等价于 `markdown_with(session, true)`。
pub fn markdown(session: &Session) -> String {
    markdown_with(session, true)
}

/// 普通全量文档；`tool_results` 为 false 时工具调用仍写名称与参数 JSON，但不写「输出：」段。
fn markdown_with(session: &Session, tool_results: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# 会话交接：{}", session.title);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- 来源：{}（{}）",
        agents::label(&session.agent),
        session.id
    );
    let _ = writeln!(out, "- 工作区：{}", session.cwd);
    let _ = writeln!(
        out,
        "- 时间：{} → {}",
        fmt_time(session.created_ms),
        fmt_time(session.updated_ms)
    );
    let _ = writeln!(out, "- 消息数：{}", session.messages.len());
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "本文档由 Agent Session Manager 自动转换，作为续聊上下文使用。工具输出超过 {OUTPUT_LIMIT} 字符时截断。"
    );

    for (index, m) in session.messages.iter().enumerate() {
        if session
            .compaction
            .as_ref()
            .is_some_and(|c| c.tail_start_index == index)
        {
            let _ = writeln!(out, "\n---\n\n> 上下文已压缩\n");
        }
        let role = match m.role {
            Role::User => "用户",
            Role::Assistant => "助手",
        };
        let _ = writeln!(out, "\n## {} · {}\n", role, fmt_time(m.time_ms));
        for b in &m.blocks {
            match b {
                Block::Text(t) => {
                    let _ = writeln!(out, "{t}\n");
                }
                Block::Thinking(t) => {
                    let _ = writeln!(out, "<thinking>\n{t}\n</thinking>\n");
                }
                Block::ToolCall {
                    id,
                    name,
                    input,
                    output,
                    status,
                } => {
                    let _ = writeln!(out, "工具调用 `{name}`（{id}，{status}）\n");
                    let _ = writeln!(
                        out,
                        "```json\n{}\n```\n",
                        serde_json::to_string_pretty(input).unwrap_or_default()
                    );
                    if tool_results {
                        if let Some(o) = output {
                            let truncated = if o.chars().count() > OUTPUT_LIMIT {
                                let cut: String = o.chars().take(OUTPUT_LIMIT).collect();
                                format!("{cut}\n…（截断，原 {} 字符）", o.chars().count())
                            } else {
                                o.clone()
                            };
                            let _ = writeln!(out, "输出：\n```\n{truncated}\n```\n");
                        }
                    }
                }
                Block::Image { reference } => {
                    let _ = writeln!(out, "[图片：{reference}]\n");
                }
                Block::File { path } => {
                    let _ = writeln!(out, "[文件：{path}]\n");
                }
            }
        }
    }
    out
}

/// 导出会话到 `dir` 下对应模式的子目录（`normal/` 或 `handoff/`），
/// 文件名沿用「标签-id.md」格式，返回落盘路径。
pub fn export_to(dir: &Path, session: &Session, mode: Mode, options: &Options) -> Result<PathBuf> {
    let content = match mode {
        Mode::Normal => markdown_with(session, options.normal_tool_results),
        Mode::Handoff => handoff::handoff_markdown(session, options.handoff_tool_results),
    };
    let target = dir.join(mode.sub_dir());
    fs::create_dir_all(&target)?;
    let path = target.join(format!(
        "{}-{}.md",
        agents::label(&session.agent),
        session.id
    ));
    fs::write(&path, content)?;
    Ok(path)
}

/// 两个入口（批量导出、转接）共用的编排：读 config.json 的导出设置，
/// 把会话导出到数据目录 `exports/` 下对应模式的子目录。
/// 同一文档无论从哪个入口导出，落盘位置与内容都相同。
pub fn export_with_config(session: &Session, mode: Mode) -> Result<PathBuf> {
    let config = crate::config::load();
    let options = Options {
        normal_tool_results: config.export.normal.tool_results,
        handoff_tool_results: config.export.handoff.tool_results,
    };
    export_to(
        &crate::transfer::data_dir().join("exports"),
        session,
        mode,
        &options,
    )
}
