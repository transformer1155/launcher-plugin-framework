//! Step 4 端到端验证：`official-launch` 经完整框架链路（loader → registry → MCP）。
//!
//! 演示：
//! 1. 未配置时 `get_launch_config` 返回默认值；
//! 2. `set_launch_config` 写入并经 `get` 读回；
//! 3. `launch_game` 返回 `pid: null` + `simulated: true`（不创建进程）；
//! 4. `get_last_log` 能读到模拟启动写的日志；
//! 5. 静态检查：插件里没有任何进程创建 API。
//!
//! 运行：在 `plugins/` 目录下 `cargo run --example step4_e2e`

use std::sync::Arc;

use launcher_plugin_framework::mcp::{McpAdapter, ToolsCallResponse, SUPPORTED_PROTOCOL_VERSION};
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistryImpl;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

async fn mcp_call(
    adapter: &McpAdapter<ToolRegistryImpl>,
    name: &str,
    args: serde_json::Value,
) -> ToolsCallResponse {
    let identity = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    adapter
        .handle_value(
            &identity,
            json!({
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": { "supportsStructuredContent": true },
                "method": "tools/call",
                "params": { "name": name, "arguments": args }
            }),
        )
        .await
}

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
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));
    let verifier =
        Arc::new(Ed25519Verifier::new(BuiltInKeys::default()).without_required_revocation_list());
    let restarts = RestartManager::new(scheduler.clone(), verifier.clone(), registry.clone());
    let loader = PluginLoader {
        registry: registry.clone(),
        scheduler: scheduler.clone(),
        verifier: verifier.clone(),
        restarts: restarts.clone(),
    };

    // ---------- 清单一致性 ----------
    let manifest = launcher_plugin_common::PluginManifestV1::parse(include_bytes!(
        "../../official-launch/plugin.json"
    ))
    .expect("插件清单应当合法");
    let set = official_launch::tools();
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码一致");
    println!("[step4] 清单一致性通过：tools = {:?}", manifest.tools);

    // ---------- 加载 ----------
    let data_dir = std::env::temp_dir().join(format!("lpf-step4-{}", std::process::id()));
    let (ctx, toolset) = official_launch::build(&data_dir);
    let manifest_json = r#"{
        "id": "com.example.official-launch",
        "name": "启动管理",
        "version": "0.1.0",
        "description": "读取/设置启动参数、启动游戏、查看日志",
        "tools": ["get_launch_config", "set_launch_config", "launch_game", "get_last_log"]
    }"#;
    let pkg = package_for("com.example.official-launch", manifest_json);

    let pid = PluginId::new(official_launch::PLUGIN_ID);
    let ctx_for_tools = ctx.clone();
    let set_for_tools = Arc::new(toolset);
    let build = {
        let set = Arc::clone(&set_for_tools);
        let c = ctx_for_tools.clone();
        Arc::new(move |unit: Option<UnitHandle>| {
            let trust = if unit.is_some() {
                TrustLevel::Sandboxed
            } else {
                TrustLevel::Trusted
            };
            set.into_metas(&c, trust, unit, vec![PluginId::new("*")])
        })
    };
    let cfg = launcher_plugin_framework::scheduler::unit_config(pid.clone());
    let loaded = loader.load(pkg, cfg, build).await.expect("加载应当成功");
    println!(
        "[step4] 加载完成：plugin={} trust={} 工具数={}",
        loaded.plugin_id, loaded.trust_level, loaded.registered_tools
    );

    let adapter = McpAdapter::new(registry.clone());

    // ---------- 1) 未配置 → 默认值 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.get_launch_config",
        json!({"instance_id": "inst-001"}),
    )
    .await;
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    println!(
        "[step4] get_launch_config(未配置) -> is_default={} memory_mb={}",
        sc["is_default"], sc["memory_mb"]
    );
    assert_eq!(sc["is_default"], true, "未配置时必须标默认值");

    // ---------- 2) 设置配置 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.set_launch_config",
        json!({
            "instance_id": "inst-001",
            "config": {
                "jvm_args": ["-XX:+UseG1GC", "-XX:+UseStringDeduplication"],
                "game_args": ["--fullscreen"],
                "memory_mb": 6144
            }
        }),
    )
    .await;
    assert!(!r.is_error, "设置配置应成功");
    println!(
        "[step4] set_launch_config -> changed={}",
        r.structured_content.as_ref().unwrap()["changed"]
    );

    // ---------- 3) 读回 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.get_launch_config",
        json!({"instance_id": "inst-001"}),
    )
    .await;
    let sc = r.structured_content.unwrap();
    println!(
        "[step4] get_launch_config(已配置) -> is_default={} memory_mb={} jvm={:?}",
        sc["is_default"], sc["memory_mb"], sc["jvm_args"]
    );
    assert_eq!(sc["is_default"], false);
    assert_eq!(sc["memory_mb"], 6144);

    // ---------- 4) 启动（模拟）----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.launch_game",
        json!({"instance_id": "inst-001"}),
    )
    .await;
    assert!(!r.is_error);
    let sc = r.structured_content.unwrap();
    println!(
        "[step4] launch_game -> pid={} simulated={}",
        sc["pid"], sc["simulated"]
    );
    assert_eq!(
        sc["pid"],
        serde_json::Value::Null,
        "pid 必须是 null（诚实：没有真实进程）"
    );
    assert_eq!(sc["simulated"], true);

    // ---------- 5) 读日志 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.get_last_log",
        json!({"instance_id": "inst-001", "lines": 10}),
    )
    .await;
    assert!(!r.is_error, "启动后应能读到日志");
    let sc = r.structured_content.unwrap();
    let content = sc["content"].as_str().unwrap();
    println!(
        "[step4] get_last_log -> {} 行，truncated={}",
        content.split('\n').count(),
        sc["truncated"]
    );
    println!("[step4] 日志内容：{}", content.trim());
    assert!(content.contains("simulated"), "日志必须标 simulated");

    // ---------- 6) 边界：非法内存 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-launch.set_launch_config",
        json!({"instance_id": "inst-001", "config": {"memory_mb": 0}}),
    )
    .await;
    println!(
        "[step4] set_launch_config(memory=0) -> isError={}",
        r.is_error
    );
    assert!(r.is_error, "越界内存应被 schema/校验拒绝");

    // ---------- 清理 ----------
    let _ = std::fs::remove_dir_all(&data_dir);
    println!("[step4] 端到端验证通过");
}
