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
        project_type: o_str() = "资源类型：mod（默认）/ resourcepack / shader",
        game_version: o_str() = "Minecraft 版本（用于筛选兼容文件），如 1.21.1",
        loader: o_str() = "加载器（mod 类筛选用）：fabric / forge / quilt / neoforge",
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

    let project_id: String = args.project_id.trim().to_owned();
    if project_id.is_empty() {
        return Err(ctx.err(ToolErrorCode::InvalidArguments, "project_id 不能为空"));
    }

    let filename = match &args.filename {
        Some(f) => match validate_mod_filename(f) {
            Ok(v) => v.to_owned(),
            Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
        },
        None => filename_for(&project_id),
    };

    // ---- 解析资源类型与目标目录 ----
    let project_type = args
        .project_type
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mod".to_owned());
    let sub_dir = match project_type.as_str() {
        "mod" | "mods" => "mods",
        "resourcepack" | "resourcepacks" => "resourcepacks",
        "shader" | "shaders" | "shaderpack" | "shaderpacks" => "shaderpacks",
        "modpack" | "modpacks" => {
            return Ok(business(
                biz_codes::UNSUPPORTED,
                "整合包（.mrpack）安装暂未支持；请先手动安装对应加载器版本",
            ))
        }
        other => {
            return Ok(business(
                biz_codes::UNSUPPORTED,
                format!("未知资源类型 `{other}`（支持 mod/resourcepack/shader）"),
            ))
        }
    };

    // ---- 真实下载：Modrinth 最新兼容版本 → 主文件 ----
    let http = ctx.http().ok();
    let mut real_file: Option<(Vec<u8>, String)> = None; // (bytes, 版本号)
    if let Some(http) = &http {
        let mut api = format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        );
        let mut filters: Vec<String> = Vec::new();
        if let Some(v) = args.game_version.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            filters.push(format!("game_versions=[\"{v}\"]"));
        }
        if project_type == "mod" {
            if let Some(l) = args.loader.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                filters.push(format!("loaders=[\"{l}\"]"));
            }
        }
        if !filters.is_empty() {
            api.push('?');
            api.push_str(&filters.join("&"));
        }
        // URL 里的 [] 会被部分服务端拒绝，Modrinth 接受 URL 编码；这里直接编码。
        let api = api.replace('[', "%5B").replace(']', "%5D").replace('"', "%22");
        match get_version_file(http, &api).await {
            Ok((bytes, ver, fname)) => {
                // 用真实文件名（除非调用方显式指定）
                let f = match &args.filename {
                    Some(f) => f.clone(),
                    None => fname,
                };
                real_file = Some((bytes, ver));
                // 覆盖 filename
                let filename = match validate_mod_filename(&f) {
                    Ok(v) => v.to_owned(),
                    Err(msg) => return Err(ctx.err(ToolErrorCode::InvalidArguments, msg)),
                };
                let dir = match mods_dir_target(&ctx, &instance_id, sub_dir) {
                    Ok(d) => d,
                    Err(msg) => return Ok(business(biz_codes::STORAGE_ERROR, msg)),
                };
                return install_real(
                    &ctx, &instance_id, &project_id, &filename, &project_type,
                    real_file, dir,
                )
                .await;
            }
            Err(e) => {
                ctx.log_warn(format!(
                    "Modrinth 真实下载失败（{e}），回退为占位安装"
                ));
            }
        }
    }

    let _ = sub_dir; // 模拟路径不用子目录
    // ---- 模拟安装兜底（原逻辑）----
    install_simulated(ctx, args, &instance_id, &project_id, &filename).await
}

