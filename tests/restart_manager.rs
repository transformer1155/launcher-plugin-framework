//! 重启管理器：退避序列、熔断、重启期间的状态、重启失败 → Failed。
//!
//! 用毫秒级退避 + `instant_sleep` 让这些测试在 1 秒内跑完，同时保持**逻辑**与
//! 生产一致（真实的 1/2/4/8/30 秒序列已在 restart.rs 的单元测试里断言）。

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::scheduler::UnitState;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

use common::*;

fn dummy_package(plugin_id: &str) -> PluginPackage {
    let raw = format!(r#"{{"id":"{plugin_id}","name":"Test","version":"0.1.0"}}"#).into_bytes();
    let manifest = serde_json::from_slice(&raw).unwrap();
    PluginPackage {
        plugin_id: PluginId::new(plugin_id),
        manifest_raw: raw,
        manifest,
        files: Vec::new(),
        signature: None,
    }
}

/// 装配一个受重启管理器管理的沙箱插件（含一个会 panic 的工具）。
async fn setup_managed(h: &Harness, plugin_id: &str) -> UnitHandle {
    let pid = PluginId::new(plugin_id);
    let unit = h
        .scheduler
        .create_unit(h.unit_cfg(plugin_id))
        .await
        .unwrap();
    h.registry
        .register_tool(sandboxed_meta(plugin_id, &unit, "boom", 5_000, |_| {
            Box::pin(async {
                panic!("kill the unit");
            })
        }))
        .unwrap();
    h.registry
        .register_tool(sandboxed_meta(plugin_id, &unit, "ok", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("ok") })
        }))
        .unwrap();

    let build: Arc<dyn Fn(UnitHandle) -> Vec<ToolMeta> + Send + Sync> = {
        let pid2 = pid.clone();
        Arc::new(move |u: UnitHandle| {
            vec![
                sandboxed_meta(&pid2.0, &u, "boom", 5_000, |_| {
                    Box::pin(async {
                        panic!("kill the unit");
                    })
                }),
                sandboxed_meta(&pid2.0, &u, "ok", 5_000, |_| {
                    Box::pin(async { ToolResult::success_text("ok") })
                }),
            ]
        })
    };

    let runtime = PluginRuntime {
        plugin_id: pid.clone(),
        package: dummy_package(plugin_id),
        loaded_digest: dummy_package(plugin_id).digest(),
        unit_config: h.unit_cfg(plugin_id),
        build_tools: build,
    };
    h.restarts.watch_plugin(runtime, unit.clone()).await;
    unit
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_triggers_restart_and_tool_becomes_available_again() {
    let h = Harness::new();
    let unit = setup_managed(&h, "p").await;

    // 崩溃
    let r = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
    assert!(r.is_error);

    // 立刻查：应当"重启中"，且 list 仍返回该工具但 available=false
    let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
    let boom = list.iter().find(|l| l.name == "p.boom").unwrap();
    assert!(
        !boom.available,
        "崩溃后应标记不可用（无论是 crashed 还是 restarting）"
    );
    assert_eq!(
        boom.unavailable_reason,
        Some(UnavailableReason::Restarting),
        "规格要求重启期间 reason 为 restarting"
    );

    // 重启期间 call 立即返回 ProcessRestarting，不排队
    let t0 = std::time::Instant::now();
    let r = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
    assert_eq!(code_of(&r).as_deref(), Some("ProcessRestarting"));
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "重启期间调用必须立即返回，不能排队"
    );

    // 等重启完成（毫秒级退避）
    let mut recovered = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
        if let Some(t) = list.iter().find(|l| l.name == "p.ok") {
            if t.available {
                recovered = true;
                break;
            }
        }
    }
    assert!(recovered, "重启后工具应恢复可用");

    // 恢复后能正常调用（新单元）
    let r = h.registry.call_tool(&h.caller, "p.ok", json!({})).await;
    assert!(!r.is_error, "重启后应能正常调用");

    // 代数变了
    let new_unit = h
        .scheduler
        .list_units()
        .into_iter()
        .find(|u| u.plugin_id == "p")
        .unwrap();
    assert_ne!(new_unit.generation, unit.generation, "重启必须换新代数");
    assert!(h.restarts.total_restarts() >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_crashes_open_circuit_breaker() {
    let h = Harness::new();
    setup_managed(&h, "p").await;

    // 反复让它崩溃。每次崩溃 → 重启 → 再崩溃。
    // 窗口内第 6 次重启应当打开熔断。
    for _ in 0..12 {
        let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
        let ok_tool = list.iter().find(|l| l.name == "p.ok");
        if let Some(t) = ok_tool {
            if t.available {
                let _ = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let states = h.restarts.states();
    let st = states.iter().find(|s| s.plugin_id == "p").unwrap();
    assert!(
        st.circuit_open,
        "窗口内重启超过 5 次应打开熔断，实际状态 {st:?}"
    );
    assert_eq!(st.state, UnitState::Failed);

    // 熔断后 list 显示 circuit_open 原因
    let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
    let t = list.iter().find(|l| l.name == "p.ok").unwrap();
    assert!(!t.available);
    assert_eq!(
        t.unavailable_reason,
        Some(UnavailableReason::CircuitOpen),
        "熔断后 reason 应为 circuit_open，实际 {:?}",
        t.unavailable_reason
    );

    // call 必须失败且不自动重启
    let r = h.registry.call_tool(&h.caller, "p.ok", json!({})).await;
    assert!(r.is_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_restart_recovers_from_open_circuit() {
    let h = Harness::new();
    setup_managed(&h, "p").await;

    for _ in 0..14 {
        let _ = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let st = h
        .restarts
        .states()
        .into_iter()
        .find(|s| s.plugin_id == "p")
        .unwrap();
    assert!(st.circuit_open, "前置条件：熔断应已打开");

    // 规格：熔断后"等用户手动恢复"。restart_unit 是唯一入口。
    let res = h
        .registry
        .restart_unit(&h.caller, &PluginId::new("p"))
        .await;
    assert!(res.is_ok(), "平台层手动重启应成功，实际 {res:?}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let st = h
        .restarts
        .states()
        .into_iter()
        .find(|s| s.plugin_id == "p")
        .unwrap();
    assert!(!st.circuit_open, "手动恢复后熔断应关闭");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_failure_marks_failed_and_stops_retrying() {
    // 构造一个"重启必然失败"的场景：build_tools 返回空 → give_up → Failed
    let h = Harness::new();
    let pid = PluginId::new("p");
    let unit = h.scheduler.create_unit(h.unit_cfg("p")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("p", &unit, "boom", 5_000, |_| {
            Box::pin(async {
                panic!("kill");
            })
        }))
        .unwrap();

    let runtime = PluginRuntime {
        plugin_id: pid.clone(),
        package: dummy_package("p"),
        loaded_digest: dummy_package("p").digest(),
        unit_config: h.unit_cfg("p"),
        build_tools: Arc::new(|_u| Vec::new()), // 空 → 必然失败
    };
    h.restarts.watch_plugin(runtime, unit.clone()).await;

    let r = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
    assert!(r.is_error);

    // 等重启流程走完
    let mut failed = false;
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let st = h.restarts.states();
        if let Some(s) = st.iter().find(|s| s.plugin_id == "p") {
            if s.state == UnitState::Failed && !s.circuit_open {
                failed = true;
                break;
            }
        }
    }
    assert!(
        failed,
        "重启失败应标记 Failed 且不熔断，实际 {:?}",
        h.restarts.states()
    );

    let st = h
        .restarts
        .states()
        .into_iter()
        .find(|s| s.plugin_id == "p")
        .unwrap();
    assert!(
        st.last_error
            .as_deref()
            .unwrap_or("")
            .contains("未声明任何工具"),
        "应记录失败原因，实际 {:?}",
        st.last_error
    );

    // Failed 之后**不再自动重试**：计数不再增长
    let before = h.restarts.total_given_up();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.restarts.total_given_up(),
        before,
        "Failed 状态不应继续触发重启尝试"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_manager_only_manages_sandboxed_plugins() {
    // 可信插件的工具没有单元 → 不在重启管理器的 entries 里。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "ok", |_| {
            Box::pin(async { ToolResult::success_text("ok") })
        }))
        .unwrap();
    assert!(
        h.restarts.states().is_empty(),
        "可信插件不应出现在重启管理器里"
    );

    // 手动重启一个可信插件 → 明确报错，而不是悄悄做点什么
    let err = h
        .registry
        .restart_unit(&h.caller, &PluginId::new("t"))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("不受重启管理器管理"),
        "实际错误：{}",
        err.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_event_subscription_fires_once_per_crash() {
    let h = Harness::new();
    let unit = setup_managed(&h, "p").await;
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    let _sub = h
        .scheduler
        .on_crash(
            unit.clone(),
            Arc::new(move |_e| {
                c2.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "p.boom", json!({})).await;
    assert!(r.is_error);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1, "一次崩溃应只广播一次");
}
