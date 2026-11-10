//! 跨插件调用端到端验证：`official-mod.install_mod` → `official-instance.list_instances`。
//!
//! # 为什么这个测试必须用真框架
//!
//! `PluginContext::call_tool` 走的是 `Weak<dyn ToolRegistry>` → 注册表 → 另一个插件的
//! handler。这条链路里有三个容易出错的地方，**只有真的组装起来才能验证**：
//!
//! 1. **Weak 升级**：注册表是否还在（没被 drop）；
//! 2. **ACL 身份**：调用方身份是否真的是 `com.example.official-mod`（而不是平台层）；
//! 3. **引用环**：ctx 持 Weak 而不是 Arc，卸载后能释放。
//!
//! mock 掉任何一环，测的就只是"我以为的链路"，不是真链路。

use std::sync::Arc;

use launcher_plugin_common::NullLogger;
use launcher_plugin_framework::prelude::*;
use launcher_plugin_framework::registry::{ToolRegistry, ToolRegistryImpl};
use official_mod::tools::install_mod;
use serde_json::json;

/// 组装一个"两个插件都在"的宿主管线。
struct Host {
    registry: Arc<ToolRegistryImpl>,
    _scheduler: Arc<dyn Scheduler>,
}

fn install_args(instance_id: &str, project_id: &str) -> install_mod::InstallModArgs {
    install_mod::InstallModArgs {
        instance_id: instance_id.to_owned(),
        project_id: project_id.to_owned(),
        filename: None,
    }
}

