//! 调度器：**只服务沙箱插件**。
//!
//! # 位置
//!
//! 可信插件的工具调用**完全不经过这里**（注册表直接 `catch_unwind` + 调用）。
//! 这使得调度器的存在不会给可信路径增加任何一次额外跳转 —— 这是规格的意图。
//!
//! # 接口（与规格逐条对应）
//!
//! ```text
//! create_unit(config) -> UnitHandle
//! submit(unit, task, timeout_ms) -> Future<TaskResult>   // 必 resolve
//! cancel(unit, task_id) -> Cancelled | Uninterruptible
//! destroy(unit)
//! on_crash(unit, callback)
//! get_state(unit) -> Running | Crashed | TimedOut | Restarting | Failed | Stopped
//! ```
//!
//! # 运行时前提（必须写清楚，否则超时语义不成立）
//!
//! 沙箱 handler 被 spawn 到 tokio 的多线程运行时上执行。**必须使用
//! `rt-multi-thread` 且 `worker_threads >= 2`**：如果一个 handler 卡在同步循环里，
//! 需要另一个 worker 线程来跑超时计时器与状态收敛。在 `current_thread` 运行时下，
//! 不可中断的 handler 会把整个运行时（等价于宿主）拖死 —— 那时"单进程内的逻辑隔离"
//! 已经完全失效。构造函数 [`TokioScheduler::new`] 会在运行时线程数不足时给出警告。
//!
//! # 单元不是线程
//!
//! "虚拟进程"在这里是：一个 `UnitConfig` + 一组原子计数器（状态 / 并发 / 速率 / 输出
//! 体积配额）+ 一个代数 + 一张在飞任务表。执行体是宿主的 tokio worker 线程。
//! 没有 fork，没有子进程，没有 socket，没有共享内存段。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{LoadError, SchedulerError};
use crate::types::{CancelOutcome, FunctionRef, PluginId, TaskId, TaskOutcome, UnitHandle};
use state::IllegalTransition;

pub mod reaper;
pub mod state;
pub mod unit;

pub use state::UnitState as State;
pub use state::{CrashReason, UnitState};

/// 便捷别名。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `create_unit` 的配置。**两级超时**在这里定义：
/// - 工具级 `timeout_ms` 每次调用单独传（[`Scheduler::submit`] 的参数）；
/// - 进程级 `idle_timeout_ms` 与可选的 `max_lifetime_ms` 是本结构里的字段。
#[derive(Debug, Clone)]
pub struct UnitConfig {
    pub plugin_id: PluginId,
    /// 进程级空闲回收。`None` = 不回收。
    ///
    /// 注意语义是"该单元**空闲**（无在飞任务）持续这么久就回收"，
    /// 不是"任何任务都没跑完就回收"。
    pub idle_timeout_ms: Option<u64>,
    /// 可选的硬上界：单元存活超过这么久即回收，无论空闲与否。
    /// 在飞任务会被 `Aborted` 收束（必 resolve）。
    pub max_lifetime_ms: Option<u64>,
    /// 并发在飞任务上限。超过即 `SandboxQuotaExceeded`（不排队，直接拒绝）。
    pub max_concurrent_calls: u32,
    /// 单次调用返回值字节上限。超过即 `SandboxQuotaExceeded`。
    pub max_output_bytes: usize,
    /// 每秒允许发起的调用数（滑动 1 秒窗口）。`None` = 不限。
    pub max_calls_per_sec: Option<u32>,
    /// 声明的内存上限。**见下方警告。**
    pub memory_limit_bytes: Option<u64>,
    /// 判定"不可中断"的宽限期：超时后等这么久，handler 仍未离开 poll 即视为不可中断。
    pub uninterruptible_grace_ms: u64,
    /// abort 之后等待任务真正消失的时长。
    pub cancel_drain_ms: u64,
}

/// `memory_limit_bytes` 的诚实说明：**这个字段在纯 Rust 单元里无法强制执行**。
///
/// 理由：宿主的 tokio 线程与 handler 共享同一份 allocator 与地址空间，
/// 没有 per-unit 的内存计量点。要真正强制执行，唯一路径是 WASM 字节码沙箱
/// （自有线性内存 + 计量）。因此本框架把它作为**声明性**字段：
/// 由 `list_units` 透出给 UI，由插件自己遵守，框架不拦。
/// 这是规格第 8 条要求的"明确指出并给出替代方案"，不是静默降级。
impl UnitConfig {
    fn p(plugin_id: impl Into<PluginId>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            idle_timeout_ms: Some(5 * 60 * 1000),
            max_lifetime_ms: None,
            max_concurrent_calls: 8,
            max_output_bytes: 1024 * 1024,
            max_calls_per_sec: Some(32),
            memory_limit_bytes: None,
            uninterruptible_grace_ms: 200,
            cancel_drain_ms: 500,
        }
    }
}

