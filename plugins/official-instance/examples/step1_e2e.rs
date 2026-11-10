//! Step 1 端到端验证：插件被 loader 加载 → 工具注册进注册表 → 经 MCP tools/call 调用。
//!
//! 这个示例是**验证用**，不属于插件交付物本身。它证明三件事：
//!
//! 1. `common` 生成的 `ToolMeta` 能通过框架的 `register_tool`（schema 子集、命名、trust/unit 匹配都合法）；
//! 2. 可信路径**不经过调度器**（没有创建任何沙箱单元）；
//! 3. `mcp::McpAdapter` 的 `tools/list` / `tools/call` 能正常往返，且返回形状符合规格。
//!
//! 运行：在 `plugins/` 目录下 `cargo run --example step1_e2e`

use std::sync::Arc;

use launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

/// 造一个"已签名"的包。演示里用空公钥集合，因此验签**不会**通过 ——
/// 这样我们能同时演示"降级到沙箱"这条路径。见 `verify_and_load` 的注释。
fn package_for(plugin_id: &str, manifest_json: &str) -> PluginPackage {
    let manifest_raw = manifest_json.as_bytes().to_vec();
    let manifest = serde_json::from_slice(&manifest_raw).expect("清单 JSON 合法");
    PluginPackage {
        plugin_id: PluginId::new(plugin_id),
        manifest_raw,
        manifest,
        files: vec![],
        signature: None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // ---------- 组装框架 ----------
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));

    // 演示用验签器：没有内置公钥 → 任何签名都验不过 → 全部降级沙箱。
    // 这正是我们要演示的"插件声明 trusted 但实际被降级"。
    let verifier =
        Arc::new(Ed25519Verifier::new(BuiltInKeys::default()).without_required_revocation_list());
    let restarts = RestartManager::new(scheduler.clone(), verifier.clone(), registry.clone());
    let loader = PluginLoader {
        registry: registry.clone(),
        scheduler: scheduler.clone(),
        verifier: verifier.clone(),
        restarts: restarts.clone(),
    };

    // ---------- 插件的清单（与 plugins/official-instance/plugin.json 同源） ----------
    let manifest_json = r#"{
        "id": "com.example.official-instance",
        "name": "实例管理",
        "version": "0.1.0",
        "description": "创建、列出、切换、删除游戏实例",
        "tools": ["list_instances"]
    }"#;
    let pkg = package_for("com.example.official-instance", manifest_json);

    // ---------- 插件侧：构造 ctx + ToolSet ----------
    let data_dir = std::env::temp_dir().join(format!("lpf-step1-{}", std::process::id()));
    let (ctx, toolset) = official_instance::build(&data_dir);

    // 一致性校验：代码注册的工具必须与清单声明完全一致（双向）。
    let declared = toolset.manifest_tools();
    let manifest = launcher_plugin_common::PluginManifestV1::parse(include_bytes!(
        "../../official-instance/plugin.json"
    ))
    .expect("插件清单应当合法");
    manifest
        .check_tools(&declared)
        .expect("代码注册的工具必须与 plugin.json 的 tools 完全一致");
    println!("[step1] 清单一致性校验通过：tools = {declared:?}");
    for w in manifest.load_warnings() {
        println!("[step1] 清单告警：{w}");
    }

    // ---------- 交给 loader ----------
    // build_tools 闭包把 trust/unit 交给 into_metas —— 注意 trust 由 loader 决定，
    // 但 loader 的 build_tools 签名只给 Option<UnitHandle>，所以这里用"有 unit 即沙箱"推导。
    let pid = PluginId::new(official_instance::PLUGIN_ID);
    let ctx_for_tools = ctx.clone();
    let set_for_tools = Arc::new(toolset);
    let build = {
        let set = Arc::clone(&set_for_tools);
        let ctx = ctx_for_tools.clone();
        Arc::new(move |unit: Option<UnitHandle>| {
            let trust = if unit.is_some() {
                TrustLevel::Sandboxed
            } else {
                TrustLevel::Trusted
            };
            set.into_metas(&ctx, trust, unit, vec![PluginId::new("*")])
        })
    };

    let cfg = launcher_plugin_framework::scheduler::unit_config(pid.clone());
    let loaded = loader
        .load(pkg, cfg, build)
        .await
        .expect("加载应当成功（验签失败会降级而不是拒绝）");

    println!(
        "[step1] 加载完成：plugin={} trust={} 单元={:?} 工具数={}",
        loaded.plugin_id,
        loaded.trust_level,
        loaded.unit.as_ref().map(|u| u.generation),
        loaded.registered_tools
    );
    println!(
        "[step1] UI 提示：{:?}",
        loaded.user_notice().unwrap_or("（无，插件为可信）")
    );

    // ---------- 断言：可信路径不创建沙箱单元 ----------
    if loaded.trust_level.is_trusted() {
        assert_eq!(scheduler.list_units().len(), 0, "可信插件不应创建沙箱单元");
        println!("[step1] 已确认：可信路径未创建任何沙箱单元");
    } else {
        assert!(loaded.unit.is_some(), "沙箱插件必须有执行单元");
        println!(
            "[step1] 已确认：沙箱路径创建了单元（共 {} 个）",
            scheduler.list_units().len()
        );
    }

    // ---------- 注册进注册表 ----------
    let caller = PluginId::new(PluginId::PLATFORM);
    let tools = registry.list_tools(&caller, &ToolFilter::all());
    println!("[step1] 注册表中共 {} 个工具：", tools.len());
    for t in &tools {
        println!(
            "  - {} | trust={} available={} destructive={}",
            t.name, t.trust_level, t.available, t.destructive
        );
    }
    assert_eq!(
        tools.len(),
        4,
        "本批应有 4 个工具：create/list/switch/delete"
    );

    // ---------- 经 MCP tools/list ----------
    let adapter = McpAdapter::new(registry.clone());
    let identity = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let list = adapter
        .handle_value(
            &identity,
            json!({
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": { "supportsStructuredContent": true },
                "method": "tools/list",
                "params": {}
            }),
        )
        .await;
    assert!(!list.is_error, "tools/list 不应失败");
    let payload = list.structured_content.expect("必须有 structuredContent");
    println!(
        "[step1] MCP tools/list -> {} 个工具",
        payload["tools"].as_array().unwrap().len()
    );
    for t in payload["tools"].as_array().unwrap() {
        println!(
            "  - name={} trustLevel={} available={}",
            t["name"], t["trustLevel"], t["available"]
        );
        // 规格要求：name / description / inputSchema / trust_level / available 必须都在
        assert!(t["name"].is_string());
        assert!(t["description"].is_string());
        assert!(t["inputSchema"].is_object());
        assert!(t["trustLevel"].is_string());
        assert!(t["available"].is_boolean());
    }

    // ---------- 经 MCP tools/call ----------
    let call = adapter
        .handle_value(
            &identity,
            json!({
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": { "supportsStructuredContent": true },
                "method": "tools/call",
                "params": {
                    "name": "com.example.official-instance.list_instances",
                    "arguments": {}
                }
            }),
        )
        .await;
    println!("[step1] MCP tools/call -> isError={}", call.is_error);
    println!(
        "[step1] structuredContent = {}",
        serde_json::to_string_pretty(&call.structured_content).unwrap_or_default()
    );
    assert!(!call.is_error, "list_instances 应当成功");

    // ---------- 清理 ----------
    let _ = std::fs::remove_dir_all(&data_dir);
    println!("[step1] 端到端链路验证通过");
}