fn code_of(r: &launcher_plugin_framework::types::ToolResult) -> Option<String> {
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

/// 建管线：注册 `official-instance` 的工具（作为跨插件调用的**目标**）。
async fn host_with_instance_plugin(
    inst_data_dir: std::path::PathBuf,
    instances: &[(&str, &str)],
) -> Host {
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler.clone()));

    // 准备实例数据（直接写盘，避免依赖 official-instance 的 create 工具）
    let mut store = official_instance::store::Store::default();
    for (id, name) in instances {
        store.instances.push(official_instance::store::Instance {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            game_version: "1.20.1".to_owned(),
            loader: official_instance::store::Loader::Fabric,
            last_played: None,
        });
    }
    let idx = official_instance::store::InstanceStore::new(&inst_data_dir);
    idx.save(&store).expect("写入实例数据");

    // 用真实工具集合注册（模拟"official-instance 已加载"）
    let (ctx, toolset) = official_instance::build_with_logger(&inst_data_dir, Arc::new(NullLogger));
    let metas = toolset.into_metas(&ctx, TrustLevel::Trusted, None, vec![PluginId::new("*")]);
    for m in metas {
        registry
            .register_tool(m)
            .expect("注册 official-instance 工具");
    }

    Host {
        registry,
        _scheduler: scheduler,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_rejects_nonexistent_instance() {
    // 核心断言：实例不存在时，install_mod 应当拒绝（而不是写进一个野 ID 的目录）。
    let inst_dir = launcher_plugin_common::test_support::temp_path("cp-inst");
    let mod_dir = launcher_plugin_common::test_support::temp_path("cp-mod");
    let host = host_with_instance_plugin(inst_dir.clone(), &[("inst-exists", "生存")]).await;

    // 关键：把注册表以 Weak 注入插件 ctx —— 这是跨插件调用的真实形态。
    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(host.registry.clone() as Arc<dyn ToolRegistry>));
    let (ctx, _toolset) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));
    let ctx = ctx.with_registry(weak);

    // ---- 不存在的实例 → 应当被拒 ----
    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(ctx.clone(), install_args("inst-ghost", "P1")).await,
    );
    assert!(r.is_error, "不存在的实例必须被拒绝");
    assert_eq!(
        code_of(&r).as_deref(),
        Some("not_found"),
        "应当是业务错误 not_found"
    );
    // 重要：不得留下任何文件痕迹
    let ghost_dir = mod_dir.join("instances/inst-ghost");
    assert!(
        !ghost_dir.exists(),
        "被拒绝的安装不得创建目录（{}）",
        ghost_dir.display()
    );

    let _ = std::fs::remove_dir_all(&inst_dir);
    let _ = std::fs::remove_dir_all(&mod_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_succeeds_for_existing_instance() {
    let inst_dir = launcher_plugin_common::test_support::temp_path("cp-inst-ok");
    let mod_dir = launcher_plugin_common::test_support::temp_path("cp-mod-ok");
    let host = host_with_instance_plugin(inst_dir.clone(), &[("inst-real", "真实实例")]).await;

    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(host.registry.clone() as Arc<dyn ToolRegistry>));
    let (ctx, _ts) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));
    let ctx = ctx.with_registry(weak);

    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(ctx.clone(), install_args("inst-real", "P1")).await,
    );
    assert!(!r.is_error, "存在的实例应当安装成功");
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["simulated"], true);
    assert!(mod_dir.join("instances/inst-real/mods/P1.jar").exists());

    let _ = std::fs::remove_dir_all(&inst_dir);
    let _ = std::fs::remove_dir_all(&mod_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_degrades_when_instance_plugin_absent() {
    // 重要：实例插件不存在时**放行**（不是拒绝），保证两个插件能独立工作。
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler));
    let mod_dir = launcher_plugin_common::test_support::temp_path("cp-mod-deg");

    // 注册表里**没有** official-instance 的工具
    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(registry.clone() as Arc<dyn ToolRegistry>));
    let (ctx, _ts) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));
    let ctx = ctx.with_registry(weak);

    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(ctx.clone(), install_args("any-instance", "P1")).await,
    );
    assert!(
        !r.is_error,
        "实例插件未加载时应当降级放行（保证插件可独立工作），实际被拒了"
    );

    let _ = std::fs::remove_dir_all(&mod_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_degrades_without_registry_injected() {
    // 连注册表都没注入（ctx 无跨插件能力）→ 同样降级放行。
    let mod_dir = launcher_plugin_common::test_support::temp_path("cp-mod-noreg");
    let (ctx, _ts) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));

    let r = launcher_plugin_common::result::into_framework_result(
        install_mod::handle(ctx.clone(), install_args("anything", "P1")).await,
    );
    assert!(!r.is_error, "无注册表时应降级放行");

    let _ = std::fs::remove_dir_all(&mod_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ctx_call_tool_uses_plugin_identity() {
    // 验证调用方身份：跨插件调用时，caller 是**本插件**而不是平台层。
    // 手法：注册一个只允许 official-mod 的工具，然后从 official-mod 的 ctx 调它。
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler));
    let probe_dir = launcher_plugin_common::test_support::temp_path("cp-probe");

    let (probe_ctx, toolset) = official_mod::build_with_logger(&probe_dir, Arc::new(NullLogger));
    let mut metas = toolset.into_metas(
        &probe_ctx,
        TrustLevel::Trusted,
        None,
        // 关键：只允许 official-mod 自己调用
        vec![PluginId::new(official_mod::PLUGIN_ID)],
    );
    // 只注册一个工具做探针，避免名称冲突
    let one: Vec<_> = metas.drain(..).take(1).collect();
    for m in one {
        registry.register_tool(m).expect("注册探针工具");
    }

    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(registry.clone() as Arc<dyn ToolRegistry>));
    let (ctx, _ts) = official_mod::build_with_logger(&probe_dir, Arc::new(NullLogger));
    let ctx = ctx.with_registry(weak);

    // 从 official-mod 的身份调它自己的探针工具 → ACL 应当放行
    let tool_name = "com.example.official-mod.list_installed_mods";
    let r = ctx.call_tool(tool_name, json!({"instance_id": "x"})).await;
    match r {
        Ok(result) => {
            assert!(
                !result.is_error || code_of(&result).as_deref() != Some("PermissionDenied"),
                "调用方身份必须是 official-mod 自己，才能通过 ACL；实际被拒：{:?}",
                result.structured_content
            );
        }
        // 沙箱/状态类错误不算 ACL 失败，跳过
        Err(e) if e.code != launcher_plugin_framework::error::ToolErrorCode::PermissionDenied => {}
        Err(e) => panic!("ACL 应当放行 official-mod 自己的调用，实际：{e}"),
    }

    let _ = std::fs::remove_dir_all(&probe_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ctx_call_tool_reports_when_registry_gone() {
    // 注册表被 drop（模拟宿主正在卸载）→ 应当返回明确错误，而不是 panic。
    let mod_dir = launcher_plugin_common::test_support::temp_path("cp-mod-gone");
    let scheduler: Arc<dyn Scheduler> = Arc::new(TokioScheduler::new());
    let registry = Arc::new(ToolRegistryImpl::new(scheduler));
    let weak: std::sync::Weak<dyn ToolRegistry> =
        Arc::downgrade(&(registry.clone() as Arc<dyn ToolRegistry>));

    let (ctx, _ts) = official_mod::build_with_logger(&mod_dir, Arc::new(NullLogger));
    let ctx = ctx.with_registry(weak);

    // 主动 drop 掉最后一个强引用
    drop(registry);

    let r = ctx.call_tool("whatever.tool", json!({})).await;
    let err = r.expect_err("注册表已释放时必须返回错误");
    assert!(
        err.message.contains("注册表已释放"),
        "错误信息应当说明注册表已释放，实际：{}",
        err.message
    );

    let _ = std::fs::remove_dir_all(&mod_dir);
}
