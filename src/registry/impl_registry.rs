//! `ToolRegistry` 的实现，包含 `call_tool` 的**完整分流逻辑**。
//!
//! # 检查顺序（规格硬要求）
//!
//! ```text
//! 1. 工具存在        → 否则 ToolNotFound
//! 2. 调用方有 ACL    → 否则 PermissionDenied
//! 3. 执行单元状态可用 → 否则 ProcessRestarting / ProcessCrashed / ProcessFailed
//! 4. 参数校验        → 否则 InvalidArguments
//! 5. 分流执行
//! ```
//!
//! 任何一步失败都**不进入执行**。第 4 步（参数校验）放在状态检查之后，是因为
//! 规格的检查顺序里没有它，而"插件正在重启"比"参数写错了"是更准确的信息 ——
//! 用户改参数也没用，得等插件起来。这条顺序在 docs/open-questions.md 里标为待确认。
//!
//! # 分流的两个分支
//!
//! ```text
//! if trust_level == Trusted:
//!     try: return await with_catch_unwind(handler(args))
//!     catch e: log; return ToolError::HandlerPanic(e)
//! else:
//!     unit = meta.unit_handle
//!     if unit.state != Running: return ProcessRestarting / ProcessCrashed
//!     return await scheduler.submit(unit, handler, meta.timeout_ms)
//! ```
//!
//! 可信分支**不经过调度器**：`catch_unwind_call` 直接驱动 handler future。
//! 因此可信插件的调用延迟里没有一次额外的 task spawn，也没有配额检查。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::catch_unwind::catch_unwind_call;
use crate::error::{RegistryError, ToolError, ToolErrorCode};
#[cfg(test)]
use crate::scheduler::CrashReason;
use crate::scheduler::{state::UnitState, Scheduler, Task};
use crate::types::{
    PluginId, ToolFilter, ToolListing, ToolMeta, ToolResult, TrustLevel, UnavailableReason,
    UnitHandle,
};

use super::{DestructiveConfirmHook, RegistryFuture, RestartHook, ToolMetaSnapshot, ToolRegistry};

// 说明：`ToolMetaSnapshotExt` 只是为了让两个模块共享一个 trait 扩展，
// 见 types.rs 末部的实现。若没有它，下面的 `from_meta` 调用需要重复字段列表。

struct Entry {
    meta: ToolMeta,
    /// 由重启管理器/加载器写入的"额外不可用提示"。
    /// 与调度器实时状态是**与**关系：任一说不可用，就不可用。
    note: Option<UnavailableReason>,
}

pub struct ToolRegistryImpl {
    entries: RwLock<HashMap<String, Entry>>,
    scheduler: Arc<dyn Scheduler>,
    restart_hook: RwLock<Option<Arc<dyn RestartHook>>>,
    confirm_hook: RwLock<Option<Arc<dyn DestructiveConfirmHook>>>,
    /// 统计：便于 UI 展示"这个插件被调了多少次"。
    call_counters: Mutex<HashMap<String, AtomicU64>>,
}

