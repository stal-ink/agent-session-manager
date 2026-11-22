//! 插件侧 SDK：实现 [`PluginAgent`](crate::sdk::PluginAgent)，再调用 [`serve`](crate::sdk::serve) 启动短命进程。
//!
//! 插件从 stdin 读取 JSON-RPC 请求，向 stdout 写响应；stderr 只用于日志。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::adapter::AgentListing;
use crate::protocol::{
    PluginRegistration, RpcRequest, RpcResponse, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST,
    JSONRPC, MAX_MESSAGE_BYTES, METHOD_NOT_FOUND, PARSE_ERROR, REGISTER_ID,
};
use crate::{Session, SessionSummary};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 插件实现的会话能力。
///
/// 完整实现与启动示例见 [`serve`] 的文档。
pub trait PluginAgent: Send + Sync {
    /// 列出会话摘要。
    fn list(&self) -> Result<Vec<SessionSummary>>;
    /// 列出会话摘要与可恢复诊断。
    ///
    /// 插件跳过无法解析的单条会话时应覆盖本方法，把原因放进
    /// [`AgentListing::warnings`](crate::adapter::AgentListing::warnings)，
    /// 不要让整个列表失败。默认实现返回空警告。
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
    /// 物理销毁会话，不可恢复。默认复用 [`PluginAgent::delete`]。
    fn destroy(&self, id: &str) -> Result<()> {
        self.delete(id)
    }
    /// 查询会话是否活跃。查询失败必须保留错误。
    fn active(&self, id: &str) -> Result<bool>;
    /// 查询备份范围。查询失败必须保留错误；空列表只代表真实的空范围。
    fn backup_paths(&self) -> Result<Vec<PathBuf>>;
    /// 打开会话前的产物修正。
    fn prepare_open(&self, _id: Option<&str>) -> Result<()> {
        Ok(())
    }
    /// 关闭目标应用前的检查。
    fn ensure_closable(&self) -> Result<()> {
        Ok(())
    }
    /// 客户端当前打开的会话 id；无法读取时返回 `None`。
    fn open_session_ids(&self) -> Option<Vec<String>> {
        None
    }
    /// 已存在的目标会话是否必须重建。
    fn needs_rewrite(&self, _id: &str) -> bool {
        false
    }
}

/// 插件注册与 `agent.describe` 使用的描述信息。
#[derive(Clone, Debug, Serialize)]
pub struct PluginDescriptor {
    /// manifest schema。
    pub schema: u32,
    /// 插件 id。
    pub id: &'static str,
    /// 产品显示名。
    pub label: &'static str,
    /// 插件版本。
    pub version: &'static str,
    /// 用于进程检测的映像名。
    pub process: &'static str,
    /// 默认启动目标。
    pub launch: &'static str,
    /// 是否可作为转接目标。
    pub can_write: bool,
    /// 写入前是否要求目标进程退出。
    pub requires_exclusive: bool,
    /// 写入失败是否用整库备份回滚。
    pub restore_on_error: bool,
}

impl PluginDescriptor {
    /// 构造描述信息，schema 固定为 2。
    ///
    /// # 示例
    ///
    /// ```
    /// use agent_core::sdk::PluginDescriptor;
    ///
    /// let descriptor = PluginDescriptor::new(
    ///     "example", "Example", "example.exe", "example", "0.1.0",
    /// );
    /// assert_eq!(descriptor.schema, 2);
    /// assert!(descriptor.can_write);
    /// ```
    pub const fn new(
        id: &'static str,
        label: &'static str,
        process: &'static str,
        launch: &'static str,
        version: &'static str,
    ) -> Self {
        Self {
            schema: 2,
            id,
            label,
            version,
            process,
            launch,
            can_write: true,
            requires_exclusive: true,
            restore_on_error: true,
        }
    }

