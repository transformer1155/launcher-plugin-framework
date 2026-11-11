//! 实时数据源：UI 与 `official-*` 插件工具之间的桥。
//!
//! # 为什么是"桥"而不是"又一份数据"
//!
//! 之前 [`crate::data::DataStore`] 是演示用的内存快照。现在这里直接调用**真实插件**：
//! 用框架的 `ToolSet` 把每个工具的 `handler` 登记进来，UI 侧用 `tokio` 运行时
//! `block_on` 调用，拿到结构化 JSON 结果后映射成领域模型。
//!
//! 这是"UI 只是插件能力的呈现"这一架构意图的真正落地 —— 数据形状与插件返回完全一致。
//!
//! # 调用方式：同步外观 / 异步内核
//!
//! 即时模式 UI 的 `frame` 必须是同步的，而插件工具是 async。桥内部持有一个 `tokio`
//! 运行时，把 `call()` 的异步调用 `block_on` 成同步结果。UI 永不持有 `Future`，
//! 因此不会阻塞渲染循环（单次调用只是一次本地文件读写或一次进程 spawn，亚毫秒级）。

use std::collections::HashMap;
use std::path::PathBuf;

use launcher_plugin_common::registry::{ErasedHandler, ToolSet};
use launcher_plugin_common::result::{into_framework_result, PluginResult};
use launcher_plugin_common::PluginContext;
use launcher_plugin_framework::types::{ContentBlock, ToolResult};
use serde_json::{json, Value};
use tokio::runtime::Runtime;

use crate::plugins_discovery::DiscoveredPlugin;

/// 一个实例（来自 `official-instance.list_instances`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveInstance {
    pub id: String,
    pub name: String,
    pub game_version: String,
    pub loader: String,
    pub last_played: Option<String>,
}

/// 一个已安装 Mod（来自 `official-mod.list_installed_mods`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveMod {
    pub filename: String,
    pub name: String,
    pub version: String,
}

/// 启动配置（来自 `official-launch.get_launch_config`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLaunchConfig {
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    pub memory_mb: u32,
}

/// 一个探测到的 Java 运行时（来自 `official-launch.detect_java`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJava {
    pub path: String,
    pub home: String,
    pub version: String,
    pub vendor: String,
    pub arch: String,
    pub display: String,
}

/// 真实数据源：持有三个官方插件的工具 handler + 一个 tokio 运行时 + 插件目录。
pub struct LiveDataSource {
    rt: Runtime,
    /// 插件目录（每个子目录是一个插件，内含 `plugin.json`）。用于真实扫描发现。
    plugins_dir: PathBuf,
    /// 全名（`插件ID.短名`）→（插件上下文，擦除后的 handler）。
    tools: HashMap<String, (PluginContext, ErasedHandler)>,
    /// 已知的真实实例 id 集合（list / create / delete 时维护）。
    /// 用于过滤 UI 侧演示 id（如 `v-109`），避免对插件做无意义调用。
    known: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl LiveDataSource {
    /// 用各自的插件数据目录构建三个官方插件并登记所有工具。
    ///
    /// `official-launch` 额外注入带白名单的真实 HTTPS 端口（`download_java`
    /// 需要访问 api.adoptium.net 与 github 下载 JDK）。
    pub fn new(plugins_dir: PathBuf) -> Result<Self, String> {
        // JDK zip ~200MB：把响应上限与读超时放宽到下载量级。
        let http_cfg = host_http::HttpConfig {
            timeout: std::time::Duration::from_secs(600),
            max_body_bytes: 512 * 1024 * 1024,
            ..Default::default()
        };
        let port = host_http::HttpsPort::with_config(http_cfg);
        Self::with_http(plugins_dir, std::sync::Arc::new(port))
    }

    /// 用注入的 HTTP 端口构建（`new` 的本体；测试用 curl 后端注入以绕沙箱 TLS）。
    pub fn with_http(
        plugins_dir: PathBuf,
        raw_http: std::sync::Arc<dyn launcher_plugin_common::context::HttpPort>,
    ) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| format!("无法创建 tokio 运行时：{e}"))?;
        let mut tools: HashMap<String, (PluginContext, ErasedHandler)> = HashMap::new();

