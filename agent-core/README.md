# agent-core

宿主与插件共用的最小基础 crate。提供统一 IR、插件协议、短命进程 SDK，以及路径、时间、文本、JSONL 与 SQLite 工具。

插件包只依赖本 crate 和自身需要的通用第三方库，不依赖宿主、Slint、UI 或宿主资源。

## 模块

| 模块 | 内容 |
|---|---|
| `adapter` | 宿主侧 `AgentAdapter` 契约、列表结果与关闭行为 |
| `ir` | 跨 agent 的统一会话、消息与内容块 |
| `jsonl` | 容错 JSONL 读取与原子文件替换 |
| `path` | Windows 路径规范化与工作区比较键 |
| `protocol` | JSON-RPC 消息、错误码、列表警告与插件注册结构 |
| `sdk` | 插件侧 `PluginAgent`、`PluginDescriptor` 与 `serve()` |
| `sqlite` | 可选的只读与可写 SQLite 连接 |
| `text` | 文本截断、拼接与工具状态归一 |
| `time` | ISO 时间、毫秒时间戳与本地时间格式化 |

## 插件入口

```rust,no_run
use std::path::PathBuf;

use agent_core::ir::{Session, SessionSummary};
use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use anyhow::{bail, Result};

struct MyAgent;

impl PluginAgent for MyAgent {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        Ok(Vec::new())
    }

    fn load(&self, _id: &str) -> Result<Session> {
        bail!("not implemented")
    }

    fn write(&self, _session: &Session, _cwd: &str, _overwrite: Option<&str>) -> Result<String> {
        bail!("not implemented")
    }

    fn delete(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    fn active(&self, _id: &str) -> Result<bool> {
        Ok(false)
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

fn main() -> Result<()> {
    serve(
        &MyAgent,
        &PluginDescriptor::new("example", "Example", "example.exe", "example", "0.1.0"),
    )
}
```

插件实现 `PluginAgent` 的 `list`、`load`、`write`、`delete`、`active` 与 `backup_paths`，其余方法按需覆写。`serve()` 完成 `plugin.register` 握手并处理 JSON-RPC 请求。完整插件包结构、manifest 模板、测试与打包步骤见仓库文档 `docs/plugin-development.md`。

单条会话无法解析但其余会话仍可用时，覆写 `list_with_warnings`，把原因放进 `AgentListing::warnings`；整个数据库或密钥不可用时仍从 `list` 返回错误。错误消息会保留 anyhow 上下文链，供宿主界面展示。

## 文档检查

```sh
RUSTDOCFLAGS="-D warnings" cargo doc -p agent-core --no-deps
```

crate 启用了 `missing_docs` 与 rustdoc 链接检查，公开项必须有文档。
