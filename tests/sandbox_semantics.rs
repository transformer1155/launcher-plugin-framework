//! 沙箱核心语义：超时可中断 vs 不可中断、崩溃、取消、配额、代数失效。
//!
//! 这些是规格里最难做对的部分，因此断言尽量精确到错误码与耗时量级。

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::scheduler::UnitState;
use serde_json::json;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interruptible_timeout_returns_timeout_code() {
    // handler await sleep(2s)，工具级超时 150ms → 可中断 → Timeout
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "slow", 150, |_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                ToolResult::success_text("nope")
            })
        }))
        .unwrap();

    let t0 = Instant::now();
    let r = h.registry.call_tool(&h.caller, "s.slow", json!({})).await;
    let elapsed = t0.elapsed();

    assert_eq!(code_of(&r).as_deref(), Some("Timeout"));
    assert!(
        elapsed >= Duration::from_millis(140),
        "不应早于超时时间返回，实际 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "可中断的超时必须及时返回，实际 {elapsed:?}"
    );

    // 单元仍然 Running（可中断超时不是崩溃）
    assert_eq!(
        h.scheduler.get_state(unit.clone()),
        Some(UnitState::Running)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uninterruptible_timeout_upgrades_to_unit_crash() {
    // handler 里 std::thread::sleep(1s)：不产生 await 点，abort 无效 → 不可中断 → 崩溃
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "blocking", 80, |_| {
            Box::pin(async {
                std::thread::sleep(Duration::from_millis(600));
                ToolResult::success_text("finished anyway")
            })
        }))
        .unwrap();

    let r = h
        .registry
        .call_tool(&h.caller, "s.blocking", json!({}))
        .await;
    assert_eq!(
        code_of(&r).as_deref(),
        Some("Uninterruptible"),
        "不可中断的 handler 必须升级为单元级崩溃"
    );

    // 规格：升级为单元级崩溃并触发重启 → 单元离开 Running
    let state = h.scheduler.get_state(unit.clone());
    assert!(
        !matches!(state, Some(UnitState::Running)),
        "不可中断之后单元不应仍是 Running，实际 {state:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sandbox_panic_crashes_unit_but_host_survives() {
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "boom", 5_000, |_| {
            Box::pin(async {
                panic!("sandbox boom");
            })
        }))
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "s.boom", json!({})).await;
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("ProcessCrashed"),
        "沙箱 panic 必须表现为 ProcessCrashed（不是 HandlerPanic）"
    );
    // 宿主活着 —— 能继续读调度器状态就证明进程没死
    assert!(h.scheduler.get_state(unit).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversation_quota_rejects_without_queueing() {
    let h = Harness::new();
    let mut cfg = h.unit_cfg("s");
    cfg.max_concurrent_calls = 1;
    let unit = h.scheduler.create_unit(cfg).await.unwrap();

    let gate = Arc::new(tokio::sync::Notify::new());
    let gate2 = gate.clone();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "hold", 10_000, move |_| {
            let g = gate2.clone();
            Box::pin(async move {
                g.notified().await;
                ToolResult::success_text("released")
            })
        }))
        .unwrap();

    // 第一个调用占住配额
    let reg = h.registry.clone();
    let caller = h.caller.clone();
    let barrier = gate.clone();
    let first = tokio::spawn(async move {
        let r = reg.call_tool(&caller, "s.hold", json!({})).await;
        r
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    // 第二个调用应立即被配额拒绝，而不是排队等待
    let t0 = Instant::now();
    let r = h.registry.call_tool(&h.caller, "s.hold", json!({})).await;
    let elapsed = t0.elapsed();
    assert_eq!(code_of(&r).as_deref(), Some("SandboxQuotaExceeded"));
    assert!(
        elapsed < Duration::from_millis(200),
        "配额拒绝必须立即返回，不能排队，实际 {elapsed:?}"
    );

    barrier.notify_waiters();
    let first_r = first.await.unwrap();
    assert!(!first_r.is_error, "第一个调用应成功");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn output_size_quota_enforced() {
    let h = Harness::new();
    let mut cfg = h.unit_cfg("s");
    cfg.max_output_bytes = 100;
    let unit = h.scheduler.create_unit(cfg).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "big", 5_000, |_| {
            Box::pin(async {
                let payload = "x".repeat(5_000);
                ToolResult::success(serde_json::json!({ "data": payload }))
            })
        }))
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "s.big", json!({})).await;
    assert_eq!(code_of(&r).as_deref(), Some("SandboxQuotaExceeded"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_reports_cancelled_for_parked_task() {
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();

    // 直接通过调度器提交，以便拿到 TaskId 做 cancel。
    let handler = FunctionRef::new("s", "s.wait", |_| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            ToolResult::success_text("done")
        })
    });
    let task = Task::new(handler, json!({}));

    let sched = h.scheduler.clone();
    let unit2 = unit.clone();
    let fut = tokio::spawn(async move { sched.submit(unit2, task, 10_000).await });
    tokio::time::sleep(Duration::from_millis(80)).await;

    // 找到一个在飞任务并 cancel。seq 单调递增，第一个任务就是 1。
    let tid = launcher_plugin_framework::types::TaskId {
        unit: unit.clone(),
        seq: 1,
    };
    let outcome = h.scheduler.cancel(unit.clone(), tid);
    assert_eq!(
        outcome,
        CancelOutcome::Cancelled,
        "parked（await sleep）的任务应可取消，实际 {outcome}"
    );

    let outcome = fut.await.unwrap();
    assert!(!outcome.is_completed(), "被取消的任务不应完成");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_reports_uninterruptible_for_blocking_task() {
    let h = Harness::new();
    let mut cfg = h.unit_cfg("s");
    cfg.uninterruptible_grace_ms = 30;
    let unit = h.scheduler.create_unit(cfg).await.unwrap();

    let hacked = Arc::new(AtomicBool::new(false));
    let hacked2 = hacked.clone();
    let handler = FunctionRef::new("s", "s.block", move |_| {
        hacked2.store(true, Ordering::SeqCst);
        Box::pin(async {
            std::thread::sleep(Duration::from_millis(500));
            ToolResult::success_text("done")
        })
    });
    let task = Task::new(handler, json!({}));
    let sched = h.scheduler.clone();
    let unit2 = unit.clone();
    let join = tokio::spawn(async move { sched.submit(unit2, task, 10_000).await });

    // 等 handler 真的进入同步阻塞
    for _ in 0..200 {
        if hacked.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(120)).await;

    let tid = launcher_plugin_framework::types::TaskId {
        unit: unit.clone(),
        seq: 1,
    };
    let outcome = h.scheduler.cancel(unit.clone(), tid);
    assert_eq!(
        outcome,
        CancelOutcome::Uninterruptible,
        "卡在同步代码的任务必须报告 Uninterruptible，实际 {outcome}"
    );
    // 协议视图必须是 Uninterruptible
    assert_eq!(outcome.as_protocol_str(), "Uninterruptible");

    let _ = join.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_unit_handle_is_rejected() {
    // 代数失效：destroy 之后旧句柄的调用必须失败，且必须 resolve（不能挂住）。
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "work", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();

    h.scheduler.destroy(unit.clone()).await;
    assert_eq!(h.scheduler.get_state(unit.clone()), None);

    let r = h.registry.call_tool(&h.caller, "s.work", json!({})).await;
    assert!(r.is_error, "销毁后的调用必须失败");
    assert_eq!(code_of(&r).as_deref(), Some("ProcessCrashed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_reaper_eventually_stops_unit() {
    let h = Harness::new();
    let mut cfg = h.unit_cfg("s");
    cfg.idle_timeout_ms = Some(300); // 300ms 空闲即回收
    let unit = h.scheduler.create_unit(cfg).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "noop", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("ok") })
        }))
        .unwrap();

    assert_eq!(
        h.scheduler.get_state(unit.clone()),
        Some(UnitState::Running)
    );
    let r = h.registry.call_tool(&h.caller, "s.noop", json!({})).await;
    assert!(!r.is_error);

    // 等待巡检器跑几轮
    tokio::time::sleep(Duration::from_millis(900)).await;
    let state = h.scheduler.get_state(unit.clone());
    assert!(
        !matches!(state, Some(UnitState::Running)),
        "空闲超时后单元不应仍 Running，实际 {state:?}"
    );

    // 之后的调用必须立即失败（不排队）
    let r = h.registry.call_tool(&h.caller, "s.noop", json!({})).await;
    assert!(r.is_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submit_always_resolves_even_when_unit_crashes_midflight() {
    // 规格：submit 必 resolve（完成/超时/崩溃）。这里制造"另一个任务把单元弄崩"的场景。
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "slow", 10_000, |_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                ToolResult::success_text("slow")
            })
        }))
        .unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "boom", 10_000, |_| {
            Box::pin(async {
                panic!("boom to kill unit");
            })
        }))
        .unwrap();

    let reg = h.registry.clone();
    let caller = h.caller.clone();
    let slow = tokio::spawn(async move { reg.call_tool(&caller, "s.slow", json!({})).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 这个调用会让单元崩溃
    let r = h.registry.call_tool(&h.caller, "s.boom", json!({})).await;
    assert!(r.is_error);

    // 在飞的那个慢任务必须 resolve（不能永远挂着）
    let r = tokio::time::timeout(Duration::from_secs(6), slow)
        .await
        .expect("在飞任务必须 resolve，不能永远挂起")
        .unwrap();
    assert!(r.is_error, "单元崩溃后在飞任务应报错而不是成功");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_rate_quota_enforced() {
    let h = Harness::new();
    let mut cfg = h.unit_cfg("s");
    cfg.max_calls_per_sec = Some(2);
    cfg.max_concurrent_calls = 10;
    let unit = h.scheduler.create_unit(cfg).await.unwrap();
    let counter = Arc::new(AtomicU32::new(0));
    let c2 = counter.clone();
    h.registry
        .register_tool(
            sandboxed_meta("s", &unit, "tick", 5_000, move |_| {
                c2.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { ToolResult::success_text("ok") })
            })
            .clone(),
        )
        .unwrap();

    let mut codes = Vec::new();
    for _ in 0..5 {
        let r = h.registry.call_tool(&h.caller, "s.tick", json!({})).await;
        codes.push(code_of(&r));
    }
    assert!(
        codes
            .iter()
            .any(|c| c.as_deref() == Some("SandboxQuotaExceeded")),
        "1 秒内第 3 次之后应被速率配额拒绝，实际 {codes:?}"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2, "超额的调用不应真正执行");
}
