//! `install_mod`：把 Mod 安装到实例（**模拟安装**）。
//!
//! # 规格要求：不实现真实下载
//!
//! > 不实现 Mod 实际下载（`install_mod` 写占位文件并返回成功，日志标注 "simulated"）
//!
//! 因此本工具**不发起任何网络请求**。它做的是：
//! 1. 校验参数（实例 ID、project_id、文件名）
//! 2. 生成一个确定性的文件名
//! 3. 在实例的 mods 目录写一个占位 `.jar`
//! 4. 把记录写进 `mods.json`
//!
//! 每次响应里都带 `"simulated": true`，日志里也标 `[simulated]` ——
//! **不能让调用方误以为真的装了一个能用的 Mod**。
//!
//! # destructive 语义
//!
//! 标记为 `destructive: true`：它会改动实例的游戏文件目录。
//! 与 `delete_instance` 不同，这里**不需要** `confirm` 参数 ——
//! 安装是"加东西"，可逆（`remove_mod` 能撤销）；删除才是不可逆的。

use crate::load_mod_store;
use crate::save_mod_store;
use crate::store::{
    ensure_mods_dir, now_rfc3339, placeholder_jar_content, validate_instance_id,
    validate_mod_filename, ModEntry, MAX_MODS_PER_INSTANCE,
};
use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::{biz_fail, tool_args, tool_def};
use launcher_plugin_framework::error::ToolErrorCode;
use serde_json::json;

tool_args! {
    pub struct InstallModArgs {
        instance_id: r_str() = "目标实例 ID",
        project_id: r_str() = "Mod 的项目 ID（来自 search_mods）",
        filename: o_str() = "可选：指定文件名；缺省时按 project_id 生成",
    }
}

/// 由 `project_id` 生成确定的文件名。
///
/// 用 `project_id` 而不是随机数：同一个 Mod 重复安装会得到同一个文件名，
/// 便于"重装即覆盖"，也让人眼能看出文件来自哪个项目。
fn filename_for(project_id: &str) -> String {
    // 只保留安全字符，避免 project_id 里出现奇怪内容时污染文件名。
    let safe: String = project_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "mod".to_owned()
    } else {
        safe
    };
    format!("{safe}.jar")
}

pub async fn handle(
    ctx: launcher_plugin_common::PluginContext,
    args: InstallModArgs,
) -> launcher_plugin_common::PluginResult {
    // ---- 参数校验 ----
    let instance_id = match validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
    };

    let project_id = args.project_id.trim();
    if project_id.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "project_id 不能为空"));
    }

    let filename = match &args.filename {
        Some(f) => match validate_mod_filename(f) {
            Ok(v) => v.to_owned(),
            Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
        },
        None => filename_for(project_id),
    };

    // ---- 跨插件校验：实例是否真实存在（可降级，见 verify_instance_exists）----
    if let Some(err) = verify_instance_exists(&ctx, &instance_id).await {
        return Ok(err);
    }

    // ---- 读索引 ----
    let mut store = load_mod_store!(ctx);
    let mods = store.instances.entry(instance_id.clone()).or_default();

    // 同一实例内文件名唯一：已存在则视为"重装"，覆盖记录而不是追加。
    let already = mods.iter().any(|m| m.filename == filename);

    if mods.len() >= MAX_MODS_PER_INSTANCE && !already {
        biz_fail!(
            biz_codes::LIMIT_EXCEEDED,
            "实例 {instance_id} 的 Mod 数量已达上限 {MAX_MODS_PER_INSTANCE}"
        );
    }

    // ---- 写占位文件（模拟安装）----
    let dir = match ensure_mods_dir(&ctx, &instance_id) {
        Ok(d) => d,
        Err(msg) => return Ok(business(biz_codes::STORAGE_ERROR, msg)),
    };
    let target = dir.join(&filename);
    if let Err(e) = std::fs::write(&target, placeholder_jar_content(project_id)) {
        ctx.log_error(format!("写入占位文件失败：{e}"));
        return Ok(business(
            biz_codes::STORAGE_ERROR,
            format!("写入 Mod 文件失败：{e}"),
        ));
    }

    // ---- 更新索引（重装则替换，否则追加）----
    let entry = ModEntry {
        filename: filename.clone(),
        name: project_id.to_owned(),
        version: "simulated".to_owned(),
        project_id: Some(project_id.to_owned()),
        installed_at: now_rfc3339(),
    };
    match mods.iter_mut().find(|m| m.filename == filename) {
        Some(existing) => *existing = entry,
        None => mods.push(entry),
    }

    save_mod_store!(ctx, &store);

    ctx.log_info(format!(
        "[simulated] 已为实例 {instance_id} 安装 Mod {project_id} -> {filename}（未真实下载）"
    ));

    Ok(launcher_plugin_common::result::success(json!({
        "ok": true,
        // 必须显式标注，否则调用方会以为真的装好了
        "simulated": true,
        "instance_id": instance_id,
        "filename": filename,
        "project_id": project_id,
        "note": "模拟安装：写入的是占位文件，不是真实 Mod；真实下载需宿主提供网络与校验能力",
    })))
}

