//! 错误类型：规格要求的 10 个错误码 + MCP 协议层同构映射。
//!
//! # 不变量
//!
//! 1. 每个错误码都有对应的 [`ToolErrorCode`] 变体，没有"临时借用别的错误码"的路径。
//! 2. 沙箱崩溃（`ProcessCrashed` / `ProcessRestarting` / `ProcessFailed` / `Timeout` /
//!    `Uninterruptible` / `SandboxQuotaExceeded`）对 MCP 调用方表现为 `is_error: true`，
//!    与工具内部业务报错**协议层面同构** —— 调用方不需要为"虚拟进程"写特殊分支。
//! 3. 崩溃类错误是"调用方视角"的：它只说明这次调用没成功，不代表插件被卸载。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 规格第 5 节的错误码表。顺序与规格一致，便于逐条核对。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolErrorCode {
    /// 工具名不在注册表中。
    ToolNotFound,
    /// 调用方未通过 ACL（未在 allowed_callers 中，也不是 "*"）。
    PermissionDenied,
    /// 执行单元正在被重启管理器重建。调用**立即返回**，不排队。
    ProcessRestarting,
    /// 执行单元已崩溃且尚未恢复。
    ProcessCrashed,
    /// 执行单元重启失败并被标记为 Failed；不再自动重试。
    ProcessFailed,
    /// 工具级 timeout_ms 到期，且该 handler 可中断，已成功中止。
    Timeout,
    /// 工具级 timeout_ms 到期，但 handler 不可中断；已升级为单元级崩溃。
    Uninterruptible,
    /// 可信插件 handler 在宿主执行上下文中 panic。**仅可信插件会返回此码。**
    HandlerPanic,
    /// 沙箱单元资源配额超限（并发调用数 / 调用速率 / 内存上限 / 结果体积）。
    SandboxQuotaExceeded,
    /// 参数未通过 input_schema 校验，或无法序列化/反序列化为 JSON。
    InvalidArguments,
}

impl ToolErrorCode {
    /// MCP 协议层的稳定错误标识（写进 `structuredContent.code`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolNotFound => "ToolNotFound",
            Self::PermissionDenied => "PermissionDenied",
            Self::ProcessRestarting => "ProcessRestarting",
            Self::ProcessCrashed => "ProcessCrashed",
            Self::ProcessFailed => "ProcessFailed",
            Self::Timeout => "Timeout",
            Self::Uninterruptible => "Uninterruptible",
            Self::HandlerPanic => "HandlerPanic",
            Self::SandboxQuotaExceeded => "SandboxQuotaExceeded",
            Self::InvalidArguments => "InvalidArguments",
        }
    }

    /// 该错误码是否只可能由沙箱执行单元产生。
    ///
    /// 用于断言不变量：`HandlerPanic` 绝不能出现在沙箱路径上。
    pub const fn is_sandbox_only(self) -> bool {
        matches!(
            self,
            Self::ProcessRestarting
                | Self::ProcessCrashed
                | Self::ProcessFailed
                | Self::Uninterruptible
                | Self::SandboxQuotaExceeded
        )
    }

    /// 是否应当让 MCP 调用方感觉"这个工具暂时不可用"（而非"我调用姿势错了"）。
    ///
    /// 决定了重试语义：true 表示可重试（可能自愈），false 表示不该盲目重试。
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::ProcessRestarting | Self::ProcessCrashed | Self::Timeout
        )
    }
}

impl fmt::Display for ToolErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一次工具调用失败的全部信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    /// 面向用户的说明。**必须**包含足够定位信息，但不泄露公钥/内部路径。
    pub message: String,
    pub plugin_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// 崩溃类错误的 panic 载荷摘要（仅可信插件有）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panic_payload: Option<String>,
}

impl ToolError {
    pub fn new(
        code: ToolErrorCode,
        plugin_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            plugin_id: plugin_id.into(),
            tool_name: None,
            panic_payload: None,
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool_name = Some(tool.into());
        self
    }

