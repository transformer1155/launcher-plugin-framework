//! 重启管理器：**只管理沙箱执行单元，不管可信插件。**
//!
//! 可信插件崩溃 = 宿主故障（因为它在宿主执行上下文里）。恢复只能靠用户重启启动器。
//! 本模块的 `on_crash` 订阅只覆盖沙箱单元，因此代码里**不存在**"尝试重启可信插件"的路径。
//!
//! # 退避与熔断
//!
//! ```text
//! 尝试次数:      1      2      3      4      5      6+
//! 退避等待:     1s     2s     4s     8s    16s     30s（上限）
//!
//! 滑动窗口 = 5 分钟：窗口内重启次数 > 5（即第 6 次）→ CircuitOpen
//!   → 停止自动重启，note_availability(CircuitOpen)，等用户手动恢复
//! ```
//!
//! # 重启流程（规格逐条）
//!
//! ```text
//! destroy 旧单元 → 重新校验插件签名 → create_unit → 重新注册工具
//!   → 通知注册表更新 available 状态
//! ```
//!
//! 任一环节失败（沙箱初始化失败 / 包被篡改）→ 标记 `Failed`，**不再自动重试**。
//!
//! # 重启期间的状态（最容易漏的一点）
//!
//! 进入重启流程的**第一步**（早于退避等待）就 `note_availability(Restarting)`。
//! 这样退避窗口里 `list_tools` 也如实显示 `available: false, reason: "restarting"`，
//! `call_tool` 立即返回 `ProcessRestarting` 而**不排队**。
//!
//! # 为什么用 `self: &Arc<Self>`
//!
//! 崩溃回调被调度器（长生命周期）持有。回调如果持 `Arc<RestartManager>`，就形成
//! `scheduler → unit → callback → manager → scheduler` 的引用环，启动器退出时泄漏。
//! 因此回调只持 `Weak<RestartManager>`；而拿到 `Weak` 必须从 `Arc` 出发，
//! 所以订阅入口用 `self: &Arc<Self>`。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::error::{ToolError, ToolErrorCode};
use crate::registry::impl_registry::ToolRegistryImpl;
use crate::registry::{RegistryFuture, RestartHook, ToolRegistry};
use crate::scheduler::state::UnitState;
use crate::scheduler::{CrashEvent, Scheduler, SubscriptionId, UnitConfig};
use crate::signature::{PluginPackage, SignatureVerifier, VerificationOutcome};
use crate::types::{PluginId, ToolMeta, TrustLevel, UnavailableReason, UnitHandle};

// ----------------------------------------------------------------------------
// 退避策略（纯函数，可单测）
// ----------------------------------------------------------------------------

/// 指数退避参数。抽成结构体是为了让测试用"毫秒级起步"快速跑完，生产用默认值。
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub factor: u32,
    /// 上限（规格：30s）。
    pub max: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            factor: 2,
            max: Duration::from_secs(30),
        }
    }
}

impl BackoffPolicy {
    /// 第 `attempt` 次重试（从 1 开始）的等待时长。
    ///
    /// `1→1s, 2→2s, 3→4s, 4→8s, 5→16s, 6+→30s`。
    pub fn delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let mut d = self.initial;
        for _ in 1..attempt {
            d = d.saturating_mul(self.factor);
            if d >= self.max {
                return self.max;
            }
        }
        d.min(self.max)
    }
}

/// 滑动窗口熔断器。
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    pub window: Duration,
    /// 窗口内允许的重启次数上限；**超过**即熔断。
    pub max_restarts_in_window: u32,
    history: HashMap<PluginId, VecDeque<Instant>>,
    open: HashMap<PluginId, Instant>,
}

impl CircuitBreaker {
    pub fn new(window: Duration, max_restarts_in_window: u32) -> Self {
        Self {
            window,
            max_restarts_in_window,
            history: HashMap::new(),
            open: HashMap::new(),
        }
    }

    /// 记一次重启。返回 `true` 表示**刚刚**打开熔断（用于日志/通知 UI）。
    pub fn record_restart(&mut self, plugin_id: &PluginId, now: Instant) -> bool {
        let hist = self.history.entry(plugin_id.clone()).or_default();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while let Some(front) = hist.front() {
            if *front < cutoff {
                hist.pop_front();
            } else {
                break;
            }
        }
        hist.push_back(now);
        let count = hist.len() as u32;

        if count > self.max_restarts_in_window && !self.open.contains_key(plugin_id) {
            self.open.insert(plugin_id.clone(), now);
            return true;
        }
        false
    }

