//! 核心数据模型：插件 ID、信任级别、工具元数据、进程内函数引用、调用结果。
//!
//! 这里**没有**异步执行逻辑，只有数据结构 + 构造校验，方便单元测试直接断言。

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;

/// 插件标识。全局唯一，同时是工具名的前缀（`插件ID.工具名`）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginId(pub String);

impl PluginId {
    /// 平台层自身的调用方标识。它不是插件，因此 `allowed_callers: ["*"]` 才允许它调用。
    pub const PLATFORM: &'static str = "__platform__";

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 工具名的强制前缀：`"<plugin_id>."`。
    pub fn tool_prefix(&self) -> String {
        format!("{}.", self.0)
    }

    /// 校验工具名属于本插件。规格要求工具名格式 `插件ID.工具名` 且全局唯一。
    pub fn owns_tool_name(&self, tool_name: &str) -> bool {
        tool_name
            .strip_prefix(&self.tool_prefix())
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('.'))
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PluginId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for PluginId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// 插件信任分级。**由加载期的签名校验结果决定**，插件自己无权声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// 签名有效且未被吊销：直接跑在宿主主执行上下文。注册表直接调用 handler，
    /// 无调度器介入。**崩溃即宿主故障。**
    Trusted,
    /// 无签名 / 签名无效 / 被吊销 / 或已显式要求：跑在虚拟进程沙箱。
    Sandboxed,
}

impl TrustLevel {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted)
    }

    /// MCP `tools/list` 里对外暴露的字符串。
    pub fn as_mcp_str(self) -> &'static str {
        match self {
            Self::Trusted => "Trusted",
            Self::Sandboxed => "Sandboxed",
        }
    }
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_mcp_str())
    }
}

/// 沙箱执行单元的句柄。**仅沙箱插件有**；可信插件的工具元数据里必须是 `None`。
///
/// 内含代数（generation），保证重启后旧句柄不会误伤新单元。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitHandle {
    pub plugin_id: PluginId,
    /// 单元代数：每次 create_unit 递增。重启后旧代数的一切操作被拒绝。
    pub generation: u64,
}

impl UnitHandle {
    pub fn new(plugin_id: PluginId, generation: u64) -> Self {
        Self {
            plugin_id,
            generation,
        }
    }
}

impl fmt::Display for UnitHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unit({}@{})", self.plugin_id, self.generation)
    }
}

// ----------------------------------------------------------------------------
// 进程内函数引用
// ----------------------------------------------------------------------------

/// handler 返回的 future。必须是 `Send`：沙箱单元要在运行时的线程池上执行它。
pub type BoxToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send + 'static>>;

/// 进程内函数引用的实际载体。
///
/// 规格里 `handler: FunctionRef` 是"进程内函数引用"。在 Rust 里它的物理形态就是一个
/// `Arc<dyn Fn>` —— 没有跨进程序列化，没有 FFI，没有函数指针地址寻址。
///
/// # 为什么 handler 返回 `ToolResult` 而不是 `Result<ToolResult, ToolError>`
///
/// 因为要区分两类失败：
/// - **handler 自己报告的业务失败** → 返回 `ToolResult { is_error: true, .. }`，
///   这属于工具语义，框架不干预；
/// - **handler panic** → 只能通过 `catch_unwind` 捕获，产生 `HandlerPanic`。
///
/// 两者在 MCP 协议层同构（都是 `isError: true`），但错误码不同。
pub type ToolHandler = Arc<dyn Fn(Value) -> BoxToolFuture + Send + Sync + 'static>;

/// 进程内函数引用。携带元数据，便于日志与 panic 归因。
#[derive(Clone)]
pub struct FunctionRef {
    handler: ToolHandler,
    pub plugin_id: PluginId,
    pub tool_name: String,
}

