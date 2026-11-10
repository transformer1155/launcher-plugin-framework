//! `official-instance`：实例管理（可信插件）。
//!
//! # 信任级别
//!
//! `plugin.json` 声明 `trust: "trusted"`、`review: "official"`。
//! **实际信任级别由框架的签名校验决定** —— 本 crate 无法自封可信。
//! 若签名校验不通过，loader 会自动降级到沙箱并提示用户（这是框架行为，插件无从干预）。
//!
//! # 工具
//!
//! | 短名 | 说明 | destructive |
//! |---|---|---|
//! | `list_instances` | 列出全部实例 | 否 |
//!
//! 工具短名必须与 `plugin.json` 的 `tools` **完全一致**，
//! 由 `ToolSet::manifest_tools()` + `manifest.check_tools()` 双向校验。

#![deny(unsafe_code)]

pub mod store;
pub mod tools;

use launcher_plugin_common::{PluginContext, ToolSet};

/// 插件 ID。**必须与 `plugin.json` 的 `id` 一致**（此处再写一份是为了让代码可自检）。
pub const PLUGIN_ID: &str = "com.example.official-instance";

/// 构造该插件的全部工具。
///
/// 加载器拿到这个 `ToolSet`，再用 `into_metas(ctx, trust, unit, ..)` 生成 `ToolMeta`。
/// 注意这里**不**决定 trust —— 那是验签的结论。
pub fn tools() -> ToolSet {
    ToolSet::new(
        PLUGIN_ID,
        vec![
            tools::list_instances::def(),
            tools::create_instance::def(),
            tools::switch_instance::def(),
            tools::delete_instance::def(),
        ],
    )
}

/// 构造上下文并返回 (ctx, ToolSet)，供宿主装配。
///
/// `data_dir` 由宿主指定（通常是 `<启动器数据目录>/plugins/<plugin_id>/`）。
pub fn build(data_dir: impl Into<std::path::PathBuf>) -> (PluginContext, ToolSet) {
    build_with_logger(
        data_dir,
        std::sync::Arc::new(launcher_plugin_common::StderrLogger),
    )
}

/// 可指定日志实现（测试用）。与其它三个插件保持同一签名。
pub fn build_with_logger(
    data_dir: impl Into<std::path::PathBuf>,
    logger: std::sync::Arc<dyn launcher_plugin_common::PluginLogger>,
) -> (PluginContext, ToolSet) {
    let ctx = PluginContext::new(PLUGIN_ID, data_dir, logger);
    (ctx, tools())
}

/// 本插件在清单里声明的工具短名（供一致性校验比对）。
pub fn declared_tools() -> Vec<&'static str> {
    tools().manifest_tools()
}
