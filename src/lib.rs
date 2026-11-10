//! # Minecraft 启动器插件化底层框架
//!
//! 单 OS 进程、无子进程、无 HTTP/socket/管道。插件间通信全部是宿主进程内的异步函数调用。
//!
//! # ⚠️ 安全边界声明（请先读这三段，再读代码）
//!
//! ## 1. 可信插件 = 宿主自身
//!
//! 签名有效且未被吊销的插件**直接跑在宿主进程主执行上下文里**，与本启动器同等权限。
//! 它的一次未捕获 native 崩溃、`std::process::exit()`、内存破坏、OOM **会带走整个启动器**。
//! 我们做的唯一防护是：
//!
//! - 调用边界 `catch_unwind` 包住 Future poll —— **只挡 Rust 的 unwind panic**；
//! - 包整体签名 + 吊销列表 —— **签名校验是唯一防线**。
//!
//! `catch_unwind` **不能**捕获：`abort()`、段错误、OOM、`exit()`、`panic = "abort"` 下的
//! 任何 panic、以及跨 `extern "C"` 边界的 panic。如果你的插件是用 C/C++ 写的
//! native 扩展，可信级别对它没有任何保护意义。
//!
//! ## 2. 沙箱插件 ≠ 安全沙箱
//!
//! 沙箱插件跑在"虚拟进程执行单元"里。执行单元是调度器内的一个数据结构（配置 + 原子
//! 计数器 + 代数 + 句柄），**不是 OS 进程、不是线程隔离域**。它提供的是：
//!
//! - **逻辑隔离**：状态、配额、超时、取消、崩溃后的单元重建；
//! - **API 限制**：只能通过工具注册表暴露的函数调用其他插件；
//! - **资源配额**：并发数、调用速率、返回体积、生命周期。
//!
//! 它**不是**安全边界。在单宿主进程内，同进程的 native 代码可以：
//!
//! - 读其他插件的内存（共享地址空间）；
//! - 调用任意宿主 API（没有 API 白名单的实现层）。
//!
//! **除非沙箱执行单元实现为 WASM 字节码沙箱**（自有线性内存 + 计量 + 无 native 逃逸），
//! 否则挡不住 native 代码越界。当前实现是纯 Rust 单元的配额与隔离，**不是** WASM。
//! `UnitConfig::memory_limit_bytes` 因此只是**声明性字段**，无法强制执行。
//!
//! ## 3. 重启管理器不覆盖可信插件
//!
//! 可信插件崩溃 = 宿主故障（因为它就在宿主里）。[`restart::RestartManager`] 只订阅
//! **沙箱单元**的崩溃事件；代码里不存在"重启可信插件"的路径。可信插件出问题的恢复
//! 手段只有一条：**用户重启启动器**。
//!
//! # 模块地图
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`error`] | 10 个错误码 + MCP 同构映射 |
//! | [`types`] | 元数据、信任级别、函数引用、结果形状 |
//! | [`json`] | inputSchema 校验（fail-closed 子集） |
//! | [`catch_unwind`] | 异步 panic 捕获（包住 Future::poll） |
//! | [`scheduler`] | 虚拟进程执行单元 + 状态机 + 配额 + 超时 |
//! | [`registry`] | 工具登记 / ACL / **call_tool 分流** |
//! | [`restart`] | 指数退避 + 滑动窗口熔断 + 重建流程 |
//! | [`signature`] | 包整体验签 + 吊销列表（自身签名） |
//! | [`loader`] | 验签 → 信任分级 → 降级到沙箱 |
//! | [`mcp`] | 无状态 MCP 语义对接 |
//!
//! # 快速上手
//!
//! ```no_run
//! use std::sync::Arc;
//! use launcher_plugin_framework::prelude::*;
//!
//! # #[tokio::main(flavor = "multi_thread", worker_threads = 4)]
//! # async fn main() {
//! // 1) 调度器 + 注册表
//! let scheduler = Arc::new(TokioScheduler::new());
//! let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));
//! # }
//! ```

pub mod catch_unwind;
pub mod error;
pub mod json;
pub mod loader;
pub mod mcp;
pub mod registry;
pub mod restart;
pub mod scheduler;
pub mod signature;
pub mod types;

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::catch_unwind::{catch_unwind_call, CatchUnwind, PanicPayload};
    pub use crate::error::{LoadError, RegistryError, SchedulerError, ToolError, ToolErrorCode};
    pub use crate::loader::{LoadedPlugin, PluginLoader};
    pub use crate::mcp::{ClientIdentity, McpAdapter, StatelessEnvelope};
    pub use crate::registry::{impl_registry::ToolRegistryImpl, ToolRegistry};
    pub use crate::restart::{BackoffPolicy, CircuitBreaker, PluginRuntime, RestartManager};
    pub use crate::scheduler::{
        state::UnitState, unit_config, CrashEvent, CrashReason, Scheduler, Task, TokioScheduler,
        UnitConfig,
    };
    pub use crate::signature::{
        BuiltInKeys, Ed25519Verifier, PluginManifest, PluginPackage, SignatureVerifier,
        VerificationOutcome,
    };
    pub use crate::types::{
        CancelOutcome, FunctionRef, PluginId, TaskOutcome, ToolFilter, ToolListing, ToolMeta,
        ToolResult, TrustLevel, UnavailableReason, UnitHandle,
    };
}

/// crate 版本，便于宿主把它写进诊断信息。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// 编译期断言：宿主必须用 unwind。
//
// `catch_unwind` 在 `panic = "abort"` 下不工作，此时可信插件机制不成立。
// 我们无法在库内强制宿主的 profile，但可以把这个立场写进文档并在 `build.rs` 检查
// 本 crate 自身的 profile（见 docs/security-boundary.md）。
//
// 注：不写成 `cfg(feature = "enforce_unwind")` 形式的编译期断言，是因为该 feature
// 需要在 Cargo.toml 里声明才能被 cfg 识别，而声明一个"默认不开、开了就报错"的 feature
// 对宿主没有实际约束力 —— 真正的约束是"宿主别用 abort profile"，那是宿主的构建配置。
