//! `list_instances`：列出全部游戏实例。
//!
//! 只读工具：不写盘、不创建目录，因此 `data_dir` 尚未存在时也能正常返回空列表。
//! 这条设计让"全新安装后第一次打开实例页"不会报错。

use crate::load_store;
use launcher_plugin_common::{tool_args, tool_def};
use serde_json::json;

tool_args! {
    /// `list_instances` 没有参数。空结构体仍生成合法的 object schema。
    pub struct ListInstancesArgs {}
}

/// 列出实例。按 `last_played` 降序（最近玩过的在前），未玩过的排最后且按名称升序。
pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    _args: ListInstancesArgs,
) -> launcher_plugin_common::PluginResult {
    let store = load_store!(ctx);
    let mut items: Vec<_> = store.instances.clone();

    // 排序规则：最近玩过的优先；都没玩过则按名称。UI 默认顺序应当稳定。
    //
    // 注意 `sort_by(a, b)` 的契约：返回 `Less` 表示 a 排在 b 前面。
    // 这里刻意不交换比较对象，按"a 相对于 b 应该排哪"直写，避免把两个分支写反。
    items.sort_by(|a, b| match (&a.last_played, &b.last_played) {
        // 都玩过：时间戳倒序（新的在前）。RFC3339 UTC 定长格式可直接字典序比较。
        (Some(ap), Some(bp)) => bp.cmp(ap),
        // a 玩过、b 没玩过 → a 在前
        (Some(_), None) => std::cmp::Ordering::Less,
        // a 没玩过、b 玩过 → a 在后
        (None, Some(_)) => std::cmp::Ordering::Greater,
        // 都没玩过：按名称升序
        (None, None) => a.name.cmp(&b.name),
    });

    ctx.log_info(format!("列出 {} 个实例", items.len()));
    Ok(launcher_plugin_common::result::success(json!({
        "instances": items.iter().map(|i| json!({
            "id": i.id,
            "name": i.name,
            "game_version": i.game_version,
            "loader": i.loader.as_str(),
            "last_played": i.last_played,
        })).collect::<Vec<_>>()
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "list_instances",
        description: "列出全部游戏实例（按最近游玩时间降序）",
        destructive: false,
        timeout_ms: 5_000,
        args: ListInstancesArgs,
        handler: crate::tools::list_instances::handle
    }
}
