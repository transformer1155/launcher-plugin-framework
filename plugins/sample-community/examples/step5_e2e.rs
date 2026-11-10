//! Step 5 端到端演示：**沙箱降级 → 正常调用 → 可中断超时 → 崩溃 → 自动重启 → 恢复可用**。
//!
//! # 为什么是独立示例而不是改 `host_demo.rs`
//!
//! 规格原文建议"在 `host_demo` 里加一段演示"，但 `examples/host_demo.rs` 位于**框架目录**，
//! 而任务是"不要修改框架层代码"。示例文件虽不是 `src/`，改动它仍会与"框架目录零改动"
//! 的约定冲突。因此这段演示放在插件侧独立示例里 —— 演示内容完全一致，且更贴近
//! "插件生态"这一侧。
//!
//! # 这条演示链验证了什么
//!
//! | 步骤 | 验证的框架能力 |
//! |---|---|
//! | 1. 无签名加载 | 验签失败 → **降级**到沙箱（不拒绝加载） |
//! | 2. `hello` 调用 | 沙箱插件在降级后**功能完好** |
//! | 3. `slow_task` 超时 | 可中断超时 → `Timeout`，单元**不崩溃** |
//! | 4. `crash_me` | 沙箱 panic → `ProcessCrashed`，**宿主存活** |
//! | 5. 崩溃后 `list_tools` | `available: false` + `reason: "restarting"` |
//! | 6. 崩溃后 `call_tool` | 立即返回 `ProcessRestarting`，**不排队** |
//! | 7. 等待重启 | 指数退避后单元重建 |
//! | 8. 恢复后调用 | 工具重新可用（新代数单元） |
//!
//! 运行：在 `plugins/` 目录下 `cargo run --example step5_e2e`

use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::ToolRegistry;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

fn package_for(plugin_id: &str, manifest_json: &str) -> PluginPackage {
    let manifest_raw = manifest_json.as_bytes().to_vec();
    let manifest = serde_json::from_slice(&manifest_raw).expect("清单 JSON 合法");
    PluginPackage {
        plugin_id: PluginId::new(plugin_id),
        manifest_raw,
        manifest,
        files: vec![],
        signature: None, // 无签名 → 必然降级
    }
}

