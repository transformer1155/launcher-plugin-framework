//! 真实网络集成测试：`official-mod.search_mods` 通过真实的 TLS 打 Modrinth。
//!
//! # 这是整条链路第一次真正联网
//!
//! 之前的测试全都用 mock `HttpPort`。这个文件把真实链路打通：
//!
//! ```text
//! search_mods::handle
//!   → ctx.http()                      插件共享层
//!   → AllowListedHttp                 白名单强制（插件侧定义的包装器）
//!   → host_http::HttpsPort            真实 TLS（rustls + rustls-rustcrypto）
//!   → api.modrinth.com                真实服务器
//! ```
//!
//! **默认全部 `#[ignore]`** —— CI 无网络时不应失败。手动运行：
//! `cargo test -p official-mod --test real_network -- --ignored --nocapture`

use std::sync::Arc;

use launcher_plugin_common::{NetworkDecl, NullLogger, PluginContext};
use launcher_plugin_framework::types::ToolResult;
use official_mod::tools::search_mods;

fn manifest_network_decl() -> NetworkDecl {
    // 从真实清单读，确保测的是**插件声明的白名单**而不是测试里另写一份。
    let raw = include_bytes!("../plugin.json");
    let m = launcher_plugin_common::PluginManifestV1::parse(raw).expect("plugin.json 合法");
    m.network.expect("official-mod 必须声明 network")
}

fn as_result(r: launcher_plugin_common::PluginResult) -> ToolResult {
    launcher_plugin_common::result::into_framework_result(r)
}

fn args(query: &str, loader: &str, ver: &str, limit: Option<u64>) -> search_mods::SearchModsArgs {
    search_mods::SearchModsArgs {
        query: query.to_owned(),
        loader: loader.to_owned(),
        game_version: ver.to_owned(),
        limit,
    }
}

