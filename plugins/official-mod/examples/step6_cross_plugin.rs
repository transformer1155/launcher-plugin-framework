//! 跨插件调用演示：`official-mod.install_mod` 真的会去查 `official-instance`。
//!
//! # 这条链路此前从未验证过
//!
//! 在加入本演示之前，`PluginContext::call_tool` 只有单元测试，
//! 四个插件的工具**没有一个真正调用别的插件**。这是最可能藏 bug 的地方：
//!
//! | 环节 | 容易出错的地方 |
//! |---|---|
//! | `Weak<dyn ToolRegistry>` 升级 | 注册表是否还活着 |
//! | caller 身份 | 是否真的是 `com.example.official-mod`（而不是平台层） |
//! | ACL | 目标工具的 `allowed_callers` 是否真的接受这个身份 |
//! | 错误分流 | "实例插件没装" vs "实例不存在" 必须区别对待 |
//!
//! # 演示的三条路径
//!
//! 1. **实例存在** → 安装成功
//! 2. **实例不存在** → 拒绝，且**不留下任何文件**
//! 3. **实例插件未加载** → 降级放行（保证插件可独立工作）
//!
//! 运行：`cargo run --example step6_cross_plugin`

use std::sync::Arc;

use launcher_plugin_common::NullLogger;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::{ToolRegistry, ToolRegistryImpl};
use official_mod::tools::install_mod;
use serde_json::json;

fn code_of(v: &Option<serde_json::Value>) -> String {
    v.as_ref()
        .and_then(|x| x.get("code"))
        .and_then(|x| x.as_str())
        .unwrap_or("<success>")
        .to_owned()
}

fn install_args(id: &str) -> install_mod::InstallModArgs {
    install_mod::InstallModArgs {
        instance_id: id.to_owned(),
        project_id: "P1".to_owned(),
        filename: None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let base = std::env::temp_dir().join(format!("lpf-step6-{}", std::process::id()));
    let inst_dir = base.join("instance-data");
    let mod_dir = base.join("mod-data");

    // ==================== 0) 组装：两个插件都在 ====================
    println!("[step6] ========== 0) 组装宿主管线（两个插件都已加载）==========");
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));

    // 准备一个真实存在的实例
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
        .expect("写入实例数据");

    // 注册 official-instance 的工具（模拟它已加载）
    let (inst_ctx, inst_toolset) =
        official_instance::build_with_logger(&inst_dir, Arc::new(NullLogger));
    for m in inst_toolset.into_metas(
        &inst_ctx,
        TrustLevel::Trusted,
        None,
        vec![PluginId::new("*")],
    ) {
        registry
            .register_tool(m)
            .expect("注册 official-instance 工具");
    }
    println!(
        "[step6] official-instance 已注册 {} 个工具",
        registry
            .list_tools(&PluginId::new(PluginId::PLATFORM), &ToolFilter::all())
            .len()
    );
    println!("[step6] 已存在实例：inst-real");

    // 构造 official-mod 的 ctx，注入注册表 Weak（跨插件调用的真实形态）
    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(registry.clone() as Arc<dyn ToolRegistry>));
    let (mod_ctx, _mod_ts) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));
    let mod_ctx = mod_ctx.with_registry(weak);

    // ==================== 1) 实例存在 → 安装成功 ====================
    println!("\n[step6] ========== 1) 实例存在 → 安装成功 ==========");
    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(mod_ctx.clone(), install_args("inst-real")).await,
    );
    println!(
        "[step6] install_mod(inst-real) -> isError={} code={}",
        r.is_error,
        code_of(&r.structured_content)
    );
    assert!(!r.is_error, "存在的实例应当安装成功");
    let sc = r.structured_content.unwrap();
    println!(
        "[step6] simulated={} filename={}",
        sc["simulated"], sc["filename"]
    );
    let placeholder = mod_dir.join("instances/inst-real/mods/P1.jar");
    assert!(placeholder.exists(), "占位文件应当落盘");
    println!("[step6] 占位文件已写入：{}", placeholder.display());

    // ==================== 2) 实例不存在 → 拒绝且无痕迹 ====================
    println!("\n[step6] ========== 2) 实例不存在 → 拒绝（且不留文件）==========");
    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(mod_ctx.clone(), install_args("inst-ghost")).await,
    );
    println!(
        "[step6] install_mod(inst-ghost) -> isError={} code={}",
        r.is_error,
        code_of(&r.structured_content)
    );
    assert!(r.is_error, "不存在的实例必须被拒绝");
    assert_eq!(
        code_of(&r.structured_content),
        "not_found",
        "应当是业务错误 not_found"
    );
    let ghost_dir = mod_dir.join("instances/inst-ghost");
    assert!(!ghost_dir.exists(), "关键：被拒绝的安装不得创建任何目录");
    println!("[step6] 已确认：未留下 inst-ghost 目录（无副作用）");

    // ==================== 3) 实例插件未加载 → 降级放行 ====================
    println!("\n[step6] ========== 3) 实例插件未加载 → 降级放行 ==========");
    let bare_scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let bare_registry = Arc::new(ToolRegistryImpl::new(bare_scheduler));
    let bare_weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(bare_registry.clone() as Arc<dyn ToolRegistry>));
    let bare_dir = base.join("bare-mod-data");
    let (bare_ctx, _bts) = official_mod::build_with_logger(&bare_dir, Arc::new(NullLogger));
    let bare_ctx = bare_ctx.with_registry(bare_weak);

    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(bare_ctx, install_args("whatever")).await,
    );
    println!("[step6] install_mod(无实例插件) -> isError={}", r.is_error);
    assert!(
        !r.is_error,
        "实例插件缺席时应当降级放行（保证 official-mod 可独立使用）"
    );
    println!("[step6] 已确认：降级放行，official-mod 可独立工作");

    // ==================== 4) 经 MCP 观察完整链路 ====================
    println!("\n[step6] ========== 4) 经 MCP 观察 ==========");
    let adapter = McpAdapter::new(registry.clone());
    let identity = ClientIdentity {
        from_host_ui: true,
        ..Default::default()
    };
    let resp = adapter
        .handle_value(
            &identity,
            json!({
                "protocolVersion": launcher_plugin_framework::mcp::SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": {"supportsStructuredContent": true},
                "method": "tools/list",
                "params": {}
            }),
        )
        .await;
    let payload = resp.structured_content.unwrap();
    println!(
        "[step6] MCP 可见工具数 = {}",
        payload["tools"].as_array().unwrap().len()
    );
    for t in payload["tools"].as_array().unwrap() {
        let name = t["name"].as_str().unwrap();
        let owner = if name.contains("instance") {
            "instance"
        } else {
            "?"
        };
        println!("[step6]   - {name}（来自 {owner} 插件）");
    }

    let _ = std::fs::remove_dir_all(&base);
    println!("\n[step6] ========== 演示完成：跨插件调用链路已验证 ==========");
}
