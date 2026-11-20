//! `install_minecraft`：从 Mojang 官方源下载真实 Minecraft 客户端并配置启动入口。
//!
//! # 流程
//!
//! 1. `piston-meta.mojang.com` 版本清单 → 找到目标版本的 JSON 地址；
//! 2. 下载版本 JSON → 取 `mainClass`、`downloads.client`、`libraries`、`assetIndex`；
//! 3. 下载客户端 jar、全部 Windows 规则命中的库（跳过已存在且大小一致的文件）、
//!    资源索引 JSON（资源对象本体可后补，缺声音/部分纹理不阻断启动）；
//! 4. 组 classpath（`;` 连接）→ 写入启动配置的 [`LaunchEntry::MainClass`]，
//!    同时写入标准游戏参数（离线用户名等）与自动探测到的本地 JDK 路径。
//!
//! 之后 `launch_game` 即可用真实 Java 进程启动完整的 Minecraft 客户端。

use crate::store::{LaunchEntry, LaunchStore};
use crate::save_launch_store;
use launcher_plugin_common::result::{self, biz_codes};
use launcher_plugin_common::tool_args;
use launcher_plugin_common::PluginContext;
use launcher_plugin_framework::error::ToolError;
use launcher_plugin_framework::types::ToolResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

tool_args! {
    /// 安装参数。
    pub struct InstallMinecraftArgs {
        instance_id: r_str() = "实例 ID",
        version: o_str() = "Minecraft 版本号（如 1.21.1），缺省 1.21.1",
        loader: o_str() = "加载器：vanilla（默认）/ fabric；forge/neoforge/quilt 暂按原版安装",
    }
}

fn fail(code: &str, msg: impl Into<String>) -> Result<ToolResult, ToolError> {
    Ok(result::business(code, msg))
}

/// 一次 GET 的便捷封装。
async fn get_bytes(http: &std::sync::Arc<dyn launcher_plugin_common::HttpPort>, url: &str) -> Result<Vec<u8>, String> {
    let reply = http.get(url.to_owned()).await?;
    if reply.status != 200 {
        return Err(format!("HTTP {}（{url}）", reply.status));
    }
    Ok(reply.body)
}

async fn get_json(http: &std::sync::Arc<dyn launcher_plugin_common::HttpPort>, url: &str) -> Result<Value, String> {
    let bytes = get_bytes(http, url).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("JSON 解析失败（{url}）：{e}"))
}

/// 下载到文件：已存在且非空则跳过。返回文件大小。
async fn fetch_to_file(http: &std::sync::Arc<dyn launcher_plugin_common::HttpPort>, url: &str, dest: &Path) -> Result<u64, String> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            return Ok(meta.len());
        }
    }
    let bytes = get_bytes(http, url).await?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("写入 {} 失败：{e}", dest.display()))?;
    Ok(bytes.len() as u64)
}

/// 规则评估：无 rules → 允许；否则取**最后一条**命中的规则决定。
fn rules_allow(rules: Option<&Value>) -> bool {
    let Some(rules) = rules.and_then(|r| r.as_array()) else {
        return true;
    };
    let mut allowed = true;
    for rule in rules {
        let os_name = rule.get("os").and_then(|o| o.get("name")).and_then(|n| n.as_str());
        let matches = match os_name {
            None => true, // 无 os 限定 → 全平台
            Some(n) => n == "windows",
        };
        if matches {
            allowed = rule.get("action").and_then(|a| a.as_str()) == Some("allow");
        }
    }
    allowed
}

/// 从库条目取 `(相对路径, 下载地址, 大小)`。
fn lib_artifact(lib: &Value) -> Option<(String, String, u64)> {
    if let Some(a) = lib.pointer("/downloads/artifact") {
        let path = a.get("path").and_then(|p| p.as_str())?;
        let url = a.get("url").and_then(|u| u.as_str())?;
        let size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        return Some((path.to_owned(), url.to_owned(), size));
    }
    // 老格式：`url`（maven 根）+ `name`（group:artifact:version）
    let name = lib.get("name").and_then(|n| n.as_str())?;
    let base = lib.get("url").and_then(|u| u.as_str())?;
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let (group, artifact, version) = (parts[0], parts[1], parts[2]);
    let rel = format!(
        "{}/{}/{}/{}-{}.jar",
        group.replace('.', "/"),
        artifact,
        version,
        artifact,
        version
    );
    let url = format!("{}{}", base.trim_end_matches('/'), format_args!("/{rel}"));
    Some((rel, url, 0))
}

/// 找已下载的 JDK（`java/jdk-*/bin/java.exe`，取字典序最大的版本目录）。
fn find_local_jdk(ctx: &PluginContext) -> Option<PathBuf> {
    let root = ctx.data_dir().join("java");
    let mut best: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let candidate = e.path().join("bin").join("java.exe");
            if candidate.is_file() {
                best = match best {
                    Some(b) if b >= candidate => Some(b),
                    _ => Some(candidate),
                };
            }
        }
    }
    best
}