impl ToolRegistryImpl {
    pub fn new(scheduler: Arc<dyn Scheduler>) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            scheduler,
            restart_hook: RwLock::new(None),
            confirm_hook: RwLock::new(None),
            call_counters: Mutex::new(HashMap::new()),
        }
    }

    fn bump_counter(&self, name: &str) {
        let mut map = self.call_counters.lock().expect("counters mutex poisoned");
        map.entry(name.to_owned())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn call_count(&self, name: &str) -> u64 {
        self.call_counters
            .lock()
            .expect("counters mutex poisoned")
            .get(name)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// 计算一个条目的运行时可用性。
    ///
    /// 优先级：显式 `note` > 调度器状态。理由：`note` 能表达调度器表达不了的状态
    /// （如 `CircuitOpen`），而且它是**更晚的信息**（重启管理器刚写的）。
    fn availability(
        &self,
        meta: &ToolMeta,
        note: Option<UnavailableReason>,
    ) -> (bool, Option<UnavailableReason>) {
        // 显式提示优先（重启中 / 熔断 / 已失败 …）。
        if let Some(reason) = note {
            return (false, Some(reason));
        }
        // 可信插件没有执行单元，永远可用（它崩了的话宿主已经没了）。
        if meta.trust_level == TrustLevel::Trusted {
            return (true, None);
        }
        // 沙箱插件：以调度器实时状态为准。
        let Some(unit) = &meta.unit_handle else {
            // 元数据非法（注册时已挡，这里是防御）。
            return (false, Some(UnavailableReason::Failed));
        };
        match self.scheduler.get_state(unit.clone()) {
            Some(UnitState::Running) => (true, None),
            Some(UnitState::Restarting) => (false, Some(UnavailableReason::Restarting)),
            Some(UnitState::Crashed) => (false, Some(UnavailableReason::Crashed)),
            Some(UnitState::TimedOut) => (false, Some(UnavailableReason::TimedOut)),
            Some(UnitState::Failed) => (false, Some(UnavailableReason::Failed)),
            Some(UnitState::Stopped) => (false, Some(UnavailableReason::Stopped)),
            // 句柄代数过期 / 单元不存在：等同于该插件没有活单元。
            None => (false, Some(UnavailableReason::Failed)),
        }
    }

    fn to_listing(&self, entry: &Entry, caller: &PluginId) -> Option<ToolListing> {
        // ACL 在 list 阶段就过滤，避免 UI 展示"看得见但点不动"的按钮。
        if !entry.meta.allows_caller(caller) {
            return None;
        }
        let (available, reason) = self.availability(&entry.meta, entry.note);
        Some(ToolListing {
            name: entry.meta.name.clone(),
            description: entry.meta.description.clone(),
            input_schema: entry.meta.input_schema.clone(),
            trust_level: entry.meta.trust_level,
            destructive: entry.meta.destructive,
            owner_plugin_id: entry.meta.owner_plugin_id.0.clone(),
            timeout_ms: entry.meta.timeout_ms,
            available,
            unavailable_reason: reason,
        })
    }

    /// 第 3 步：执行单元状态检查。可信插件直接通过。
    fn check_unit_state(&self, meta: &ToolMeta) -> Result<(), ToolError> {
        if meta.trust_level == TrustLevel::Trusted {
            return Ok(());
        }
        let Some(unit) = &meta.unit_handle else {
            return Err(ToolError::process_failed(
                meta.owner_plugin_id.as_str(),
                &meta.name,
                "沙箱工具缺少 unit_handle（元数据不一致）",
            ));
        };
        match self.scheduler.get_state(unit.clone()) {
            Some(UnitState::Running) => Ok(()),
            Some(UnitState::Restarting) => Err(ToolError::process_restarting(
                meta.owner_plugin_id.as_str(),
                &meta.name,
            )),
            Some(UnitState::Failed) => Err(ToolError::process_failed(
                meta.owner_plugin_id.as_str(),
                &meta.name,
                "重启失败后已停止自动重试，等待用户手动恢复",
            )),
            Some(UnitState::Stopped) => Err(ToolError::process_failed(
                meta.owner_plugin_id.as_str(),
                &meta.name,
                "执行单元已被停止",
            )),
            // Crashed / TimedOut / 不存在
            _ => Err(ToolError::process_crashed(
                meta.owner_plugin_id.as_str(),
                &meta.name,
            )),
        }
    }
}

// ----------------------------------------------------------------------------
// trait 实现
// ----------------------------------------------------------------------------

impl ToolRegistry for ToolRegistryImpl {
    fn register_tool(&self, meta: ToolMeta) -> Result<(), RegistryError> {
        // --- 元数据自洽性（名称格式 / trust 与 unit 匹配 / schema 合法性） ---
        if let Err(msg) = meta.validate() {
            return Err(RegistryError::MalformedName {
                name: format!("{} ({msg})", meta.name),
                expected_prefix: meta.owner_plugin_id.tool_prefix(),
            });
        }

        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        if entries.contains_key(&meta.name) {
            return Err(RegistryError::DuplicateName {
                name: meta.name.clone(),
            });
        }

        // 一个插件可以有多个工具，但它们必须共用同一个执行单元。
        let expected_unit = entries
            .values()
            .find(|e| e.meta.owner_plugin_id == meta.owner_plugin_id)
            .and_then(|e| e.meta.unit_handle.clone());
        if let (Some(expected), Some(given)) = (expected_unit, meta.unit_handle.as_ref()) {
            if expected.generation != given.generation {
                return Err(RegistryError::TrustLevelMismatch {
                    name: meta.name.clone(),
                });
            }
        }

        let name = meta.name.clone();
        entries.insert(name, Entry { meta, note: None });
        Ok(())
    }

    fn unregister_tool(&self, caller: &PluginId, name: &str) -> Result<(), RegistryError> {
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        let Some(entry) = entries.get(name) else {
            // 注销不存在的工具：幂等成功还是报错？这里选择幂等成功，
            // 因为卸载流程不该因为"已经没了"而失败。
            return Ok(());
        };
        // 平台层可注销任何工具；插件只能注销自己的。
        let is_platform = caller.as_str() == PluginId::PLATFORM;
        if !is_platform && entry.meta.owner_plugin_id != *caller {
            return Err(RegistryError::ForeignOwnership {
                name: name.to_owned(),
                owner: entry.meta.owner_plugin_id.0.clone(),
            });
        }
        entries.remove(name);
        Ok(())
    }

    fn list_tools(&self, caller: &PluginId, filter: &ToolFilter) -> Vec<ToolListing> {
        let entries = self.entries.read().expect("entries rwlock poisoned");
        let mut out: Vec<ToolListing> = entries
            .values()
            .filter_map(|e| self.to_listing(e, caller))
            .filter(|l| filter.matches(l))
            .collect();
        // 稳定排序：UI 才不会在每次 poll 时跳动。
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn call_tool(
        &self,
        caller: &PluginId,
        name: &str,
        args: serde_json::Value,
    ) -> RegistryFuture<'_, ToolResult> {
        let name = name.to_owned();
        let caller = caller.clone();
        Box::pin(async move {
            // ============ 第 1 步：工具存在 ============
            // 注意：这里必须先把元数据的关键字段 copy 出来，然后**释放读锁**，
            // 因为后面的 await 可能很长（沙箱 handler 可以跑几百毫秒）。
            // 持锁跨 await 会让 RwLock 变成全局串行点。
            let (meta, note) = {
                let entries = self.entries.read().expect("entries rwlock poisoned");
                match entries.get(&name) {
                    Some(e) => (e.meta.clone(), e.note),
                    None => {
                        return ToolResult::error(ToolError::tool_not_found(caller.as_str(), &name))
                    }
                }
            };

            // ============ 第 2 步：ACL ============
            if !meta.allows_caller(&caller) {
                return ToolResult::error(ToolError::permission_denied(
                    caller.as_str(),
                    &name,
                    meta.owner_plugin_id.as_str(),
                ));
            }

            // ============ 第 3 步：执行单元状态 ============
            // 显式 note（重启中/熔断）优先：它比调度器状态更晚、更准确。
            if let Some(reason) = note {
                return ToolResult::error(self.note_to_error(&meta, reason));
            }
            if let Err(e) = self.check_unit_state(&meta) {
                return ToolResult::error(e);
            }

            // ============ 第 4 步：参数校验 ============
            if let Err(e) = crate::json::validate(&args, &meta.input_schema) {
                return ToolResult::error(ToolError::invalid_arguments(
                    meta.owner_plugin_id.as_str(),
                    &name,
                    &e.to_string(),
                ));
            }

            // ============ 破坏性操作的二次确认（可选挂载点） ============
            if meta.destructive {
                let hook = self
                    .confirm_hook
                    .read()
                    .expect("confirm_hook rwlock poisoned")
                    .clone();
                if let Some(hook) = hook {
                    let snap = ToolMetaSnapshot::from_meta(&meta);
                    if let Some(deny) = hook.confirm(&caller, &snap).await {
                        return ToolResult::error(deny);
                    }
                }
            }

            self.bump_counter(&name);

            // ============ 第 5 步：分流执行 ============
            if meta.trust_level == TrustLevel::Trusted {
                self.dispatch_trusted(meta, args).await
            } else {
                self.dispatch_sandboxed(meta, args).await
            }
        })
    }

    fn restart_unit(
        &self,
        caller: &PluginId,
        plugin_id: &PluginId,
    ) -> RegistryFuture<'_, Result<(), ToolError>> {
        let caller = caller.clone();
        let plugin_id = plugin_id.clone();
        Box::pin(async move {
            // 规格：restart_unit 仅平台层/用户可调用，插件不可自行重启。
            if caller.as_str() != PluginId::PLATFORM {
                return Err(ToolError::new(
                    ToolErrorCode::PermissionDenied,
                    caller.as_str(),
                    "restart_unit 仅平台层/用户可调用，插件不可自行重启",
                ));
            }
            let hook = self
                .restart_hook
                .read()
                .expect("restart_hook rwlock poisoned")
                .clone();
            let Some(hook) = hook else {
                return Err(ToolError::new(
                    ToolErrorCode::ProcessFailed,
                    plugin_id.as_str(),
                    "重启管理器尚未装配，无法重启",
                ));
            };
            hook.request_manual_restart(plugin_id).await
        })
    }

    fn note_availability(&self, plugin_id: &PluginId, reason: Option<UnavailableReason>) {
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        for e in entries.values_mut() {
            if e.meta.owner_plugin_id == *plugin_id {
                e.note = reason;
            }
        }
    }

    fn rebind_unit(&self, plugin_id: &PluginId, unit: UnitHandle) -> Result<usize, RegistryError> {
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        let mut n = 0;
        for e in entries.values_mut() {
            if e.meta.owner_plugin_id != *plugin_id {
                continue;
            }
            // 可信插件的工具不该有 unit_handle；沙箱插件的必须更新到新代数。
            e.meta.unit_handle = Some(unit.clone());
            e.note = None; // 重新可用
            n += 1;
        }
        Ok(n)
    }

    fn unregister_plugin(&self, plugin_id: &PluginId) -> usize {
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        let doomed: Vec<String> = entries
            .values()
            .filter(|e| e.meta.owner_plugin_id == *plugin_id)
            .map(|e| e.meta.name.clone())
            .collect();
        for name in &doomed {
            entries.remove(name);
        }
        doomed.len()
    }

    fn tool_meta(&self, name: &str) -> Option<ToolMetaSnapshot> {
        let entries = self.entries.read().expect("entries rwlock poisoned");
        entries
            .get(name)
            .map(|e| ToolMetaSnapshot::from_meta(&e.meta))
    }

    fn install_restart_hook(&self, hook: Arc<dyn RestartHook>) {
        *self
            .restart_hook
            .write()
            .expect("restart_hook rwlock poisoned") = Some(hook);
    }

    fn set_destructive_confirm_hook(&self, hook: Arc<dyn DestructiveConfirmHook>) {
        *self
            .confirm_hook
            .write()
            .expect("confirm_hook rwlock poisoned") = Some(hook);
    }
}

