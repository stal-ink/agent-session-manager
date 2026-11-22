//! 跨 agent 的统一会话表示。
//!
//! IR 只保留各端都能表达的公共语义；产品私有字段由插件在读取时丢弃或降级。

use serde::{Deserialize, Serialize};

/// 消息角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// 用户。
    User,
    /// 助手。
    Assistant,
}

/// 消息内容块。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Block {
    /// 纯文本。
    Text(String),
    /// 思考或推理文本。
    Thinking(String),
    /// 工具调用。
    ToolCall {
        /// 调用 id，用于与结果配对。
        id: String,
        /// 工具名称。
        name: String,
        /// 参数。
        input: serde_json::Value,
        /// 结果正文；尚未返回时为空。
        output: Option<String>,
        /// 归一后的状态。
        status: String,
    },
    /// 图片引用，不包含二进制。
    Image {
        /// 图片地址或引用。
        reference: String,
    },
    /// 文件引用。
    File {
        /// 文件路径。
        path: String,
    },
}

/// 一条消息。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// 角色。
    pub role: Role,
    /// 消息时间，Unix 毫秒。
    pub time_ms: i64,
    /// 内容块。
    pub blocks: Vec<Block>,
}

/// 最近一次上下文压缩。
///
/// 摘要正文不进入 `messages`，也不在导出与界面展示；`tail_start_index`
/// 是 `messages` 中压缩边界后第一条消息的下标。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compaction {
    /// 压缩摘要。
    pub summary_text: String,
    /// 压缩边界后的第一条消息下标。
    pub tail_start_index: usize,
    /// 压缩时间，Unix 毫秒。
    pub time_ms: i64,
}

/// 一个完整会话。
///
/// # 示例
///
/// ```
/// use agent_core::ir::{Block, Message, Role, Session};
///
/// let session = Session {
///     agent: "example".into(),
///     id: "session-1".into(),
///     title: "示例会话".into(),
///     cwd: "C:\\work".into(),
///     created_ms: 1_000,
///     updated_ms: 2_000,
///     messages: vec![Message {
///         role: Role::User,
///         time_ms: 2_000,
///         blocks: vec![Block::Text("你好".into())],
///     }],
///     compaction: None,
/// };
/// assert_eq!(session.messages.len(), 1);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    /// 来源 agent id。
    pub agent: String,
    /// 会话 id。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 工作区路径。
    pub cwd: String,
    /// 创建时间，Unix 毫秒。
    pub created_ms: i64,
    /// 更新时间，Unix 毫秒。
    pub updated_ms: i64,
    /// 消息列表。
    pub messages: Vec<Message>,
    /// 最近一次压缩。
    pub compaction: Option<Compaction>,
}

/// 会话列表摘要。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 来源 agent id。
    pub agent: String,
    /// 会话 id。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 工作区路径。
    pub cwd: String,
    /// 创建时间，Unix 毫秒。
    pub created_ms: i64,
    /// 更新时间，Unix 毫秒。
    pub updated_ms: i64,
    /// 消息数。
    pub message_count: i64,
    /// 是否已归档。
    pub archived: bool,
    /// 是否已删除。
    pub deleted: bool,
    /// 所属工作区是否归档或删除。
    pub workspace_deleted: bool,
}

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