/// 便捷构造器。
pub fn unit_config(plugin_id: impl Into<PluginId>) -> UnitConfig {
    UnitConfig::p(plugin_id)
}

/// 一次任务提交的载荷。
#[derive(Clone)]
pub struct Task {
    pub handler: FunctionRef,
    pub args: serde_json::Value,
}

impl Task {
    pub fn new(handler: FunctionRef, args: serde_json::Value) -> Self {
        Self { handler, args }
    }
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("handler", &self.handler)
            .field("args", &self.args)
            .finish()
    }
}

/// 崩溃事件。订阅者（RestartManager）据此决策。
#[derive(Debug, Clone)]
pub struct CrashEvent {
    pub unit: UnitHandle,
    pub state: UnitState,
    pub reason: CrashReason,
    /// 距上次成功启动的毫秒数。用于判断"是不是启动即崩"。
    pub uptime_ms: u64,
}

/// `on_crash` 的回调。
pub type CrashCallback = Arc<dyn Fn(CrashEvent) + Send + Sync + 'static>;

/// 订阅 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubscriptionId(pub u64);

/// `list_units` 的快照，供 UI 与诊断使用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitSnapshot {
    pub plugin_id: String,
    pub generation: u64,
    pub state: UnitState,
    pub in_flight: u32,
    pub max_concurrent: u32,
    pub total_tasks: u64,
    pub crashes: u64,
    pub idle_ms: u64,
    pub uptime_ms: u64,
    pub memory_limit_bytes: Option<u64>,
}

/// 调度器接口。**只有沙箱插件会出现在这里面。**
pub trait Scheduler: Send + Sync + 'static {
    /// 创建沙箱执行单元。成功返回时状态即为 `Running`。
    ///
    /// 同一 `plugin_id` 已存在活单元时返回 `LoadError::PluginIdTaken`。
    fn create_unit(&self, config: UnitConfig) -> BoxFuture<'_, Result<UnitHandle, LoadError>>;

    /// 异步提交。**保证 resolve**：完成 / 超时 / 崩溃 / 配额 / 被 abort，必居其一。
    ///
    /// `timeout_ms` 是**工具级**超时，优先于单元的 idle / max_lifetime。
    fn submit(&self, unit: UnitHandle, task: Task, timeout_ms: u64) -> BoxFuture<'_, TaskOutcome>;

    /// 尝试取消一个在飞任务。
    ///
    /// 返回 [`CancelOutcome::Uninterruptible`] 表示 handler 正卡在不产生 await 点的
    /// 同步代码里，无法中止 —— 调用方（注册表）应当据此升级为单元级崩溃。
    fn cancel(&self, unit: UnitHandle, task_id: TaskId) -> CancelOutcome;

    /// 销毁单元。所有在飞任务以 `TaskOutcome::Aborted` 收束。
    fn destroy(&self, unit: UnitHandle) -> BoxFuture<'_, ()>;

    /// 订阅崩溃事件。返回的 ID 可用于 [`Scheduler::unsubscribe_crash`]。
    fn on_crash(
        &self,
        unit: UnitHandle,
        callback: CrashCallback,
    ) -> Result<SubscriptionId, SchedulerError>;

    fn unsubscribe_crash(&self, id: SubscriptionId) -> bool;

    /// 读取状态。句柄代数过期或单元不存在时返回 `None`。
    fn get_state(&self, unit: UnitHandle) -> Option<UnitState>;

    fn list_units(&self) -> Vec<UnitSnapshot>;

    // ---- 以下仅供平台层 / RestartManager 使用，插件无权调用 ----

    /// 强制迁移状态（走合法性检查，非法迁移返回错误而不是写入）。
    fn set_state(&self, unit: UnitHandle, to: UnitState) -> Result<(), IllegalTransition>;

    /// 主动上报一次崩溃（置 `Crashed` + 广播事件 + 收束在飞任务）。
    ///
    /// 注册表在"超时且不可中断"时会调用它。返回 `false` 表示单元已不是 Running
    /// （可能已被别的路径先处理了，属于正常竞态）。
    fn raise_crash(&self, unit: UnitHandle, reason: CrashReason) -> bool;

    /// 读取当前代数（`create_unit`/重启后递增）。供重启管理器确认"我拿到的是新单元"。
    fn generation_of(&self, plugin_id: &PluginId) -> Option<u64>;
}

/// 默认实现：tokio。
pub struct TokioScheduler {
    pub(crate) inner: Arc<unit::SchedulerInner>,
}

pub type Shared = Arc<dyn Scheduler>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = unit_config("demo");
        assert!(c.idle_timeout_ms.unwrap() > 0);
        assert!(c.max_concurrent_calls > 0);
        assert!(c.max_output_bytes > 0);
        assert!(c.uninterruptible_grace_ms > 0);
    }
}
