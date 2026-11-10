//! 工具注册表：**所有插件（可信 + 沙箱）**在初始化时向这里登记工具。
//!
//! # 为什么注册表是唯一入口
//!
//! MCP 的 `tools/list` / `tools/call` 就是这里的 `list_tools` / `call_tool`。
//! 让注册表成为唯一入口，可以保证：
//! - ACL 在 **list 阶段和 call 阶段用同一套判定**（否则会出现"列表里看不见但调得通"，
//!   或者反过来）；
//! - 所有执行路径（可信直调 / 沙箱调度）都在同一个地方分流，不存在绕过；
//! - 崩溃类错误与业务错误在**同一个出口**被统一成 MCP 形状。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::{RegistryError, ToolError};
use crate::types::{
    PluginId, ToolFilter, ToolListing, ToolMeta, ToolResult, UnavailableReason, UnitHandle,
};

pub mod impl_registry;

pub use impl_registry::ToolRegistryImpl;

/// 别名，避免与本 crate 的 `types::ToolResult` 混淆。
pub type RegistryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 手动重启入口（由 `RestartManager` 实现，在注册表构造完成后注入）。
///
/// 之所以用 trait + 后期注入，而不是让注册表直接持有重启管理器：两者互相需要，
/// 直接持有会形成 `Arc` 循环。注入点只有一个，且注入前 `restart_unit` 会返回
/// 明确错误而不是假装成功。
pub trait RestartHook: Send + Sync + 'static {
    /// 平台层/用户请求立即重启某个沙箱单元。
    fn request_manual_restart(
        &self,
        plugin_id: PluginId,
    ) -> RegistryFuture<'_, Result<(), ToolError>>;
}

/// 工具注册表接口（与规格逐条对应）。
pub trait ToolRegistry: Send + Sync + 'static {
    /// 登记工具。同一 `plugin_id` 只能有一个执行单元，所以沙箱工具必须带 `unit_handle`。
    fn register_tool(&self, meta: ToolMeta) -> Result<(), RegistryError>;

    /// 注销工具。会校验调用方是否是该工具的所有者。
    fn unregister_tool(&self, caller: &PluginId, name: &str) -> Result<(), RegistryError>;

    /// 列出调用方可见的工具。**ACL 在此过滤**，并附带 `available` 状态。
    fn list_tools(&self, caller: &PluginId, filter: &ToolFilter) -> Vec<ToolListing>;

    /// 调用工具。四步检查 → 分流执行。
    fn call_tool(
        &self,
        caller: &PluginId,
        name: &str,
        args: serde_json::Value,
    ) -> RegistryFuture<'_, ToolResult>;

    /// 重启沙箱单元。**仅平台层/用户可调用**；插件不可自行重启。
    fn restart_unit(
        &self,
        caller: &PluginId,
        plugin_id: &PluginId,
    ) -> RegistryFuture<'_, Result<(), ToolError>>;

    // ---- 供重启管理器/加载器使用的内部接口 ----

    /// 设置/清除某插件工具的"不可用提示"。
    ///
    /// 注册表同时参考两处信息：调度器的实时状态 + 这里的提示。提示优先，
    /// 因为它能表达调度器状态表达不了的东西（例如"重启中""熔断打开"）。
    fn note_availability(&self, plugin_id: &PluginId, reason: Option<UnavailableReason>);

    /// 把某个插件全部工具迁移到新的执行单元（重启成功后调用）。
    fn rebind_unit(&self, plugin_id: &PluginId, unit: UnitHandle) -> Result<usize, RegistryError>;

    /// 移除某个插件的全部工具（卸载用）。
    fn unregister_plugin(&self, plugin_id: &PluginId) -> usize;

    /// 读取工具元数据副本（不含 handler），用于诊断。
    fn tool_meta(&self, name: &str) -> Option<ToolMetaSnapshot>;

    /// 注入手动重启入口。
    fn install_restart_hook(&self, hook: Arc<dyn RestartHook>);

    /// 设置破坏性工具的二次确认开关。框架只透传，不代替 UI 决策。
    fn set_destructive_confirm_hook(&self, hook: Arc<dyn DestructiveConfirmHook>);
}

/// 元数据快照（`ToolMeta` 含 `Arc<dyn Fn>`，不能直接 clone 给外部）。
#[derive(Debug, Clone)]
pub struct ToolMetaSnapshot {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub trust_level: crate::types::TrustLevel,
    pub allowed_callers: Vec<PluginId>,
    pub destructive: bool,
    pub timeout_ms: u64,
    pub owner_plugin_id: PluginId,
    pub unit_handle: Option<UnitHandle>,
}

impl ToolMetaSnapshot {
    pub fn from_meta(m: &ToolMeta) -> Self {
        Self {
            name: m.name.clone(),
            description: m.description.clone(),
            input_schema: m.input_schema.clone(),
            trust_level: m.trust_level,
            allowed_callers: m.allowed_callers.clone(),
            destructive: m.destructive,
            timeout_ms: m.timeout_ms,
            owner_plugin_id: m.owner_plugin_id.clone(),
            unit_handle: m.unit_handle.clone(),
        }
    }
}

/// 破坏性操作的二次确认钩子。
///
/// 框架**不做**确认决策（那是 UI 的事），只提供这个挂载点，让平台层把
/// "这个工具要动用户的存档，先问一下"这类策略接进来。返回 `None` 表示放行。
pub trait DestructiveConfirmHook: Send + Sync + 'static {
    fn confirm(
        &self,
        caller: &PluginId,
        tool: &ToolMetaSnapshot,
    ) -> RegistryFuture<'_, Option<ToolError>>;
}