        let (ic, iset) = official_instance::build(plugins_dir.join("official-instance"));
        register(&mut tools, official_instance::PLUGIN_ID, &ic, &iset);

        let decl = launcher_plugin_common::NetworkDecl {
            allow_hosts: vec![
                "api.adoptium.net".to_owned(),
                "github.com".to_owned(),
                "objects.githubusercontent.com".to_owned(),
                "piston-meta.mojang.com".to_owned(),
                "piston-data.mojang.com".to_owned(),
                "launchermeta.mojang.com".to_owned(),
                "libraries.minecraft.net".to_owned(),
                "meta.fabricmc.net".to_owned(),
                "maven.fabricmc.net".to_owned(),
                // 资源对象（纹理/音效）下载站：assets/objects 全部从这里下载，
                // 缺了它游戏会大量报 NoSuchFileException。
                "resources.download.minecraft.net".to_owned(),
            ],
            reason: "download_java 下载 JDK；install_minecraft 下载 Minecraft 客户端、库、natives 与资源对象"
                .to_owned(),
        };
        let http = host_http::allowlisted_with(raw_http, &decl);
        let (lc, lset) = official_launch::build_with_http(
            plugins_dir.join("official-launch"),
            std::sync::Arc::new(launcher_plugin_common::StderrLogger),
            http,
        );
        register(&mut tools, official_launch::PLUGIN_ID, &lc, &lset);

        let (mc, mset) = official_mod::build(plugins_dir.join("official-mod"));
        register(&mut tools, official_mod::PLUGIN_ID, &mc, &mset);

