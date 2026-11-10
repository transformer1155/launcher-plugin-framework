//! 虚拟进程执行单元的实体实现（tokio）。
//!
//! # 超时两级配置如何真正生效
//!
//! - **工具级 `timeout_ms`**：作为 [`Scheduler::submit`] 的参数传入，与 handler 的 future
//!   **race 在调用方 task 上**。因为计时器不是 handler 的一部分，所以 handler 阻塞 poll
//!   时计时器仍能触发。这是区分"可中断 / 不可中断"的前提。
//! - **进程级 `idle_timeout_ms` / `max_lifetime_ms`**：由 [`super::reaper`] 后台巡检。
//!   工具级超时**优先**：巡检器不会去动正在被 submit 计时器 race 的任务，
//!   只是把单元置为 `TimedOut` 并回收空闲单元。
//!
//! # 可中断性判定：`in_poll` 探针
//!
//! `abort()` 只能在"任务不被任何 worker 线程持有"或"下一次 poll 边界"生效。因此：
//!
//! | handler 此刻在做什么 | `in_poll` | abort 能否生效 |
//! |---|---|---|
//! | 已返回 Pending，等待唤醒（如 `sleep`） | false | **立刻生效** |
//! | 已完成 | false | 无需生效 |
//! | 正卡在单次 poll 内的同步循环 | true | **无法生效**（线程被占住） |
//! | 正在正常的一次 poll 内（微秒级） | true | 一到边界就生效 |
//!
//! 所以判定规则是"**`in_poll` 连续为 true 超过 `uninterruptible_grace_ms`**" →
//! 判定不可中断。这条规则同时满足两个容易搞错的场景：
//!
//! - handler `await tokio::time::sleep(10s)`、超时 1s：`in_poll == false`，判定可中断 ✅
//!   （如果用"心跳计数是否前进"来判定，这里会**误判**为不可中断，因为 parked 的
//!   future 在 10 秒内根本不会被 poll）
//! - handler 里 `std::thread::sleep(1s)`：`in_poll == true` 持续 1s，判定不可中断 ✅

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::catch_unwind::{CatchUnwind, PanicPayload};
use crate::error::{LoadError, SchedulerError, ToolError};
use crate::types::{CancelOutcome, PluginId, TaskId, TaskOutcome, ToolResult, UnitHandle};

use super::state::{is_transition_allowed, CrashReason, IllegalTransition, UnitState};
use super::{CrashCallback, CrashEvent, SubscriptionId, Task, UnitConfig, UnitSnapshot};

// ----------------------------------------------------------------------------
// 探针：这是"可中断 / 不可中断"判定的全部依据
// ----------------------------------------------------------------------------

/// 记录 handler future 当前是否正被 poll，以及这一次 poll 已经持续了多久。
pub(crate) struct PollProbe {
    in_poll: AtomicBool,
    /// 进入 poll 时的 elapsed ms（相对 unit 起点）。
    entered_at_ms: AtomicU64,
    polls_completed: AtomicU64,
    origin: Instant,
}

impl PollProbe {
    fn new(origin: Instant) -> Arc<Self> {
        Arc::new(Self {
            in_poll: AtomicBool::new(false),
            entered_at_ms: AtomicU64::new(0),
            polls_completed: AtomicU64::new(0),
            origin,
        })
    }

    fn elapsed_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn enter(&self) {
        self.entered_at_ms
            .store(self.elapsed_ms(), Ordering::SeqCst);
        self.in_poll.store(true, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.in_poll.store(false, Ordering::SeqCst);
        self.polls_completed.fetch_add(1, Ordering::SeqCst);
    }

    fn is_in_poll(&self) -> bool {
        self.in_poll.load(Ordering::SeqCst)
    }

    /// 若正在 poll，返回这次 poll 已持续的毫秒数。
    fn in_poll_for_ms(&self) -> Option<u64> {
        if !self.is_in_poll() {
            return None;
        }
        let entered = self.entered_at_ms.load(Ordering::SeqCst);
        Some(self.elapsed_ms().saturating_sub(entered))
    }

    fn polls_completed(&self) -> u64 {
        self.polls_completed.load(Ordering::SeqCst)
    }
}

/// poll 退出时复位标志。用 `Drop` 而不是手写 `store`，是为了**unwind 时也能复位**。
struct PollGuard(Arc<PollProbe>);

impl Drop for PollGuard {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// 把探针贴到 handler future 上。
struct ProbedFuture<F> {
    inner: Pin<Box<F>>,
    probe: Arc<PollProbe>,
}

impl<F: Future> Future for ProbedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `ProbedFuture` 只含 `Pin<Box<..>>` 与 `Arc`，本身 Unpin，可以安全 get_mut。
        let this = self.get_mut();
        this.probe.enter();
        let _guard = PollGuard(Arc::clone(&this.probe));
        this.inner.as_mut().poll(cx)
        // _guard 在这里（正常返回或 unwind 时）drop → in_poll = false
    }
}