/// 从结果里取 `structuredContent.code`。
fn code_of(v: &Option<serde_json::Value>) -> String {
    v.as_ref()
        .and_then(|x| x.get("code"))
        .and_then(|x| x.as_str())
        .unwrap_or("<none>")
        .to_owned()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));
    // 演示用：空公钥集合 + 允许无吊销列表 → 无签名插件必然降级
    let verifier =
        Arc::new(Ed25519Verifier::new(BuiltInKeys::default()).without_required_revocation_list());
    let restarts = RestartManager::new(scheduler.clone(), verifier.clone(), registry.clone());
    let loader = PluginLoader {
        registry: registry.clone(),
        scheduler: scheduler.clone(),
        verifier: verifier.clone(),
        restarts: restarts.clone(),
    };

    // ==================== 1) 无签名加载 → 降级 ====================
    let manifest = launcher_plugin_common::PluginManifestV1::parse(include_bytes!(
        "../../sample-community/plugin.json"
    ))
    .expect("插件清单应当合法");
    let set = sample_community::tools();
    manifest
        .check_tools(&set.manifest_tools())
        .expect("清单与代码的工具必须一致");

    println!("[step5] ========== 1) 加载无签名社区插件 ==========");
    println!(
        "[step5] 清单声明：trust={:?} review={:?}",
        manifest.trust, manifest.review
    );
    for w in manifest.load_warnings() {
        println!("[step5] 清单告警（Q8 要求）：{w}");
    }

    let data_dir = std::env::temp_dir().join(format!("lpf-step5-{}", std::process::id()));
    let (ctx, toolset) = sample_community::build(&data_dir);

    let manifest_json = r#"{
        "id": "com.example.sample-community",
        "name": "社区示例插件",
        "version": "0.1.0",
        "description": "无签名沙箱插件",
        "tools": ["hello", "slow_task", "crash_me"]
    }"#;
    let pkg = package_for("com.example.sample-community", manifest_json);

    let pid = PluginId::new(sample_community::PLUGIN_ID);
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
    let loaded = loader
        .load(pkg, cfg, build)
        .await
        .expect("无签名应当降级加载而不是被拒绝");

    println!(
        "[step5] 加载结果：trust={} 单元代数={:?} 工具数={}",
        loaded.trust_level,
        loaded.unit.as_ref().map(|u| u.generation),
        loaded.registered_tools
    );
    println!(
        "[step5] UI 提示：{}",
        loaded.user_notice().unwrap_or("（无）")
    );
    assert_eq!(
        loaded.trust_level,
        TrustLevel::Sandboxed,
        "无签名插件必须降级到沙箱"
    );
    assert!(loaded.unit.is_some(), "沙箱插件必须有执行单元");
    let first_generation = loaded.unit.as_ref().unwrap().generation;

    let caller = PluginId::new(PluginId::PLATFORM);

    // ==================== 2) hello：沙箱里正常工作 ====================
    println!("\n[step5] ========== 2) hello（沙箱功能完好）==========");
    let r = registry
        .call_tool(
            &caller,
            "com.example.sample-community.hello",
            json!({"name": "世界"}),
        )
        .await;
    assert!(!r.is_error, "降级到沙箱后功能应完好");
    let sc = r.structured_content.unwrap();
    println!("[step5] hello -> {}", sc["greeting"]);
    assert_eq!(sc["sandboxed"], true);

    // ==================== 3) slow_task：可中断超时 ====================
    println!("\n[step5] ========== 3) slow_task（可中断超时 → Timeout）==========");
    let t0 = Instant::now();
    let r = registry
        .call_tool(
            &caller,
            "com.example.sample-community.slow_task",
            json!({"seconds": 10}),
        )
        .await;
    let elapsed = t0.elapsed();
    let code = code_of(&r.structured_content);
    println!(
        "[step5] slow_task -> isError={} code={code} 耗时={}ms",
        r.is_error,
        elapsed.as_millis()
    );
    assert!(r.is_error);
    assert_eq!(code, "Timeout", "可中断超时应返回 Timeout");
    assert!(
        elapsed < Duration::from_secs(5),
        "可中断超时必须及时返回，实际 {elapsed:?}"
    );
    // 关键：超时后单元仍然可用（不是崩溃）
    let state = scheduler.get_state(loaded.unit.clone().unwrap());
    println!("[step5] 超时后单元状态 = {state:?}（应为 Running）");
    assert!(
        matches!(state, Some(UnitState::Running)),
        "可中断超时不应导致单元崩溃"
    );

    // ==================== 4) crash_me：沙箱崩溃，宿主存活 ====================
    println!("\n[step5] ========== 4) crash_me（沙箱崩溃，宿主存活）==========");
    let r = registry
        .call_tool(
            &caller,
            "com.example.sample-community.crash_me",
            json!({"note": "demo"}),
        )
        .await;
    let code = code_of(&r.structured_content);
    println!("[step5] crash_me -> isError={} code={code}", r.is_error);
    assert!(r.is_error);
    assert_eq!(
        code, "ProcessCrashed",
        "沙箱 panic 应表现为 ProcessCrashed（不是 HandlerPanic）"
    );
    println!("[step5] 已确认：宿主进程仍然存活（能继续执行到这里）");

    // ==================== 5) 崩溃后 list_tools ====================
    println!("\n[step5] ========== 5) 崩溃后 list_tools（available=false）==========");
    tokio::time::sleep(Duration::from_millis(30)).await;
    let list = registry.list_tools(&caller, &ToolFilter::all());
    for t in &list {
        println!(
            "[step5] tool {} available={} reason={:?}",
            t.name, t.available, t.unavailable_reason
        );
    }
    let hello = list
        .iter()
        .find(|t| t.name == "com.example.sample-community.hello")
        .expect("hello 必须仍在列表里（规格要求重启期间仍返回）");
    assert!(!hello.available, "崩溃后应标记不可用");
    assert_eq!(
        hello.unavailable_reason,
        Some(UnavailableReason::Restarting),
        "规格要求 reason 为 restarting"
    );

    // ==================== 6) 崩溃后 call_tool：立即失败不排队 ====================
    println!("\n[step5] ========== 6) 崩溃后 call_tool（立即失败，不排队）==========");
    let t0 = Instant::now();
    let r = registry
        .call_tool(
            &caller,
            "com.example.sample-community.hello",
            json!({"name": "x"}),
        )
        .await;
    let elapsed = t0.elapsed();
    let code = code_of(&r.structured_content);
    println!(
        "[step5] call after crash -> code={code} 耗时={}ms",
        elapsed.as_millis()
    );
    assert_eq!(
        code, "ProcessRestarting",
        "重启期间应立即返回 ProcessRestarting"
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "重启期间调用必须立即返回，不能排队，实际 {elapsed:?}"
    );

    // ==================== 7) 等待自动重启 ====================
    println!("\n[step5] ========== 7) 等待自动重启（指数退避）==========");
    let mut recovered = false;
    let mut waited = Duration::ZERO;
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += Duration::from_millis(50);
        let list = registry.list_tools(&caller, &ToolFilter::all());
        if let Some(t) = list
            .iter()
            .find(|t| t.name == "com.example.sample-community.hello")
        {
            if t.available {
                recovered = true;
                break;
            }
        }
    }
    println!(
        "[step5] 等待 {}ms 后恢复可用 = {recovered}",
        waited.as_millis()
    );
    assert!(recovered, "重启后工具应恢复可用");

    // 代数必须变了（证明是重建而不是原地复活）
    let new_unit = scheduler
        .list_units()
        .into_iter()
        .find(|u| u.plugin_id == sample_community::PLUGIN_ID)
        .expect("重启后应有单元");
    println!(
        "[step5] 单元代数：{} -> {}",
        first_generation, new_unit.generation
    );
    assert_ne!(
        first_generation, new_unit.generation,
        "重启必须换新代数（证明确实重建了单元）"
    );
    println!(
        "[step5] 重启统计：total_restarts={} total_given_up={}",
        restarts.total_restarts(),
        restarts.total_given_up()
    );
    assert!(restarts.total_restarts() >= 1);

    // ==================== 8) 恢复后调用成功 ====================
    println!("\n[step5] ========== 8) 恢复后调用成功 ==========");
    let r = registry
        .call_tool(
            &caller,
            "com.example.sample-community.hello",
            json!({"name": "回来了"}),
        )
        .await;
    assert!(!r.is_error, "重启后应能正常调用");
    let sc = r.structured_content.unwrap();
    println!("[step5] hello(after restart) -> {}", sc["greeting"]);

    // ==================== 9) 经 MCP 观察最终状态 ====================
    println!("\n[step5] ========== 9) MCP tools/list 最终状态 ==========");
    let adapter = McpAdapter::new(registry.clone());
    let identity = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let resp = adapter
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
    let payload = resp.structured_content.unwrap();
    for t in payload["tools"].as_array().unwrap() {
        println!(
            "[step5] MCP: name={} trustLevel={} available={}",
            t["name"], t["trustLevel"], t["available"]
        );
        assert_eq!(t["trustLevel"], "Sandboxed", "必须始终显示沙箱");
        assert_eq!(t["available"], true, "恢复后应可用");
    }

    // ==================== 清理 ====================
    let _ = std::fs::remove_dir_all(&data_dir);
    println!("\n[step5] ========== 演示完成：全部 9 步通过 ==========");
}