/// 跨插件校验实例是否存在。
///
/// # 返回语义（三态，这点很重要）
///
/// | 返回 | 含义 | 行为 |
/// |---|---|---|
/// | `None` | 校验通过，**或** 无法校验（降级） | 继续安装 |
/// | `Some(业务错误)` | **确认**实例不存在 | 中止，返回 `not_found` |
///
/// # 为什么"无法校验"要放行，而不是拒绝
///
/// 本插件与 `official-instance` 是**独立**的两个插件，可以单独加载/卸载。
/// 如果"查不到实例插件"就拒绝安装，那么：
///
/// - 用户只装了 `official-mod` 时，它完全不可用（哪怕他知道自己在干什么）；
/// - 两个插件的加载顺序会变成一个隐式依赖（谁先加载谁赢）；
/// - 卸载 `official-instance` 会让 `official-mod` 变成废件。
///
/// 所以只有**明确得知实例不存在**时才拒绝。这个区分靠的是框架错误码：
///
/// - `ToolNotFound`（`official-instance` 没加载）→ 无法校验 → 放行
/// - `PermissionDenied`（ACL 没给）→ 无法校验 → 放行
/// - `ProcessRestarting` / `ProcessCrashed` 等 → 同插件正在重启 → **放行**
///   （不能因为别人在重启就让自己的调用失败，那会把故障放大成雪崩）
/// - 工具调用**成功**且结果里没有这个 ID → 确认不存在 → 拒绝
///
/// 这是一条刻意选择的"宽松失败"路径，已在 `plugins/README.md` 记为已知限制。
async fn verify_instance_exists(
    ctx: &launcher_plugin_common::PluginContext,
    instance_id: &str,
) -> Option<launcher_plugin_framework::types::ToolResult> {
    // 实例插件的工具全名。
    const LIST_TOOL: &str = "com.example.official-instance.list_instances";

    let result = match ctx.call_tool(LIST_TOOL, serde_json::json!({})).await {
        Ok(r) => r,
        Err(mut e) => {
            // 无法校验（未加载 / 无权限 / 对方重启中）→ 降级放行，但记一条 WARN。
            let why = e.message.clone();
            e.message = format!("跨插件实例校验被跳过（{why}）");
            ctx.log_warn(format!(
                "无法通过 official-instance 校验实例 {instance_id} 是否存在（{why}），继续安装"
            ));
            return None;
        }
    };

    // 对方返回了业务错误（例如它自己存储坏了）→ 同样无法确认，放行。
    if result.is_error {
        ctx.log_warn(format!(
            "official-instance.list_instances 返回错误，跳过实例存在性校验（{instance_id}）"
        ));
        return None;
    }

    // 校验成功：检查列表里是否有这个 ID。
    let ids: Vec<String> = result
        .structured_content
        .as_ref()
        .and_then(|v| v.get("instances"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|i| i.get("id").and_then(|x| x.as_str()))
                .map(|s| s.to_owned())
                .collect()
        })
        .unwrap_or_default();

    if ids.iter().any(|id| id == instance_id) {
        ctx.log_info(format!("跨插件校验通过：实例 {instance_id} 存在"));
        return None;
    }

    ctx.log_warn(format!(
        "跨插件校验失败：实例 {instance_id} 不存在（当前共 {} 个实例）",
        ids.len()
    ));
    Some(business(
        biz_codes::NOT_FOUND,
        format!(
            "实例 {instance_id} 不存在，无法安装 Mod。请先通过 official-instance.create_instance 创建它"
        ),
    ))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "install_mod",
        description: "安装 Mod 到实例（当前为模拟安装，写入占位文件）",
        destructive: true,
        timeout_ms: 10_000,
        args: InstallModArgs,
        handler: crate::tools::install_mod::handle
    }
}