    pub fn is_open(&self, plugin_id: &PluginId) -> bool {
        self.open.contains_key(plugin_id)
    }

    pub fn count_in_window(&self, plugin_id: &PluginId, now: Instant) -> u32 {
        let Some(hist) = self.history.get(plugin_id) else {
            return 0;
        };
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        hist.iter().filter(|t| **t >= cutoff).count() as u32
    }

    /// 用户手动恢复：清空历史并关闭熔断。
    pub fn reset(&mut self, plugin_id: &PluginId) {
        self.history.remove(plugin_id);
        self.open.remove(plugin_id);
    }
}

// ----------------------------------------------------------------------------
// 插件运行时
// ----------------------------------------------------------------------------

/// 重建一个插件所需的全部信息。
///
/// 重启管理器不持有业务逻辑，只拿这个结构"重新走一遍加载流程"。具体有哪些工具、
/// handler 是谁，由加载器注入的 `build_tools` 闭包提供 —— 因此本模块不依赖
/// 任何工具的具体实现。
pub struct PluginRuntime {
    pub plugin_id: PluginId,
    /// 加载时的包。每次重启都重新验签它。
    pub package: PluginPackage,
    /// 加载时记录的包摘要。重启时比对其是否变化，用于发现"运行期间包被改写"。
    pub loaded_digest: [u8; 32],
    /// 单元配置（重启时复用同样的配额）。
    pub unit_config: UnitConfig,
    /// 用新单元句柄重建该插件全部工具的元数据。
    pub build_tools: Arc<dyn Fn(UnitHandle) -> Vec<ToolMeta> + Send + Sync>,
}

impl std::fmt::Debug for PluginRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRuntime")
            .field("plugin_id", &self.plugin_id)
            .finish_non_exhaustive()
    }
}

// ----------------------------------------------------------------------------
// 可观测状态
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRestartState {
    pub plugin_id: String,
    pub state: UnitState,
    /// 当前已重试次数（成功后清零）。
    pub attempts: u32,
    /// 窗口内重启次数。
    pub restarts_in_window: u32,
    pub circuit_open: bool,
    pub last_error: Option<String>,
}

struct Entry {
    runtime: PluginRuntime,
    unit: UnitHandle,
    crash_sub: Option<SubscriptionId>,
    /// 当前连续重试次数（成功重启后清零）。
    attempts: u32,
    /// 防并发双重启（两次崩溃事件几乎同时到达时只重启一次）。
    restarting: bool,
    last_error: Option<String>,
    /// 已标记 Failed：不再自动重启。
    failed: bool,
    /// 已被主动停止：不再自动重启。
    stopped: bool,
}

// ----------------------------------------------------------------------------
// RestartManager
// ----------------------------------------------------------------------------

/// 退避等待的抽象。生产用 tokio sleep；测试注入"立即返回"以毫秒级跑完。
pub type SleepFn = Arc<dyn Fn(Duration) -> SleepFuture + Send + Sync>;
pub type SleepFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

pub fn tokio_sleep(d: Duration) -> SleepFuture {
    Box::pin(tokio::time::sleep(d))
}

pub fn instant_sleep(_d: Duration) -> SleepFuture {
    Box::pin(async {})
}

pub struct RestartManager {
    scheduler: Arc<dyn Scheduler>,
    verifier: Arc<dyn SignatureVerifier>,
    registry: Arc<ToolRegistryImpl>,
    backoff: BackoffPolicy,
    breaker: Mutex<CircuitBreaker>,
    entries: RwLock<HashMap<PluginId, Entry>>,
    sleep: SleepFn,
    total_restarts: AtomicU64,
    total_given_up: AtomicU64,
}

