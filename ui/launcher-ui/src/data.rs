//! 数据层：UI 与插件之间的桥。
//!
//! # 为什么 UI 不直接调插件工具
//!
//! 插件工具是 async 的，而即时模式 UI 的 `frame` 必须同步。若在 `frame` 里
//! 用 `block_on` 等待网络/磁盘，整个界面会卡死。
//!
//! 因此采用**快照模型**：
//!
//! ```text
//! 后台线程（tokio）            UI 线程
//!   ├─ 调插件工具                 ├─ frame() 只读快照
//!   ├─ 更新快照（Mutex）   ───►   └─ 生成绘制指令
//!   └─ 循环
//! ```
//!
//! UI 永不阻塞；数据最多延迟一个刷新周期（默认 100ms）。
//!
//! # 本仓库的约束
//!
//! UI 目前运行在**没有插件宿主**的环境里（插件加载需要签名/装配管线）。
//! 因此 [`DataStore`] 支持两种来源：
//!
//! 1. **Live**：接入了真实插件（生产形态，本仓库未启用）；
//! 2. **Demo**：内置示例数据（用于界面自测与截图）。
//!
//! 这不是"偷懒"，而是诚实：没有宿主装配，UI 无法凭空拿到真实实例列表。
//! Demo 数据与真实数据结构完全一致，接入宿主后只需换数据源。

use std::sync::{Arc, Mutex};

/// 一个实例（与 `official-instance` 的返回结构一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceView {
    pub id: String,
    pub name: String,
    pub game_version: String,
    pub loader: String,
    pub last_played: Option<String>,
}

/// 一个已安装 Mod（与 `official-mod` 的返回结构一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModView {
    pub filename: String,
    pub name: String,
    pub version: String,
}

/// 启动配置（与 `official-launch` 的返回结构一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfigView {
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    pub memory_mb: u32,
    pub is_default: bool,
}

/// UI 读取的完整快照。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub instances: Vec<InstanceView>,
    pub mods: Vec<ModView>,
    pub launch_config: Option<LaunchConfigView>,
    /// 上次操作的结果提示（显示在顶栏）。
    pub toast: Option<Toast>,
    /// 数据来源说明（"示例数据" / "实时"）。
    pub source: DataSource,
    /// 游戏运行日志（环形缓冲，最新在后）。
    pub log_lines: Vec<String>,
}

/// 数据来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataSource {
    /// 内置示例数据。
    #[default]
    Demo,
    /// 来自真实插件。
    Live,
}

/// 顶栏提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    /// 是否错误（决定颜色）。
    pub is_error: bool,
}

/// 线程安全的快照容器。
#[derive(Clone)]
pub struct DataStore {
    inner: Arc<Mutex<Snapshot>>,
}

impl DataStore {
    pub fn new_demo() -> Self {
        let s = Self {
            inner: Arc::new(Mutex::new(Snapshot::default())),
        };
        s.reset_demo();
        s
    }