        Ok(Self {
            rt,
            plugins_dir,
            tools,
            known: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// 该 id 是否是插件侧真实存在的实例（用于过滤演示 id）。
    pub fn has_instance(&self, id: &str) -> bool {
        self.known.lock().map(|g| g.contains(id)).unwrap_or(false)
    }

    /// 真实插件发现：扫描插件目录，返回磁盘上所有 `plugin.json` 描述的插件，
    /// 并合并内嵌的内置清单（磁盘同 id 优先）。这样全新安装也能在插件页看到内置插件。
    ///
    /// 这与「已激活的工具」是两回事：单进程单二进制下，只有内置 id 对应的代码被编译进
    /// 二进制；其余被发现的插件会照常展示，但标注「需重新编译启动器才能加载」。
    pub fn discover_plugins(&self) -> Vec<DiscoveredPlugin> {
        let mut disk = crate::plugins_discovery::scan_plugins(&self.plugins_dir);
        for b in crate::plugins_discovery::builtin_plugins() {
            if !disk.iter().any(|p| p.id == b.id) {
                disk.push(b);
            }
        }
        disk.sort_by(|a, b| a.name.cmp(&b.name));
        disk
    }

    /// 插件目录（每个子目录是一个插件，内含 `plugin.json`）。
    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.plugins_dir
    }

    // -----------------------------------------------------------------------
    // 底层调用：全名 + JSON 参数 → JSON 结果
    // -----------------------------------------------------------------------

    fn call(&self, full: &str, args: Value) -> Result<Value, String> {
        let (ctx, handler) = self
            .tools
            .get(full)
            .ok_or_else(|| format!("工具不存在：{full}"))?;
        let fut = handler(ctx.clone(), args);
        let res: PluginResult = self.rt.block_on(fut);
        let tr: ToolResult = into_framework_result(res);
        if tr.is_error {
            let msg = tr
                .structured_content
                .as_ref()
                .and_then(|s| s.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_owned())
                .or_else(|| {
                    tr.content.iter().find_map(|c| match c {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_else(|| "未知错误".to_owned());
            return Err(msg);
        }
        Ok(tr.structured_content.unwrap_or(Value::Null))
    }

    // -----------------------------------------------------------------------
    // 类型化包装：official-instance
    // -----------------------------------------------------------------------

    pub fn list_instances(&self) -> Result<Vec<LiveInstance>, String> {
        let v = self.call("com.example.official-instance.list_instances", json!({}))?;
        let arr = v
            .get("instances")
            .and_then(|a| a.as_array())
            .ok_or_else(|| "list_instances 返回缺少 instances 数组".to_owned())?;
        let mut out = Vec::with_capacity(arr.len());
        for it in arr {
            out.push(LiveInstance {
                id: str_field(it, "id")?,
                name: str_field(it, "name")?,
                game_version: str_field(it, "game_version").unwrap_or_default(),
                loader: str_field(it, "loader").unwrap_or_default(),
                last_played: it
                    .get("last_played")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_owned()),
            });
        }
        // 维护已知实例名单（供 has_instance 过滤演示 id）。
        if let Ok(mut g) = self.known.lock() {
            g.extend(out.iter().map(|i| i.id.clone()));
        }
        Ok(out)
    }

    pub fn create_instance(
        &self,
        name: &str,
        game_version: &str,
        loader: &str,
    ) -> Result<String, String> {
        let v = self.call(
            "com.example.official-instance.create_instance",
            json!({ "name": name, "game_version": game_version, "loader": loader }),
        )?;
        let id = v
            .get("instance_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "create_instance 返回缺少 instance_id".to_owned())?;
        if let Ok(mut g) = self.known.lock() {
            g.insert(id.to_owned());
        }
        Ok(id.to_owned())
    }

    pub fn delete_instance(&self, instance_id: &str) -> Result<(), String> {
        self.call(
            "com.example.official-instance.delete_instance",
            json!({ "instance_id": instance_id, "confirm": true }),
        )
        .map(|_| ())?;
        if let Ok(mut g) = self.known.lock() {
            g.remove(instance_id);
        }
        Ok(())
    }

    pub fn switch_instance(&self, instance_id: &str) -> Result<(), String> {
        self.call(
            "com.example.official-instance.switch_instance",
            json!({ "instance_id": instance_id }),
        )
        .map(|_| ())
    }

    // -----------------------------------------------------------------------
    // 类型化包装：official-launch
    // -----------------------------------------------------------------------

    pub fn get_launch_config(&self, instance_id: &str) -> Result<LiveLaunchConfig, String> {
        let v = self.call(
            "com.example.official-launch.get_launch_config",
            json!({ "instance_id": instance_id }),
        )?;
        Ok(LiveLaunchConfig {
            jvm_args: arr_str(&v, "jvm_args"),
            game_args: arr_str(&v, "game_args"),
            memory_mb: v.get("memory_mb").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        })
    }

    /// 启动前就绪检查：返回 `(入口类型, java 路径)`。
    /// `entry_kind == "none"` 说明该实例还没装配过真实 Minecraft 入口
    /// （`install_minecraft` 未跑或失败）；`java_path` 为 `None` 表示
    /// 将回退到 PATH 上的 `java`（本机没装 Java 时会 spawn 失败）。
    pub fn launch_readiness(&self, instance_id: &str) -> Result<(String, Option<String>), String> {
        let v = self.call(
            "com.example.official-launch.get_launch_config",
            json!({ "instance_id": instance_id }),
        )?;
        let kind = v
            .get("entry_kind")
            .and_then(|x| x.as_str())
            .unwrap_or("none")
            .to_owned();
        let java = v
            .get("java_path")
            .and_then(|x| x.as_str())
            .map(|s| s.to_owned());
        Ok((kind, java))
    }

    /// 设置实例的启动参数。`official-launch` 只接受 JVM 参数 / 游戏参数 / 内存三项；
    /// Java 可执行文件由 `launch_game` 解析（缺省用 PATH 上的 `java`），启动入口默认是
    /// `LaunchEntry::None`（真实 spawn 一次 `java -version` 做探测）。
    pub fn set_launch_config(
        &self,
        instance_id: &str,
        jvm_args: &[String],
        game_args: &[String],
        memory_mb: u32,
    ) -> Result<(), String> {
        let config = json!({
            "jvm_args": jvm_args,
            "game_args": game_args,
            "memory_mb": memory_mb,
        });
        self.call(
            "com.example.official-launch.set_launch_config",
            json!({ "instance_id": instance_id, "config": config }),
        )
        .map(|_| ())
    }

    /// 真实启动：spawn Java 子进程并返回真实 PID。失败（如 java 不可用）返回错误。
    pub fn launch_game(&self, instance_id: &str) -> Result<u32, String> {
        let v = self.call(
            "com.example.official-launch.launch_game",
            json!({ "instance_id": instance_id }),
        )?;
        let pid = v
            .get("pid")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "launch_game 返回缺少 pid".to_owned())?;
        Ok(pid as u32)
    }

    /// 当前已启动游戏进程的退出码（若已退出）；`None` 表示仍在运行或从未启动。
    pub fn launch_status(&self) -> Option<i32> {
        official_launch::store::launch_status()
    }

    /// 取走游戏进程新增的输出行（stdout/stderr 后台线程持续喂入）。
    pub fn drain_game_log(&self) -> Vec<String> {
        official_launch::store::drain_game_log()
    }

    /// 资源对象下载进度：`(实例 id, 已完成字节, 总字节, 瞬时速度 KB/s)`。
    /// 无进行中的下载时为 `None`。
    pub fn asset_progress(&self) -> Option<(String, u64, u64, u64)> {
        official_launch::store::asset_progress()
    }

    /// 取消启动 / 停止运行：杀死真实游戏进程（若存在）。返回是否成功杀掉一个进程。
    pub fn kill_game(&self) -> bool {
        official_launch::store::kill_child()
    }

    /// 扫描本机 Java 运行时（真实探测：PATH / JAVA_HOME / 安装目录 / 注册表）。
    pub fn detect_java(&self) -> Result<Vec<LiveJava>, String> {
        let v = self.call("com.example.official-launch.detect_java", json!({}))?;
        let arr = v
            .get("runtimes")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .iter()
            .map(|j| LiveJava {
                path: str_field(j, "path").unwrap_or_default(),
                home: str_field(j, "home").unwrap_or_default(),
                version: str_field(j, "version").unwrap_or_default(),
                vendor: str_field(j, "vendor").unwrap_or_default(),
                arch: str_field(j, "arch").unwrap_or_default(),
                display: str_field(j, "display").unwrap_or_default(),
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // 类型化包装：official-mod
    // -----------------------------------------------------------------------

    /// 下载 JDK（真实 HTTPS，分钟级耗时——**必须在后台线程调用**）。
    /// 成功返回安装路径（`...\java\jdk-{major}`）。
    pub fn download_java(&self, major: u32) -> Result<(String, String), String> {
        let v = self.call(
            "com.example.official-launch.download_java",
            json!({ "major": major }),
        )?;
        let path = str_field(&v, "path").unwrap_or_default();
        let version = str_field(&v, "version").unwrap_or_default();
        Ok((path, version))
    }

    /// 从 Mojang 官方源下载真实 Minecraft 客户端并配置启动入口（分钟级耗时，
    /// **必须在后台线程调用**）。
    pub fn install_minecraft(
        &self,
        instance_id: &str,
        version: &str,
        loader: &str,
    ) -> Result<Value, String> {
        self.call(
            "com.example.official-launch.install_minecraft",
            json!({ "instance_id": instance_id, "version": version, "loader": loader }),
        )
    }

    /// 拉取 Mojang 官方可安装版本清单（供安装表单点选；后台线程调用）。
    /// 返回 `(id, 类型, 发布时间)` 列表，按清单原序（最新在前）。
    pub fn list_versions(&self) -> Result<Vec<(String, String, String)>, String> {
        let v = self.call(
            "com.example.official-launch.list_versions",
            json!({ "kind": "release" }),
        )?;
        let arr = v
            .get("versions")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .iter()
            .map(|e| {
                (
                    e.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_owned(),
                    e.get("type").and_then(|x| x.as_str()).unwrap_or_default().to_owned(),
                    e.get("release_time").and_then(|x| x.as_str()).unwrap_or_default().to_owned(),
                )
            })
            .collect())
    }

    pub fn list_mods(&self, instance_id: &str) -> Result<Vec<LiveMod>, String> {
        let v = self.call(
            "com.example.official-mod.list_installed_mods",
            json!({ "instance_id": instance_id }),
        )?;
        let arr = v
            .get("mods")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .iter()
            .map(|m| LiveMod {
                filename: str_field(m, "filename").unwrap_or_default(),
                name: str_field(m, "name").unwrap_or_default(),
                version: str_field(m, "version").unwrap_or_default(),
            })
            .collect())
    }

    pub fn install_mod(
        &self,
        instance_id: &str,
        project_id: &str,
        filename: &str,
    ) -> Result<(), String> {
        self.call(
            "com.example.official-mod.install_mod",
            json!({ "instance_id": instance_id, "project_id": project_id, "filename": filename }),
        )
        .map(|_| ())
    }

    pub fn remove_mod(&self, instance_id: &str, filename: &str) -> Result<(), String> {
        self.call(
            "com.example.official-mod.remove_mod",
            json!({ "instance_id": instance_id, "filename": filename }),
        )
        .map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

fn register(
    tools: &mut HashMap<String, (PluginContext, ErasedHandler)>,
    plugin_id: &str,
    ctx: &PluginContext,
    set: &ToolSet,
) {
    for def in &set.tools {
        let full = format!("{plugin_id}.{}", def.short_name);
        tools.insert(full, (ctx.clone(), def.handler.clone()));
    }
}

fn str_field(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| format!("字段缺失或类型错误：{key}"))
}

fn arr_str(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(tag: &str) -> LiveDataSource {
        // 每个测试用独立临时目录，避免并行执行时并发写 instances.json 造成损坏。
        let dir = launcher_plugin_common::test_support::temp_path(tag);
        LiveDataSource::new(dir).expect("构建 LiveDataSource 应成功")
    }

    #[test]
    fn builds_and_lists_empty_instances() {
        let src = source("live-empty");
        let insts = src
            .list_instances()
            .expect("list_instances 应返回（空）列表");
        assert!(insts.is_empty(), "全新数据目录应没有实例");
    }

    #[test]
    fn create_then_list_roundtrip() {
        let src = source("live-roundtrip");
        let id = src
            .create_instance("测试实例", "1.20.1", "fabric")
            .expect("create_instance 应成功");
        assert!(!id.is_empty(), "应返回实例 ID");
        let insts = src.list_instances().expect("list 应成功");
        assert_eq!(insts.len(), 1);
        assert_eq!(insts[0].name, "测试实例");
        assert_eq!(insts[0].loader, "fabric");

        // 删除（二次确认）后列表应为空
        src.delete_instance(&id).expect("delete 应成功");
        assert!(src.list_instances().unwrap().is_empty());
    }

    #[test]
    fn launch_config_set_and_get() {
        let src = source("live-launchcfg");
        let id = src
            .create_instance("配置测试", "1.20.1", "vanilla")
            .unwrap();
        src.set_launch_config(&id, &["-XX:+UseG1GC".to_owned()], &[], 4096)
            .expect("set_launch_config 应成功");
        let cfg = src.get_launch_config(&id).expect("get 应成功");
        assert_eq!(cfg.memory_mb, 4096);
        assert_eq!(cfg.jvm_args, vec!["-XX:+UseG1GC".to_owned()]);
    }
}