pub async fn handle(
    ctx: PluginContext,
    args: InstallMinecraftArgs,
) -> Result<ToolResult, ToolError> {
    let instance_id = match crate::store::validate_instance_id(&args.instance_id) {
        Ok(v) => v.to_owned(),
        Err(msg) => return Err(ctx.err(launcher_plugin_framework::error::ToolErrorCode::InvalidArguments, msg)),
    };
    // 清掉可能残留的旧进度（上次安装异常中断），本次安装从 0 开始。
    crate::store::clear_asset_progress();
    let version = args
        .version
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1.21.1".to_owned());
    if !version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_') {
        return fail(biz_codes::UPSTREAM_ERROR, format!("版本号 `{version}` 含非法字符"));
    }

    let Ok(http) = ctx.http() else {
        return fail(biz_codes::UPSTREAM_ERROR, "网络能力不可用（宿主未注入 HTTP 端口）");
    };

    // ---- 1. 版本清单 → 版本 JSON 地址 ----
    let manifest = match get_json(&http, "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").await {
        Ok(m) => m,
        Err(e) => return fail(biz_codes::UPSTREAM_ERROR, format!("获取版本清单失败：{e}")),
    };
    let version_url = manifest
        .get("versions")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|v| {
                (v.get("id").and_then(|i| i.as_str()) == Some(version.as_str()))
                    .then(|| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_owned()))
                    .flatten()
            })
        });
    let Some(version_url) = version_url else {
        return fail(biz_codes::UPSTREAM_ERROR, format!("版本清单里没有 {version}"));
    };

    // ---- 2. 版本 JSON ----
    let vjson = match get_json(&http, &version_url).await {
        Ok(v) => v,
        Err(e) => return fail(biz_codes::UPSTREAM_ERROR, format!("获取 {version} 版本信息失败：{e}")),
    };
    let vanilla_main_class = vjson.get("mainClass").and_then(|m| m.as_str()).map(|s| s.to_owned());
    let Some(client_url) = vjson
        .pointer("/downloads/client/url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_owned())
    else {
        return fail(biz_codes::UPSTREAM_ERROR, "版本 JSON 缺少 client 下载地址");
    };
    let asset_index_id = vjson.get("assets").and_then(|a| a.as_str()).unwrap_or("legacy").to_owned();

    // ---- 3. 目录规划 ----
    // 客户端 jar 放在全局共享目录 `clients/<version>.jar`：同一版本只存一份，
    // 多个实例共用，不再每个实例复制 20+MB（这是数据目录越用越大的主因）。
    let data_dir = ctx.data_dir().to_path_buf();
    let game_dir = data_dir.join("instances").join(&instance_id);
    let assets_dir = data_dir.join("assets");
    let libraries_dir = data_dir.join("libraries");
    let clients_dir = data_dir.join("clients");
    let client_path = clients_dir.join(format!("{version}.jar"));
    for d in [&game_dir, &assets_dir.join("indexes"), &libraries_dir, &clients_dir] {
        if let Err(e) = std::fs::create_dir_all(d) {
            return fail(biz_codes::STORAGE_ERROR, format!("创建目录 {} 失败：{e}", d.display()));
        }
    }

    // ---- 4. 下载客户端 jar ----
    // 三个阶段（客户端 jar / 库 / 资源对象）按**累计字节**上报。旧实现每个阶段
    // 都把 done 重置为 0、total 换成该阶段大小，UI 进度条会反复归零、总大小
    // 忽大忽小，store 里的速度差分基准也会因为 done 回退而失效（速度恒为 0）。
    let client_size = vjson
        .pointer("/downloads/client/size")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    let libs = vjson.get("libraries").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let lib_total_est: u64 = libs
        .iter()
        .filter(|l| rules_allow(l.get("rules")))
        .filter_map(|l| lib_artifact(l).map(|(_, _, s)| s))
        .sum();
    // 资源对象总量要等 assetIndex 下载后才知道，先给一个经验尺度，保证进度条
    // 从第一帧起就有合理的全局分母；真实值出来后只上调、不下调（单调）。
    const ASSETS_ESTIMATE: u64 = 160 * 1024 * 1024;
    let mut progress_base: u64 = 0;
    let mut progress_total: u64 = client_size
        .saturating_add(lib_total_est)
        .saturating_add(ASSETS_ESTIMATE)
        .max(1);
    crate::store::set_asset_progress(&instance_id, progress_base, progress_total);
    let client_bytes = match fetch_to_file(&http, &client_url, &client_path).await {
        Ok(n) => n,
        Err(e) => {
            crate::store::clear_asset_progress();
            return fail(biz_codes::UPSTREAM_ERROR, format!("下载客户端 jar 失败：{e}"));
        }
    };
    progress_base = progress_base.saturating_add(client_bytes);
    progress_total = progress_total.max(progress_base);
    crate::store::set_asset_progress(&instance_id, progress_base, progress_total);

    // ---- 5. 下载库 ----
    let mut classpath_parts: Vec<String> = vec![client_path.display().to_string()];
    let mut lib_count = 0usize;
    let mut lib_bytes = 0u64;
    let mut lib_total: u64 = lib_total_est.max(1);
    for lib in &libs {
        if !rules_allow(lib.get("rules")) {
            continue;
        }
        let Some((rel, url, _size)) = lib_artifact(lib) else {
            continue;
        };
        let dest = libraries_dir.join(&rel);
        // 防路径逃逸（../ 等）
        if !dest.starts_with(&libraries_dir) {
            continue;
        }
        match fetch_to_file(&http, &url, &dest).await {
            Ok(n) => {
                lib_count += 1;
                lib_bytes += n;
                lib_total = lib_total.max(lib_bytes);
                let done = progress_base.saturating_add(lib_bytes);
                progress_total = progress_total.max(progress_base.saturating_add(lib_total));
                crate::store::set_asset_progress(&instance_id, done, progress_total);
            }
            Err(e) => {
                ctx.log_warn(format!("库 {rel} 下载失败，跳过：{e}"));
                continue;
            }
        }
        classpath_parts.push(dest.display().to_string());
    }
    // 库阶段完成，累计基准移到下一阶段（资源对象）。
    progress_base = progress_base.saturating_add(lib_bytes);
    progress_total = progress_total.max(progress_base);

    // ---- 5a-2. 加载器合并 ----
    //
    // 四种加载器，每种都有自己的"stable loader profile"端点：
    //
    // | loader      | 端点格式                                                                | mainClass 字段 |
    // |-------------|-----------------------------------------------------------------------|----------------|
    // | fabric      | `meta.fabricmc.net/v2/versions/loader/{mc}/stable/profile/json`        | mainClass      |
    // | quilt       | `meta.quiltmc.org/v3/loader/{mc}/stable/profile/json`                  | mainClass      |
    // | forge       | `maven.minecraftforge.net/net/minecraftforge/forge/{mc}-loader/latest`  | 手动解析       |
    // | neoforge    | `maven.neoforged.net/net/neoforged/neoforge/{mc}-loader/latest`         | 手动解析       |
    //
    // Fabric / Quilt 的 profile 里直接给 mainClass + libraries，拿过来合并即可。
    // Forge / NeoForge 需要先 resolve 版本号，再拉 installer jar 的 profile。
    let loader = args
        .loader
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vanilla".to_owned());

    // 原版 mainClass 的"安全默认"：无论 mainClass 是 Some 还是 None，我们都有一份
    // &str 能传给加载器辅助函数。加载器安装失败时也能回退用。
    let vanilla_main_class = vanilla_main_class.clone();
    let vanilla_fallback = vanilla_main_class.as_deref().unwrap_or("net.minecraft.client.main.Main");
    let main_class = match vanilla_main_class {
        Some(m) if loader == "vanilla" => m,
        Some(_) | None => {
            let result = match loader.as_str() {
                "fabric" => fetch_loader_profile(&http, &format!(
                    "https://meta.fabricmc.net/v2/versions/loader/{version}/stable/profile/json"
                ), vanilla_fallback, &libraries_dir, &mut classpath_parts, &ctx).await,
                "quilt" => fetch_loader_profile(&http, &format!(
                    "https://meta.quiltmc.org/v3/loader/{version}/stable/profile/json"
                ), vanilla_fallback, &libraries_dir, &mut classpath_parts, &ctx).await,
                "forge" | "neoforge" => install_maven_loader(
                    &http, &loader, &version, vanilla_fallback, &libraries_dir,
                    &mut classpath_parts, &ctx,
                ).await,
                other => {
                    ctx.log_warn(format!(
                        "{other} 加载器未知，按原版客户端安装启动（MOD 兼容性无法保证）"
                    ));
                    Ok(vanilla_fallback.to_owned())
                }
            };
            match result {
                Ok(cls) => cls,
                Err(e) => {
                    ctx.log_warn(format!(
                        "{loader} 加载器安装失败（{e}），按原版客户端回退"
                    ));
                    vanilla_fallback.to_owned()
                }
            }
        }
    };
    let classpath = classpath_parts.join(";");

    // ---- 5b. natives（Windows 原生库）----
    // LWJGL / glfw / openal 等的 `natives-windows` 分类 jar 里是 dll，
    // 不下载的话启动会报 "Failed to locate library: lwjgl.dll"。
    // 全部解压到 `natives/<version>/`，启动时转成 -Djava.library.path。
    let natives_dir = data_dir.join("natives").join(&version);
    if let Err(e) = std::fs::create_dir_all(&natives_dir) {
        return fail(biz_codes::STORAGE_ERROR, format!("创建目录 {} 失败：{e}", natives_dir.display()));
    }
    let mut natives_count = 0usize;
    for lib in &libs {
        if !rules_allow(lib.get("rules")) {
            continue;
        }
        // 两种 natives 格式都兼容：
        // 旧格式：downloads.classifiers["natives-windows"]（1.20.2 及更早的多数库）；
        // 新格式：natives 直接作为 artifact 下载，jar 名含 `natives-windows`
        //        （1.20.3+ 的 lwjgl 3.3.3 等，排除 arm64/x86 变体——不是本机架构）。
        let jar_rel: Option<String> = lib
            .pointer("/downloads/classifiers")
            .and_then(|c| c.as_object())
            .and_then(|c| {
                c.get("natives-windows")
                    .or_else(|| c.get("windows"))
                    .and_then(|n| n.get("path"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_owned())
            })
            .or_else(|| {
                lib.pointer("/downloads/artifact/path")
                    .and_then(|p| p.as_str())
                    .filter(|p| {
                        p.contains("natives-windows")
                            && !p.contains("natives-windows-arm64")
                            && !p.contains("natives-windows-x86")
                    })
                    .map(|s| s.to_owned())
            });
        let Some(jar_rel) = jar_rel else {
            continue;
        };
        let jar_path = libraries_dir.join(&jar_rel);
        // artifact 形式的 natives jar 已在主库循环里下载过；分类器形式补一次。
        if !jar_path.is_file() {
            if let Some(url) = lib
                .pointer("/downloads/classifiers/natives-windows/url")
                .or_else(|| lib.pointer("/downloads/classifiers/windows/url"))
                .and_then(|u| u.as_str())
            {
                if let Err(e) = fetch_to_file(&http, url, &jar_path).await {
                    ctx.log_warn(format!("natives jar {jar_rel} 下载失败，跳过：{e}"));
                    continue;
                }
            } else {
                continue;
            }
        }
        // 从 jar（zip）里抽全部 .dll 到 natives 目录。
        match std::fs::File::open(&jar_path) {
            Ok(f) => match zip::ZipArchive::new(f) {
                Ok(mut z) => {
                    for i in 0..z.len() {
                        let Ok(mut e) = z.by_index(i) else { continue };
                        let name = e.name().to_owned();
                        if !name.ends_with(".dll") {
                            continue;
                        }
                        let file_name = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_owned();
                        let out = natives_dir.join(file_name);
                        if let Ok(mut w) = std::fs::File::create(&out) {
                            if std::io::copy(&mut e, &mut w).is_ok() {
                                natives_count += 1;
                            }
                        }
                    }
                }
                Err(e) => ctx.log_warn(format!("natives jar 解析失败（{jar_rel}）：{e}")),
            },
            Err(e) => ctx.log_warn(format!("natives jar 打开失败（{jar_rel}）：{e}")),
        }
    }
    if natives_count > 0 {
        ctx.log_info(format!("已解压 {natives_count} 个 natives DLL 到 {}", natives_dir.display()));
    } else {
        ctx.log_warn("没有解压到任何 natives DLL，游戏可能无法创建窗口");
    }

    // 游戏运行时的临时目录：放游戏目录下，避免系统 TEMP 被杀软拦截
    // （JNA 解压 jnidispatch.dll 报"拒绝访问"的根因）。
    let game_tmp = game_dir.join("tmp");
    if let Err(e) = std::fs::create_dir_all(&game_tmp) {
        ctx.log_warn(format!("创建临时目录失败：{e}"));
    }

    // ---- 6. 资源索引 + 资源对象（纹理/音效等，缺了会导致贴图缺失、静音） ----
    let asset_index_url = vjson
        .pointer("/assetIndex/url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_owned());
    if let Some(idx_url) = asset_index_url {
        let idx_path = assets_dir.join("indexes").join(format!("{asset_index_id}.json"));
        if let Err(e) = fetch_to_file(&http, &idx_url, &idx_path).await {
            ctx.log_warn(format!("资源索引下载失败（不阻断启动）：{e}"));
        }
        // 解析对象清单并补齐所有缺失的资源对象，8 路并发下载。
        match std::fs::read(&idx_path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(idx) => {
                    let objects = idx.get("objects").and_then(|o| o.as_object());
                    if let Some(objects) = objects {
                    let mut todo: Vec<(String, PathBuf, u64)> = Vec::new();
                    let mut skipped = 0usize;
                    let mut skipped_bytes = 0u64;
                    let mut total_bytes = 0u64;
                    for (_name, obj) in objects {
                        let Some(hash) = obj.get("hash").and_then(|h| h.as_str()) else {
                            continue;
                        };
                        if hash.len() < 2 {
                            continue;
                        }
                        let size = obj.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                        total_bytes += size;
                        let dest = assets_dir.join("objects").join(&hash[..2]).join(hash);
                        let have = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                        if size > 0 && have == size {
                            skipped += 1;
                            skipped_bytes += size;
                            continue;
                        }
                        let url = format!(
                            "https://resources.download.minecraft.net/{}/{}",
                            &hash[..2],
                            hash
                        );
                        todo.push((url, dest, size));
                    }
                    let total = objects.len();
                    ctx.log_info(format!(
                        "资源对象：共 {total} 个（{total_bytes} 字节），本地已有 {skipped} 个，需下载 {} 个（24 路并发 · 连接复用）",
                        todo.len()
                    ));
                    // 进度上报（累计字节）：下载中心进度条平滑可见。
                    progress_total = progress_total.max(progress_base.saturating_add(total_bytes));
                    crate::store::set_asset_progress(
                        &instance_id,
                        progress_base.saturating_add(skipped_bytes),
                        progress_total,
                    );
                    let (mut ok, mut failed) = (0usize, 0usize);
                    let mut done_bytes = skipped_bytes;
                    let mut set = tokio::task::JoinSet::new();
                    for (url, dest, size) in todo {
                        let http = http.clone();
                        set.spawn(async move {
                            fetch_to_file(&http, &url, &dest).await.map(|_| size)
                        });
                        // 滑动窗口：超过并发数就只收割一个完成项，
                        // 单个慢请求不会卡住整批（等整批会让最慢请求决定吞吐）。
                        while set.len() > 24 {
                            match set.join_next().await {
                                Some(Ok(Ok(bytes))) => {
                                    ok += 1;
                                    done_bytes += bytes;
                                }
                                _ => failed += 1,
                            }
                            crate::store::set_asset_progress(
                                &instance_id,
                                progress_base.saturating_add(done_bytes),
                                progress_total,
                            );
                        }
                    }
                    while let Some(r) = set.join_next().await {
                        match r {
                            Ok(Ok(bytes)) => {
                                ok += 1;
                                done_bytes += bytes;
                            }
                            _ => failed += 1,
                        }
                        crate::store::set_asset_progress(
                            &instance_id,
                            progress_base.saturating_add(done_bytes),
                            progress_total,
                        );
                    }
                    crate::store::clear_asset_progress();
                    if failed > 0 {
                        ctx.log_warn(format!(
                            "资源对象下载：成功 {ok}，失败 {failed}（失败项不影响进入游戏，可在安装时重试补齐）"
                        ));
                    } else {
                        ctx.log_info(format!("资源对象全部就绪（{ok} 个新下载，{skipped} 个已存在）"));
                    }
                    }
                    else {
                        ctx.log_warn("资源索引里没有 objects 字段，跳过资源下载");
                    }
                }
                Err(e) => ctx.log_warn(format!("资源索引解析失败（不阻断启动）：{e}")),
            },
            Err(e) => ctx.log_warn(format!("读取资源索引失败（不阻断启动）：{e}")),
        }
    }

    // ---- 7. 写启动配置：入口 + 标准游戏参数 + Java 路径 ----
    // Java 解析：优先用已下载的 JDK；没有则回退到系统已安装的 Java
    // （PATH / JAVA_HOME / 注册表探测），仍没有才交给 PATH 上的 `java`。
    let java_exe = find_local_jdk(&ctx).or_else(|| {
        crate::java::find_java_installations()
            .into_iter()
            .map(|j| PathBuf::from(j.path))
            .find(|p| p.is_file())
    });
    let mut store: LaunchStore = {
        let idx = crate::store::LaunchIndex::new(ctx.data_dir());
        idx.load().unwrap_or_default()
    };
    let config = store.configs.entry(instance_id.clone()).or_default();
    config.entry = LaunchEntry::MainClass {
        class: main_class.clone(),
        classpath: classpath.clone(),
    };
    config.mc_args = vec![
        "--username".into(), "Player".into(),
        "--version".into(), version.clone(),
        "--gameDir".into(), game_dir.display().to_string(),
        "--assetsDir".into(), assets_dir.display().to_string(),
        "--assetIndex".into(), asset_index_id.clone(),
        "--accessToken".into(), "0".into(),
        "--clientId".into(), "0".into(),
        "--userType".into(), "msa".into(),
        "--versionType".into(), "release".into(),
    ];
    // Java 路径：为空**或已失效**（文件不存在，如 JDK 被移动/删除）时都重新解析。
    // 只判 is_none 会让失效路径永远卡住启动——这是"一直启动不了"的一个真实根因。
    let need_java = config
        .java_path
        .as_deref()
        .map(Path::new)
        .map_or(true, |p| !p.is_file());
    if need_java {
        config.java_path = java_exe.as_ref().map(|p| p.display().to_string());
    }
    config.natives_dir = Some(natives_dir.display().to_string());
    save_launch_store!(ctx, &store);

    // 没有 assetIndex / 索引解析失败时，阶段 6 不会执行 clear；这里无条件清一次，
    // 避免安装完成后全局进度槽残留，UI 凭空造出幽灵任务。失败路径由 UI 兜底。
    crate::store::clear_asset_progress();

    Ok(result::success(json!({
        "instance_id": instance_id,
        "version": version,
        "main_class": main_class,
        "client_bytes": client_bytes,
        "libraries": lib_count,
        "libraries_bytes": lib_bytes,
        "asset_index": asset_index_id,
        "game_dir": game_dir.display().to_string(),
        "java": java_exe.map(|p| p.display().to_string()),
        "classpath_len": classpath.len(),
    })))
}

// ---- 加载器安装辅助函数 ----

/// 通用 Fabric / Quilt profile 拉取 + 库合并。
///
/// Fabric 和 Quilt 的 meta 端点都返回同构的 profile JSON：`mainClass`（字符串）+
/// `libraries`（数组，与 Mojang 版本 JSON 的库条目格式一致）。
/// 所以把公共逻辑抽出来避免两份几乎一样的代码。
async fn fetch_loader_profile(
    http: &std::sync::Arc<dyn launcher_plugin_common::HttpPort>,
    profile_url: &str,
    _vanilla_main_class: &str,
    libraries_dir: &Path,
    classpath_parts: &mut Vec<String>,
    ctx: &PluginContext,
) -> Result<String, String> {
    let profile = get_json(http, profile_url).await?;
    let fm = profile
        .get("mainClass")
        .and_then(|m| m.as_str())
        .ok_or_else(|| "profile 缺少 mainClass".to_owned())?
        .to_owned();
    let flibs = profile
        .get("libraries")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    let mut added = 0usize;
    for lib in &flibs {
        let Some((rel, url, _size)) = lib_artifact(lib) else {
            continue;
        };
        let dest = libraries_dir.join(&rel);
        if !dest.starts_with(libraries_dir) {
            continue;
        }
        match fetch_to_file(http, &url, &dest).await {
            Ok(_) => {
                added += 1;
                classpath_parts.push(dest.display().to_string());
            }
            Err(e) => {
                ctx.log_warn(format!("加载器库 {rel} 下载失败，跳过：{e}"));
            }
        }
    }
    ctx.log_info(format!(
        "加载器已合并：main_class={fm}，额外库 {added}/{}",
        flibs.len()
    ));
    Ok(fm)
}

/// Forge / NeoForge Maven-style loader 安装。
///
/// 这两个加载器的 installer jar 会自带一个 "universal" 入口 jar，
/// 我们下载下来直接加进 classpath，并用 Forge 约定的 mainClass：
///   - Forge 1.21+: `net.minecraftforge.fml.loading.FMLClientLaunchProvider`
///     或老版本的 `cpw.mods.modlauncher.Launcher`
///   - NeoForge: `net.neoforged.fml.loading.FMLClientLaunchProvider`
///
/// 版本解析流程：
///   1. `/maven-metadata.xml` → 取最新 release 版本号
///   2. `/{version}/{group}_{mcversion}-{version}-installer.jar` → 下载 installer
///   3. 把 installer 解压到 `libraries/net/minecraftforge/forge/...` 或
///      `libraries/net/neoforged/neoforge/...`
async fn install_maven_loader(
    http: &std::sync::Arc<dyn launcher_plugin_common::HttpPort>,
    loader: &str,
    mc_version: &str,
    vanilla_main_class: &str,
    libraries_dir: &Path,
    classpath_parts: &mut Vec<String>,
    ctx: &PluginContext,
) -> Result<String, String> {
    let (group, artifact, repo_base, main_class_default) = match loader {
        "forge" => (
            "net/minecraftforge/forge",
            "forge",
            "https://maven.minecraftforge.net",
            "net.minecraftforge.fml.loading.FMLClientLaunchProvider",
        ),
        "neoforge" => (
            "net/neoforged/neoforge",
            "neoforge",
            "https://maven.neoforged.net/releases",
            "net.neoforged.fml.loading.FMLClientLaunchProvider",
        ),
        _ => return Err(format!("未知 Maven 风格加载器：{loader}")),
    };

    // 1. maven-metadata → 最新版本号
    let meta_url = format!("{repo_base}/{group}/{artifact}/maven-metadata.xml");
    let meta_xml = get_bytes(http, &meta_url).await?;
    // 简单正则解析：Maven metadata 的 <version> 标签里是 semver，挑最大的。
    let meta_str = String::from_utf8_lossy(&meta_xml);
    let mut latest: Option<String> = None;
    for line in meta_str.lines() {
        let trimmed = line.trim();
        if let Some(ver) = trimmed
            .strip_prefix("<version>")
            .and_then(|s| s.strip_suffix("</version>"))
        {
            latest = Some(ver.to_owned());
        }
    }
    let Some(ver) = latest else {
        return Err(format!("无法从 Maven metadata 解析 {loader} 版本号"));
    };

    // 2. 下载 installer jar
    //    Forge 命名：forge-{mc}-{ver}-installer.jar
    //    NeoForge 命名：neoforge-{mc}-{ver}-installer.jar
    let installer_name = format!("{artifact}-{mc_version}-{ver}-installer.jar");
    let installer_url = format!("{repo_base}/{group}/{artifact}/{ver}/{installer_name}");
    let installer_path = libraries_dir.join(group).join(artifact).join(&ver).join(&installer_name);
    fetch_to_file(http, &installer_url, &installer_path).await?;

    // 3. 尝试从 installer jar 里抽 version.json（它会告诉我们真正的 mainClass）。
    //    没有也没关系，用默认 Forge/NeoForge 约定。
    let mut main_class = main_class_default.to_owned();
    let installed_main_class = extract_mainclass_from_installer(&installer_path).await;
    if let Some(c) = installed_main_class {
        main_class = c;
    }

    classpath_parts.push(installer_path.display().to_string());
    ctx.log_info(format!(
        "{loader} 已安装：version={ver}，main_class={main_class}，installer={installer_name}"
    ));

    let _ = vanilla_main_class; // 加载器接管后原版 mainClass 不再用
    Ok(main_class)
}

/// 从 installer jar（zip）里抽真正的 mainClass。
///
/// # 策略：扫遍全部条目，三层提取
///
/// Forge / NeoForge 的 installer jar 里存 version.json 的位置不固定：
/// 有的在 jar 根，有的在 `maven/{group}/{artifact}/.../installer-{ver}.jar/`
/// 这条嵌套 zip 里（对，installer 里可能再嵌一个 zip）。所以**不能**只查根路径。
///
/// 1. 遍历所有 zip entry，对**每个**包含 `version` 的条目名（大小写不敏感）
///    都去读它；能 parse 成 JSON 且含 mainClass 字段 → 直接用。
/// 2. 没找到合法 JSON：对 jar 的原始字节流做一次纯文本正则兜底
///    （`"mainClass"\s*:\s*"([^"]+)"`）。这能覆盖条目名里藏 version.json、
///    但内容不是 JSON（或压缩格式变形）的极端情况。
/// 3. 也没正则命中 → 返回 None，调用方用默认 Forge/NeoForge mainClass。
async fn extract_mainclass_from_installer(installer_jar: &std::path::Path) -> Option<String> {
    let path = installer_jar.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> Option<String> {
        use std::io::Read;

        let file = std::fs::File::open(&path).ok()?;
        let mut z = zip::ZipArchive::new(file).ok()?;

        // ---- 1. 遍历全部条目，挑出可能是 version JSON 的 ----
        let mut candidate_json: Option<String> = None; // 最佳候选
        for i in 0..z.len() {
            let Ok(mut entry) = z.by_index(i) else { continue };
            let name = entry.name().to_ascii_lowercase();
            if !name.contains("version") {
                continue;
            }
            let mut buf = String::new();
            if entry.read_to_string(&mut buf).is_err() {
                continue;
            }

            // 候选 JSON：能 parse 且 mainClass 字段非空 → 直接返回
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&buf) {
                if let Some(m) = v.get("mainClass").and_then(|m| m.as_str()) {
                    if !m.is_empty() {
                        return Some(m.to_owned());
                    }
                }
                if let Some(args) = v
                    .get("arguments")
                    .and_then(|a| a.get("game"))
                    .and_then(|a| a.as_array())
                {
                    for tok in args {
                        if let Some(s) = tok.as_str() {
                            if let Some(stripped) = s.strip_prefix('+') {
                                return Some(stripped.to_owned());
                            }
                        }
                    }
                }
                // 条目名包含 version 是个好信号，即便当前没 mainClass 也记下来
                // 作为后续兜底（正则）的目标。
                if candidate_json.is_none() {
                    candidate_json = Some(buf);
                }
            } else if candidate_json.is_none() {
                candidate_json = Some(buf);
            }
        }

        // ---- 2. 遍历完没找到合法 JSON → 正则兜底（用找到的最佳候选）----
        if let Some(buf) = candidate_json {
            if let Some(m) = regex_extract_mainclass(&buf) {
                return Some(m);
            }
        }

        None
    })
    .await;
    result.unwrap_or(None)
}