    fn registration(&self) -> Result<PluginRegistration> {
        let executable = std::env::current_exe().context("无法确定插件可执行文件路径")?;
        Ok(PluginRegistration {
            schema: self.schema,
            id: self.id.into(),
            version: self.version.into(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            ir_version: crate::protocol::IR_VERSION,
            sha256: sha256_file(&executable)?,
        })
    }
}

/// 短命插件入口：先发送 `plugin.register` 申请，宿主接受后处理 JSON-RPC 请求。
///
/// stdin 关闭即退出。单条消息超过 [`crate::protocol::MAX_MESSAGE_BYTES`] 时返回错误。
///
/// # 示例
///
/// ```rust,no_run
/// use std::path::PathBuf;
///
/// use agent_core::ir::{Session, SessionSummary};
/// use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
/// use anyhow::{bail, Result};
///
/// struct Example;
///
/// # impl PluginAgent for Example {
/// #     fn list(&self) -> Result<Vec<SessionSummary>> { Ok(Vec::new()) }
/// #     fn load(&self, _id: &str) -> Result<Session> { bail!("not implemented") }
/// #     fn write(&self, _session: &Session, _cwd: &str, _overwrite: Option<&str>) -> Result<String> {
/// #         bail!("not implemented")
/// #     }
/// #     fn delete(&self, _id: &str) -> Result<()> { Ok(()) }
/// #     fn active(&self, _id: &str) -> Result<bool> { Ok(false) }
/// #     fn backup_paths(&self) -> Result<Vec<PathBuf>> { Ok(Vec::new()) }
/// # }
/// fn main() -> Result<()> {
///     serve(
///         &Example,
///         &PluginDescriptor::new("example", "Example", "example.exe", "example", "0.1.0"),
///     )
/// }
/// ```
pub fn serve(agent: &dyn PluginAgent, descriptor: &PluginDescriptor) -> Result<()> {
    let mut stdin = BufReader::new(std::io::stdin().lock());
    let mut stdout = std::io::stdout().lock();
    write_line(
        &mut stdout,
        &RpcRequest::new(
            REGISTER_ID,
            "plugin.register",
            serde_json::to_value(descriptor.registration()?)?,
        ),
    )?;
    let response: RpcResponse =
        serde_json::from_str(&read_line(&mut stdin)?.context("宿主未响应插件注册申请")?)
            .context("解析宿主注册响应失败")?;
    response.validate(REGISTER_ID)?;
    let accepted = response
        .into_result()?
        .get("accepted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !accepted {
        anyhow::bail!("宿主拒绝了插件注册申请");
    }

    while let Some(line) = read_line(&mut stdin)? {
        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(request) if request.jsonrpc == JSONRPC => {
                let id = request.id;
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(agent, descriptor, &request)
                })) {
                    Ok(Ok(result)) => RpcResponse::success(id, result),
                    Ok(Err(error)) => error.into_response(id),
                    Err(_) => RpcResponse::failure(
                        id,
                        INTERNAL_ERROR,
                        "internal",
                        "插件处理请求时发生内部错误",
                    ),
                }
            }
            Ok(request) => RpcResponse::failure(
                request.id,
                INVALID_REQUEST,
                "invalid_request",
                "JSON-RPC 版本必须为 2.0",
            ),
            Err(error) => RpcResponse::failure(
                0,
                PARSE_ERROR,
                "parse_error",
                format!("JSON-RPC 解析失败：{error}"),
            ),
        };
        write_line(&mut stdout, &response)?;
    }
    Ok(())
}

struct DispatchError {
    code: i32,
    kind: &'static str,
    error: anyhow::Error,
}

