//! Mod 索引持久化：`mods.json`，按实例分组。
//!
//! # 存储形态
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "instances": {
//!     "inst-123-001": [
//!       { "filename": "fabric-api-0.90.jar", "name": "Fabric API", "version": "0.90.0",
//!         "project_id": "P7dR8mSH", "installed_at": "2026-11-07T13:53:45Z" }
//!     ]
//!   }
//! }
//! ```
//!
//! 用 `BTreeMap` 而不是 `HashMap`：保证 JSON 键序稳定，`instances.json` 的 diff
//! 在版本管理里才有意义（也便于人眼看）。
//!
//! # 为什么按实例分组的单一文件
//!
//! 一个插件的私有数据目录只有一个。按实例分文件会让"列出所有实例的 Mod"变成
//! 目录遍历 + N 次读盘；单一文件一次读写即可，代价是个体实例的 Mod 数量上限很小（几十个）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::{PluginContext, PluginResult};
use serde::{Deserialize, Serialize};

/// 一条已安装 Mod 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModEntry {
    /// 实例目录里的文件名（含扩展名）。**同一实例内唯一**。
    pub filename: String,
    /// 人类可读的 Mod 名。
    pub name: String,
    pub version: String,
    /// 来源项目的 ID（Modrinth 的 `project_id` / slug）。可能为空（手动放置的 Mod）。
    #[serde(default)]
    pub project_id: Option<String>,
    /// 安装时间（RFC3339 UTC）。
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModStore {
    pub schema_version: u32,
    /// instance_id → mods
    pub instances: BTreeMap<String, Vec<ModEntry>>,
}

impl Default for ModStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            instances: BTreeMap::new(),
        }
    }
}

const STORE_FILE: &str = "mods.json";
const CURRENT_SCHEMA: u32 = 1;

/// 一个实例的 Mod 数量上限。防止误点"安装"把索引撑爆。
pub const MAX_MODS_PER_INSTANCE: usize = 512;

pub struct ModIndex {
    path: PathBuf,
}

impl ModIndex {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(STORE_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ModStore, StoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(ModStore::default()),
            Ok(bytes) => {
                let s: ModStore = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                if s.schema_version > CURRENT_SCHEMA {
                    return Err(StoreError::TooNew(s.schema_version));
                }
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModStore::default()),
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }

    /// 原子写（临时文件 + rename），与 `official-instance` 同一策略。
    pub fn save(&self, store: &ModStore) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(store).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::write(&tmp, &bytes).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Io(String),
    Corrupt(String),
    TooNew(u32),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "存储 IO 失败：{m}"),
            Self::Corrupt(m) => write!(f, "mods.json 损坏：{m}"),
            Self::TooNew(v) => write!(
                f,
                "mods.json 的 schema_version={v} 高于当前支持的 {CURRENT_SCHEMA}"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

pub fn store_error_result(e: StoreError) -> PluginResult {
    let _ = biz_codes::STORAGE_ERROR;
    Ok(business(biz_codes::STORAGE_ERROR, e.to_string()))
}

/// 当前时间 RFC3339 UTC（与 `official-instance` 用同一算法，避免两个插件时间格式不一致）。
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, se) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

/// 校验 `instance_id` 的形状。
///
/// 这里只做**形状**校验（非空、无路径字符），不校验"实例是否真实存在" —
/// 后者需要跨插件调用 `official-instance.list_instances`，两个插件应能独立工作。
pub fn validate_instance_id(id: &str) -> Result<&str, String> {
    let t = id.trim();
    if t.is_empty() {
        return Err("instance_id 不能为空".to_owned());
    }
    if t.contains('/') || t.contains('\\') || t.contains("..") || t.contains(':') {
        return Err(format!(
            "instance_id `{t}` 含非法字符（不允许路径分隔符或 `..`）"
        ));
    }
    if t.len() > 128 {
        return Err("instance_id 过长（>128）".to_owned());
    }
    Ok(t)
}

/// 校验 Mod 文件名。
///
/// 只允许 `<name>.jar` / `<name>.jar.disabled`（不区分大小写）。
/// 拒绝路径分隔符：文件名会被拼进**磁盘路径**写占位文件，放行就是路径穿越。
pub fn validate_mod_filename(name: &str) -> Result<&str, String> {
    let t = name.trim();
    if t.is_empty() {
        return Err("filename 不能为空".to_owned());
    }
    if t.contains('/') || t.contains('\\') || t.contains("..") || t.contains(':') {
        return Err(format!("filename `{t}` 不能包含路径分隔符或 `..`"));
    }
    let lower = t.to_lowercase();
    if !(lower.ends_with(".jar") || lower.ends_with(".jar.disabled")) {
        return Err(format!("filename `{t}` 必须以 .jar 或 .jar.disabled 结尾"));
    }
    if t.len() > 200 {
        return Err("filename 过长（>200）".to_owned());
    }
    Ok(t)
}

/// Mod 在实例内的磁盘放置目录（相对插件数据目录）。
pub fn mods_dir_for(instance_id: &str) -> String {
    format!("instances/{instance_id}/mods")
}