impl FunctionRef {
    pub fn new(
        plugin_id: impl Into<PluginId>,
        tool_name: impl Into<String>,
        f: impl Fn(Value) -> BoxToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Arc::new(f),
            plugin_id: plugin_id.into(),
            tool_name: tool_name.into(),
        }
    }

    /// 从已有的 `ToolHandler` 构造（用于把 handler 在加载器/测试间传递）。
    pub fn from_handler(
        plugin_id: impl Into<PluginId>,
        tool_name: impl Into<String>,
        handler: ToolHandler,
    ) -> Self {
        Self {
            handler,
            plugin_id: plugin_id.into(),
            tool_name: tool_name.into(),
        }
    }

    /// **直接调用**。仅用于可信插件路径（由注册表在 `catch_unwind` 保护下使用），
    /// 以及沙箱单元在调度器 task 内部真正执行 handler 时。
    ///
    /// 注意：这里故意不提供任何内部 `catch_unwind`。保护放在调用点，原因是
    /// 异步执行的 panic 可能发生在 future 的 poll 里，而不是在构造 future 的这一刻。
    pub fn invoke(&self, args: Value) -> BoxToolFuture {
        (self.handler)(args)
    }

    pub fn handler_arc(&self) -> ToolHandler {
        Arc::clone(&self.handler)
    }
}

impl fmt::Debug for FunctionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FunctionRef")
            .field("plugin_id", &self.plugin_id)
            .field("tool_name", &self.tool_name)
            .finish_non_exhaustive()
    }
}

// ----------------------------------------------------------------------------
// 工具元数据
// ----------------------------------------------------------------------------

/// 规格第 2 节定义的完整工具元数据。
#[derive(Clone)]
pub struct ToolMeta {
    /// 全局唯一，格式 `"插件ID.工具名"`。
    pub name: String,
    pub description: String,
    /// JSON Schema。register_tool 时校验其只使用受支持子集。
    pub input_schema: Value,
    pub handler: FunctionRef,
    pub trust_level: TrustLevel,
    /// `["*"]` 表示任意调用方；否则是调用方 plugin_id 白名单。
    ///
    /// **不要**把 `["*"]` 与"平台层"混为一谈：平台层也需要显式列入或走 `"*"`。
    pub allowed_callers: Vec<PluginId>,
    /// 是否需要用户二次确认（破坏性操作）。框架只透传，不代替 UI 决策。
    pub destructive: bool,
    /// 工具级超时，单次调用。**优先于**单元级 idle / max_lifetime。
    pub timeout_ms: u64,
    pub owner_plugin_id: PluginId,
    /// 仅沙箱插件有。
    pub unit_handle: Option<UnitHandle>,
}

impl ToolMeta {
    /// 校验元数据内部一致性（`register_tool` 的第一道关）。
    ///
    /// 返回 `Err(错误串)` 由调用方转成 `RegistryError`。
    pub fn validate(&self) -> Result<(), String> {
        if !self.owner_plugin_id.owns_tool_name(&self.name) {
            return Err(format!(
                "工具名 {} 不符合格式 {}<工具名>",
                self.name,
                self.owner_plugin_id.tool_prefix()
            ));
        }
        if self.handler.plugin_id != self.owner_plugin_id {
            return Err("handler 的 plugin_id 与 owner_plugin_id 不一致".to_owned());
        }
        if self.handler.tool_name != self.name {
            return Err("handler 的 tool_name 与 name 不一致".to_owned());
        }
        match (self.trust_level, &self.unit_handle) {
            // 可信插件带 unit_handle → 说明它其实该走沙箱，元数据自相矛盾。
            (TrustLevel::Trusted, Some(_)) => {
                return Err("Trusted 工具不得携带 unit_handle".to_owned())
            }
            // 沙箱插件没 unit_handle → 无法定位执行单元。
            (TrustLevel::Sandboxed, None) => {
                return Err("Sandboxed 工具必须携带 unit_handle".to_owned())
            }
            _ => {}
        }
        if self.timeout_ms == 0 {
            return Err("timeout_ms 必须 > 0（0 会让每次调用立即超时）".to_owned());
        }
        if self.allowed_callers.is_empty() {
            return Err("allowed_callers 不能为空；完全私有请写 ['*'] 之外的自身 ID".to_owned());
        }
        crate::json::validate_schema(&self.input_schema).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// ACL 判定。**list 阶段和 call 阶段必须用同一个函数**，否则两处会不一致。
    pub fn allows_caller(&self, caller: &PluginId) -> bool {
        let wildcard = PluginId::new("*");
        self.allowed_callers
            .iter()
            .any(|c| c == &wildcard || c == caller)
    }

    /// 是否是沙箱插件（走调度器）。
    pub fn is_sandboxed(&self) -> bool {
        matches!(self.trust_level, TrustLevel::Sandboxed)
    }
}

impl fmt::Debug for ToolMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolMeta")
            .field("name", &self.name)
            .field("trust_level", &self.trust_level)
            .field("owner_plugin_id", &self.owner_plugin_id)
            .field("destructive", &self.destructive)
            .field("timeout_ms", &self.timeout_ms)
            .field("unit_handle", &self.unit_handle)
            .field("allowed_callers", &self.allowed_callers)
            .finish_non_exhaustive()
    }
}

