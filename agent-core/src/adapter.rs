//! 宿主侧适配器契约。
//!
//! 宿主只由通用 `ProcessPluginAdapter` 实现本契约；插件进程通过 JSON-RPC
//! 实现同一组能力。软删除与物理销毁、排他与回滚策略都由 manifest 声明。

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;

use crate::ir::{Session, SessionSummary};
use crate::protocol::PluginWarning;

/// 关闭目标应用时的行为。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseBehavior {
    /// 按运行时注册表中的 process 关闭。
    Process,
    /// 关闭另一个桌面进程。
    Image(String),
    /// 不结束进程，只检查会话锁。
    CheckOnly,
}

/// 会话列表及插件附带的可恢复诊断。
#[derive(Clone, Debug, Default)]
pub struct AgentListing {
    /// 成功读取的会话摘要。
    pub sessions: Vec<SessionSummary>,
    /// 插件跳过但未让整个列表失败的问题。
    pub warnings: Vec<PluginWarning>,
}

/// 宿主与插件之间的会话操作契约。
pub trait AgentAdapter: Send + Sync {
    /// 列出会话摘要。
    fn list(&self) -> Result<Vec<SessionSummary>>;
    /// 列出会话摘要与可恢复诊断。
    ///
    /// 默认实现丢弃警告；需要报告单条会话解析失败的适配器覆盖本方法。
    fn list_with_warnings(&self) -> Result<AgentListing> {
        Ok(AgentListing {
            sessions: self.list()?,
            warnings: Vec::new(),
        })
    }
    /// 读取单个会话并转换为统一 IR。
    fn load(&self, id: &str) -> Result<Session>;
    /// 写入会话；`overwrite` 非空时覆盖已有目标会话。
    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String>;
    /// 删除会话。
    fn delete(&self, id: &str) -> Result<()>;
    /// 物理销毁会话，不可恢复。删除本身就是物理删除的插件可直接复用 `delete`。
    fn destroy(&self, id: &str) -> Result<()> {
        self.delete(id)
    }
    /// 查询会话是否活跃。查询失败必须保留错误，不能误判为“不活跃”。
    fn active(&self, id: &str) -> Result<bool>;
    /// 查询备份范围。查询失败必须保留错误；空列表只代表真实的空范围。
    fn backup_paths(&self) -> Result<Vec<PathBuf>>;
    /// 写入共享存储前是否必须确认目标进程已退出。
    fn requires_exclusive(&self) -> bool {
        true
    }
    /// 写入失败时是否用整库备份回滚。
    fn restore_on_error(&self) -> bool {
        true
    }
    /// 是否有可探测的桌面进程。
    fn tracks_process(&self) -> bool {
        true
    }
    /// 打开会话前的产物修正。
    fn prepare_open(&self, _session_id: Option<&str>) -> Result<()> {
        Ok(())
    }
    /// 关闭目标应用前的检查。
    fn ensure_closable(&self) -> Result<()> {
        Ok(())
    }
    /// 关闭目标应用时使用的行为。
    fn close_behavior(&self) -> CloseBehavior {
        CloseBehavior::Process
    }
    /// 客户端当前打开的会话 id；无法读取时返回 `None`。
    fn open_session_ids(&self) -> Option<HashSet<String>> {
        None
    }
    /// doctor 展示的存储路径，键是给人看的中文标签。
    fn diagnostics(&self) -> Vec<(&'static str, PathBuf)> {
        Vec::new()
    }
    /// 已存在的目标会话是否必须重建。
    fn needs_rewrite(&self, _id: &str) -> bool {
        false
    }
}
