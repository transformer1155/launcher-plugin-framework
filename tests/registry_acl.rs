//! 注册表：错误码、ACL、检查顺序、list/call 一致性。
//!
//! 这些测试断言的是**规格里的检查顺序**，不是实现细节。每个测试对应规格中的一条要求。

mod common;

use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use serde_json::json;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_not_found_when_missing() {
    let h = Harness::new();
    let r = h
        .registry
        .call_tool(&h.caller, "nobody.nothing", json!({}))
        .await;
    assert!(r.is_error);
    assert_eq!(code_of(&r).as_deref(), Some("ToolNotFound"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permission_denied_before_execution() {
    let h = Harness::new();
    let mut meta = trusted_meta("a", "secret", |_| {
        Box::pin(async { ToolResult::success_text("should not run") })
    });
    meta.allowed_callers = vec![PluginId::new("friend")];
    h.registry.register_tool(meta).unwrap();

    let r = h
        .registry
        .call_tool(&PluginId::new("stranger"), "a.secret", json!({}))
        .await;
    assert_eq!(code_of(&r).as_deref(), Some("PermissionDenied"));

    // 被允许的调用方能拿到结果（证明上面是 ACL 而不是别的失败）
    let r = h
        .registry
        .call_tool(&PluginId::new("friend"), "a.secret", json!({}))
        .await;
    assert!(!r.is_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acl_filtered_at_list_stage_too() {
    // 规格：ACL 在 list 阶段过滤。可见性与可调用性必须一致。
    let h = Harness::new();
    let mut meta = trusted_meta("a", "secret", |_| {
        Box::pin(async { ToolResult::success_text("ok") })
    });
    meta.allowed_callers = vec![PluginId::new("friend")];
    h.registry.register_tool(meta).unwrap();

    let stranger_view = h
        .registry
        .list_tools(&PluginId::new("stranger"), &ToolFilter::all());
    assert!(stranger_view.is_empty(), "无权调用者不应在列表中看到该工具");

    let friend_view = h
        .registry
        .list_tools(&PluginId::new("friend"), &ToolFilter::all());
    assert_eq!(friend_view.len(), 1);
    assert!(friend_view[0].available);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_arguments_when_schema_fails() {
    let h = Harness::new();
    let mut meta = trusted_meta("a", "need_path", |_| {
        Box::pin(async { ToolResult::success_text("ran") })
    });
    meta.input_schema = json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
        "additionalProperties": false
    });
    h.registry.register_tool(meta).unwrap();

    let r = h
        .registry
        .call_tool(&h.caller, "a.need_path", json!({}))
        .await;
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "缺必填应被拒"
    );

    let r = h
        .registry
        .call_tool(&h.caller, "a.need_path", json!({"path": 1}))
        .await;
    assert_eq!(
        code_of(&r).as_deref(),
        Some("InvalidArguments"),
        "类型错应被拒"
    );

    let r = h
        .registry
        .call_tool(&h.caller, "a.need_path", json!({"path": "/tmp"}))
        .await;
    assert!(!r.is_error, "合法参数应执行");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_tool_name_rejected() {
    let h = Harness::new();
    let m1 = trusted_meta("a", "dup", |_| {
        Box::pin(async { ToolResult::success_text("1") })
    });
    let m2 = trusted_meta("a", "dup", |_| {
        Box::pin(async { ToolResult::success_text("2") })
    });
    assert!(h.registry.register_tool(m1).is_ok());
    let err = h.registry.register_tool(m2).unwrap_err();
    assert!(
        matches!(
            err,
            launcher_plugin_framework::error::RegistryError::DuplicateName { .. }
        ),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_name_must_be_prefixed_by_owner() {
    let h = Harness::new();
    let mut meta = trusted_meta("a", "x", |_| {
        Box::pin(async { ToolResult::success_text("1") })
    });
    // 所有者是 a，但名字改成 b.x
    meta.name = "b.x".to_owned();
    meta.handler = FunctionRef::new(PluginId::new("a"), "b.x", |_| {
        Box::pin(async { ToolResult::success_text("1") })
    });
    assert!(h.registry.register_tool(meta).is_err(), "前缀不符必须拒绝");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_tool_must_not_reference_a_unit() {
    // 可信路径不经过调度器的结构性保证：元数据里不能有 unit_handle。
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("a")).await.unwrap();
    let mut meta = trusted_meta("a", "t", |_| {
        Box::pin(async { ToolResult::success_text("1") })
    });
    meta.unit_handle = Some(unit);
    let err = h.registry.register_tool(meta).unwrap_err();
    assert!(
        matches!(
            err,
            launcher_plugin_framework::error::RegistryError::MalformedName { .. }
        ),
        "Trusted + unit_handle 必须被拒：{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_cannot_unregister_foreign_tool() {
    let h = Harness::new();
    let m = trusted_meta("owner", "t", |_| {
        Box::pin(async { ToolResult::success_text("1") })
    });
    h.registry.register_tool(m).unwrap();
    let err = h
        .registry
        .unregister_tool(&PluginId::new("attacker"), "owner.t")
        .unwrap_err();
    assert!(
        matches!(
            err,
            launcher_plugin_framework::error::RegistryError::ForeignOwnership { .. }
        ),
        "{err:?}"
    );
    // 平台层可以
    assert!(h
        .registry
        .unregister_tool(&PluginId::new(PluginId::PLATFORM), "owner.t")
        .is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_cannot_restart_itself() {
    // 规格：restart_unit 仅平台层/用户可调用，插件不可自行重启。
    let h = Harness::new();
    let err = h
        .registry
        .restart_unit(&PluginId::new("some_plugin"), &PluginId::new("some_plugin"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ToolErrorCode::PermissionDenied);
    assert!(err.message.contains("仅平台层"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_returns_trust_level_and_availability() {
    // 规格 tools/list 必须返回 name/description/inputSchema/trust_level/available。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "ok", |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();

    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "work", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();

    let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
    assert_eq!(list.len(), 2);
    let trusted = list.iter().find(|l| l.name == "t.ok").unwrap();
    let sandboxed = list.iter().find(|l| l.name == "s.work").unwrap();
    assert_eq!(trusted.trust_level, TrustLevel::Trusted);
    assert_eq!(sandboxed.trust_level, TrustLevel::Sandboxed);
    assert!(trusted.available && sandboxed.available);
    assert!(sandboxed.unavailable_reason.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_is_sorted_and_stable() {
    let h = Harness::new();
    for name in ["zeta", "alpha", "mid"] {
        h.registry
            .register_tool(trusted_meta("p", name, |_| {
                Box::pin(async { ToolResult::success_text("1") })
            }))
            .unwrap();
    }
    let list = h.registry.list_tools(&h.caller, &ToolFilter::all());
    let names: Vec<_> = list.iter().map(|l| l.name.clone()).collect();
    assert_eq!(names, vec!["p.alpha", "p.mid", "p.zeta"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filter_by_trust_level_and_availability() {
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "ok", |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(
            sandboxed_meta("s", &unit, "work", 5_000, |_| {
                Box::pin(async { ToolResult::success_text("1") })
            })
            .clone(),
        )
        .unwrap();

    let only_sandbox = ToolFilter {
        trust_level: Some(TrustLevel::Sandboxed),
        ..ToolFilter::all()
    };
    let list = h.registry.list_tools(&h.caller, &only_sandbox);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "s.work");
}
