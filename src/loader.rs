//! 插件加载器：**验签 → 决定信任分级 → 注册工具 → 装配沙箱**。
//!
//! # 这一层负责的唯一决策
//!
//! ```text
//! 验签 Valid（且未被吊销）            → TrustLevel::Trusted    → 无单元，直接调
//! 其余一切（Unsigned/Invalid/Revoked/ → TrustLevel::Sandboxed → create_unit + 调度器
//!   UnsupportedAlgorithm/RevocationUnverifiable）
//! ```
//!
//! 签名校验失败**降级到沙箱，不拒绝加载**。这是规格的硬要求，也是本模块最核心的一条
//! 分支。UI 提示文案来自 [`crate::signature::VerificationOutcome::user_message`]。
//!
//! # 与安全边界声明的关系
//!
//! 加载器是"唯一防线"的执行点。它一旦判断错误（例如把无效签名判成 Valid），
//! 插件就会进入宿主执行上下文并拥有与启动器等权限 —— 之后没有任何组件会再拦它。
//! 因此本模块的所有判定都写成"默认拒绝、显式放行"的形状。

use std::sync::Arc;

use crate::error::{LoadError, ToolError, ToolErrorCode};
use crate::registry::impl_registry::ToolRegistryImpl;
use crate::registry::ToolRegistry;
use crate::restart::{ManagerRestartHook, PluginRuntime, RestartManager};
use crate::scheduler::Scheduler;
use crate::signature::{PluginPackage, SignatureVerifier, VerificationOutcome};
use crate::types::{PluginId, ToolMeta, TrustLevel, UnitHandle};

/// 一次加载的结果。
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub plugin_id: PluginId,
    pub trust_level: TrustLevel,
    /// 仅沙箱插件有。
    pub unit: Option<UnitHandle>,
    /// 验签结论。**必须透出给 UI**（规格要求提示"未通过签名校验，运行在受限沙箱"）。
    pub verification: VerificationOutcome,
    pub registered_tools: usize,
}

impl LoadedPlugin {
    /// UI 提示文案。可信插件返回 `None`（不需要提示）。
    pub fn user_notice(&self) -> Option<&'static str> {
        if self.trust_level.is_trusted() {
            None
        } else {
            Some(self.verification.user_message())
        }
    }
}

/// 加载器。持有加载插件所需的全部依赖。
pub struct PluginLoader {
    pub registry: Arc<ToolRegistryImpl>,
    pub scheduler: Arc<dyn Scheduler>,
    pub verifier: Arc<dyn SignatureVerifier>,
    pub restarts: Arc<RestartManager>,
}

impl PluginLoader {
    /// 加载一个插件。
    ///
    /// `build_tools`: 构造该插件的工具元数据。**关键区别**：
    /// - 可信插件调用 `build_tools(None)`，元数据里 `unit_handle` 必须为 `None`；
    /// - 沙箱插件调用 `build_tools(Some(unit))`，元数据里 `unit_handle` 为对应句柄。
    ///
    /// 把"是否需要单元"交给同一个闭包，是为了让测试与真实插件复用同一份工具定义。
    pub async fn load(
        &self,
        package: PluginPackage,
        unit_config_template: crate::scheduler::UnitConfig,
        build_tools: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync>,
    ) -> Result<LoadedPlugin, LoadError> {
        // ---- 0) 清单自洽性：清单里的 id 必须是包的 plugin_id ----
        if package.manifest.id != package.plugin_id.0 {
            return Err(LoadError::PluginIdMismatch {
                declared: package.manifest.id.clone(),
                actual: package.plugin_id.0.clone(),
            });
        }
        let plugin_id = package.plugin_id.clone();
        let loaded_digest = package.digest();

        // ---- 1) 验签（**永不 Err**，失败是一种结论）----
        let outcome = self.verifier.verify(&package);

        // ---- 2) 决定信任分级：只有 Valid 才进宿主上下文 ----
        if outcome.is_trusted() {
            return self
                .load_trusted(plugin_id, package, outcome, build_tools)
                .await;
        }

        // ---- 2') 降级到沙箱（规格：不拒绝加载）----
        match outcome.user_message() {
            "" => {}
            msg => eprintln!("[loader] {plugin_id}: {msg}（{}）", outcome.detail()),
        }
        self.load_sandboxed(
            plugin_id,
            package,
            outcome,
            loaded_digest,
            unit_config_template,
            build_tools,
        )
        .await
    }

    async fn load_trusted(
        &self,
        plugin_id: PluginId,
        _package: PluginPackage,
        outcome: VerificationOutcome,
        build_tools: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync>,
    ) -> Result<LoadedPlugin, LoadError> {
        let metas = build_tools(None);
        if metas.is_empty() {
            return Err(LoadError::BadManifest("插件未声明任何工具".to_owned()));
        }
        // 强制校验：可信插件不得携带 unit_handle。这是 `ToolMeta::validate` 已做的，
        // 但这里再明确一次，因为它是"可信路径不经过调度器"的结构性保证。
        for m in &metas {
            if m.unit_handle.is_some() {
                return Err(LoadError::BadManifest(format!(
                    "可信插件 {} 的工具 {} 携带了 unit_handle",
                    plugin_id, m.name
                )));
            }
            if !m.trust_level.is_trusted() {
                return Err(LoadError::BadManifest(format!(
                    "可信插件 {} 的工具 {} 声明为 Sandboxed",
                    plugin_id, m.name
                )));
            }
        }
        let mut count = 0;
        for meta in metas {
            self.registry
                .register_tool(meta)
                .map_err(|e| LoadError::BadManifest(e.to_string()))?;
            count += 1;
        }
        Ok(LoadedPlugin {
            plugin_id,
            trust_level: TrustLevel::Trusted,
            unit: None,
            verification: outcome,
            registered_tools: count,
        })
    }

