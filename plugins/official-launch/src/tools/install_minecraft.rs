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
    let client_bytes = match fetch_to_file(&http, &client_url, &client_path).await {
        Ok(n) => n,
        Err(e) => return fail(biz_codes::UPSTREAM_ERROR, format!("下载客户端 jar 失败：{e}")),
    };

    // ---- 5. 下载库 ----
    let libs = vjson.get("libraries").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let mut classpath_parts: Vec<String> = vec![client_path.display().to_string()];
    let mut lib_count = 0usize;
    let mut lib_bytes = 0u64;
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
            }
            Err(e) => {
                ctx.log_warn(format!("库 {rel} 下载失败，跳过：{e}"));
                continue;
            }
        }
        classpath_parts.push(dest.display().to_string());
    }

    // ---- 5a-2. 加载器合并（Fabric：真实安装；其余暂按原版并明确警告）----
    let loader = args
        .loader
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vanilla".to_owned());
    let main_class = match vanilla_main_class {
        Some(m) if loader == "vanilla" => m,
        Some(vanilla) => {
            if loader == "fabric" {
                // Fabric 官方 meta：stable loader 的完整 profile（含 mainClass 与额外库）。
                let profile_url =
                    format!("https://meta.fabricmc.net/v2/versions/loader/{version}/stable/profile/json");
                match get_json(&http, &profile_url).await {
                    Ok(profile) => {
                        let fm = profile
                            .get("mainClass")
                            .and_then(|m| m.as_str())
                            .unwrap_or("net.fabricmc.loader.impl.launch.knot")
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
                            if !dest.starts_with(&libraries_dir) {
                                continue;
                            }
                            match fetch_to_file(&http, &url, &dest).await {
                                Ok(_) => {
                                    added += 1;
                                    classpath_parts.push(dest.display().to_string());
                                }
                                Err(e) => {
                                    ctx.log_warn(format!("Fabric 库 {rel} 下载失败，跳过：{e}"));
                                }
                            }
                        }
                        ctx.log_info(format!(
                            "Fabric loader 已合并：main_class={fm}，额外库 {added}/{}",
                            flibs.len()
                        ));
                        fm
                    }
                    Err(e) => {
                        ctx.log_warn(format!("获取 Fabric profile 失败（{e}），按原版客户端安装"));
                        vanilla
                    }
                }
            } else {
                ctx.log_warn(format!(
                    "{loader} 加载器安装暂未支持，将按原版客户端安装启动（MOD 兼容性无法保证）"
                ));
                vanilla
            }
        }
        None if loader == "fabric" => "net.fabricmc.loader.impl.launch.knot".to_owned(),
        None => return fail(biz_codes::UPSTREAM_ERROR, "版本 JSON 缺少 mainClass"),
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
                    // 进度上报（字节）：下载中心进度条平滑可见。
                    crate::store::set_asset_progress(&instance_id, skipped_bytes, total_bytes);
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
                                done_bytes,
                                total_bytes,
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
                        crate::store::set_asset_progress(&instance_id, done_bytes, total_bytes);
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