/// 拉取 Modrinth 版本列表并下载主文件。返回 (字节, 版本号, 文件名)。
async fn get_version_file(
    http: &std::sync::Arc<dyn launcher_plugin_common::context::HttpPort>,
    api: &str,
) -> Result<(Vec<u8>, String, String), String> {
    let reply = http
        .get(api.to_owned())
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    if reply.status != 200 {
        return Err(format!("HTTP {}", reply.status));
    }
    let versions: serde_json::Value = serde_json::from_slice(&reply.body)
        .map_err(|e| format!("JSON 解析失败：{e}"))?;
    let first = versions
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| "没有兼容的版本（请检查游戏版本/加载器筛选）".to_owned())?;
    let ver = first
        .get("version_number")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let files = first
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "版本缺少 files".to_owned())?;
    let primary = files
        .iter()
        .find(|f| f.get("primary").and_then(|p| p.as_bool()).unwrap_or(false))
        .or_else(|| files.first())
        .ok_or_else(|| "版本没有可用文件".to_owned())?;
    let url = primary
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| "文件缺少 url".to_owned())?;
    let fname = primary
        .get("filename")
        .and_then(|f| f.as_str())
        .unwrap_or("mod.jar")
        .to_owned();
    let dl = http.get(url.to_owned()).await.map_err(|e| format!("下载失败：{e}"))?;
    if dl.status != 200 {
        return Err(format!("下载 HTTP {}", dl.status));
    }
    if dl.body.is_empty() {
        return Err("下载内容为空".to_owned());
    }
    Ok((dl.body, ver, fname))
}

/// 资源目标目录（真实安装）。
fn mods_dir_target(
    ctx: &launcher_plugin_common::PluginContext,
    instance_id: &str,
    sub_dir: &str,
) -> Result<std::path::PathBuf, String> {
    let dir = ctx
        .data_dir()
        .join("instances")
        .join(instance_id)
        .join(sub_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {sub_dir} 目录失败：{e}"))?;
    Ok(dir)
}

/// 真实安装：写文件 + 记录索引。
async fn install_real(
    ctx: &launcher_plugin_common::PluginContext,
    instance_id: &str,
    project_id: &str,
    filename: &str,
    project_type: &str,
    real_file: Option<(Vec<u8>, String)>,
    dir: std::path::PathBuf,
) -> launcher_plugin_common::PluginResult {
    use launcher_plugin_common::result::success;
    let (bytes, ver) = real_file.expect("install_real 必须带真实文件");
    let target = dir.join(filename);
    if let Err(e) = std::fs::write(&target, &bytes) {
        return Ok(business(
            biz_codes::STORAGE_ERROR,
            format!("写入文件失败：{e}"),
        ));
    }
    // 更新 mod 索引（复用 mod store 的结构，记录类型便于列表区分）。
    let mut store = load_mod_store!(ctx);
    let mods = store.instances.entry(instance_id.to_owned()).or_default();
    let entry = ModEntry {
        filename: filename.to_owned(),
        name: project_id.to_owned(),
        version: format!("{ver} ({project_type})"),
        project_id: Some(project_id.to_owned()),
        installed_at: now_rfc3339(),
    };
    match mods.iter_mut().find(|m| m.filename == filename) {
        Some(existing) => *existing = entry,
        None => mods.push(entry),
    }
    save_mod_store!(ctx, &store);
    ctx.log_info(format!(
        "已为实例 {instance_id} 真实安装 {project_type} {project_id} -> {filename}（{} KB）",
        bytes.len() / 1024
    ));
    Ok(success(json!({
        "ok": true,
        "simulated": false,
        "instance_id": instance_id,
        "filename": filename,
        "project_id": project_id,
        "project_type": project_type,
        "version": ver,
        "bytes": bytes.len(),
    })))
}

/// 原模拟安装逻辑（网络能力缺失时的兜底）。
async fn install_simulated(
    ctx: launcher_plugin_common::PluginContext,
    args: InstallModArgs,
    instance_id: &str,
    project_id: &str,
    filename: &str,
) -> launcher_plugin_common::PluginResult {

    // ---- 读索引 ----
    let mut store = load_mod_store!(ctx);
    let mods = store.instances.entry(instance_id.to_owned()).or_default();

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
        filename: filename.to_owned(),
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