    pub fn with_payload(mut self, payload: impl Into<String>) -> Self {
        self.panic_payload = Some(payload.into());
        self
    }

    // ---- 常用构造器：调用点直接 `ToolError::tool_not_found(..)`，避免手写错误码 ----

    pub fn tool_not_found(caller: &str, name: &str) -> Self {
        Self::new(
            ToolErrorCode::ToolNotFound,
            caller,
            format!("工具 {name} 不存在（未注册或已注销）"),
        )
        .with_tool(name)
    }

    pub fn permission_denied(caller: &str, name: &str, owner: &str) -> Self {
        Self::new(
            ToolErrorCode::PermissionDenied,
            owner,
            format!("调用方 {caller} 未被工具 {name} 的 allowed_callers 允许"),
        )
        .with_tool(name)
    }

    pub fn process_restarting(owner: &str, name: &str) -> Self {
        Self::new(
            ToolErrorCode::ProcessRestarting,
            owner,
            "沙箱执行单元正在重启，本次调用未执行、未排队".to_owned(),
        )
        .with_tool(name)
    }

    pub fn process_crashed(owner: &str, name: &str) -> Self {
        Self::new(
            ToolErrorCode::ProcessCrashed,
            owner,
            "沙箱执行单元已崩溃，尚未恢复".to_owned(),
        )
        .with_tool(name)
    }

    pub fn process_failed(owner: &str, name: &str, why: &str) -> Self {
        Self::new(
            ToolErrorCode::ProcessFailed,
            owner,
            format!("沙箱执行单元重启失败，已标记 Failed，不再自动重试：{why}"),
        )
        .with_tool(name)
    }

    pub fn timeout(owner: &str, name: &str, timeout_ms: u64) -> Self {
        Self::new(
            ToolErrorCode::Timeout,
            owner,
            format!("工具调用超过 {timeout_ms}ms，已被中断"),
        )
        .with_tool(name)
    }

    pub fn uninterruptible(owner: &str, name: &str, timeout_ms: u64) -> Self {
        Self::new(
            ToolErrorCode::Uninterruptible,
            owner,
            format!(
                "工具调用超过 {timeout_ms}ms 且 handler 不可中断，已升级为单元级崩溃并触发重启"
            ),
        )
        .with_tool(name)
    }

    pub fn handler_panic(owner: &str, name: &str, payload: &str) -> Self {
        Self::new(
            ToolErrorCode::HandlerPanic,
            owner,
            "可信插件 handler 在宿主执行上下文中 panic（宿主进程仍然存活）",
        )
        .with_tool(name)
        .with_payload(payload)
    }

    pub fn quota_exceeded(owner: &str, name: &str, why: &str) -> Self {
        Self::new(
            ToolErrorCode::SandboxQuotaExceeded,
            owner,
            format!("沙箱配额超限：{why}"),
        )
        .with_tool(name)
    }

    pub fn invalid_arguments(owner: &str, name: &str, why: &str) -> Self {
        Self::new(
            ToolErrorCode::InvalidArguments,
            owner,
            format!("参数不合法：{why}"),
        )
        .with_tool(name)
    }

    /// 转成 MCP `tools/call` 的返回体。
    pub fn into_mcp_result(self) -> crate::types::ToolResult {
        crate::types::ToolResult::error(self)
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (plugin={})",
            self.code, self.message, self.plugin_id
        )
    }
}

impl std::error::Error for ToolError {}