// ----------------------------------------------------------------------------
// 分流实现（独立方法，便于单独阅读与测试）
// ----------------------------------------------------------------------------

impl ToolRegistryImpl {
    fn note_to_error(&self, meta: &ToolMeta, reason: UnavailableReason) -> ToolError {
        let pid = meta.owner_plugin_id.as_str();
        let name = meta.name.as_str();
        match reason {
            UnavailableReason::Restarting => ToolError::process_restarting(pid, name),
            UnavailableReason::CircuitOpen => ToolError::process_failed(
                pid,
                name,
                "滑动窗口内重启次数超限，熔断已打开，等待用户手动恢复",
            ),
            UnavailableReason::Failed => {
                ToolError::process_failed(pid, name, "插件已标记 Failed，不再自动重试")
            }
            UnavailableReason::Stopped => {
                ToolError::process_failed(pid, name, "插件执行单元已停止")
            }
            UnavailableReason::TimedOut => ToolError::process_crashed(pid, name),
            UnavailableReason::Crashed => ToolError::process_crashed(pid, name),
        }
    }

    /// **可信插件路径：直接调用 + 异步 panic 捕获。**
    ///
    /// 无调度器、无 task spawn、无配额。这是规格的明确要求，也是可信插件性能的来源。
    async fn dispatch_trusted(&self, meta: ToolMeta, args: serde_json::Value) -> ToolResult {
        let plugin_id = meta.owner_plugin_id.as_str().to_owned();
        let tool_name = meta.name.clone();
        let handler = meta.handler.handler_arc();

        // `catch_unwind_call` 覆盖两个阶段：构造 future（同步）与 poll future（异步）。
        // 若只包住 poll，handler 在返回 future 之前 panic 就会逃逸到宿主。
        let outcome = catch_unwind_call(&plugin_id, &tool_name, || handler(args)).await;

        match outcome {
            Ok(result) => result,
            Err(payload) => {
                // 规格：catch e: log(plugin_id, tool_name, e); return HandlerPanic(e)
                // 日志由 catch_unwind_call 内部已打（含 payload），这里构造协议结果。
                self.bump_panic(&tool_name);
                ToolResult::error(
                    ToolError::handler_panic(&plugin_id, &tool_name, &payload.text)
                        .with_tool(&tool_name),
                )
            }
        }
    }