impl DispatchError {
    fn invalid_params(error: anyhow::Error) -> Self {
        Self {
            code: INVALID_PARAMS,
            kind: "invalid_params",
            error,
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        Self {
            code: INTERNAL_ERROR,
            kind: "internal",
            error,
        }
    }

    fn into_response(self, id: u64) -> RpcResponse {
        RpcResponse::failure(id, self.code, self.kind, format!("{:#}", self.error))
    }
}

fn dispatch(
    agent: &dyn PluginAgent,
    descriptor: &PluginDescriptor,
    request: &RpcRequest,
) -> std::result::Result<Value, DispatchError> {
    match request.method.as_str() {
        "agent.describe" => {
            serde_json::to_value(descriptor).map_err(|error| DispatchError::internal(error.into()))
        }
        "agent.list" => agent
            .list_with_warnings()
            .map(|listing| {
                json!({
                    "sessions": listing.sessions,
                    "warnings": listing.warnings,
                })
            })
            .map_err(DispatchError::internal),
        "agent.load" => {
            let id = string_param(request, "id")?;
            agent
                .load(id)
                .map(|session| json!({ "session": session }))
                .map_err(DispatchError::internal)
        }
        "agent.write" => {
            let session: Session = serde_json::from_value(
                request
                    .params
                    .get("session")
                    .cloned()
                    .context("agent.write 缺少 session")
                    .map_err(DispatchError::invalid_params)?,
            )
            .map_err(|error| DispatchError::invalid_params(error.into()))?;
            let cwd = request
                .params
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let overwrite = request.params.get("overwrite").and_then(Value::as_str);
            agent
                .write(&session, cwd, overwrite)
                .map(|target_id| json!({ "target_id": target_id }))
                .map_err(DispatchError::internal)
        }
        "agent.delete" => agent
            .delete(string_param(request, "id")?)
            .map(|()| json!({}))
            .map_err(DispatchError::internal),
        "agent.destroy" => agent
            .destroy(string_param(request, "id")?)
            .map(|()| json!({}))
            .map_err(DispatchError::internal),
        "agent.active" => agent
            .active(string_param(request, "id")?)
            .map(|active| json!({ "active": active }))
            .map_err(DispatchError::internal),
        "agent.backup_paths" => agent
            .backup_paths()
            .map(|paths| {
                let paths: Vec<String> = paths
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                json!({ "paths": paths })
            })
            .map_err(DispatchError::internal),
        "agent.needs_rewrite" => Ok(json!({
            "needs_rewrite": agent.needs_rewrite(string_param(request, "id")?)
        })),
        "agent.prepare_open" => {
            let id = request.params.get("id").and_then(Value::as_str);
            agent
                .prepare_open(id)
                .map(|()| json!({}))
                .map_err(DispatchError::internal)
        }
        "agent.ensure_closable" => agent
            .ensure_closable()
            .map(|()| json!({}))
            .map_err(DispatchError::internal),
        "agent.open_session_ids" => Ok(json!({
            "ids": agent.open_session_ids()
        })),
        _ => Err(DispatchError {
            code: METHOD_NOT_FOUND,
            kind: "unsupported",
            error: anyhow::anyhow!("未知方法 {}", request.method),
        }),
    }
}

fn string_param<'a>(
    request: &'a RpcRequest,
    name: &str,
) -> std::result::Result<&'a str, DispatchError> {
    request
        .params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{} 缺少字符串参数 {name}", request.method))
        .map_err(DispatchError::invalid_params)
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if line.len() > MAX_MESSAGE_BYTES {
        anyhow::bail!("JSON-RPC 消息超过 {MAX_MESSAGE_BYTES} 字节");
    }
    Ok(Some(line))
}

fn write_line(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PluginWarning;

    struct WarningAgent;

    impl PluginAgent for WarningAgent {
        fn list(&self) -> Result<Vec<SessionSummary>> {
            Ok(Vec::new())
        }

        fn load(&self, _id: &str) -> Result<Session> {
            anyhow::bail!("unused")
        }

        fn write(
            &self,
            _session: &Session,
            _cwd: &str,
            _overwrite: Option<&str>,
        ) -> Result<String> {
            anyhow::bail!("unused")
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

        fn list_with_warnings(&self) -> Result<AgentListing> {
            Ok(AgentListing {
                sessions: Vec::new(),
                warnings: vec![PluginWarning {
                    kind: "parse_skipped".into(),
                    message: "跳过损坏会话".into(),
                    session_id: Some("bad".into()),
                }],
            })
        }
    }

    #[test]
    fn list_dispatch_includes_warnings() {
        let request = RpcRequest::new(1, "agent.list", serde_json::json!({}));
        let descriptor =
            PluginDescriptor::new("example", "Example", "example.exe", "example", "0.1.0");
        let result = dispatch(&WarningAgent, &descriptor, &request)
            .map_err(|error| error.error.to_string())
            .unwrap();
        assert_eq!(result["warnings"][0]["kind"], "parse_skipped");
        assert_eq!(result["warnings"][0]["session_id"], "bad");
    }

    #[test]
    fn dispatch_errors_keep_the_context_chain() {
        let error = anyhow::anyhow!("inner").context("outer");
        let message = DispatchError::internal(error)
            .into_response(1)
            .error
            .unwrap()
            .message;
        assert!(message.contains("outer"));
        assert!(message.contains("inner"));
    }
}
