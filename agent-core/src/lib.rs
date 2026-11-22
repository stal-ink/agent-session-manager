#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![doc = include_str!("../README.md")]

/// 宿主侧适配器契约与关闭行为。
pub mod adapter;
/// 跨 agent 的统一会话、消息与内容块。
pub mod ir;
/// 容错 JSONL 读取与原子文件替换。
pub mod jsonl;
/// Windows 路径规范化与工作区比较键。
pub mod path;
/// JSON-RPC 消息、错误码与插件注册结构。
pub mod protocol;
/// 插件侧入口、请求分发与注册握手。
pub mod sdk;
/// 可选的只读与可写 SQLite 连接。
#[cfg(feature = "sqlite")]
pub mod sqlite;
/// 文本截断、拼接与工具状态归一。
pub mod text;
/// ISO 时间、毫秒时间戳与本地时间格式化。
pub mod time;

pub use adapter::{AgentAdapter, CloseBehavior};
pub use ir::*;