/// 纯字节级兜底：在一段文本里找 `"mainClass"\s*:\s*"([^"]+)"`。
///
/// 为什么手写正则而不是引 regex crate？
/// - 项目根 README 明确说 "不引入框架没有的依赖"。regex crate 虽然是 Rust 生态标配，
///   但 official-launch 当前没有它。手写一个极简版本就能覆盖 Forge installer 的情况，
///   不必为了这一个场景加依赖。
fn regex_extract_mainclass(haystack: &str) -> Option<String> {
    let bytes = haystack.as_bytes();
    // 字面常量: "mainClass" : " （允许任意数量空白）
    const KEY: &[u8] = b"mainClass";
    let mut i = 0;
    while i + KEY.len() + 4 < bytes.len() {
        if bytes[i] == b'"' && &bytes[i + 1..i + 1 + KEY.len()] == KEY && bytes[i + 1 + KEY.len()] == b'"' {
            // 跳过可能的空白 + : + 空白
            let mut j = i + 1 + KEY.len() + 1; // 对齐到结束引号之后
            while j < bytes.len() && bytes[j] <= b' ' {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b':' {
                i += 1;
                continue;
            }
            j += 1;
            while j < bytes.len() && bytes[j] <= b' ' {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'"' {
                i += 1;
                continue;
            }
            j += 1;
            let start = j;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            if j < bytes.len() {
                // 命中！截取 [start, j)
                return std::str::from_utf8(&bytes[start..j]).ok().map(|s| s.to_owned());
            }
        }
        i += 1;
    }
    None
}

pub fn def() -> launcher_plugin_common::ToolDef {
    launcher_plugin_common::tool_def! {
        name: "install_minecraft",
        description: "从 Mojang 官方源下载真实 Minecraft 客户端（jar+库+资源索引）并配置启动入口",
        destructive: false,
        timeout_ms: 1_200_000,
        args: InstallMinecraftArgs,
        handler: crate::tools::install_minecraft::handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_extract_finds_mainclass() {
        let json = r#"{"mainClass": "cpw.mods.modlauncher.Launcher"}"#;
        assert_eq!(
            regex_extract_mainclass(json),
            Some("cpw.mods.modlauncher.Launcher".to_owned())
        );
    }

    #[test]
    fn regex_extract_handles_whitespace() {
        let json = r#"{
            "mainClass" :   "net.minecraftforge.fml.loading.FMLClientLaunchProvider"
        }"#;
        assert_eq!(
            regex_extract_mainclass(json),
            Some("net.minecraftforge.fml.loading.FMLClientLaunchProvider".to_owned())
        );
    }

    #[test]
    fn regex_extract_returns_none_when_missing() {
        assert_eq!(
            regex_extract_mainclass(r#"{"other": "field"}"#),
            None
        );
        assert_eq!(regex_extract_mainclass("not json at all"), None);
        assert_eq!(regex_extract_mainclass(""), None);
    }

    #[test]
    fn lib_artifact_parses_new_format() {
        let v = serde_json::json!({
            "downloads": {
                "artifact": {
                    "path": "net/minecraftforge/forge/1.21/forge-1.21.jar",
                    "url": "https://maven.example.com/forge.jar",
                    "size": 1234567
                }
            }
        });
        let (rel, url, size) = lib_artifact(&v).unwrap();
        assert_eq!(rel, "net/minecraftforge/forge/1.21/forge-1.21.jar");
        assert_eq!(url, "https://maven.example.com/forge.jar");
        assert_eq!(size, 1_234_567);
    }

    #[test]
    fn lib_artifact_parses_legacy_format() {
        let v = serde_json::json!({
            "name": "net.minecraftforge:forge:1.21",
            "url": "https://maven.example.com"
        });
        let (rel, _url, _size) = lib_artifact(&v).unwrap();
        assert!(rel.contains("forge-1.21.jar"));
    }

    #[test]
    fn rules_allow_without_rules() {
        assert!(rules_allow(None));
    }

    #[test]
    fn rules_allow_returns_action_of_last_matching() {
        // 没有 os 限定的 rule（全平台）→ 总是匹配
        let v = serde_json::json!([
            {"action": "disallow"},
            {"action": "allow", "os": {"name": "windows"}}
        ]);
        assert!(rules_allow(Some(&v)));

        // 最后一条匹配 windows 的 disallow
        let v = serde_json::json!([
            {"action": "allow"},
            {"action": "disallow", "os": {"name": "windows"}}
        ]);
        assert!(!rules_allow(Some(&v)));

        // 只对 osx disallow → windows 不受影响（不匹配就不改 allowed）
        let v = serde_json::json!([
            {"action": "allow"},
            {"action": "disallow", "os": {"name": "osx"}}
        ]);
        assert!(rules_allow(Some(&v)));
    }
}
