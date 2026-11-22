//! 插件 JSON-RPC 消息、错误码与注册结构。
//!
//! 插件与宿主通过 stdin/stdout 逐行传输 UTF-8 JSON。每条请求与响应各占一行。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 版本字符串。
pub const JSONRPC: &str = "2.0";
/// 插件协议版本。
pub const PROTOCOL_VERSION: u32 = 1;
/// IR 版本。
pub const IR_VERSION: u32 = 1;
/// 单条 JSON-RPC 消息的最大字节数。
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// 注册申请的请求 id。
pub const REGISTER_ID: u64 = 0;
/// 首个业务请求的 id。
pub const REQUEST_ID: u64 = 1;

/// JSON 解析失败。
pub const PARSE_ERROR: i32 = -32700;
/// 请求格式无效。
pub const INVALID_REQUEST: i32 = -32600;
/// 方法不存在。
pub const METHOD_NOT_FOUND: i32 = -32601;
/// 参数无效。
pub const INVALID_PARAMS: i32 = -32602;
/// 插件内部错误。
pub const INTERNAL_ERROR: i32 = -32603;
/// 插件注册冲突。
pub const PLUGIN_CONFLICT: i32 = -32001;
/// 插件请求超时。
pub const PLUGIN_TIMEOUT: i32 = -32002;

/// JSON-RPC 请求。
///
/// # 示例
///
/// ```
/// use agent_core::protocol::RpcRequest;
///
/// let request = RpcRequest::new(1, "agent.list", serde_json::json!({}));
/// assert_eq!(request.jsonrpc, "2.0");
/// assert_eq!(request.method, "agent.list");
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpcRequest {
    /// 协议版本，固定为 [`JSONRPC`]。
    pub jsonrpc: String,
    /// 请求 id。
    pub id: u64,
    /// 方法名。
    pub method: String,
    /// 参数。
    pub params: Value,
}

impl RpcRequest {
    /// 构造请求。
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 响应。
///
/// # 示例
///
/// ```
/// use agent_core::protocol::RpcResponse;
///
/// let response = RpcResponse::success(1, serde_json::json!({ "sessions": [] }));
/// assert!(response.error.is_none());
/// assert_eq!(response.into_result().unwrap()["sessions"], serde_json::json!([]));
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpcResponse {
    /// 协议版本，固定为 [`JSONRPC`]。
    pub jsonrpc: String,
    /// 与请求对应的 id。
    pub id: u64,
    /// 成功结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// 构造成功响应。
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// 构造错误响应；`kind` 写入 `error.data.kind`。
    pub fn failure(id: u64, code: i32, kind: &str, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: Some(serde_json::json!({ "kind": kind })),
            }),
        }
    }

    /// 校验版本、id 与 result/error 二选一约束。
    pub fn validate(&self, expected_id: u64) -> anyhow::Result<()> {
        if self.jsonrpc != JSONRPC {
            anyhow::bail!("插件响应的 JSON-RPC 版本无效");
        }
        if self.id != expected_id {
            anyhow::bail!("插件响应 id 不匹配：期望 {expected_id}，收到 {}", self.id);
        }
        if self.result.is_some() == self.error.is_some() {
            anyhow::bail!("插件响应必须且只能包含 result 或 error");
        }
        Ok(())
    }

    /// 取出成功结果；错误响应转为 `anyhow::Error`。
    pub fn into_result(self) -> anyhow::Result<Value> {
        if let Some(error) = self.error {
            let kind = error
                .data
                .as_ref()
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("internal");
            anyhow::bail!("插件错误 {kind}（{}）：{}", error.code, error.message);
        }
        self.result
            .ok_or_else(|| anyhow::anyhow!("插件响应缺少 result"))
    }
}

/// JSON-RPC 错误。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    /// 错误码。
    pub code: i32,
    /// 错误消息。
    pub message: String,
    /// 附加数据，通常包含 `kind`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `agent.list` 成功结果中的非致命诊断。
///
/// 插件无法解析某个会话但其余会话仍可用时返回。`kind` 由插件定义，宿主
/// 只负责展示，不按类别分支。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginWarning {
    /// 插件定义的诊断类别。
    pub kind: String,
    /// 面向用户的说明。
    pub message: String,
    /// 相关会话 id；非会话级问题为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// 插件注册申请。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistration {
    /// manifest schema。
    pub schema: u32,
    /// 插件 id。
    pub id: String,
    /// 插件版本。
    pub version: String,
    /// 插件协议版本。
    pub protocol: u32,
    /// IR 版本。
    pub ir_version: u32,
    /// 插件可执行文件 SHA-256。
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_error_golden_shapes_are_stable() {
        let request = RpcRequest::new(1, "agent.list", serde_json::json!({}));
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"method":"agent.list","params":{}}"#
        );
        let response = RpcResponse::failure(1, PLUGIN_CONFLICT, "plugin_conflict", "冲突");
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["error"]["code"], -32001);
        assert_eq!(value["error"]["data"]["kind"], "plugin_conflict");
        assert!(value.get("result").is_none());
    }

    #[test]
    fn response_rejects_wrong_id_and_ambiguous_body() {
        let mut response = RpcResponse::success(2, serde_json::json!({}));
        assert!(response.validate(1).is_err());
        response.id = 1;
        response.error = Some(RpcError {
            code: INTERNAL_ERROR,
            message: "bad".into(),
            data: None,
        });
        assert!(response.validate(1).is_err());
    }
}