impl std::fmt::Debug for RestartManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestartManager")
            .field("backoff", &self.backoff)
            .field(
                "total_restarts",
                &self.total_restarts.load(Ordering::Relaxed),
            )
            .field(
                "total_given_up",
                &self.total_given_up.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl RestartManager {
    pub fn new(
        scheduler: Arc<dyn Scheduler>,
        verifier: Arc<dyn SignatureVerifier>,
        registry: Arc<ToolRegistryImpl>,
    ) -> Arc<Self> {
        Self::with_sleep(
            scheduler,
            verifier,
            registry,
            BackoffPolicy::default(),
            Arc::new(tokio_sleep),
        )
    }

    /// 可注入退避参数与 sleep（测试用）。
    pub fn with_sleep(
        scheduler: Arc<dyn Scheduler>,
        verifier: Arc<dyn SignatureVerifier>,
        registry: Arc<ToolRegistryImpl>,
        backoff: BackoffPolicy,
        sleep: SleepFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            scheduler,
            verifier,
            registry,
            backoff,
            // 规格：5 分钟窗口，超过 5 次 → 熔断
            breaker: Mutex::new(CircuitBreaker::new(Duration::from_secs(5 * 60), 5)),
            entries: RwLock::new(HashMap::new()),
            sleep,
            total_restarts: AtomicU64::new(0),
            total_given_up: AtomicU64::new(0),
        })
    }

    /// 开始管理一个沙箱插件。
    ///
    /// 前置条件：该插件的工具已通过 `registry.register_tool` 注册。
    pub async fn watch_plugin(self: &Arc<Self>, runtime: PluginRuntime, unit: UnitHandle) {
        let plugin_id = runtime.plugin_id.clone();

        // 先登记 entry，再订阅崩溃事件：避免"刚订阅就崩溃"的回调找不到 entry。
        {
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            entries.insert(
                plugin_id.clone(),
                Entry {
                    runtime,
                    unit: unit.clone(),
                    crash_sub: None,
                    attempts: 0,
                    restarting: false,
                    last_error: None,
                    failed: false,
                    stopped: false,
                },
            );
        }

        let sub = self.subscribe_crash(Arc::downgrade(self), &plugin_id, unit.clone());
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        if let Some(e) = entries.get_mut(&plugin_id) {
            e.crash_sub = sub;
        }
    }

    /// 订阅某个单元的崩溃事件。回调只持 `Weak<Self>`，避免引用环。
    ///
    /// # 为什么在回调里**同步**标记 restarting
    ///
    /// 崩溃回调由调度器在 `submit` 返回**之前**同步调用。如果只把标记动作放进
    /// spawn 出去的异步任务里，那么"`call_tool` 返回 ProcessCrashed"到"标记为 restarting"
    /// 之间存在一个窗口，`list_tools` 会先看到 `crashed` 而不是规格要求的 `restarting`。
    /// 因此这里在回调内先做一次**只拿锁、不做 await** 的快速标记，让状态对外立即一致；
    /// 权威判定（熔断 / Failed）仍由异步流程负责。
    fn subscribe_crash(
        &self,
        weak: std::sync::Weak<Self>,
        plugin_id: &PluginId,
        unit: UnitHandle,
    ) -> Option<SubscriptionId> {
        let want = plugin_id.clone();
        let callback: crate::scheduler::CrashCallback = Arc::new(move |event: CrashEvent| {
            // 回调在调度器线程上**同步**执行，绝不能阻塞，也不能 await。
            let Some(mgr) = weak.upgrade() else {
                return; // 管理器已销毁（启动器退出中），忽略。
            };
            if event.unit.plugin_id != want {
                return;
            }

            // ---- 快速标记（同步、只拿锁）----
            // 若插件已 Failed / 已停止 / 熔断已打开，就不要谎报"重启中"。
            let should_mark_restarting = {
                let entries = mgr.entries.read().expect("entries rwlock poisoned");
                match entries.get(&event.unit.plugin_id) {
                    Some(e) if e.failed || e.stopped => false,
                    _ => {
                        let open = mgr
                            .breaker
                            .lock()
                            .expect("breaker mutex poisoned")
                            .is_open(&event.unit.plugin_id);
                        !open
                    }
                }
            };
            if should_mark_restarting {
                mgr.registry
                    .note_availability(&event.unit.plugin_id, Some(UnavailableReason::Restarting));
            }

            let mgr = Arc::clone(&mgr);
            // 现在才 spawn 异步的重启流程。
            tokio::spawn(async move {
                mgr.handle_crash(event).await;
            });
        });
        self.scheduler.on_crash(unit, callback).ok()
    }

    /// 崩溃处理：判定 → 退避 → 重建。
    async fn handle_crash(self: Arc<Self>, event: CrashEvent) {
        self.handle_crash_inner(event, false).await;
    }

    /// `manual == true` 表示用户/平台手动重启：重置熔断与 Failed 标记。
    async fn handle_crash_inner(self: Arc<Self>, event: CrashEvent, manual: bool) {
        let plugin_id = event.unit.plugin_id.clone();

        // ---- 取快照并做初步判定（不持锁做 await） ----
        let (should_restart, attempt) = {
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            let Some(entry) = entries.get_mut(&plugin_id) else {
                // 未受管的插件：不做任何事。这可能是一个已卸载但崩溃回调还没解绑的单元。
                return;
            };

            if manual {
                entry.failed = false;
                entry.stopped = false;
                entry.attempts = 0;
                self.breaker
                    .lock()
                    .expect("breaker mutex poisoned")
                    .reset(&plugin_id);
            }

            if entry.restarting {
                // 上一个重启流程还没走完。丢弃重复事件，不叠加重试。
                return;
            }
            if entry.failed || entry.stopped {
                // 已放弃或已停止：不再自动重启。
                return;
            }
            // 熔断打开：不自动重启；但手动重启已经在上面的 reset 里解锁了。
            if self
                .breaker
                .lock()
                .expect("breaker mutex poisoned")
                .is_open(&plugin_id)
            {
                // 已经在熔断：确保注册表也显示 CircuitOpen。
                self.registry
                    .note_availability(&plugin_id, Some(UnavailableReason::CircuitOpen));
                return;
            }

            entry.last_error = Some(event.reason.to_string());
            entry.attempts = entry.attempts.saturating_add(1);
            entry.restarting = true;
            (true, entry.attempts)
        };

        if !should_restart {
            return;
        }

        // ---- 第一步：立刻标记"重启中"（早于退避等待）----
        self.registry
            .note_availability(&plugin_id, Some(UnavailableReason::Restarting));
        // 单元状态也推进到 Restarting，让 get_state 与 UI 一致。
        let _ = self
            .scheduler
            .set_state(event.unit.clone(), UnitState::Restarting);

        // ---- 熔断计数（在退避之前记，这样"第 6 次"立即熔断）----
        let circuit_just_opened = self
            .breaker
            .lock()
            .expect("breaker mutex poisoned")
            .record_restart(&plugin_id, Instant::now());

        if circuit_just_opened {
            // 规格：窗口内超过 5 次 → 停止自动重启，等用户手动恢复。
            self.total_given_up.fetch_add(1, Ordering::Relaxed);
            self.registry
                .note_availability(&plugin_id, Some(UnavailableReason::CircuitOpen));
            let _ = self
                .scheduler
                .set_state(event.unit.clone(), UnitState::Failed);
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            if let Some(e) = entries.get_mut(&plugin_id) {
                e.failed = true;
                e.restarting = false;
                e.last_error =
                    Some("5 分钟窗口内重启次数超过 5 次，熔断已打开；等待用户手动恢复".to_owned());
            }
            return;
        }

        // ---- 退避等待 ----
        let delay = self.backoff.delay(attempt);
        (self.sleep)(delay).await;

        // ---- 重建 ----
        self.rebuild(plugin_id, event.unit).await;
    }

    /// 完整重启流程：destroy → 重新验签 → create_unit → 重新注册工具 → 更新注册表。
    async fn rebuild(self: Arc<Self>, plugin_id: PluginId, old_unit: UnitHandle) {
        // 取出运行时信息（clone 出闭包，不持锁跨 await）。
        let (runtime_pkg, loaded_digest, cfg, build_tools) = {
            let entries = self.entries.read().expect("entries rwlock poisoned");
            match entries.get(&plugin_id) {
                Some(e) => (
                    e.runtime.package.clone(),
                    e.runtime.loaded_digest,
                    e.runtime.unit_config.clone(),
                    Arc::clone(&e.runtime.build_tools),
                ),
                None => return,
            }
        };

        // 注意：这里不能用 async move 闭包捕获 self/plugin_id/old_unit —— 闭包会
        // 把它们 move 走，后续步骤就用不了了。改为在每个失败点显式调用 give_up。
        // ---- 1) destroy 旧单元 ----
        self.scheduler.destroy(old_unit.clone()).await;

        // ---- 2) 重新校验插件签名 ----
        // 三个有意义的结果：
        //   * 包摘要变了 → 运行期间包被改写 → Failed（这不是"降级"，是篡改）
        //   * 验签结论从 Valid 变成 Invalid/Revoked → 记录，但**继续在沙箱里跑**
        //     （它本来就在沙箱里；吊销不等于禁止运行，规格只要求降级）
        //   * Valid（热重载后重新获得可信签名）→ 记录"可升级为 Trusted"，但仍留在沙箱
        //     （升级执行上下文需要原子地切换工具 trust_level，见 open-questions Q3）
        if runtime_pkg.digest() != loaded_digest {
            return self
                .give_up(
                    plugin_id,
                    old_unit.clone(),
                    "插件包在运行期间被修改（摘要与加载时不一致）".to_owned(),
                )
                .await;
        }
        let outcome = self.verifier.verify(&runtime_pkg);
        match &outcome {
            VerificationOutcome::Valid { .. } => {
                // 已是沙箱：保持沙箱，但记录"现在可以升级"。
                eprintln!(
                    "[restart] {plugin_id}: 重启时验签通过，插件具备 Trusted 资格（当前仍运行在沙箱）"
                );
            }
            VerificationOutcome::Unsigned
            | VerificationOutcome::Invalid { .. }
            | VerificationOutcome::Revoked { .. }
            | VerificationOutcome::UnsupportedAlgorithm { .. }
            | VerificationOutcome::RevocationUnverifiable { .. } => {
                // 一切非 Valid 结论 → 继续沙箱。这是规格指定的降级路径，不是失败。
                eprintln!(
                    "[restart] {plugin_id}: 重启时未通过签名校验（{}），继续运行在受限沙箱",
                    outcome.detail()
                );
            }
        }

        // ---- 3) create_unit ----
        let mut cfg = cfg;
        cfg.plugin_id = plugin_id.clone();
        let new_unit = match self.scheduler.create_unit(cfg).await {
            Ok(u) => u,
            Err(e) => {
                return self
                    .give_up(plugin_id, old_unit.clone(), format!("沙箱初始化失败：{e}"))
                    .await;
            }
        };

        // ---- 4) 重新注册工具（先在注册表里清掉旧代数的绑定，再按新句柄登记）----
        // 顺序：先 rebind（把已有条目的 unit_handle 换到新代数）再补注册。
        // 如果插件在重启期间新增了工具，build_tools 会返回比原来更多的元数据，
        // 这里对新出现的名字补 register_tool。
        let tools = build_tools(new_unit.clone());
        if tools.is_empty() {
            self.scheduler.destroy(new_unit.clone()).await;
            return self
                .give_up(
                    plugin_id,
                    old_unit.clone(),
                    "插件未声明任何工具（build_tools 返回空）".to_owned(),
                )
                .await;
        }
        let _ = self.registry.rebind_unit(&plugin_id, new_unit.clone());
        for meta in tools {
            if self.registry.tool_meta(&meta.name).is_none() {
                if let Err(e) = self.registry.register_tool(meta) {
                    self.scheduler.destroy(new_unit.clone()).await;
                    return self
                        .give_up(
                            plugin_id,
                            old_unit.clone(),
                            format!("重新注册工具失败：{e}"),
                        )
                        .await;
                }
            }
        }
        // 重新绑定后要确保 trust_level 仍是 Sandboxed（rebind 只改 unit_handle）。
        debug_assert!(
            self.registry
                .list_tools(
                    &PluginId::new(PluginId::PLATFORM),
                    &crate::types::ToolFilter::all()
                )
                .iter()
                .all(|l| l.trust_level == TrustLevel::Sandboxed),
            "重启管理器只应管理沙箱插件"
        );

        // ---- 5) 通知注册表：恢复可用 ----
        self.registry.note_availability(&plugin_id, None);

        // ---- 6) 迁移崩溃订阅到新单元 ----
        let old_sub = {
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            entries.get_mut(&plugin_id).and_then(|e| e.crash_sub.take())
        };
        if let Some(sub) = old_sub {
            self.scheduler.unsubscribe_crash(sub);
        }
        let weak = Arc::downgrade(&self);
        let sub = self.subscribe_crash(weak, &plugin_id, new_unit.clone());

        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        if let Some(e) = entries.get_mut(&plugin_id) {
            e.unit = new_unit.clone();
            e.crash_sub = sub;
            e.attempts = 0; // 成功重启，连续失败计数清零
            e.restarting = false;
            e.last_error = None;
        }
        self.total_restarts.fetch_add(1, Ordering::Relaxed);
        eprintln!("[restart] {plugin_id}: 重启成功，新单元 {new_unit}");
    }

    /// 放弃自动重启：标记 Failed + 更新可用性。
    async fn give_up(&self, plugin_id: PluginId, unit: UnitHandle, reason: String) {
        self.total_given_up.fetch_add(1, Ordering::Relaxed);
        let _ = self.scheduler.set_state(unit, UnitState::Failed);
        self.registry
            .note_availability(&plugin_id, Some(UnavailableReason::Failed));
        let mut entries = self.entries.write().expect("entries rwlock poisoned");
        if let Some(e) = entries.get_mut(&plugin_id) {
            e.failed = true;
            e.restarting = false;
            e.last_error = Some(reason.clone());
        }
        eprintln!("[restart] {plugin_id}: 标记 Failed，不再自动重试：{reason}");
    }

    /// 用户/平台手动恢复：关闭熔断、清 Failed，然后立即重启。
    pub async fn manual_restart(self: &Arc<Self>, plugin_id: &PluginId) -> Result<(), ToolError> {
        // 需要一个"事件"来驱动重建。用单元当前快照伪造一个 reason=InitFailed 的事件。
        let unit = {
            let entries = self.entries.read().expect("entries rwlock poisoned");
            match entries.get(plugin_id) {
                Some(e) => e.unit.clone(),
                None => {
                    return Err(ToolError::new(
                        ToolErrorCode::ProcessFailed,
                        plugin_id.as_str(),
                        "该插件不受重启管理器管理（未安装或为可信插件）",
                    ))
                }
            }
        };
        // 清掉 failed/stopped + 重置熔断，然后复用 handle_crash_inner。
        {
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            if let Some(e) = entries.get_mut(plugin_id) {
                e.failed = false;
                e.stopped = false;
                e.attempts = 0;
                e.restarting = false;
            }
            self.breaker
                .lock()
                .expect("breaker mutex poisoned")
                .reset(plugin_id);
        }
        self.registry
            .note_availability(plugin_id, Some(UnavailableReason::Restarting));

        let event = CrashEvent {
            unit: unit.clone(),
            state: UnitState::Restarting,
            reason: crate::scheduler::CrashReason::InitFailed {
                detail: "manual restart".to_owned(),
            },
            uptime_ms: 0,
        };
        let mgr = Arc::clone(self);
        mgr.handle_crash_inner(event, true).await;
        // 检查结果。
        let entries = self.entries.read().expect("entries rwlock poisoned");
        match entries.get(plugin_id) {
            Some(e) if e.failed => Err(ToolError::process_failed(
                plugin_id.as_str(),
                "",
                e.last_error.as_deref().unwrap_or("重启失败"),
            )),
            Some(_) => Ok(()),
            None => Err(ToolError::new(
                ToolErrorCode::ProcessFailed,
                plugin_id.as_str(),
                "插件未被管理",
            )),
        }
    }

    /// 停止管理某个插件（卸载时调用）。
    pub async fn forget_plugin(&self, plugin_id: &PluginId) {
        let (sub, unit, stopped_already) = {
            let mut entries = self.entries.write().expect("entries rwlock poisoned");
            match entries.get_mut(plugin_id) {
                Some(e) => {
                    e.stopped = true;
                    (e.crash_sub.take(), Some(e.unit.clone()), false)
                }
                None => (None, None, true),
            }
        };
        if let Some(sub) = sub {
            self.scheduler.unsubscribe_crash(sub);
        }
        if let Some(unit) = unit {
            let _ = self.scheduler.set_state(unit, UnitState::Stopped);
        }
        if !stopped_already {
            self.registry
                .note_availability(plugin_id, Some(UnavailableReason::Stopped));
        }
    }

    /// 观测全部受管插件的状态。
    pub fn states(&self) -> Vec<PluginRestartState> {
        let entries = self.entries.read().expect("entries rwlock poisoned");
        let breaker = self.breaker.lock().expect("breaker mutex poisoned");
        let now = Instant::now();
        entries
            .values()
            .map(|e| PluginRestartState {
                plugin_id: e.runtime.plugin_id.0.clone(),
                state: self
                    .scheduler
                    .get_state(e.unit.clone())
                    .unwrap_or(UnitState::Failed),
                attempts: e.attempts,
                restarts_in_window: breaker.count_in_window(&e.runtime.plugin_id, now),
                circuit_open: breaker.is_open(&e.runtime.plugin_id),
                last_error: e.last_error.clone(),
            })
            .collect()
    }

    pub fn total_restarts(&self) -> u64 {
        self.total_restarts.load(Ordering::Relaxed)
    }

    pub fn total_given_up(&self) -> u64 {
        self.total_given_up.load(Ordering::Relaxed)
    }
}

