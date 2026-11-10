//! MCP 语义对接：无状态信封、tools/list 字段、tools/call 同构、协议版本校验。

mod common;

use launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use serde_json::json;

use common::*;

fn body_list() -> serde_json::Value {
    json!({
        "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
        "clientCapabilities": {"supportsStructuredContent": true},
        "method": "tools/list",
        "params": {}
    })
}

fn body_call(name: &str, args: serde_json::Value) -> serde_json::Value {
    json!({
        "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
        "clientCapabilities": {"supportsStructuredContent": true},
        "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_list_exposes_required_fields() {
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

    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let resp = adapter.handle_value(&id, body_list()).await;
    assert!(!resp.is_error);

    let payload = resp.structured_content.unwrap();
    let tools = payload["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    for t in tools {
        // 规格要求：name / description / inputSchema / trust_level / available
        assert!(t["name"].is_string());
        assert!(t["description"].is_string());
        assert!(t["inputSchema"].is_object());
        assert!(
            matches!(t["trustLevel"].as_str(), Some("Trusted" | "Sandboxed")),
            "trustLevel 必须是 Trusted/Sandboxed，实际 {}",
            t["trustLevel"]
        );
        assert!(t["available"].is_boolean());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_call_returns_mcp_shape_for_success() {
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "echo", |_| {
            Box::pin(async { ToolResult::success(json!({"ok": true})) })
        }))
        .unwrap();
    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let resp = adapter
        .handle_value(&id, body_call("t.echo", json!({})))
        .await;
    assert!(!resp.is_error);
    assert!(!resp.content.is_empty(), "content 必须非空");
    assert!(resp.structured_content.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sandbox_crash_and_business_error_are_protocol_isomorphic() {
    // 规格核心：沙箱崩溃对调用方表现为 isError: true，与工具内部报错**协议层面同构**。
    // 用户类插件不需要为"虚拟进程"写特殊分支。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "biz", |_| {
            Box::pin(async { ToolResult::business_error("bad input") })
        }))
        .unwrap();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "boom", 5_000, |_| {
            Box::pin(async {
                panic!("sandbox crash");
            })
        }))
        .unwrap();

    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };

    let biz = adapter
        .handle_value(&id, body_call("t.biz", json!({})))
        .await;
    let crash = adapter
        .handle_value(&id, body_call("s.boom", json!({})))
        .await;

    // 协议层：两者形状完全一致 —— 都是 isError + 单块 Text 内容 + 有 structuredContent
    assert!(biz.is_error, "业务错误必须 isError");
    assert!(crash.is_error, "沙箱崩溃必须 isError");
    assert_eq!(biz.content.len(), 1);
    assert_eq!(crash.content.len(), 1);
    assert!(
        matches!(
            biz.content[0],
            launcher_plugin_framework::types::ContentBlock::Text { .. }
        ) && matches!(
            crash.content[0],
            launcher_plugin_framework::types::ContentBlock::Text { .. }
        ),
        "两者都必须是可以被纯文本客户端消费的 Text 块"
    );
    assert!(biz.structured_content.is_some() && crash.structured_content.is_some());
    // 只有结构化字段里的 code 有区别
    let biz_code = biz
        .structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str());
    let crash_code = crash
        .structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str());
    assert_eq!(biz_code, None, "业务失败不带框架错误码");
    assert_eq!(crash_code, Some("ProcessCrashed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_version_is_validated_per_request() {
    // 无状态版本：每个请求自带协议版本，服务端逐请求校验。
    let h = Harness::new();
    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity::default();
    let mut body = body_list();
    body["protocolVersion"] = json!("1999-01-01");
    let resp = adapter.handle_value(&id, body).await;
    assert!(resp.is_error);
    assert_eq!(
        resp.structured_content.as_ref().unwrap()["code"],
        "InvalidArguments"
    );
    assert!(resp.structured_content.as_ref().unwrap()["supported"]
        .as_str()
        .unwrap()
        .contains(SUPPORTED_PROTOCOL_VERSION));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_method_is_reported() {
    let h = Harness::new();
    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity::default();
    let resp = adapter
        .handle_value(
            &id,
            json!({
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "method": "resources/list",
                "params": {}
            }),
        )
        .await;
    assert!(resp.is_error, "本框架只实现 tools/list 与 tools/call");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_initialize_handshake_needed() {
    // 无状态版本：没有 initialize 握手的痕迹。
    let h = Harness::new();
    h.registry
        .register_tool(trusted_meta("t", "ok", |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();
    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity::default();
    // 直接 tools/call，不做任何"初始化"
    let resp = adapter
        .handle_value(&id, body_call("t.ok", json!({})))
        .await;
    assert!(!resp.is_error, "不握手也应能直接调用");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_shows_restarting_reason_through_mcp() {
    let h = Harness::new();
    let unit = h.scheduler.create_unit(h.unit_cfg("s")).await.unwrap();
    h.registry
        .register_tool(sandboxed_meta("s", &unit, "work", 5_000, |_| {
            Box::pin(async { ToolResult::success_text("1") })
        }))
        .unwrap();
    h.registry
        .note_availability(&PluginId::new("s"), Some(UnavailableReason::Restarting));

    let adapter = McpAdapter::new(h.registry.clone());
    let id = ClientIdentity::default();
    let resp = adapter.handle_value(&id, body_list()).await;
    let payload = resp.structured_content.unwrap();
    let tool = &payload["tools"][0];
    assert_eq!(tool["available"], false);
    assert_eq!(tool["unavailableReason"], "restarting");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caller_identity_maps_to_acl() {
    // ACL 决定可见性：不同的 caller 看到不同的工具集。
    let h = Harness::new();
    let mut meta = trusted_meta("t", "priv", |_| {
        Box::pin(async { ToolResult::success_text("secret") })
    });
    meta.allowed_callers = vec![PluginId::new("friend")];
    h.registry.register_tool(meta).unwrap();

    let adapter = McpAdapter::new(h.registry.clone()).with_caller_resolver(|id| {
        PluginId::new(id.plugin_id.clone().unwrap_or("stranger".to_owned()))
    });

    let friend = ClientIdentity {
        plugin_id: Some("friend".to_owned()),
        ..Default::default()
    };
    let stranger = ClientIdentity {
        plugin_id: Some("stranger".to_owned()),
        ..Default::default()
    };

    let f = adapter.handle_value(&friend, body_list()).await;
    let s = adapter.handle_value(&stranger, body_list()).await;
    assert_eq!(
        f.structured_content.unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.structured_content.unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