/// `list_tools` 的返回项：元数据 + **运行时可用性**。
///
/// 规格要求"重启期间 list_tools 仍返回该工具，标记 available: false,
/// reason: 'restarting'"。所以可用性必须是返回项的一部分，而不是过滤掉整个工具。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolListing {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub trust_level: TrustLevel,
    pub destructive: bool,
    pub owner_plugin_id: String,
    pub timeout_ms: u64,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<UnavailableReason>,
}

/// 不可用原因。与 `ToolErrorCode` 是两套词表：这里是"状态"，那里是"一次调用的失败"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    Restarting,
    Crashed,
    Failed,
    TimedOut,
    CircuitOpen,
    Stopped,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restarting => "restarting",
            Self::Crashed => "crashed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::CircuitOpen => "circuit_open",
            Self::Stopped => "stopped",
        }
    }
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `list_tools` 的过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ToolFilter {
    /// 只看某个信任级别。
    pub trust_level: Option<TrustLevel>,
    /// 只看某个插件的工具。
    pub owner_plugin_id: Option<PluginId>,
    /// 名字子串匹配（大小写敏感）。用于 UI 搜索框。
    pub name_contains: Option<String>,
    /// 是否包含当前不可用的工具。默认 **true** —— 规格明确要求重启期间仍可见。
    pub include_unavailable: bool,
}

impl ToolFilter {
    pub fn all() -> Self {
        Self {
            include_unavailable: true,
            ..Default::default()
        }
    }

    /// 只保留执行单元正在 Running 的沙箱工具 + 所有可信工具。
    pub fn available_only() -> Self {
        Self {
            include_unavailable: false,
            ..Default::default()
        }
    }

    pub fn matches(&self, listing: &ToolListing) -> bool {
        if let Some(t) = self.trust_level {
            if listing.trust_level != t {
                return false;
            }
        }
        if let Some(ref o) = self.owner_plugin_id {
            if listing.owner_plugin_id != o.0 {
                return false;
            }
        }
        if let Some(ref s) = self.name_contains {
            if !listing.name.contains(s.as_str()) {
                return false;
            }
        }
        if !self.include_unavailable && !listing.available {
            return false;
        }
        true
    }
}

// ----------------------------------------------------------------------------
// 调用结果（MCP 形状）
// ----------------------------------------------------------------------------

/// MCP `tools/call` 的 `content` 块。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text { text: String },
    Json { json: Value },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}