    pub fn new_empty() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Snapshot::default())),
        }
    }

    /// 读取快照（克隆一份，避免持锁时做 UI 工作）。
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().expect("快照锁被毒化").clone()
    }

    /// 替换整个快照。
    pub fn set(&self, snap: Snapshot) {
        *self.inner.lock().expect("快照锁被毒化") = snap;
    }

    /// 修改快照。
    pub fn update<F: FnOnce(&mut Snapshot)>(&self, f: F) {
        let mut g = self.inner.lock().expect("快照锁被毒化");
        f(&mut g);
    }

    /// 显示一条提示（3 秒后由 UI 侧忽略；这里只存最新一条）。
    pub fn toast(&self, text: impl Into<String>, is_error: bool) {
        self.update(|s| {
            s.toast = Some(Toast {
                text: text.into(),
                is_error,
            })
        });
    }

    /// 重置为示例数据。
    pub fn reset_demo(&self) {
        let now = "2026-11-08T10:00:00Z".to_owned();
        self.set(Snapshot {
            instances: vec![
                InstanceView {
                    id: "inst-demo-001".into(),
                    name: "生存服".into(),
                    game_version: "1.20.1".into(),
                    loader: "fabric".into(),
                    last_played: Some(now.clone()),
                },
                InstanceView {
                    id: "inst-demo-002".into(),
                    name: "红石实验".into(),
                    game_version: "1.20.1".into(),
                    loader: "forge".into(),
                    last_played: Some("2026-11-07T09:30:00Z".into()),
                },
                InstanceView {
                    id: "inst-demo-003".into(),
                    name: "纯净原版".into(),
                    game_version: "1.21".into(),
                    loader: "vanilla".into(),
                    last_played: None,
                },
            ],
            mods: vec![
                ModView {
                    filename: "fabric-api-0.92.2.jar".into(),
                    name: "Fabric API".into(),
                    version: "0.92.2".into(),
                },
                ModView {
                    filename: "sodium-0.5.8.jar".into(),
                    name: "Sodium".into(),
                    version: "0.5.8".into(),
                },
                ModView {
                    filename: "jei-1.20.1-15.3.0.jar".into(),
                    name: "Just Enough Items".into(),
                    version: "15.3.0".into(),
                },
            ],
            launch_config: Some(LaunchConfigView {
                jvm_args: vec!["-XX:+UseG1GC".into(), "-XX:+UseStringDeduplication".into()],
                game_args: vec!["--fullscreen".into()],
                memory_mb: 6144,
                is_default: false,
            }),
            toast: None,
            source: DataSource::Demo,
            log_lines: vec![
                "[10:00:00] [INFO] 加载实例配置".into(),
                "[10:00:01] [INFO] 解析 JVM 参数 2 项".into(),
                "[10:00:02] [WARN] memory_mb=6144 超过推荐值 4096".into(),
                "[10:00:03] [INFO] 启动（simulated）：未创建任何 OS 进程".into(),
            ],
        });
    }

    // ---------------------------------------------------------------
    // 可写操作：让界面上的按钮真正改变数据，而不是只弹一条提示
    // ---------------------------------------------------------------

    /// 新建实例。校验规则与 `official-instance.create_instance` 保持一致，
    /// 便于以后把 DataStore 换成真实插件数据源时行为不变。
    pub fn add_instance(
        &self,
        name: &str,
        game_version: &str,
        loader: &str,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err("实例名需为 1..64 个字符".to_owned());
        }
        if name.contains('/')
            || name.contains('\\')
            || name.contains(':')
            || name.contains("..")
            || name.chars().any(|c| c.is_control())
        {
            return Err("实例名不能包含 / \\ : .. 或控制字符".to_owned());
        }
        let parts: Vec<&str> = game_version.trim().split('.').collect();
        if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.parse::<u32>().is_err()) {
            return Err("版本格式应为 x.y 或 x.y.z".to_owned());
        }
        if !matches!(loader.trim(), "vanilla" | "fabric" | "forge") {
            return Err("加载器需为 vanilla / fabric / forge".to_owned());
        }
        let mut id = String::new();
        self.update(|s| {
            let lower = name.to_lowercase();
            if s.instances.iter().any(|i| i.name.to_lowercase() == lower) {
                return; // 重名由调用方看成"未生成 id"
            }
            let mut n = s.instances.len() + 1;
            loop {
                let candidate = format!("inst-user-{n:04}");
                if !s.instances.iter().any(|i| i.id == candidate) {
                    id = candidate.clone();
                    break;
                }
                n += 1;
            }
            s.instances.push(InstanceView {
                id: id.clone(),
                name: name.to_owned(),
                game_version: game_version.trim().to_owned(),
                loader: loader.trim().to_owned(),
                last_played: None,
            });
        });
        if id.is_empty() {
            Err("已存在同名实例（不区分大小写）".to_owned())
        } else {
            Ok(id)
        }
    }

    /// 批量删除实例。
    pub fn remove_instances(&self, ids: &[String]) {
        self.update(|s| s.instances.retain(|i| !ids.contains(&i.id)));
    }

    /// 安装一个 Mod（占位安装：写一条记录，不下载文件）。
    pub fn add_mod(&self, filename: &str, name: &str, version: &str) -> Result<(), String> {
        let filename = filename.trim();
        if filename.is_empty() || name.trim().is_empty() {
            return Err("文件名与名称不能为空".to_owned());
        }
        let mut exists = false;
        self.update(|s| {
            exists = s.mods.iter().any(|m| m.filename == filename);
            if !exists {
                s.mods.push(ModView {
                    filename: filename.to_owned(),
                    name: name.trim().to_owned(),
                    version: version.trim().to_owned(),
                });
            }
        });
        if exists {
            Err("该 Mod 已安装".to_owned())
        } else {
            Ok(())
        }
    }

    /// 移除 Mod。
    pub fn remove_mod(&self, filename: &str) {
        self.update(|s| s.mods.retain(|m| m.filename != filename));
    }

    /// 保存启动配置。
    pub fn set_launch(&self, cfg: LaunchConfigView) {
        self.update(|s| s.launch_config = Some(cfg));
    }

    /// 追加一行日志（最多保留 200 行）。
    pub fn push_log(&self, line: impl Into<String>) {
        self.update(|s| {
            s.log_lines.push(line.into());
            if s.log_lines.len() > 200 {
                let drop = s.log_lines.len() - 200;
                s.log_lines.drain(0..drop);
            }
        });
    }

    /// 清掉提示。
    pub fn clear_toast(&self) {
        self.update(|s| s.toast = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_snapshot_has_content() {
        let d = DataStore::new_demo();
        let s = d.snapshot();
        assert_eq!(s.instances.len(), 3);
        assert_eq!(s.mods.len(), 3);
        assert!(s.launch_config.is_some());
        assert_eq!(s.source, DataSource::Demo);
    }

    #[test]
    fn toast_is_stored() {
        let d = DataStore::new_demo();
        d.toast("删除成功", false);
        let s = d.snapshot();
        let t = s.toast.unwrap();
        assert_eq!(t.text, "删除成功");
        assert!(!t.is_error);
    }

    #[test]
    fn update_mutates() {
        let d = DataStore::new_empty();
        d.update(|s| {
            s.instances.push(InstanceView {
                id: "x".into(),
                name: "n".into(),
                game_version: "1.20.1".into(),
                loader: "vanilla".into(),
                last_played: None,
            })
        });
        assert_eq!(d.snapshot().instances.len(), 1);
    }

    #[test]
    fn store_is_shared_across_clones() {
        let a = DataStore::new_empty();
        let b = a.clone();
        b.update(|s| s.source = DataSource::Live);
        assert_eq!(
            a.snapshot().source,
            DataSource::Live,
            "克隆应共享同一份数据"
        );
    }

    #[test]
    fn demo_instances_have_distinct_ids() {
        let s = DataStore::new_demo().snapshot();
        let mut ids = std::collections::BTreeSet::new();
        for i in &s.instances {
            assert!(ids.insert(i.id.clone()), "示例实例 ID 必须唯一");
        }
    }
}
