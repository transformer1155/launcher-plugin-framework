//! MCP 语义对接层：**2026-07-28 无状态版本**（无 `initialize` 握手）。
//!
//! # 为什么这一层几乎是空壳
//!
//! 规格要求 `tools/list` = `registry.list_tools`、`tools/call` = `registry.call_tool`。
//! 因此这一层的职责只有一个：**把内部类型精确映射到 MCP 的 wire shape**，不做任何
//! 额外决策。所有 ACL、状态、错误分流都发生在注册表里 —— 如果把判断写到这里，
//! 就会出现"两套真相"。
//!
//! # 无状态语义（本层必须遵守的三条）
//!
//! 1. **没有 `initialize` 握手**。每个请求自带 `protocol_version` 与 `client_capabilities`，
//!    服务端不维护"会话已初始化"状态。框架里唯一的"状态"是插件执行单元的运行时状态，
//!    它与协议会话无关（同一个插件对两个不同的 MCP 客户端是同一份单元）。
//! 2. **每个请求独立可解析**。因此本层的输入是一个完整 JSON 值 + 客户端 ID，
//!    不依赖任何前置调用。任何前置检查都从注册表的当前快照重新做。
//! 3. **`tools/call` 永远返回 `{ content, structuredContent, isError }`**。沙箱崩溃与
//!    工具内部报错在协议层面同构 —— 见 [`crate::types::ToolResult`] 文档。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::registry::ToolRegistry;
use crate::types::{PluginId, ToolFilter, ToolResult};

/// MCP 无状态请求的公共信封。
///
/// 字段用 camelCase 序列化 —— 这是 MCP 的 wire 约定（`protocolVersion`、
/// `clientCapabilities`），不是 Rust 的 snake_case 风格。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatelessEnvelope {
    /// 协议版本，例如 `"2026-07-28"`。**每个请求自带**，不靠握手协商。
    pub protocol_version: String,
    /// 客户端声明的能力。本框架只用到"是否支持 structuredContent"。
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// 客户端能否消费 `structuredContent`。false 时仍然返回它（见下），
    /// 但内容块里一定有可读文本，保证纯文本客户端可用。
    #[serde(default)]
    pub supports_structured_content: bool,
}

/// 本框架实现的协议版本。
pub const SUPPORTED_PROTOCOL_VERSION: &str = "2026-07-28";

/// `tools/list` 的响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsListResponse {
    pub tools: Vec<McpToolDescriptor>,
}

/// MCP 工具描述符。规格要求返回
/// `name / description / inputSchema / trust_level / available`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// 非标准但规格要求暴露：`"Trusted" | "Sandboxed"`。
    /// 放在顶层而不是 `annotations` 里，因为规格的返回字段清单就是平铺的。
    pub trust_level: &'static str,
    /// 非标准：规格要求 `available` 与不可用原因。
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    /// 非标准但有用：破坏性操作标记，供客户端做二次确认提示。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
}

/// `tools/call` 的响应。**就是** [`ToolResult`] 的 MCP 形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCallResponse {
    pub content: Vec<crate::types::ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl From<ToolResult> for ToolsCallResponse {
    fn from(r: ToolResult) -> Self {
        Self {
            content: r.content,
            structured_content: r.structured_content,
            is_error: r.is_error,
        }
    }
}

/// 一次 `tools/call` 的请求体。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCallRequest {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// MCP 适配器。持有注册表与"如何把客户端映射成 caller_id"的策略。
pub struct McpAdapter<R: ToolRegistry> {
    registry: std::sync::Arc<R>,
    /// 从 MCP 客户端标识解析出 `caller_id`。
    ///
    /// 注意这不是"给插件用的 ID 空间"的安全边界：它只是 ACL 的输入。
    /// 真正的安全边界是签名校验（决定执行上下文），两者不要混淆。
    caller_resolver: Box<dyn Fn(&ClientIdentity) -> PluginId + Send + Sync>,
}

/// MCP 客户端身份。**无状态版本下它每个请求都重传**（或由宿主侧连接元信息提供）。
#[derive(Debug, Clone, Default)]
pub struct ClientIdentity {
    /// 客户端自报的插件 ID。只有当该客户端确实是某个已加载插件时才应当映射成它；
    /// 外部 MCP 客户端应当映射为平台层。
    pub plugin_id: Option<String>,
    /// 是否来自宿主自身的 UI（可信来源）。
    pub from_host_ui: bool,
}