/// MCP `tools/call` 的返回体：`{ content, structuredContent, isError }`。
///
/// # 协议层面同构
///
/// 沙箱崩溃 → `is_error: true`；工具内部业务报错 → 也是 `is_error: true`。
/// 区别只体现在 `structuredContent.code` 上。**用户类插件不需要为"虚拟进程"写
/// 特殊分支** —— 这是规格第 6 节的硬要求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(value: Value) -> Self {
        Self {
            content: vec![ContentBlock::Json {
                json: value.clone(),
            }],
            structured_content: Some(value),
            is_error: false,
        }
    }

    pub fn success_text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            content: vec![ContentBlock::Text { text: text.clone() }],
            structured_content: Some(serde_json::json!({ "text": text })),
            is_error: false,
        }
    }

    /// 工具内部的业务失败。`code` 若为 `None`，MCP 调用方仍能通过 `isError` 判断失败。
    pub fn business_error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            content: vec![ContentBlock::Text {
                text: message.clone(),
            }],
            structured_content: Some(serde_json::json!({ "message": message })),
            is_error: true,
        }
    }

    /// 框架产生的错误（含沙箱崩溃）。**唯一**把 `ToolError` 变成 MCP 结果的入口。
    pub fn error(e: ToolError) -> Self {
        let mut structured = serde_json::json!({
            "code": e.code.as_str(),
            "message": e.message,
            "pluginId": e.plugin_id,
            "retryable": e.code.is_retryable(),
        });
        if let Some(tool) = &e.tool_name {
            structured["toolName"] = Value::String(tool.clone());
        }
        if let Some(p) = &e.panic_payload {
            structured["panicPayload"] = Value::String(p.clone());
        }
        Self {
            content: vec![ContentBlock::Text {
                text: format!("{}: {}", e.code, e.message),
            }],
            structured_content: Some(structured),
            is_error: true,
        }
    }

    /// 结果体积（字节），用于沙箱单元的返回值配额。
    pub fn approximate_size_bytes(&self) -> usize {
        let structured = self
            .structured_content
            .as_ref()
            .and_then(|v| serde_json::to_vec(v).ok())
            .map(|v| v.len())
            .unwrap_or(0);
        let content: usize = self
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Json { json } => {
                    serde_json::to_vec(json).map(|v| v.len()).unwrap_or(0)
                }
            })
            .sum();
        structured + content
    }
}

// ----------------------------------------------------------------------------
// 任务标识
// ----------------------------------------------------------------------------

/// 单元内任务 ID。由调度器分配，单调递增，单元内唯一。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId {
    pub unit: UnitHandle,
    pub seq: u64,
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/task#{}", self.unit, self.seq)
    }
}

/// `submit` 的异步结果。**保证 resolve**：完成 / 超时 / 崩溃三者必有其一。
#[derive(Debug, Clone)]
pub enum TaskOutcome {
    /// handler 正常返回（其中可能带 `is_error: true` 的业务失败）。
    Completed(ToolResult),
    /// 工具级超时，且 handler 可中断，已被成功中止。
    TimedOut(ToolError),
    /// 超时且 handler 不可中断 → 升级为单元级崩溃。
    Uninterruptible(ToolError),
    /// 单元已崩溃（沙箱内 panic / 被判定不可中断）。
    Crashed(ToolError),
    /// 配额拒绝。
    QuotaExceeded(ToolError),
    /// 单元在任务执行期间被 destroy / 重启（代数失效）。
    Aborted(ToolError),
}

impl TaskOutcome {
    pub fn into_result(self) -> ToolResult {
        match self {
            Self::Completed(r) => {
                // 返回值配额在调度器侧已经检查过；这里只做最终保证：
                // 任何 outcome 都必须是 MCP 形状，保证协议同构。
                if r.structured_content.is_some() || !r.content.is_empty() {
                    r
                } else {
                    ToolResult::success_text("(empty)")
                }
            }
            Self::TimedOut(e)
            | Self::Uninterruptible(e)
            | Self::Crashed(e)
            | Self::QuotaExceeded(e)
            | Self::Aborted(e) => ToolResult::error(e),
        }
    }

    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }
}

/// `cancel` 的返回值。**必须能报告 `Uninterruptible`** —— 这是规格的硬要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 已成功中止：future 在下一次 await 点被 drop。
    Cancelled,
    /// 无法中止：handler 正卡在不产出 await 点的同步代码里。
    Uninterruptible,
    /// 任务在 cancel 调用前已经结束（正常完成或已崩溃）。不是错误，只是错过时机。
    AlreadyFinished,
    /// 任务在 cancel 调用前已被超时机制处理。
    AlreadyTimedOut,
    /// 任务 ID 在该单元中不存在。
    NotFound,
}

