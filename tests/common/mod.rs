//! 测试支撑：构造一个最小的宿主管线（调度器 + 注册表 + 验签器 + 重启管理器）。
//!
//! 所有集成测试共用它，避免每个测试重复装配。

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::restart::SleepFn;
use launcher_plugin_framework::scheduler::UnitConfig;
use launcher_plugin_framework::types::BoxToolFuture;

pub struct Harness {
    pub scheduler: Arc<dyn Scheduler>,
    pub registry: Arc<ToolRegistryImpl>,
    pub verifier: Arc<Ed25519Verifier>,
    pub restarts: Arc<RestartManager>,
    pub caller: PluginId,
}

/// 构造一个"允许无吊销列表"的验签器（集成测试不关心吊销，只关心分流）。
pub fn permissive_verifier() -> Arc<Ed25519Verifier> {
    Arc::new(Ed25519Verifier::new(BuiltInKeys::default()).without_required_revocation_list())
}

impl Harness {
    /// 常规装配：毫秒级退避 + 立即 sleep，便于快速断言重启序列。
    pub fn new() -> Self {
        Self::with_sleep(Arc::new(launcher_plugin_framework::restart::instant_sleep))
    }

    /// 可注入 sleep：用于测试真实的退避等待时长。
    pub fn with_sleep(sleep: SleepFn) -> Self {
        let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
        let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));
        let verifier = permissive_verifier();
        let restarts = RestartManager::with_sleep(
            scheduler.clone(),
            verifier.clone(),
            registry.clone(),
            BackoffPolicy {
                initial: Duration::from_millis(1),
                factor: 2,
                max: Duration::from_millis(30),
            },
            sleep,
        );
        registry.install_restart_hook(Arc::new(
            launcher_plugin_framework::restart::ManagerRestartHook(Arc::clone(&restarts)),
        ));
        Self {
            scheduler,
            registry,
            verifier,
            restarts,
            caller: PluginId::new(PluginId::PLATFORM),
        }
    }

    pub fn unit_cfg(&self, plugin_id: &str) -> UnitConfig {
        let mut c = unit_config(plugin_id);
        c.idle_timeout_ms = None;
        c.max_lifetime_ms = None;
        c.max_concurrent_calls = 8;
        c.max_calls_per_sec = None;
        c.uninterruptible_grace_ms = 50;
        c
    }
}

/// 构造一个沙箱工具元数据。
pub fn sandboxed_meta(
    pid: &str,
    unit: &UnitHandle,
    tool: &str,
    timeout_ms: u64,
    body: impl Fn(serde_json::Value) -> BoxToolFuture + Send + Sync + 'static,
) -> ToolMeta {
    let pid = PluginId::new(pid);
    let name = format!("{pid}.{tool}");
    ToolMeta {
        name: name.clone(),
        description: format!("test {tool}"),
        input_schema: serde_json::json!({"type": "object"}),
        handler: FunctionRef::new(pid.clone(), name.clone(), body),
        trust_level: TrustLevel::Sandboxed,
        allowed_callers: vec![PluginId::new("*")],
        destructive: false,
        timeout_ms,
        owner_plugin_id: pid.clone(),
        unit_handle: Some(unit.clone()),
    }
}

/// 构造一个可信工具元数据。
pub fn trusted_meta(
    pid: &str,
    tool: &str,
    body: impl Fn(serde_json::Value) -> BoxToolFuture + Send + Sync + 'static,
) -> ToolMeta {
    let pid = PluginId::new(pid);
    let name = format!("{pid}.{tool}");
    ToolMeta {
        name: name.clone(),
        description: format!("test {tool}"),
        input_schema: serde_json::json!({"type": "object"}),
        handler: FunctionRef::new(pid.clone(), name.clone(), body),
        trust_level: TrustLevel::Trusted,
        allowed_callers: vec![PluginId::new("*")],
        destructive: false,
        timeout_ms: 30_000,
        owner_plugin_id: pid.clone(),
        unit_handle: None,
    }
}

/// 提取 `structuredContent.code`。
pub fn code_of(r: &ToolResult) -> Option<String> {
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}