/// 在飞并发计数。**归 spawned task 所有**：任务真正结束（完成/panic/被 abort drop）时才减。
///
/// 这一点很关键：不可中断的任务在被判定为崩溃后仍在跑，此时 `in_flight` 必须继续
/// 计入它 —— 否则配额会把"其实还占着资源"的调用算成空闲。
struct InFlightGuard(Arc<AtomicU32>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// ----------------------------------------------------------------------------
// 速率窗口
// ----------------------------------------------------------------------------

struct RateWindow {
    recent: VecDeque<Instant>,
    limit: Option<u32>,
}

impl RateWindow {
    fn new(limit: Option<u32>) -> Self {
        Self {
            recent: VecDeque::new(),
            limit,
        }
    }

    fn check_and_record(&mut self, now: Instant) -> Result<(), (u32, u32)> {
        let Some(limit) = self.limit else {
            return Ok(());
        };
        let cutoff = now - Duration::from_secs(1);
        while let Some(front) = self.recent.front() {
            if *front < cutoff {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        let in_window = self.recent.len() as u32;
        if in_window >= limit {
            return Err((limit, in_window));
        }
        self.recent.push_back(now);
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// 单元
// ----------------------------------------------------------------------------

/// 在飞任务登记项。仅供 `cancel` 与崩溃收敛使用。
struct Slot {
    #[allow(dead_code)] // 保留完整任务标识，便于诊断打印与未来扩展
    id: TaskId,
    abort: AbortHandle,
    probe: Arc<PollProbe>,
    #[allow(dead_code)] // 任务开始时刻，用于未来的耗时统计
    started: Instant,
}

pub(crate) struct Unit {
    pub(crate) handle: UnitHandle,
    pub(crate) config: UnitConfig,
    state: AtomicU8,
    in_flight: Arc<AtomicU32>,
    total_tasks: AtomicU64,
    crashes: AtomicU64,
    created_at: Instant,
    last_activity_ms: AtomicU64,
    slots: Mutex<HashMap<u64, Slot>>,
    next_seq: AtomicU64,
    rate: Mutex<RateWindow>,
    crash_subs: Mutex<HashMap<SubscriptionId, CrashCallback>>,
}

impl Unit {
    pub(crate) fn new(handle: UnitHandle, config: UnitConfig) -> Arc<Self> {
        let now = Instant::now();
        let rate_limit = config.max_calls_per_sec;
        Arc::new(Self {
            handle,
            state: AtomicU8::new(UnitState::Running.as_u8()),
            in_flight: Arc::new(AtomicU32::new(0)),
            total_tasks: AtomicU64::new(0),
            crashes: AtomicU64::new(0),
            created_at: now,
            last_activity_ms: AtomicU64::new(0),
            slots: Mutex::new(HashMap::new()),
            next_seq: AtomicU64::new(1),
            config,
            rate: Mutex::new(RateWindow::new(rate_limit)),
            crash_subs: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn state(&self) -> UnitState {
        UnitState::from_u8(self.state.load(Ordering::SeqCst)).unwrap_or(UnitState::Failed)
    }

    /// 写状态，走合法性检查。返回 `false` 表示迁移非法（框架 bug），状态未改变。
    pub(crate) fn try_set_state(&self, to: UnitState) -> bool {
        let from = self.state();
        if !is_transition_allowed(from, to) {
            return false;
        }
        self.state.store(to.as_u8(), Ordering::SeqCst);
        true
    }

    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.created_at.elapsed().as_millis() as u64
    }

    pub(crate) fn idle_ms(&self) -> u64 {
        self.elapsed_ms()
            .saturating_sub(self.last_activity_ms.load(Ordering::SeqCst))
    }

    fn touch_activity(&self) {
        self.last_activity_ms
            .store(self.elapsed_ms(), Ordering::SeqCst);
    }

    /// 配额 + 状态双重准入。**不排队**：任何一项不满足立即返回。
    fn try_admit(&self) -> Result<(), TaskOutcome> {
        // 1) 状态
        if !self.state().accepts_tasks() {
            return Err(TaskOutcome::Crashed(self.state_error()));
        }
        // 2) 并发配额
        loop {
            let cur = self.in_flight.load(Ordering::SeqCst);
            if cur >= self.config.max_concurrent_calls {
                return Err(TaskOutcome::QuotaExceeded(ToolError::quota_exceeded(
                    self.handle.plugin_id.as_str(),
                    "",
                    &format!("并发调用数 {cur}/{} 已满", self.config.max_concurrent_calls),
                )));
            }
            if self
                .in_flight
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
        // 3) 速率配额。失败要把刚加的并发计数退回去。
        if let Err((limit, in_window)) = self
            .rate
            .lock()
            .expect("rate mutex poisoned")
            .check_and_record(Instant::now())
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(TaskOutcome::QuotaExceeded(ToolError::quota_exceeded(
                self.handle.plugin_id.as_str(),
                "",
                &format!("1 秒内调用数 {in_window}/{limit} 已达上限"),
            )));
        }
        self.total_tasks.fetch_add(1, Ordering::SeqCst);
        self.touch_activity();
        Ok(())
    }

    fn state_error(&self) -> ToolError {
        let name = "";
        let pid = self.handle.plugin_id.as_str();
        match self.state() {
            UnitState::Restarting => ToolError::process_restarting(pid, name),
            UnitState::Failed => ToolError::process_failed(pid, name, "重启失败后已停止自动重试"),
            UnitState::Stopped => ToolError::process_failed(pid, name, "单元已被停止"),
            _ => ToolError::process_crashed(pid, name),
        }
    }

    fn admit_error_with_tool(outcome: TaskOutcome, tool: &str) -> TaskOutcome {
        // 把配额/崩溃错误里缺失的 tool_name 补上，便于 UI 直接展示。
        // 准入阶段的错误在拿到 handler 之前产生，tool_name 是空串占位，
        // 这里统一填成真实工具名（空串和 None 都要填）。
        fn patch(mut e: ToolError, tool: &str) -> ToolError {
            if e.tool_name.as_deref().unwrap_or("").is_empty() {
                e.tool_name = Some(tool.to_owned());
            }
            e
        }
        match outcome {
            TaskOutcome::Crashed(e) => TaskOutcome::Crashed(patch(e, tool)),
            TaskOutcome::TimedOut(e) => TaskOutcome::TimedOut(patch(e, tool)),
            TaskOutcome::Uninterruptible(e) => TaskOutcome::Uninterruptible(patch(e, tool)),
            TaskOutcome::QuotaExceeded(e) => TaskOutcome::QuotaExceeded(patch(e, tool)),
            TaskOutcome::Aborted(e) => TaskOutcome::Aborted(patch(e, tool)),
            TaskOutcome::Completed(_) => outcome,
        }
    }

    fn register_slot(&self, seq: u64, abort: AbortHandle, probe: Arc<PollProbe>) {
        let id = TaskId {
            unit: self.handle.clone(),
            seq,
        };
        let mut slots = self.slots.lock().expect("slots mutex poisoned");
        slots.insert(
            seq,
            Slot {
                id,
                abort,
                probe,
                started: Instant::now(),
            },
        );
    }

    fn take_slot(&self, seq: u64) -> Option<Slot> {
        self.slots
            .lock()
            .expect("slots mutex poisoned")
            .remove(&seq)
    }

    /// 取某个在飞任务的探针。
    ///
    /// `cancel` 需要它来判定可中断性；保留为独立方法而不是内联，是为了让
    /// "读取探针"与"移除登记项"这两步在调用点清晰分离（顺序错了会判定失败）。
    #[allow(dead_code)]
    pub(crate) fn find_probe(&self, seq: u64) -> Option<Arc<PollProbe>> {
        self.slots
            .lock()
            .expect("slots mutex poisoned")
            .get(&seq)
            .map(|s| Arc::clone(&s.probe))
    }

    /// 收束所有在飞任务（abort 只是"下次 poll 边界生效"，不保证立刻停）。
    pub(crate) fn abort_all(&self) -> usize {
        let mut slots = self.slots.lock().expect("slots mutex poisoned");
        let n = slots.len();
        for (_, slot) in slots.drain() {
            slot.abort.abort();
        }
        n
    }

    pub(crate) fn snapshot_in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn subscribe(&self, id: SubscriptionId, cb: CrashCallback) {
        self.crash_subs
            .lock()
            .expect("crash_subs mutex poisoned")
            .insert(id, cb);
    }

    fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.crash_subs
            .lock()
            .expect("crash_subs mutex poisoned")
            .remove(&id)
            .is_some()
    }

    fn notify_crash(&self, reason: CrashReason) {
        let event = CrashEvent {
            unit: self.handle.clone(),
            state: self.state(),
            reason,
            uptime_ms: self.elapsed_ms(),
        };
        // 先 clone 回调再释放锁：回调里可能会重新进注册表/调度器，不能在持锁时调用。
        let subs: Vec<CrashCallback> = self
            .crash_subs
            .lock()
            .expect("crash_subs mutex poisoned")
            .values()
            .cloned()
            .collect();
        for cb in subs {
            cb(event.clone());
        }
    }
}

// ----------------------------------------------------------------------------
// SchedulerInner
// ----------------------------------------------------------------------------

pub(crate) struct SchedulerInner {
    units: Mutex<HashMap<PluginId, Arc<Unit>>>,
    next_generation: AtomicU64,
    next_sub_id: AtomicU64,
}

impl SchedulerInner {
    fn unit_for(&self, handle: &UnitHandle) -> Option<Arc<Unit>> {
        let units = self.units.lock().expect("units mutex poisoned");
        let u = units.get(&handle.plugin_id)?;
        if u.handle.generation != handle.generation {
            // 代数过期：这是重启后遗留的旧句柄。必须拒绝，否则会打到新单元上。
            return None;
        }
        Some(Arc::clone(u))
    }

    fn insert_unit(&self, unit: Arc<Unit>) {
        self.units
            .lock()
            .expect("units mutex poisoned")
            .insert(unit.handle.plugin_id.clone(), Arc::clone(&unit));
    }

    fn remove_unit(&self, plugin_id: &PluginId, generation: u64) -> Option<Arc<Unit>> {
        let mut units = self.units.lock().expect("units mutex poisoned");
        let cur = units.get(plugin_id)?;
        if cur.handle.generation != generation {
            return None;
        }
        units.remove(plugin_id)
    }
}

/// mojibake-free 说明：`expect` 在框架内部用于**不可恢复的锁毒化**。
///
/// 锁毒化意味着某个 holder 在持锁期间 panic —— 在沙箱路径上这已被 `catch_unwind`
/// 兜住且不会持锁 panic（我们全程避免 await 期间持锁）。如果真发生了，说明框架
/// 自身有 bug，此时 panic 让宿主重启是比"带病继续跑"更好的选择。
// 可信插件路径的 panic 绝不经过这些锁（它不进调度器）。
impl super::TokioScheduler {
    /// 创建调度器。
    ///
    /// # 运行时要求
    ///
    /// 必须在 **`multi_thread` 且 `worker_threads >= 2`** 的 tokio 运行时内使用。
    /// 单 worker 时，一个不可中断的 handler 会占死唯一的 worker，超时计时器与
    /// 状态收敛都停摆 —— 此时"逻辑隔离"名存实亡。调试构建下给出警告。
    pub fn new() -> Self {
        #[cfg(debug_assertions)]
        {
            match tokio::runtime::Handle::try_current() {
                Ok(_) => { /* 无法可靠读取 worker 数，靠文档约束 */ }
                Err(_) => eprintln!(
                    "[scheduler] 警告：TokioScheduler 在非 tokio 上下文创建，\
                     submit 时会 panic。请先构造 multi_thread 运行时。"
                ),
            }
        }
        Self {
            inner: Arc::new(SchedulerInner {
                units: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(1),
                next_sub_id: AtomicU64::new(1),
            }),
        }
    }

    fn next_generation(&self) -> u64 {
        self.inner.next_generation.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for super::TokioScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Scheduler for super::TokioScheduler {
    fn create_unit(
        &self,
        config: UnitConfig,
    ) -> super::BoxFuture<'_, Result<UnitHandle, LoadError>> {
        let inner = Arc::clone(&self.inner);
        let generation = self.next_generation();
        Box::pin(async move {
            // 复用已有活单元：同一 plugin_id 不允许两个单元，否则"当前单元是哪个"无解。
            {
                let units = inner.units.lock().expect("units mutex poisoned");
                if let Some(cur) = units.get(&config.plugin_id) {
                    if !cur.state().is_terminal() {
                        // 幂等：如果已经在跑，直接返回现有句柄而不是报错，
                        // 让加载器可以安全重入。
                        return Ok(cur.handle.clone());
                    }
                }
            }
            let handle = UnitHandle::new(config.plugin_id.clone(), generation);
            let unit = Unit::new(handle.clone(), config);
            inner.insert_unit(Arc::clone(&unit));
            // 进程级回收巡检。
            super::reaper::spawn_lifecycle_watch(unit);
            Ok(handle)
        })
    }

    fn submit(
        &self,
        unit: UnitHandle,
        task: Task,
        timeout_ms: u64,
    ) -> super::BoxFuture<'_, TaskOutcome> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let Some(u) = inner.unit_for(&unit) else {
                // 代数过期 / 已销毁：不排队、不重建，直接告知。必 resolve。
                return TaskOutcome::Crashed(ToolError::process_crashed(
                    unit.plugin_id.as_str(),
                    &task.handler.tool_name,
                ));
            };

            if let Err(outcome) = u.try_admit() {
                return Unit::admit_error_with_tool(outcome, &task.handler.tool_name);
            }

            let seq = u.next_seq.fetch_add(1, Ordering::SeqCst);
            let probe = PollProbe::new(u.created_at);

            // 构造 handler future。构造本身也可能 panic —— 由 CatchUnwind 覆盖。
            let fut = task.handler.invoke(task.args);
            let guarded = CatchUnwind::new(ProbedFuture {
                inner: Box::pin(fut),
                probe: Arc::clone(&probe),
            });

            let (tx, rx) = oneshot::channel::<Result<ToolResult, PanicPayload>>();
            let inflight_owner = InFlightGuard(Arc::clone(&u.in_flight));
            let u_for_task = Arc::clone(&u);

            let join = tokio::spawn(async move {
                let _inflight = inflight_owner;
                let r = guarded.await;
                let _ = tx.send(r);
                // 任务正常结束：自己清理登记项（若 cancel/timeout 已清理则 remove 是 no-op）。
                let _ = u_for_task;
            });
            let abort = join.abort_handle();
            u.register_slot(seq, abort.clone(), Arc::clone(&probe));

            let timeout = Duration::from_millis(timeout_ms);

            // ---------------- 核心 race：handler vs 工具级计时器 ----------------
            let outcome: Result<Result<ToolResult, PanicPayload>, ()> = tokio::select! {
                r = rx => Ok(r.unwrap_or_else(|_| {
                    // 发送端被 drop 但任务没结束 —— 只可能发生在 abort 路径。
                    Err(PanicPayload { text: "<task aborted>".to_owned() })
                })),
                _ = tokio::time::sleep(timeout) => Err(()),
            };

            match outcome {
                Ok(Ok(result)) => {
                    // 并发计数由 task 内部的 InFlightGuard 负责递减，这里不动。
                    u.take_slot(seq);
                    if result.approximate_size_bytes() > u.config.max_output_bytes {
                        return TaskOutcome::QuotaExceeded(ToolError::quota_exceeded(
                            u.handle.plugin_id.as_str(),
                            &task.handler.tool_name,
                            &format!(
                                "返回值 {} 字节超过上限 {}",
                                result.approximate_size_bytes(),
                                u.config.max_output_bytes
                            ),
                        ));
                    }
                    TaskOutcome::Completed(result)
                }
                Ok(Err(payload)) => {
                    // 沙箱 handler panic：宿主存活，单元崩溃。
                    u.take_slot(seq);
                    u.crashes.fetch_add(1, Ordering::SeqCst);
                    let e = ToolError::new(
                        crate::error::ToolErrorCode::ProcessCrashed,
                        u.handle.plugin_id.as_str(),
                        format!("沙箱 handler panic：{}", payload.text),
                    )
                    .with_tool(&task.handler.tool_name);
                    u.try_set_state(UnitState::Crashed);
                    u.abort_all();
                    u.notify_crash(CrashReason::HandlerPanic {
                        payload: payload.text.clone(),
                    });
                    TaskOutcome::Crashed(e)
                }
                Err(()) => {
                    // ---------------- 工具级超时 ----------------
                    // 注意顺序：先 abort（对 parked 任务立刻生效），再由 probe 判定。
                    // 先 abort 再判定不会误报：abort 之后 parked 的 task 会在下一次
                    // 调度点被 drop，但 handler 卡在同步代码里时 abort 完全无效，
                    // in_poll 仍为 true。这正是我们想要的判据。
                    abort.abort();
                    let grace = Duration::from_millis(u.config.uninterruptible_grace_ms);
                    let interruptible = wait_interruptible(&probe, grace).await;
                    u.take_slot(seq);

                    if interruptible {
                        TaskOutcome::TimedOut(ToolError::timeout(
                            u.handle.plugin_id.as_str(),
                            &task.handler.tool_name,
                            timeout_ms,
                        ))
                    } else {
                        // 不可中断 → 升级为单元级崩溃。
                        u.crashes.fetch_add(1, Ordering::SeqCst);
                        u.try_set_state(UnitState::Crashed);
                        u.abort_all();
                        u.notify_crash(CrashReason::UninterruptibleTimeout { timeout_ms });
                        TaskOutcome::Uninterruptible(ToolError::uninterruptible(
                            u.handle.plugin_id.as_str(),
                            &task.handler.tool_name,
                            timeout_ms,
                        ))
                    }
                }
            }
        })
    }

    fn cancel(&self, unit: UnitHandle, task_id: TaskId) -> CancelOutcome {
        let Some(u) = self.inner.unit_for(&unit) else {
            return CancelOutcome::NotFound;
        };
        if task_id.unit != unit {
            return CancelOutcome::NotFound;
        }
        let grace = u.config.uninterruptible_grace_ms;

        // 判定必须在**移除登记项之前**做，否则拿不到 probe。
        let verdict = {
            let slots = u.slots.lock().expect("slots mutex poisoned");
            let Some(slot) = slots.get(&task_id.seq) else {
                return CancelOutcome::NotFound;
            };
            // 两条判据：
            // 1) 本就在 poll 外（parked / 已完成）→ 可中断
            // 2) 在 poll 内但已超过宽限期 → 不可中断
            match slot.probe.in_poll_for_ms() {
                None => Some(true),
                Some(ms) => Some(ms < grace),
            }
            .map(|interruptible| (interruptible, slot.abort.clone()))
        };

        match verdict {
            None => CancelOutcome::NotFound,
            Some((false, _abort)) => {
                // 不可中断：不 abort（abort 也无效，徒留假象），如实上报。
                CancelOutcome::Uninterruptible
            }
            Some((true, abort)) => {
                abort.abort();
                u.take_slot(task_id.seq);
                CancelOutcome::Cancelled
            }
        }
    }

    fn destroy(&self, unit: UnitHandle) -> super::BoxFuture<'_, ()> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            if let Some(u) = inner.remove_unit(&unit.plugin_id, unit.generation) {
                u.abort_all();
                // 崩溃订阅者需要知道"这不是崩溃，是停止"，因此不 notify_crash。
                u.state.store(UnitState::Stopped.as_u8(), Ordering::SeqCst);
            }
        })
    }

    fn on_crash(
        &self,
        unit: UnitHandle,
        callback: CrashCallback,
    ) -> Result<SubscriptionId, SchedulerError> {
        let u = self
            .inner
            .unit_for(&unit)
            .ok_or(SchedulerError::UnitNotFound)?;
        let id = SubscriptionId(self.inner.next_sub_id.fetch_add(1, Ordering::SeqCst));
        u.subscribe(id, callback);
        Ok(id)
    }

    fn unsubscribe_crash(&self, id: SubscriptionId) -> bool {
        let units = self.inner.units.lock().expect("units mutex poisoned");
        let mut any = false;
        for u in units.values() {
            if u.unsubscribe(id) {
                any = true;
            }
        }
        any
    }

    fn get_state(&self, unit: UnitHandle) -> Option<UnitState> {
        self.inner.unit_for(&unit).map(|u| u.state())
    }

    fn list_units(&self) -> Vec<UnitSnapshot> {
        let units = self.inner.units.lock().expect("units mutex poisoned");
        units
            .values()
            .map(|u| UnitSnapshot {
                plugin_id: u.handle.plugin_id.0.clone(),
                generation: u.handle.generation,
                state: u.state(),
                in_flight: u.in_flight.load(Ordering::SeqCst),
                max_concurrent: u.config.max_concurrent_calls,
                total_tasks: u.total_tasks.load(Ordering::SeqCst),
                crashes: u.crashes.load(Ordering::SeqCst),
                idle_ms: u.idle_ms(),
                uptime_ms: u.elapsed_ms(),
                memory_limit_bytes: u.config.memory_limit_bytes,
            })
            .collect()
    }

    fn set_state(&self, unit: UnitHandle, to: UnitState) -> Result<(), IllegalTransition> {
        let u = self.inner.unit_for(&unit).ok_or(IllegalTransition {
            from: UnitState::Stopped,
            to,
        })?;
        let from = u.state();
        if !u.try_set_state(to) {
            return Err(IllegalTransition { from, to });
        }
        Ok(())
    }

    fn raise_crash(&self, unit: UnitHandle, reason: CrashReason) -> bool {
        let Some(u) = self.inner.unit_for(&unit) else {
            return false;
        };
        if !u.state().accepts_tasks() {
            // 已被别的路径（超时 / 另一路径的崩溃）先处理了。这是正常竞态。
            return false;
        }
        u.crashes.fetch_add(1, Ordering::SeqCst);
        let ok = u.try_set_state(UnitState::Crashed);
        if !ok {
            return false;
        }
        u.abort_all();
        u.notify_crash(reason);
        true
    }

    fn generation_of(&self, plugin_id: &PluginId) -> Option<u64> {
        let units = self.inner.units.lock().expect("units mutex poisoned");
        units.get(plugin_id).map(|u| u.handle.generation)
    }
}

/// 等待 handler 离开 poll，最多 `grace`。返回 true 表示可中断。
///
/// 用 1ms 粒度的轮询而不是订阅某种事件：handler 的 poll 边界在 Rust 里没有可观测的回调，
/// 探针已经是能做到的最细粒度。`grace` 本身是几百毫秒量级，1ms 轮询开销可忽略。
async fn wait_interruptible(probe: &Arc<PollProbe>, grace: Duration) -> bool {
    // 第一判据：此刻就不在 poll 内 → 立刻可中断（parked 的 future 可被 abort 直接 drop）。
    if !probe.is_in_poll() {
        return true;
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
        if !probe.is_in_poll() {
            return true;
        }
    }
    // 持续在 poll 内超过 grace：卡在同步代码里。
    false
}

/// 供测试/诊断读取探针的一次性快照。
#[allow(dead_code)]
pub(crate) fn probe_snapshot(probe: &PollProbe) -> (bool, u64, u64) {
    (
        probe.is_in_poll(),
        probe.in_poll_for_ms().unwrap_or(0),
        probe.polls_completed(),
    )
}