impl CancelOutcome {
    /// 规格接口 `cancel(unit, task_id) -> Cancelled | Uninterruptible`。
    ///
    /// 额外三个变体（`AlreadyFinished` / `AlreadyTimedOut` / `NotFound`）是我补充的：
    /// 规格没写"取消一个已结束任务"该怎么办，把它们塞进 `Cancelled` 会撒谎，
    /// 塞进 `Uninterruptible` 会误导用户去重启插件。见 docs/open-questions.md。
    pub fn as_protocol_str(self) -> &'static str {
        match self {
            Self::Cancelled | Self::AlreadyFinished | Self::AlreadyTimedOut | Self::NotFound => {
                "Cancelled"
            }
            Self::Uninterruptible => "Uninterruptible",
        }
    }
}

impl fmt::Display for CancelOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Cancelled => "Cancelled",
            Self::Uninterruptible => "Uninterruptible",
            Self::AlreadyFinished => "AlreadyFinished",
            Self::AlreadyTimedOut => "AlreadyTimedOut",
            Self::NotFound => "NotFound",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(trust: TrustLevel, unit: Option<UnitHandle>) -> ToolMeta {
        let pid = PluginId::new("demo");
        let name = "demo.echo".to_owned();
        ToolMeta {
            name: name.clone(),
            description: "echo".into(),
            input_schema: serde_json::json!({"type": "object"}),
            handler: FunctionRef::new(pid.clone(), name, |_a| {
                Box::pin(async { ToolResult::success_text("hi") })
            }),
            trust_level: trust,
            allowed_callers: vec![PluginId::new("*")],
            destructive: false,
            timeout_ms: 5_000,
            owner_plugin_id: pid,
            unit_handle: unit,
        }
    }

    #[test]
    fn tool_name_prefix_enforced() {
        let pid = PluginId::new("demo");
        assert!(pid.owns_tool_name("demo.echo"));
        assert!(!pid.owns_tool_name("demo."));
        assert!(!pid.owns_tool_name("demo.a.b"));
        assert!(!pid.owns_tool_name("other.echo"));
    }

    #[test]
    fn trusted_meta_must_not_have_unit() {
        let h = UnitHandle::new(PluginId::new("demo"), 1);
        let m = meta(TrustLevel::Trusted, Some(h));
        assert!(m.validate().is_err());
    }

    #[test]
    fn sandboxed_meta_must_have_unit() {
        let m = meta(TrustLevel::Sandboxed, None);
        assert!(m.validate().is_err());
    }

    #[test]
    fn valid_meta_passes() {
        assert!(meta(TrustLevel::Trusted, None).validate().is_ok());
        let h = UnitHandle::new(PluginId::new("demo"), 1);
        assert!(meta(TrustLevel::Sandboxed, Some(h)).validate().is_ok());
    }

    #[test]
    fn acl_wildcard_and_specific() {
        let m = meta(TrustLevel::Trusted, None);
        assert!(m.allows_caller(&PluginId::new("anyone")));

        let mut m2 = m.clone();
        m2.allowed_callers = vec![PluginId::new("friend")];
        assert!(m2.allows_caller(&PluginId::new("friend")));
        assert!(!m2.allows_caller(&PluginId::new("stranger")));
        // 平台层也不是默认允许的 —— 必须显式写进白名单或 "*"。
        assert!(!m2.allows_caller(&PluginId::new(PluginId::PLATFORM)));
    }

    #[test]
    fn error_result_is_mcp_isomorphic() {
        let r = ToolResult::error(ToolError::process_crashed("demo", "demo.echo"));
        assert!(r.is_error);
        let s = r.structured_content.unwrap();
        assert_eq!(s["code"], "ProcessCrashed");
        // 业务失败与崩溃失败在协议层面同构
        let b = ToolResult::business_error("bad input");
        assert!(b.is_error);
        assert_eq!(b.content.len(), 1);
    }
}