/// 用真实主机 HTTP 装配 ctx。
fn real_ctx(tag: &str) -> (PluginContext, Arc<launcher_plugin_common::CapturingLogger>) {
    let dir = launcher_plugin_common::test_support::temp_path(tag);
    let logger = Arc::new(launcher_plugin_common::CapturingLogger::default());
    let ctx = PluginContext::new("com.example.official-mod", dir, logger.clone());
    // 真实传输层 + 白名单（白名单取自真实 plugin.json）
    let port = host_http::allowlisted(&manifest_network_decl());
    (ctx.with_http(port), logger)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实网络；用 --ignored 手动运行"]
async fn real_search_returns_actual_mods() {
    let (ctx, logger) = real_ctx("search");
    let r =
        as_result(search_mods::handle(ctx.clone(), args("jei", "fabric", "1.20.1", Some(5))).await);

    assert!(
        !r.is_error,
        "真实搜索应当成功；失败内容：{:?}",
        r.structured_content
    );
    let sc = r.structured_content.expect("必须有 structuredContent");
    let results = sc["results"].as_array().expect("results 必须是数组");

    println!("[real-net] 搜索 'jei' 命中 {} 条", results.len());
    for item in results.iter().take(3) {
        println!(
            "[real-net]   - {} | {} | {}",
            item["name"].as_str().unwrap_or("?"),
            item["project_id"].as_str().unwrap_or("?"),
            item["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>()
        );
    }

    assert!(!results.is_empty(), "应当至少命中一条");
    // 结构完整性：每条必须三字段齐全
    for item in results {
        assert!(
            item["project_id"].as_str().is_some_and(|s| !s.is_empty()),
            "project_id 不能为空"
        );
        assert!(
            item["name"].as_str().is_some_and(|s| !s.is_empty()),
            "name 不能为空"
        );
        assert!(item["description"].is_string(), "description 必须是字符串");
    }
    assert_eq!(sc["source"], "modrinth");

    // 日志里不应出现用户查询原文（隐私）
    let entries = logger.take();
    for (_, msg) in &entries {
        assert!(!msg.contains("jei"), "日志不应包含查询原文：{msg}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实网络；用 --ignored 手动运行"]
async fn real_search_respects_limit() {
    let (ctx, _l) = real_ctx("limit");
    let r = as_result(search_mods::handle(ctx, args("sodium", "fabric", "1.20.1", Some(2))).await);
    let sc = r.structured_content.unwrap();
    let results = sc["results"].as_array().unwrap();
    assert!(
        results.len() <= 2,
        "limit=2 时不应返回 {} 条",
        results.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实网络；用 --ignored 手动运行"]
async fn real_search_vanilla_loader_works() {
    // vanilla 走"只用 versions facet"的分支，不能崩。
    let (ctx, _l) = real_ctx("vanilla");
    let r =
        as_result(search_mods::handle(ctx, args("optifine", "vanilla", "1.20.1", Some(3))).await);
    assert!(
        !r.is_error,
        "vanilla 搜索不应失败：{:?}",
        r.structured_content
    );
    assert!(
        r.structured_content.unwrap()["results"].is_array(),
        "results 必须是数组（可以为空）"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实网络；用 --ignored 手动运行"]
async fn real_network_blocked_by_allowlist() {
    // 核心安全断言：用**真实传输层**但白名单故意写错域名，
    // 请求必须发不出去（不是"发了被拒"）。
    let dir = launcher_plugin_common::test_support::temp_path("blocked");
    let ctx = PluginContext::new("com.example.official-mod", dir, Arc::new(NullLogger));
    // 白名单里只有一个无关域名
    let wrong_decl = NetworkDecl {
        allow_hosts: vec!["example.invalid".to_owned()],
        reason: "intentionally wrong for test".to_owned(),
    };
    let port = host_http::allowlisted(&wrong_decl);
    let ctx = ctx.with_http(port);

    let r = as_result(search_mods::handle(ctx, args("jei", "fabric", "1.20.1", None)).await);
    assert!(r.is_error, "白名单外必须失败");
    let code = r
        .structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "upstream_error", "应当是 upstream_error 业务错误");
    // 错误信息里应说明是白名单拦的
    let msg = r
        .structured_content
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(msg.contains("白名单"), "错误应说明白名单拦截：{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实网络；用 --ignored 手动运行"]
async fn real_search_against_real_instance_flow() {
    // 完整业务流：先查实例存在性（跨插件），再联网搜索。
    // 这条同时验证了"跨插件调用 + 真实网络"能在同一个 ctx 上共存。
    let inst_dir = launcher_plugin_common::test_support::temp_path("fullflow-inst");
    let mod_dir = launcher_plugin_common::test_support::temp_path("fullflow-mod");

    // 造一个实例插件 + 注册表
    let scheduler: Arc<dyn launcher_plugin_framework::scheduler::Scheduler> =
        Arc::new(launcher_plugin_framework::scheduler::TokioScheduler::new());
    let registry = Arc::new(launcher_plugin_framework::registry::ToolRegistryImpl::new(
        scheduler,
    ));

    let mut store = official_instance::store::Store::default();
    store.instances.push(official_instance::store::Instance {
        id: "inst-real".to_owned(),
        name: "真实实例".to_owned(),
        game_version: "1.20.1".to_owned(),
        loader: official_instance::store::Loader::Fabric,
        last_played: None,
    });
    official_instance::store::InstanceStore::new(&inst_dir)
        .save(&store)
        .unwrap();

    let (inst_ctx, inst_ts) = official_instance::build_with_logger(&inst_dir, Arc::new(NullLogger));
    for m in inst_ts.into_metas(
        &inst_ctx,
        launcher_plugin_framework::types::TrustLevel::Trusted,
        None,
        vec![launcher_plugin_framework::types::PluginId::new("*")],
    ) {
        registry.register_tool(m).unwrap();
    }

    // mod 插件 ctx：同时注入注册表(Weak) + 真实网络
    use launcher_plugin_framework::registry::ToolRegistry;
    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(registry.clone() as Arc<dyn ToolRegistry>));
    let logger = Arc::new(launcher_plugin_common::CapturingLogger::default());
    let (ctx, _ts) = official_mod::build_with_logger(&mod_dir, logger.clone());
    let ctx = ctx
        .with_registry(weak)
        .with_http(host_http::allowlisted(&manifest_network_decl()));

    // 1) 搜索（真实网络）
    let r = as_result(
        search_mods::handle(ctx.clone(), args("fabric api", "fabric", "1.20.1", Some(3))).await,
    );
    assert!(!r.is_error, "真实搜索应成功：{:?}", r.structured_content);
    println!(
        "[real-net] 完整流程：搜索命中 {} 条",
        r.structured_content.unwrap()["results"]
            .as_array()
            .unwrap()
            .len()
    );

    // 2) 安装到真实存在的实例（会走跨插件校验）
    let r = as_result(
        official_mod::tools::install_mod::handle(
            ctx.clone(),
            official_mod::tools::install_mod::InstallModArgs {
                instance_id: "inst-real".to_owned(),
                project_id: "P7dR8mSH".to_owned(),
                filename: None,
            },
        )
        .await,
    );
    assert!(!r.is_error, "对存在的实例安装应成功");
    assert!(mod_dir
        .join("instances/inst-real/mods/P7dR8mSH.jar")
        .exists());

    // 3) 安装到不存在的实例（应被跨插件校验拒绝）
    let r = as_result(
        official_mod::tools::install_mod::handle(
            ctx,
            official_mod::tools::install_mod::InstallModArgs {
                instance_id: "inst-ghost".to_owned(),
                project_id: "P9".to_owned(),
                filename: None,
            },
        )
        .await,
    );
    assert!(r.is_error, "不存在的实例必须被拒");
    println!("[real-net] 完整流程：跨插件校验正确拒绝了不存在的实例");

    let _ = std::fs::remove_dir_all(&inst_dir);
    let _ = std::fs::remove_dir_all(&mod_dir);
}