    /// **沙箱插件路径：交给调度器。**
    async fn dispatch_sandboxed(&self, meta: ToolMeta, args: serde_json::Value) -> ToolResult {
        let Some(unit) = meta.unit_handle.clone() else {
            return ToolResult::error(ToolError::process_failed(
                meta.owner_plugin_id.as_str(),
                &meta.name,
                "沙箱工具缺少 unit_handle",
            ));
        };
        // 二次确认（状态检查已在 call_tool 里做过，但两处 await 之间状态可能变；
        // 交给 scheduler.submit 再判一次，它的准入检查不会放过非 Running）。
        let task = Task::new(meta.handler.clone(), args);
        let outcome = self.scheduler.submit(unit, task, meta.timeout_ms).await;

        // 关键：**任何** outcome 都转成 MCP 形状。崩溃与业务报错同构。
        let result = outcome.into_result();

        // 若调度器回报"不可中断"，规格要求升级为单元级崩溃 —— 调度器内部已经
        // 自行 raise_crash，这里只需确保错误码正确（scheduler 已返回 Uninterruptible）。
        result
    }

    fn bump_panic(&self, name: &str) {
        // 计数器复用同一张表即可；单独一个方法是为了语义清晰。
        self.bump_counter(name);
    }
}

/// 诊断：把一次调用的完整决策链描述成字符串。给 `--verbose` 或日志用。
pub fn describe_dispatch(
    meta: &ToolMeta,
    caller: &PluginId,
    unit_state: Option<UnitState>,
) -> String {
    let acl = if meta.allows_caller(caller) {
        "allow"
    } else {
        "deny"
    };
    match meta.trust_level {
        TrustLevel::Trusted => {
            format!("trusted path: acl={acl} -> catch_unwind(handler)  [scheduler not involved]")
        }
        TrustLevel::Sandboxed => format!(
            "sandboxed path: acl={acl} -> unit_state={} -> scheduler.submit(timeout={}ms)",
            unit_state
                .map(|s| s.to_string())
                .unwrap_or_else(|| "<missing>".into()),
            meta.timeout_ms
        ),
    }
}

/// 测试辅助：直接构造一个"沙箱崩溃时调度器返回的事件原因"。
///
/// 只在测试编译单元里有意义（生产代码不需要伪造崩溃事件）。
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn crash_reason_for_test() -> CrashReason {
    CrashReason::HandlerPanic {
        payload: "test".into(),
    }
}