impl<R: ToolRegistry + 'static> McpAdapter<R> {
    pub fn new(registry: std::sync::Arc<R>) -> Self {
        Self {
            registry,
            // 默认策略：宿主 UI → 平台层；其余 → 平台层（外部客户端等同于平台层权限）。
            // 这是保守选择：外部客户端拿到的 ACL 是平台层的，而不是"某个插件"的。
            caller_resolver: Box::new(|id: &ClientIdentity| {
                if id.from_host_ui {
                    PluginId::new(PluginId::PLATFORM)
                } else if let Some(pid) = &id.plugin_id {
                    PluginId::new(pid.clone())
                } else {
                    PluginId::new(PluginId::PLATFORM)
                }
            }),
        }
    }

    pub fn with_caller_resolver(
        mut self,
        f: impl Fn(&ClientIdentity) -> PluginId + Send + Sync + 'static,
    ) -> Self {
        self.caller_resolver = Box::new(f);
        self
    }

    /// 校验信封的协议版本。无状态版本下这是**每个请求**都要做的一步。
    ///
    /// 本框架只支持一个版本；未来若需要多版本，扩在这里，而不是在注册表里。
    pub fn check_envelope(envelope: &StatelessEnvelope) -> Result<(), ToolsCallResponse> {
        if envelope.protocol_version == SUPPORTED_PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(ToolsCallResponse {
                content: vec![crate::types::ContentBlock::text(format!(
                    "不支持的协议版本 {}（本框架支持 {}）",
                    envelope.protocol_version, SUPPORTED_PROTOCOL_VERSION
                ))],
                structured_content: Some(json!({
                    "code": "InvalidArguments",
                    "message": "unsupported protocol version",
                    "supported": SUPPORTED_PROTOCOL_VERSION,
                })),
                is_error: true,
            })
        }
    }

    /// `tools/list`。
    pub fn tools_list(&self, identity: &ClientIdentity, filter: &ToolFilter) -> ToolsListResponse {
        let caller = (self.caller_resolver)(identity);
        let listings = self.registry.list_tools(&caller, filter);
        ToolsListResponse {
            tools: listings
                .into_iter()
                .map(|l| McpToolDescriptor {
                    name: l.name,
                    description: l.description,
                    input_schema: l.input_schema,
                    trust_level: l.trust_level.as_mcp_str(),
                    available: l.available,
                    unavailable_reason: l.unavailable_reason.map(|r| r.as_str().to_owned()),
                    destructive: l.destructive,
                })
                .collect(),
        }
    }

    /// `tools/call`。
    pub async fn tools_call(
        &self,
        identity: &ClientIdentity,
        request: ToolsCallRequest,
    ) -> ToolsCallResponse {
        let caller = (self.caller_resolver)(identity);
        let r = self
            .registry
            .call_tool(&caller, &request.name, request.arguments)
            .await;
        ToolsCallResponse::from(r)
    }

    /// 一站式入口：解析完整 JSON-RPC 风格请求（无握手）。
    ///
    /// 请求形状（简化，因为本框架不处理 JSON-RPC 的 id/method 路由，只处理 body）：
    /// ```json
    /// {
    ///   "protocolVersion": "2026-07-28",
    ///   "clientCapabilities": { "supportsStructuredContent": true },
    ///   "method": "tools/list" | "tools/call",
    ///   "params": { ... }
    /// }
    /// ```
    pub async fn handle_value(&self, identity: &ClientIdentity, body: Value) -> ToolsCallResponse {
        let envelope = match serde_json::from_value::<StatelessEnvelope>(json!({
            "protocolVersion": body.get("protocolVersion").cloned()
                .or_else(|| body.get("protocol_version").cloned())
                .unwrap_or(Value::Null),
            "clientCapabilities": body.get("clientCapabilities").cloned()
                .or_else(|| body.get("client_capabilities").cloned())
                .unwrap_or_else(|| json!({})),
        })) {
            Ok(e) => e,
            Err(e) => {
                return ToolsCallResponse {
                    content: vec![crate::types::ContentBlock::text(format!(
                        "请求信封非法：{e}"
                    ))],
                    structured_content: Some(json!({
                        "code": "InvalidArguments",
                        "message": format!("bad envelope: {e}"),
                    })),
                    is_error: true,
                }
            }
        };
        if let Err(resp) = Self::check_envelope(&envelope) {
            return resp;
        }

        let method = body.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "tools/list" => {
                let filter = ToolFilter::all();
                let list = self.tools_list(identity, &filter);
                ToolsCallResponse {
                    content: vec![crate::types::ContentBlock::Json {
                        json: serde_json::to_value(&list).unwrap_or(Value::Null),
                    }],
                    structured_content: Some(serde_json::to_value(&list).unwrap_or(Value::Null)),
                    is_error: false,
                }
            }
            "tools/call" => {
                let params = body.get("params").cloned().unwrap_or(Value::Null);
                match serde_json::from_value::<ToolsCallRequest>(params) {
                    Ok(req) => self.tools_call(identity, req).await,
                    Err(e) => ToolsCallResponse {
                        content: vec![crate::types::ContentBlock::text(format!(
                            "tools/call 参数非法：{e}"
                        ))],
                        structured_content: Some(json!({
                            "code": "InvalidArguments",
                            "message": format!("bad params: {e}"),
                        })),
                        is_error: true,
                    },
                }
            }
            other => ToolsCallResponse {
                content: vec![crate::types::ContentBlock::text(format!(
                    "未知方法 {other}（本框架只实现 tools/list 与 tools/call）"
                ))],
                structured_content: Some(json!({
                    "code": "ToolNotFound",
                    "message": format!("unknown method {other}"),
                })),
                is_error: true,
            },
        }
    }
}
