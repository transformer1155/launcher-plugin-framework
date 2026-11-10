//! 端到端演示：一个可信插件 + 一个沙箱插件，含崩溃与重启。
//!
//! 运行：`cargo run --example host_demo`
//!
//! 这个例子刻意只做"框架本身"的事 —— 不涉及 Mod 管理、版本下载等业务逻辑。

use std::sync::Arc;
use std::time::Duration;

use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

fn tool_meta_sandboxed(
    pid: &PluginId,
    unit: UnitHandle,
    name: &str,
    timeout_ms: u64,
    body: impl Fn(serde_json::Value) -> launcher_plugin_framework::types::BoxToolFuture
        + Send
        + Sync
        + 'static,
) -> ToolMeta {
    let full = format!("{}.{}", pid, name);
    ToolMeta {
        name: full.clone(),
        description: format!("沙箱工具 {name}"),
        input_schema: json!({"type": "object"}),
        handler: FunctionRef::new(pid.clone(), full.clone(), body),
        trust_level: TrustLevel::Sandboxed,
        allowed_callers: vec![PluginId::new("*")],
        destructive: false,
        timeout_ms,
        owner_plugin_id: pid.clone(),
        unit_handle: Some(unit),
    }
}

fn tool_meta_trusted(
    pid: &PluginId,
    name: &str,
    body: impl Fn(serde_json::Value) -> launcher_plugin_framework::types::BoxToolFuture
        + Send
        + Sync
        + 'static,
) -> ToolMeta {
    let full = format!("{}.{}", pid, name);
    ToolMeta {
        name: full.clone(),
        description: format!("可信工具 {name}"),
        input_schema: json!({"type": "object"}),
        handler: FunctionRef::new(pid.clone(), full.clone(), body),
        trust_level: TrustLevel::Trusted,
        allowed_callers: vec![PluginId::new("*")],
        destructive: false,
        timeout_ms: 30_000,
        owner_plugin_id: pid.clone(),
        unit_handle: None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // ---------- 组装 ----------
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));

    // 演示用的验签器：内置公钥集合为空（真实产品从启动器二进制读取），
    // 因此任何插件都验签不过 —— 正好演示"降级到沙箱"这条主路径。
    // `without_required_revocation_list` 只在这个演示里用：真实产品应当装载
    // 已签名的吊销列表，否则默认会 fail-closed（那也是正确行为）。
    let verifier =
        Arc::new(Ed25519Verifier::new(BuiltInKeys::default()).without_required_revocation_list());

    let restarts = RestartManager::new(scheduler.clone(), verifier.clone(), registry.clone());
    let loader = PluginLoader {
        registry: registry.clone(),
        scheduler: scheduler.clone(),
        verifier: verifier.clone(),
        restarts: restarts.clone(),
    };

    // ---------- 1) 可信插件 ----------
    // 演示用"空公钥集合"时无法通过验签，因此这里手动走可信路径：
    // 直接注册工具（模拟"加载器判定为 Valid"之后的动作）。
    let trusted_id = PluginId::new("trusted_demo");
    registry
        .register_tool(tool_meta_trusted(&trusted_id, "echo", |args| {
            Box::pin(async move { ToolResult::success(json!({"echo": args, "from": "trusted"})) })
        }))
        .expect("注册可信工具");
    println!("[demo] 可信插件就绪（工具: trusted_demo.echo）");

    // ---------- 2) 沙箱插件（含一个会 panic 的工具） ----------
    let sandbox_pkg = PluginPackage {
        plugin_id: PluginId::new("sandbox_demo"),
        manifest_raw: br#"{"id":"sandbox_demo","name":"Sandbox Demo","version":"0.1.0"}"#.to_vec(),
        manifest: serde_json::from_slice(
            br#"{"id":"sandbox_demo","name":"Sandbox Demo","version":"0.1.0"}"#,
        )
        .unwrap(),
        files: vec![],
        signature: None, // 无签名 → 降级沙箱
    };

    let build_tools: Arc<dyn Fn(Option<UnitHandle>) -> Vec<ToolMeta> + Send + Sync> =
        Arc::new(move |unit: Option<UnitHandle>| {
            let unit = unit.expect("沙箱插件必须拿到单元");
            let pid = PluginId::new("sandbox_demo");
            vec![
                tool_meta_sandboxed(&pid, unit.clone(), "slow", 200, |_a| {
                    // 可中断：await sleep
                    Box::pin(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        ToolResult::success_text("slow done")
                    })
                }),
                tool_meta_sandboxed(&pid, unit.clone(), "boom", 5_000, |_a| {
                    Box::pin(async move {
                        panic!("沙箱 handler 故意 panic");
                    })
                }),
            ]
        });

    let cfg = unit_config("sandbox_demo");
    let loaded = loader
        .load(sandbox_pkg, cfg, build_tools)
        .await
        .expect("沙箱插件应当加载成功（验签失败只降级，不拒绝）");
    println!(
        "[demo] 沙箱插件就绪：trust={} 工具数={} UI提示={:?}",
        loaded.trust_level,
        loaded.registered_tools,
        loaded.user_notice()
    );

    let caller = PluginId::new(PluginId::PLATFORM);

    // ---------- 3) 可信插件调用：正常 + panic 捕获 ----------
    let r = registry
        .call_tool(&caller, "trusted_demo.echo", json!({"msg": "hi"}))
        .await;
    println!(
        "[demo] trusted echo -> is_error={} {:?}",
        r.is_error, r.structured_content
    );

    // ---------- 4) 沙箱：工具级超时（可中断） ----------
    let t0 = std::time::Instant::now();
    let r = registry
        .call_tool(&caller, "sandbox_demo.slow", json!({}))
        .await;
    println!(
        "[demo] sandbox slow -> is_error={} code={} 耗时={}ms",
        r.is_error,
        r.structured_content.as_ref().unwrap()["code"],
        t0.elapsed().as_millis()
    );

    // ---------- 5) 沙箱：崩溃 ----------
    let r = registry
        .call_tool(&caller, "sandbox_demo.boom", json!({}))
        .await;
    println!(
        "[demo] sandbox boom -> is_error={} code={}",
        r.is_error,
        r.structured_content.as_ref().unwrap()["code"]
    );

    // ---------- 6) 崩溃后：list_tools 应显示不可用 ----------
    tokio::time::sleep(Duration::from_millis(50)).await;
    let list = registry.list_tools(&caller, &ToolFilter::all());
    for t in &list {
        println!(
            "[demo] tool {} trust={} available={} reason={:?}",
            t.name, t.trust_level, t.available, t.unavailable_reason
        );
    }

    // ---------- 7) 崩溃后调用应立即失败（不排队） ----------
    let r = registry
        .call_tool(&caller, "sandbox_demo.slow", json!({}))
        .await;
    println!(
        "[demo] call after crash -> is_error={} code={}",
        r.is_error,
        r.structured_content.as_ref().unwrap()["code"]
    );

    // ---------- 8) 重启状态 ----------
    tokio::time::sleep(Duration::from_millis(500)).await;
    for s in restarts.states() {
        println!("[demo] restart state: {s:?}");
    }

    // ---------- 9) MCP 适配 ----------
    let adapter = McpAdapter::new(registry.clone());
    let identity = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let list = adapter.tools_list(&identity, &ToolFilter::all());
    println!("[demo] mcp tools/list -> {} 个工具", list.tools.len());

    let resp = adapter
        .handle_value(
            &identity,
            json!({
                "protocolVersion": launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": {"supportsStructuredContent": true},
                "method": "tools/call",
                "params": {"name": "trusted_demo.echo", "arguments": {"x": 1}}
            }),
        )
        .await;
    println!(
        "[demo] mcp tools/call -> is_error={} {:?}",
        resp.is_error, resp.structured_content
    );
}
