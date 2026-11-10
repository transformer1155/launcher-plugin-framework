//! 可信插件路径：HandlerPanic 捕获，且**宿主存活**。
//!
//! 这些测试证明规格的核心承诺："可信插件直接跑在宿主主执行上下文、注册表直接调用
//! handler、无调度器介入、调用边界 catch_unwind 捕获 panic"。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use launcher_plugin_framework::error::ToolErrorCode;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use serde_json::json;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handler_panic_returns_handler_panic_code() {
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "boom", |_| {
            Box::pin(async {
                panic!("trusted handler exploded");
            })
        }))
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "t.boom", json!({})).await;
    assert!(r.is_error);
    assert_eq!(
        code_of(&r).as_deref(),
        Some("HandlerPanic"),
        "可信插件 panic 必须返回 HandlerPanic"
    );

    let structured = r.structured_content.unwrap();
    assert!(
        structured["panicPayload"]
            .as_str()
            .unwrap_or("")
            .contains("exploded"),
        "panic 载荷必须透出，实际 {structured}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_survives_trusted_panic_and_can_keep_calling() {
    // 这是最重要的一条：可信插件 panic 之后，宿主必须继续正常工作。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "boom", |_| {
            Box::pin(async {
                panic!("boom");
            })
        }))
        .unwrap();
    h.registry
        .register_tool(trusted_meta("t", "ok", |_| {
            Box::pin(async { ToolResult::success_text("still alive") })
        }))
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "t.boom", json!({})).await;
    assert_eq!(code_of(&r).as_deref(), Some("HandlerPanic"));

    // 同一个进程继续服务
    let r = h.registry.call_tool(&h.caller, "t.ok", json!({})).await;
    assert!(!r.is_error, "panic 之后宿主应仍能正常调用其他工具");

    // 崩溃的那个工具也还能再调（可信插件不进重启管理器，工具本身不卸载）
    let r = h.registry.call_tool(&h.caller, "t.boom", json!({})).await;
    assert_eq!(code_of(&r).as_deref(), Some("HandlerPanic"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_before_future_is_constructed_is_caught() {
    // 构造 future 阶段就 panic（不是 poll 期间）。只包住 poll 的实现会漏掉这个。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "early", |_args| {
            panic!("panic while building the future");
            #[allow(unreachable_code)]
            Box::pin(async { ToolResult::success_text("never") })
        }))
        .unwrap();

    let r = h.registry.call_tool(&h.caller, "t.early", json!({})).await;
    assert_eq!(code_of(&r).as_deref(), Some("HandlerPanic"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_path_does_not_go_through_scheduler() {
    // 结构性断言：可信工具在注册表里注册后，调度器里**没有**任何单元。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "echo", |_| {
            Box::pin(async { ToolResult::success_text("ok") })
        }))
        .unwrap();

    let units_before = h.scheduler.list_units().len();
    let r = h.registry.call_tool(&h.caller, "t.echo", json!({})).await;
    assert!(!r.is_error);
    let units_after = h.scheduler.list_units().len();
    assert_eq!(
        units_before, units_after,
        "可信插件调用不得创建/触碰任何沙箱单元"
    );
    assert_eq!(units_after, 0, "本测试中不应存在任何单元");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_concurrent_calls_are_not_quota_limited() {
    // 可信路径没有配额：并发 10 个都能跑完（沙箱才有并发上限）。
    let h = Harness::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = hits.clone();
    h.registry
        .register_tool(trusted_meta("t", "work", move |_| {
            hits2.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                ToolResult::success_text("ok")
            })
        }))
        .unwrap();

    let mut joins = Vec::new();
    for _ in 0..10 {
        let reg = h.registry.clone();
        let caller = h.caller.clone();
        joins.push(tokio::spawn(async move {
            reg.call_tool(&caller, "t.work", json!({})).await
        }));
    }
    for j in joins {
        let r = j.await.unwrap();
        assert!(!r.is_error, "可信插件不应有配额限制");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn business_error_and_panic_are_distinguishable() {
    // 协议层同构（都 is_error: true），但错误码不同。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "biz", |_| {
            Box::pin(async { ToolResult::business_error("user input rejected") })
        }))
        .unwrap();
    h.registry
        .register_tool(trusted_meta("t", "panic", |_| {
            Box::pin(async {
                panic!("crash");
            })
        }))
        .unwrap();

    let biz = h.registry.call_tool(&h.caller, "t.biz", json!({})).await;
    let pan = h.registry.call_tool(&h.caller, "t.panic", json!({})).await;

    assert!(biz.is_error && pan.is_error, "协议层必须同构");
    assert_eq!(
        code_of(&biz),
        None,
        "业务失败不应带框架错误码（它是工具语义）"
    );
    assert_eq!(code_of(&pan).as_deref(), Some("HandlerPanic"));
    assert!(
        !ToolErrorCode::HandlerPanic.is_sandbox_only(),
        "HandlerPanic 只属于可信插件"
    );
}