/// 确保目录存在并返回绝对路径。
pub fn ensure_mods_dir(ctx: &PluginContext, instance_id: &str) -> Result<PathBuf, String> {
    let rel = mods_dir_for(instance_id);
    let dir = ctx.data_path(&rel).map_err(|e| e.message)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 Mod 目录失败：{e}"))?;
    Ok(dir)
}

/// 便捷宏：读 store，失败提前返回业务错误。
#[macro_export]
macro_rules! load_mod_store {
    ($ctx:expr) => {{
        let idx = $crate::store::ModIndex::new($ctx.data_dir());
        match idx.load() {
            ::core::result::Result::Ok(s) => s,
            ::core::result::Result::Err(e) => {
                $ctx.log_error(format!("读取 mods.json 失败：{e}"));
                return $crate::store::store_error_result(e);
            }
        }
    }};
}

/// 便捷宏：写 store，失败提前返回业务错误。
#[macro_export]
macro_rules! save_mod_store {
    ($ctx:expr, $store:expr) => {{
        let idx = $crate::store::ModIndex::new($ctx.data_dir());
        if let ::core::result::Result::Err(e) = idx.save($store) {
            $ctx.log_error(format!("写入 mods.json 失败：{e}"));
            return $crate::store::store_error_result(e);
        }
    }};
}

/// 生成占位文件内容（`install_mod` 演示用）。
pub fn placeholder_jar_content(project_id: &str) -> String {
    format!(
        "/* simulated jar placeholder\n\
         project_id = {project_id}\n\
         generated by official-mod (模拟安装，非真实 Mod 文件)\n\
         */\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        // 用共享临时目录（进程退出时统一清理）。
        let d = launcher_plugin_common::test_support::temp_path(tag);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(file: &str) -> ModEntry {
        ModEntry {
            filename: file.to_owned(),
            name: "n".to_owned(),
            version: "1".to_owned(),
            project_id: Some("P1".to_owned()),
            installed_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn missing_file_is_empty() {
        let d = tmp("missing");
        let s = ModIndex::new(&d);
        assert!(s.load().unwrap().instances.is_empty());
    }

    #[test]
    fn roundtrip_and_btreemap_order_is_stable() {
        let d = tmp("roundtrip");
        let s = ModIndex::new(&d);
        let mut store = ModStore::default();
        store
            .instances
            .insert("inst-b".into(), vec![entry("b.jar")]);
        store
            .instances
            .insert("inst-a".into(), vec![entry("a.jar")]);
        s.save(&store).unwrap();

        let raw = std::fs::read_to_string(s.path()).unwrap();
        // BTreeMap 的键序稳定：inst-a 应在 inst-b 之前
        let a = raw.find("inst-a").unwrap();
        let b = raw.find("inst-b").unwrap();
        assert!(a < b, "键序应当稳定（字典序），实际 a={a} b={b}");

        let back = s.load().unwrap();
        assert_eq!(back.instances.len(), 2);
    }

    #[test]
    fn corrupt_and_future_schema() {
        let d = tmp("corrupt");
        let s = ModIndex::new(&d);
        std::fs::write(s.path(), b"{{").unwrap();
        assert!(matches!(s.load(), Err(StoreError::Corrupt(_))));

        std::fs::write(s.path(), br#"{"schema_version":9,"instances":{}}"#).unwrap();
        assert!(matches!(s.load(), Err(StoreError::TooNew(9))));
    }

    #[test]
    fn validate_instance_id_rejects_traversal() {
        assert!(validate_instance_id("inst-123-001").is_ok());
        for bad in ["", "   ", "../x", "a/b", "a\\b", "C:"] {
            assert!(validate_instance_id(bad).is_err(), "`{bad}` 应被拒");
        }
        // 过长
        let long = "a".repeat(129);
        assert!(validate_instance_id(&long).is_err());
    }

    #[test]
    fn validate_mod_filename_requires_jar_suffix() {
        assert!(validate_mod_filename("fabric-api.jar").is_ok());
        assert!(
            validate_mod_filename("Fabric-API.JAR").is_ok(),
            "大小写不应影响"
        );
        assert!(validate_mod_filename("old.jar.disabled").is_ok());
        for bad in [
            "",
            "noext",
            "x.txt",
            "../evil.jar",
            "a/b.jar",
            "a\\b.jar",
            "C:x.jar",
        ] {
            assert!(validate_mod_filename(bad).is_err(), "`{bad}` 应被拒");
        }
    }

    #[test]
    fn mods_dir_is_scoped_under_instance() {
        assert_eq!(mods_dir_for("inst-1"), "instances/inst-1/mods");
    }

    #[test]
    fn placeholder_content_is_clearly_fake() {
        let c = placeholder_jar_content("P1");
        assert!(c.contains("simulated"), "必须明确标注模拟");
        assert!(c.contains("P1"));
        assert!(!c.contains("PK"), "不能长得像真的 zip/jar");
    }

    #[test]
    fn rfc3339_shape() {
        let t = now_rfc3339();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z'));
    }
}