// ----------------------------------------------------------------------------
// RestartHook：把重启管理器接到注册表的 restart_unit
// ----------------------------------------------------------------------------

pub struct ManagerRestartHook(pub Arc<RestartManager>);

impl RestartHook for ManagerRestartHook {
    fn request_manual_restart(
        &self,
        plugin_id: PluginId,
    ) -> RegistryFuture<'_, Result<(), ToolError>> {
        let mgr = Arc::clone(&self.0);
        Box::pin(async move { mgr.manual_restart(&plugin_id).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_sequence_matches_spec() {
        let p = BackoffPolicy::default();
        assert_eq!(p.delay(1), Duration::from_secs(1));
        assert_eq!(p.delay(2), Duration::from_secs(2));
        assert_eq!(p.delay(3), Duration::from_secs(4));
        assert_eq!(p.delay(4), Duration::from_secs(8));
        assert_eq!(p.delay(5), Duration::from_secs(16));
        assert_eq!(p.delay(6), Duration::from_secs(30));
        assert_eq!(p.delay(20), Duration::from_secs(30), "上限必须是 30s");
    }

    #[test]
    fn circuit_opens_at_sixth_restart() {
        let mut b = CircuitBreaker::new(Duration::from_secs(300), 5);
        let pid = PluginId::new("demo");
        let t0 = Instant::now();
        for i in 1..=5 {
            assert!(
                !b.record_restart(&pid, t0 + Duration::from_secs(i)),
                "第 {i} 次不应熔断"
            );
        }
        assert!(
            b.record_restart(&pid, t0 + Duration::from_secs(6)),
            "第 6 次应熔断"
        );
        assert!(b.is_open(&pid));
    }

    #[test]
    fn circuit_window_slides() {
        let mut b = CircuitBreaker::new(Duration::from_secs(300), 5);
        let pid = PluginId::new("demo");
        let t0 = Instant::now();
        for i in 0..5 {
            b.record_restart(&pid, t0 + Duration::from_secs(i));
        }
        assert!(!b.is_open(&pid));
        assert_eq!(b.count_in_window(&pid, t0 + Duration::from_secs(10)), 5);
        // 400 秒后，旧记录全部滑出窗口
        assert_eq!(b.count_in_window(&pid, t0 + Duration::from_secs(400)), 0);
    }

    #[test]
    fn manual_reset_closes_circuit() {
        let mut b = CircuitBreaker::new(Duration::from_secs(300), 5);
        let pid = PluginId::new("demo");
        let t0 = Instant::now();
        for i in 0..6 {
            b.record_restart(&pid, t0 + Duration::from_secs(i));
        }
        assert!(b.is_open(&pid));
        b.reset(&pid);
        assert!(!b.is_open(&pid));
        assert_eq!(b.count_in_window(&pid, t0 + Duration::from_secs(7)), 0);
    }

    #[test]
    fn millisecond_backoff_for_fast_tests() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(1),
            factor: 2,
            max: Duration::from_millis(30),
        };
        assert_eq!(p.delay(1), Duration::from_millis(1));
        assert_eq!(p.delay(5), Duration::from_millis(16));
        assert_eq!(p.delay(9), Duration::from_millis(30));
    }
}
