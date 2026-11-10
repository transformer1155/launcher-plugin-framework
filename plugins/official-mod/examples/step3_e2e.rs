//! Step 3 端到端验证：`official-mod` 经完整框架链路（loader → registry → MCP）。
//!
//! 演示四件事：
//!
//! 1. 插件清单的 `network.allow_hosts` 与代码声明一致；
//! 2. `search_mods` 在**没有**宿主 HTTP 时返回业务错误 `upstream_error`（方案 a）；
//! 3. 注入一个 mock HTTP 后搜索成功，且白名单强制生效（非白名单域名请求发不出去）；
//! 4. `install_mod` 不联网，写占位文件并标 `simulated`。
//!
//! 运行：在 `plugins/` 目录下 `cargo run --example step3_e2e`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use launcher_plugin_common::{AllowListedHttp, HttpPort, HttpReply, NetworkDecl, PluginContext};
use launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::signature::PluginPackage;
use serde_json::json;

/// 经 MCP 调一个工具（避免在 main 里写重复的闭包）。
async fn mcp_call(
    adapter: &McpAdapter<ToolRegistryImpl>,
    name: &str,
    args: serde_json::Value,
) -> launcher_plugin_framework::mcp::ToolsCallResponse {
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

/// 假的 Modrinth 响应。
struct MockModrinth {
    calls: Arc<AtomicUsize>,
}

impl HttpPort for MockModrinth {
    fn get(
        &self,
        _url: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HttpReply, String>> + Send>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(HttpReply {
                status: 200,
                body: br#"{"hits":[
                    {"project_id":"P1","title":"JEI","description":"recipe viewer"},
                    {"project_id":"P2","title":"Sodium","description":"faster renderer"}
                ]}"#
                .to_vec(),
            })
        })
    }
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

    // ---------- 清单一致性 + 网络声明 ----------
    let manifest = launcher_plugin_common::PluginManifestV1::parse(include_bytes!(
        "../../official-mod/plugin.json"
    ))
    .expect("插件清单应当合法");

    let set = official_mod::tools();
    manifest
        .check_tools(&set.manifest_tools())
        .expect("plugin.json 的 tools 必须与代码注册一致");
    println!("[step3] 清单一致性通过：tools = {:?}", manifest.tools);

    let net = manifest.network.as_ref().expect("必须声明 network");
    println!(
        "[step3] 网络声明：allow_hosts = {:?}，原因：{}",
        net.allow_hosts, net.reason
    );
    assert_eq!(
        net.allow_hosts,
        official_mod::REQUIRED_HOSTS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "代码与清单的域名白名单必须一致"
    );

    // ---------- 加载插件（无网络能力）----------
    let data_dir = std::env::temp_dir().join(format!("lpf-step3-{}", std::process::id()));
    let (ctx, toolset) = official_mod::build(&data_dir);

    let manifest_json = r#"{
        "id": "com.example.official-mod",
        "name": "Mod 管理",
        "version": "0.1.0",
        "description": "列出、搜索、安装、移除 Mod",
        "tools": ["list_installed_mods", "search_mods", "install_mod", "remove_mod"]
    }"#;
    let pkg = package_for("com.example.official-mod", manifest_json);

    let pid = PluginId::new(official_mod::PLUGIN_ID);
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
        .expect("加载应当成功（验签失败降级而非拒绝）");

    println!(
        "[step3] 加载完成：plugin={} trust={} 工具数={}",
        loaded.plugin_id, loaded.trust_level, loaded.registered_tools
    );

    let adapter = McpAdapter::new(registry.clone());

    // ---------- 1) 无网络：search_mods 应返回业务错误 upstream_error ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-mod.search_mods",
        json!({"query": "jei", "loader": "fabric", "game_version": "1.20.1"}),
    )
    .await;
    let code = r
        .structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    println!(
        "[step3] 无网络 search_mods -> isError={} code={code}",
        r.is_error
    );
    assert!(r.is_error);
    assert_eq!(
        code, "upstream_error",
        "按裁决 (a)：应返回业务错误 upstream_error"
    );

    // ---------- 2) 注入 mock HTTP + 白名单强制 ----------
    // 注意：这里要**重建**插件上下文才能注入 http（ctx 是克隆进去的）。
    // 真实宿主应当在加载前就装配好，这里只为演示两种状态。
    let calls = Arc::new(AtomicUsize::new(0));
    let decl = NetworkDecl {
        allow_hosts: net.allow_hosts.clone(),
        reason: net.reason.clone(),
    };
    let gated = Arc::new(AllowListedHttp::new(
        Arc::new(MockModrinth {
            calls: Arc::clone(&calls),
        }),
        &decl,
    ));

    // 直接调 handler（不经框架），因为已加载的 meta 里捕获的是无 http 的 ctx。
    let ctx_with_http = PluginContext::new(
        official_mod::PLUGIN_ID,
        &data_dir,
        Arc::new(launcher_plugin_common::StderrLogger),
    )
    .with_http(gated);

    let r = launcher_plugin_common::result::into_framework_result(
        official_mod::tools::search_mods::handle(
            ctx_with_http.clone(),
            official_mod::tools::search_mods::SearchModsArgs {
                query: "jei".into(),
                loader: "fabric".into(),
                game_version: "1.20.1".into(),
                limit: None,
            },
        )
        .await,
    );
    assert!(!r.is_error, "有网络时搜索应成功");
    let sc = r.structured_content.unwrap();
    let results = sc["results"].as_array().unwrap();
    println!(
        "[step3] 有网络 search_mods -> {} 条结果，首条 = {}",
        results.len(),
        results[0]["name"]
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "白名单内应当真的发了一次请求"
    );

    // ---------- 3) 白名单强制：换成非白名单域名应发不出去 ----------
    let denied_calls = Arc::new(AtomicUsize::new(0));
    let deny_decl = NetworkDecl {
        allow_hosts: vec!["evil.example".to_owned()], // 故意不含 api.modrinth.com
        reason: "demo".to_owned(),
    };
    let denied = Arc::new(AllowListedHttp::new(
        Arc::new(MockModrinth {
            calls: Arc::clone(&denied_calls),
        }),
        &deny_decl,
    ));
    let ctx_denied = PluginContext::new(
        official_mod::PLUGIN_ID,
        &data_dir,
        Arc::new(launcher_plugin_common::StderrLogger),
    )
    .with_http(denied);
    let r = launcher_plugin_common::result::into_framework_result(
        official_mod::tools::search_mods::handle(
            ctx_denied,
            official_mod::tools::search_mods::SearchModsArgs {
                query: "jei".into(),
                loader: "fabric".into(),
                game_version: "1.20.1".into(),
                limit: None,
            },
        )
        .await,
    );
    assert!(r.is_error, "白名单外必须失败");
    assert_eq!(
        denied_calls.load(Ordering::SeqCst),
        0,
        "关键：被白名单拦下的请求绝不能到达传输层"
    );
    println!("[step3] 白名单强制生效：非白名单域名请求未发出（transport calls=0）");

    // ---------- 4) install_mod：不联网，写占位文件 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-mod.install_mod",
        json!({"instance_id": "inst-001", "project_id": "P1"}),
    )
    .await;
    assert!(!r.is_error, "模拟安装应成功");
    let sc = r.structured_content.as_ref().unwrap();
    println!(
        "[step3] install_mod -> simulated={} filename={}",
        sc["simulated"], sc["filename"]
    );
    assert_eq!(sc["simulated"], true, "必须显式标注 simulated");

    let placeholder = data_dir.join("instances/inst-001/mods/P1.jar");
    assert!(placeholder.exists(), "占位文件应当落盘");
    let content = std::fs::read_to_string(&placeholder).unwrap();
    assert!(content.contains("simulated"), "占位内容必须明确标注模拟");
    println!("[step3] 占位文件已写入：{}", placeholder.display());

    // ---------- 5) list_installed_mods 能看到它 ----------
    let r = mcp_call(
        &adapter,
        "com.example.official-mod.list_installed_mods",
        json!({"instance_id": "inst-001"}),
    )
    .await;
    let sc = r.structured_content.as_ref().unwrap();
    let mods = sc["mods"].as_array().unwrap();
    println!("[step3] list_installed_mods -> {} 个 Mod", mods.len());
    assert_eq!(mods.len(), 1);

    // ---------- 清理 ----------
    let _ = std::fs::remove_dir_all(&data_dir);
    println!("[step3] 端到端验证通过");
}