/// 注册表自身操作的错误（不是一次工具调用，而是"登记/注销被拒绝"）。
///
/// 与 [`ToolError`] 分开，是因为它们发生在**加载期**、由平台层触发，
/// 不能被建模成"某次调用失败了"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryError {
    /// 工具名已存在（全局唯一，格式 `插件ID.工具名`）。
    DuplicateName { name: String },
    /// 工具名不符合 `插件ID.工具名` 格式，或与 owner_plugin_id 不一致。
    MalformedName {
        name: String,
        expected_prefix: String,
    },
    /// 注册者的 trust_level 与它提供的 unit_handle 不匹配（可信插件不得带 unit_handle，
    /// 沙箱插件必须带）。
    TrustLevelMismatch { name: String },
    /// 调用方不是平台层，却调用了平台层专属接口（restart_unit）。
    PlatformOnly,
    /// 沙箱插件试图注销/覆盖别的插件的工具。
    ForeignOwnership { name: String, owner: String },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => write!(f, "工具名重复：{name}"),
            Self::MalformedName {
                name,
                expected_prefix,
            } => {
                write!(f, "工具名 {name} 必须以 {expected_prefix} 开头")
            }
            Self::TrustLevelMismatch { name } => {
                write!(f, "工具 {name} 的 trust_level 与 unit_handle 不匹配")
            }
            Self::PlatformOnly => f.write_str("该接口仅平台层/用户可调用，插件不可自行重启"),
            Self::ForeignOwnership { name, owner } => {
                write!(f, "工具 {name} 属于 {owner}，当前插件无权注销")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// 沙箱调度器的错误（创建/提交阶段的同步失败，异步结果走 [`crate::types::TaskOutcome`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulerError {
    /// 单元不存在或已被 destroy。
    UnitNotFound,
    /// 单元当前状态不接受新任务（Crashed / TimedOut / Restarting / Failed / Stopped）。
    UnitNotAccepting {
        state: crate::scheduler::state::UnitState,
    },
    /// 并发配额打满。调用方应把它映射为 `SandboxQuotaExceeded`。
    ConcurrencyQuota { limit: u32, in_flight: u32 },
    /// 任务 id 在该单元中不存在（cancel 用）。
    TaskNotFound,
    /// 单元已 destroy，无法 create 之外的操作。
    UnitDestroyed,
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnitNotFound => f.write_str("执行单元不存在"),
            Self::UnitNotAccepting { state } => write!(f, "执行单元当前状态 {state} 不接受新任务"),
            Self::ConcurrencyQuota { limit, in_flight } => {
                write!(f, "并发配额超限：{in_flight}/{limit}")
            }
            Self::TaskNotFound => f.write_str("任务不存在或已结束"),
            Self::UnitDestroyed => f.write_str("执行单元已销毁"),
        }
    }
}

impl std::error::Error for SchedulerError {}

/// 加载期错误（验签、清单、包结构）。**注意**：验签失败**不是**这个类型，
/// 验签失败会降级到沙箱并继续加载（见 [`crate::signature::SignatureOutcome`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadError {
    /// 清单 JSON 结构非法。
    BadManifest(String),
    /// 清单声明的 plugin_id 与目录/包名不一致。
    PluginIdMismatch { declared: String, actual: String },
    /// 包里缺少清单文件。
    MissingManifest,
    /// 插件 ID 已被另一个已加载插件占用。
    PluginIdTaken(String),
    /// 包内某个文件的摘要与签名清单不一致（**这不是验签失败，是包被篡改**，
    /// 因此拒绝加载而不是降级——见 README 的"降级 vs 拒绝"一节）。
    PackageTampered { file: String },
    /// 沙箱单元创建失败。
    SandboxInitFailed(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadManifest(m) => write!(f, "插件清单非法：{m}"),
            Self::PluginIdMismatch { declared, actual } => {
                write!(f, "plugin_id 不一致：清单声明 {declared}，实际 {actual}")
            }
            Self::MissingManifest => f.write_str("插件包缺少清单"),
            Self::PluginIdTaken(id) => write!(f, "plugin_id {id} 已被占用"),
            Self::PackageTampered { file } => write!(f, "插件包文件摘要不符：{file}"),
            Self::SandboxInitFailed(m) => write!(f, "沙箱初始化失败：{m}"),
        }
    }
}

impl std::error::Error for LoadError {}