    async fn load_sandboxed(
        &self,
        plugin_id: PluginId,
        package: PluginPackage,
        outcome: VerificationOutcome,
        loaded_digest: [u8; 32],
        mut cfg: crate::scheduler::UnitConfig,
        build_tools: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync>,
    ) -> Result<LoadedPlugin, LoadError> {
        // ---- 1) 创建沙箱执行单元 ----
        cfg.plugin_id = plugin_id.clone();
        let unit = self
            .scheduler
            .create_unit(cfg.clone())
            .await
            .map_err(|e| LoadError::SandboxInitFailed(e.to_string()))?;

        // ---- 2) 注册工具（先注册，后装配重启管理器）----
        let metas = build_tools(Some(unit.clone()));
        if metas.is_empty() {
            // 回滚：不留下一个没人用的单元。
            self.scheduler.destroy(unit.clone()).await;
            return Err(LoadError::BadManifest("插件未声明任何工具".to_owned()));
        }
        for m in &metas {
            if m.unit_handle.as_ref().map(|u| u.generation) != Some(unit.generation) {
                self.scheduler.destroy(unit.clone()).await;
                return Err(LoadError::BadManifest(format!(
                    "沙箱插件 {} 的工具 {} 的 unit_handle 与创建出的单元不一致",
                    plugin_id, m.name
                )));
            }
        }
        let mut count = 0;
        for meta in metas {
            if let Err(e) = self.registry.register_tool(meta) {
                self.scheduler.destroy(unit.clone()).await;
                return Err(LoadError::BadManifest(e.to_string()));
            }
            count += 1;
        }

        // ---- 3) 装配重启管理（订阅该单元的崩溃事件）----
        // `build_tools` 需要在重启时用**新单元句柄**重建工具元数据，因此这里要把
        // 原始闭包包一层：它接收闭包，返回闭包。注意 build_tools 的签名是
        // `Fn(Option<UnitHandle>) -> Vec<ToolMeta>`，重启时需要 Some(新单元)。
        let restart_builder: Arc<dyn Fn(UnitHandle) -> Vec<ToolMeta> + Send + Sync> = {
            let bt = Arc::clone(&build_tools);
            Arc::new(move |h: UnitHandle| bt(Some(h)))
        };
        let runtime = PluginRuntime {
            plugin_id: plugin_id.clone(),
            package,
            loaded_digest,
            unit_config: cfg,
            build_tools: restart_builder,
        };
        self.restarts.watch_plugin(runtime, unit.clone()).await;

        // 注册表需要能通过 restart_unit 找到重启管理器。
        self.registry
            .install_restart_hook(Arc::new(ManagerRestartHook(Arc::clone(&self.restarts))));

        Ok(LoadedPlugin {
            plugin_id,
            trust_level: TrustLevel::Sandboxed,
            unit: Some(unit),
            verification: outcome,
            registered_tools: count,
        })
    }

    /// 卸载：注销工具 + 停止单元 + 停止受管。
    pub async fn unload(&self, loaded: &LoadedPlugin) {
        self.restarts.forget_plugin(&loaded.plugin_id).await;
        if let Some(unit) = loaded.unit.clone() {
            self.scheduler.destroy(unit).await;
        }
        self.registry.unregister_plugin(&loaded.plugin_id);
    }

    /// 热重载：重新验签 → 若信任级别变化则迁移执行上下文。
    ///
    /// 当前实现**保守**：只允许 Sandboxed → Sandboxed（重建单元）与
    /// Trusted → Trusted（原地替换工具）。跨级别迁移（Sandboxed ↔ Trusted）
    /// 需要原子地切换已在册工具的 trust_level 与 handler 来源，
    /// 见 docs/open-questions.md Q3。
    pub async fn hot_reload(
        &self,
        loaded: &LoadedPlugin,
        package: PluginPackage,
        build_tools: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync>,
        cfg: crate::scheduler::UnitConfig,
    ) -> Result<LoadedPlugin, ToolError> {
        let outcome = self.verifier.verify(&package);
        if outcome.is_trusted() != loaded.trust_level.is_trusted() {
            return Err(ToolError::new(
                ToolErrorCode::ProcessFailed,
                loaded.plugin_id.as_str(),
                format!(
                    "热重载改变了信任分级（{} -> {}），跨执行上下文迁移尚未实现；请先卸载再加载",
                    loaded.trust_level,
                    if outcome.is_trusted() {
                        TrustLevel::Trusted
                    } else {
                        TrustLevel::Sandboxed
                    }
                ),
            ));
        }
        // 卸载后重新加载：语义上等价于"停旧、起新"，中间窗口内工具短暂不可用。
        self.unload(loaded).await;
        self.load(package, cfg, build_tools).await.map_err(|e| {
            ToolError::new(
                ToolErrorCode::ProcessFailed,
                loaded.plugin_id.as_str(),
                format!("热重载失败：{e}"),
            )
        })
    }
}
