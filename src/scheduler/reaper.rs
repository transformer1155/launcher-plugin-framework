//! 进程级生命周期回收：空闲回收（`idle_timeout_ms`）与硬上界（`max_lifetime_ms`）。
//!
//! # 与工具级超时的关系（规格：工具级超时优先）
//!
//! 回收器**只处理空闲单元**：`in_flight == 0` 且空闲超过 `idle_timeout_ms`。
//! 有在飞任务的单元不会被巡检器动，因此不会出现"工具级超时还没到，单元先被回收了"
//! 的越界行为。`max_lifetime_ms` 是例外：它是硬上界，到点即回收，在飞任务以
//! `Aborted` 收束（必 resolve，不违反"submit 必 resolve"）。
//!
//! # 为什么用轮询而不是 per-unit timer
//!
//! per-unit timer 需要在 create/destroy/状态迁移时反复重置，任何一个重置点漏掉就会
//! 变成"永不回收"或"提前回收"的 bug。单条巡检器（250ms 粒度）的失效面小得多，
//! 且 idle/lifetime 都是秒级阈值，250ms 的偏差对语义无影响。

use std::sync::Arc;
use std::time::Duration;

use super::state::UnitState;
use super::unit::Unit;

/// 巡检粒度。idle/lifetime 阈值都是秒级，250ms 足够。
const TICK: Duration = Duration::from_millis(250);

/// 为一个单元拉起生命周期巡检。
///
/// 巡检在单元被 `Stopped`/移除后自然退出：每轮先检查状态，终态即 break。
/// 这避免了 destroy 之后仍持有 `Arc<Unit>` 造成的内存泄漏。
pub(crate) fn spawn_lifecycle_watch(unit: Arc<Unit>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;

            let state = unit.state();
            if state.is_terminal() {
                // 已被 destroy：巡检任务自行退出。
                return;
            }

            // 非 Running 状态（Crashed / Restarting / Failed）由重启管理器负责，
            // 回收器不插手，否则会和重启流程抢状态。
            if state != UnitState::Running {
                continue;
            }

            // ---- 硬上界：max_lifetime_ms ----
            if let Some(max) = unit.config.max_lifetime_ms {
                if unit.elapsed_ms() >= max {
                    reap(&unit, UnitState::TimedOut);
                    return;
                }
            }

            // ---- 空闲回收：只在没有任何在飞任务时 ----
            if let Some(idle_limit) = unit.config.idle_timeout_ms {
                let in_flight = unit.snapshot_in_flight();
                if in_flight == 0 && unit.idle_ms() >= idle_limit {
                    reap(&unit, UnitState::TimedOut);
                    return;
                }
            }
        }
    });
}

/// 回收：置状态 + 收束在飞任务。
///
/// 注意这里**不**广播崩溃事件：空闲/到期回收不是"崩溃"，重启管理器不应把它当成
/// 故障来退避计数。下次 `call_tool` 会看到 `TimedOut` 并返回 `ProcessCrashed`，
/// 由平台层（或加载器）按需拉起 —— 这条路径见 README 的"空闲回收后如何恢复"。
fn reap(unit: &Arc<Unit>, to: UnitState) {
    if !unit.try_set_state(to) {
        // 状态已被别的路径改了：不抢。
        return;
    }
    unit.abort_all();
}
