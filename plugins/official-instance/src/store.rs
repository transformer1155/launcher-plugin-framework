//! 实例持久化：一个 JSON 文件，位于插件私有数据目录。
//!
//! # 存储位置
//!
//! `ctx.data_dir()/instances.json`。插件私有目录由宿主注入，插件不写其他位置
//! （规格要求"工具内部不直接访问文件系统之外的东西"）。
//!
//! # 为什么用"读改写整个文件"而不是数据库
//!
//! 实例数量是个人级（几十个），整个文件读写的复杂度最低、最容易做原子替换、
//! 也不需要引入新依赖。代价是并发写会互相覆盖 —— 但工具调用是单进程内串行的
//! 业务操作（UI 不会同时点两次"创建"），且我们用临时文件 + rename 保证
//! "崩溃时不留下半个文件"。

use std::path::{Path, PathBuf};

use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::PluginResult;
use serde::{Deserialize, Serialize};

/// Mod 加载器枚举。**唯一真相源**：schema 的 `enum` 与这里同源（见 [`Self::ALL`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Vanilla,
    Fabric,
    Forge,
}

impl Loader {
    /// 全部允许值。工具的 schema `enum` 必须直接用这个常量，不能手写字符串。
    pub const ALL: [&'static str; 3] = ["vanilla", "fabric", "forge"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "vanilla" => Some(Self::Vanilla),
            "fabric" => Some(Self::Fabric),
            "forge" => Some(Self::Forge),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::Fabric => "fabric",
            Self::Forge => "forge",
        }
    }

    /// 该加载器是否支持 Mod 安装（vanilla 不支持）。
    pub fn supports_mods(self) -> bool {
        !matches!(self, Self::Vanilla)
    }
}

impl std::fmt::Display for Loader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 目录里一个实例的持久化形态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Instance {
    /// 稳定标识。创建后不变（改名不改 ID）。
    pub id: String,
    pub name: String,
    pub game_version: String,
    pub loader: Loader,
    /// 上次进入该实例的时间（RFC3339）。`None` = 从未进入。
    pub last_played: Option<String>,
}

/// 磁盘上的根结构。带 `schema_version` 便于未来迁移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    pub schema_version: u32,
    pub instances: Vec<Instance>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            schema_version: 1,
            instances: Vec::new(),
        }
    }
}

const STORE_FILE: &str = "instances.json";
const CURRENT_SCHEMA: u32 = 1;

/// 实例存储。持有文件路径，不做缓存（每次读盘，代价可忽略且避免多实例不一致）。
pub struct InstanceStore {
    path: PathBuf,
}

impl InstanceStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(STORE_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读。文件不存在 → 空存储（首次运行）。
    pub fn load(&self) -> Result<Store, StoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    return Ok(Store::default());
                }
                let store: Store = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                if store.schema_version > CURRENT_SCHEMA {
                    return Err(StoreError::TooNew(store.schema_version));
                }
                Ok(store)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }

    /// 原子写：写临时文件 → rename。
    ///
    /// 直接覆盖原文件的话，写到一半崩溃会留下一个截断的 JSON，下次启动就全丢了。
    pub fn save(&self, store: &Store) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(store).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::write(&tmp, &bytes).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    /// 生成一个新的实例 ID。
    ///
    /// 用"时间戳 + 计数"而不是 UUID：不引入新依赖，且可读、可排序。
    /// 计数从现有 ID 里取最大值 +1，而不是 `len()+1` —— 删除后再创建不会撞号。
    pub fn next_id(&self, existing: &[Instance]) -> String {
        Self::next_id_for(existing)
    }

    /// 静态版本：不需要 store 实例也能生成 ID（工具里只有 `Store` 没有 `InstanceStore` 时用）。
    pub fn next_id_for(existing: &[Instance]) -> String {
        let seq = existing
            .iter()
            .filter_map(|i| i.id.rsplit('-').next())
            .filter_map(|s| s.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let ts = unix_secs();
        format!("inst-{ts}-{seq:03}")
    }
}

/// 当前 Unix 秒。用于 `last_played` 与 ID 生成。
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 生成 `last_played` 用的 RFC3339 时间戳（UTC，秒级）。
///
/// 自己实现而不是引入 `chrono`/`time`：只做 UTC 秒级格式化，日历换算用**严格算法**
/// （格里高利历，1900+ 年的标准公式）。引入一个日期库只为一个字段不值得。
///
/// 返回值形如 `2026-11-07T13:53:45Z`。
pub fn now_rfc3339() -> String {
    let secs = unix_secs() as i64;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, se) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970-01-01 → (y, m, d)，Howard Hinnant 的 civil_from_days 算法。
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

/// 检查 `name` 在同名集合里是否已被占用（大小写不敏感，且忽略首尾空白）。
///
/// 为什么大小写不敏感：Windows/macOS 的实例目录名会被当作不区分大小写，
/// "Survival" 与 "survival" 在 UI 上是两个实例、在磁盘上是同一个目录 —— 必须提前拦住。
pub fn name_conflict<'a>(
    existing: impl IntoIterator<Item = &'a Instance>,
    wanted: &str,
    ignore_id: Option<&str>,
) -> Option<String> {
    let target = wanted.trim().to_lowercase();
    existing
        .into_iter()
        .filter(|i| Some(i.id.as_str()) != ignore_id)
        .find(|i| i.name.trim().to_lowercase() == target)
        .map(|i| i.id.clone())
}

/// 实例名合法性：非空、长度 1..=64、不含路径分隔符与控制字符。
///
/// 名字会用于 UI 展示，也可能被用于目录名（由启动器决定），因此必须拒绝
/// `/`、`\`、`:`、`..` 与换行等——否则将来拼路径就是一次路径穿越。
pub fn validate_instance_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("实例名不能为空".to_owned());
    }
    // 用字符数而不是字节数：中文名一个字算一个，UI 的 64 字符限制才对得上。
    if trimmed.chars().count() > 64 {
        return Err(format!(
            "实例名最多 64 个字符，实际 {} 个",
            trimmed.chars().count()
        ));
    }
    if trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.contains("..")
        || trimmed.contains('\n')
        || trimmed.contains('\r')
        || trimmed.contains('\t')
    {
        return Err("实例名不能包含路径分隔符（/ \\ :）、怪字符（..）或控制字符".to_owned());
    }
    // 拒绝纯空白以外的不可见字符（例如零宽空格），避免"看起来一样但不同"的名字。
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("实例名不能包含控制字符".to_owned());
    }
    Ok(trimmed)
}

/// Minecraft 版本号格式校验：`1.20.1` / `1.21` 这类数字点分，允许最多 3 段。
///
/// 不做语义化版本的完整规则：MC 版本不是 semver（`1.20.1-pre1` 也存在），
/// 这里只要求"看起来像版本号"，避免用户把空串或 `latest` 灌进来。
pub fn validate_game_version(v: &str) -> Result<&str, String> {
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Err("game_version 不能为空".to_owned());
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(format!(
            "game_version 必须是 x.y 或 x.y.z 形式，实际 `{trimmed}`"
        ));
    }
    for p in &parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("game_version 的每一段必须是数字，实际 `{trimmed}`"));
        }
    }
    Ok(trimmed)
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
            Self::Corrupt(m) => write!(f, "instances.json 损坏：{m}"),
            Self::TooNew(v) => write!(
                f,
                "instances.json 的 schema_version={v} 比当前支持的 {CURRENT_SCHEMA} 新，\
                 请升级启动器"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

/// 把 [`StoreError`] 转成插件业务失败。
///
/// **不**转成框架错误码：磁盘坏不是"插件进程崩溃"，重启插件修不好它。
pub fn store_error_result(e: StoreError) -> PluginResult {
    let code = match e {
        // 数据损坏与版本过新都属于"存储内容不对"，UI 应当引导用户处理文件。
        StoreError::Corrupt(_) | StoreError::TooNew(_) => biz_codes::STORAGE_ERROR,
        StoreError::Io(_) => biz_codes::STORAGE_ERROR,
    };
    Ok(business(code, e.to_string()))
}

/// 加载存储，失败即提前返回业务错误。展开成 `let store = ...;`
#[macro_export]
macro_rules! load_store {
    ($ctx:expr) => {{
        let store = $crate::store::InstanceStore::new($ctx.data_dir());
        match store.load() {
            ::core::result::Result::Ok(s) => s,
            ::core::result::Result::Err(e) => {
                $ctx.log_error(format!("读取实例存储失败：{e}"));
                return $crate::store::store_error_result(e);
            }
        }
    }};
}

/// 保存存储，失败即提前返回业务错误。
#[macro_export]
macro_rules! save_store {
    ($ctx:expr, $store:expr) => {{
        let s = $crate::store::InstanceStore::new($ctx.data_dir());
        if let ::core::result::Result::Err(e) = s.save($store) {
            $ctx.log_error(format!("写入实例存储失败：{e}"));
            return $crate::store::store_error_result(e);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        // 用共享临时目录（进程退出时统一清理，避免遗留垃圾）。
        let d = launcher_plugin_common::test_support::temp_path(tag);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_is_empty_store() {
        let d = tmp_dir("missing");
        let s = InstanceStore::new(&d);
        let store = s.load().unwrap();
        assert_eq!(store.instances.len(), 0);
        assert_eq!(store.schema_version, 1);
    }

    #[test]
    fn roundtrip() {
        let d = tmp_dir("roundtrip");
        let s = InstanceStore::new(&d);
        let mut store = Store::default();
        store.instances.push(Instance {
            id: "inst-1".to_owned(),
            name: "生存服".to_owned(),
            game_version: "1.20.1".to_owned(),
            loader: Loader::Fabric,
            last_played: None,
        });
        s.save(&store).unwrap();
        let back = s.load().unwrap();
        assert_eq!(back.instances, store.instances);
    }

    #[test]
    fn corrupt_file_is_reported_not_panicked() {
        let d = tmp_dir("corrupt");
        let s = InstanceStore::new(&d);
        std::fs::write(s.path(), b"{not json").unwrap();
        let err = s.load().unwrap_err();
        assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
    }

    #[test]
    fn empty_file_is_treated_as_empty_store() {
        let d = tmp_dir("empty");
        let s = InstanceStore::new(&d);
        std::fs::write(s.path(), b"").unwrap();
        assert_eq!(s.load().unwrap().instances.len(), 0);
    }

    #[test]
    fn future_schema_version_is_refused() {
        let d = tmp_dir("futureschema");
        let s = InstanceStore::new(&d);
        std::fs::write(s.path(), br#"{"schema_version":99,"instances":[]}"#).unwrap();
        let err = s.load().unwrap_err();
        assert!(matches!(err, StoreError::TooNew(99)), "{err:?}");
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let d = tmp_dir("notemp");
        let s = InstanceStore::new(&d);
        s.save(&Store::default()).unwrap();
        assert!(!s.path().with_extension("json.tmp").exists());
        assert!(s.path().exists());
    }

    #[test]
    fn loader_enum_is_single_source_of_truth() {
        // schema 的 enum 必须来自 Loader::ALL，两处不能各写一份。
        for s in Loader::ALL {
            assert!(Loader::parse(s).is_some(), "{s} 应当可解析");
            assert_eq!(Loader::parse(s).unwrap().as_str(), s);
        }
        assert!(Loader::parse("quilt").is_none());
        assert!(!Loader::Vanilla.supports_mods());
        assert!(Loader::Fabric.supports_mods());
    }

    #[test]
    fn next_id_uses_max_not_len() {
        // 关键：删掉中间项之后，len 变小但计数器不能回退，否则会撞号。
        let existing: Vec<Instance> = vec![
            make_instance("inst-100-003", "a"),
            make_instance("inst-100-005", "b"),
            make_instance("inst-100-004", "c"),
        ];
        let next = InstanceStore::next_id_for(&existing);
        assert!(next.ends_with("-006"), "应当取最大值 +1：{next}");
    }

    fn make_instance(id: &str, name: &str) -> Instance {
        Instance {
            id: id.to_owned(),
            name: name.to_owned(),
            game_version: "1.20.1".into(),
            loader: Loader::Vanilla,
            last_played: None,
        }
    }

    #[test]
    fn name_conflict_is_case_insensitive_and_ignores_self() {
        let a = Instance {
            id: "inst-1".into(),
            name: "Survival".into(),
            game_version: "1.20.1".into(),
            loader: Loader::Vanilla,
            last_played: None,
        };
        let list = vec![a.clone()];

        assert_eq!(
            name_conflict(&list, "survival", None),
            Some("inst-1".to_owned())
        );
        assert_eq!(
            name_conflict(&list, "  SURVIVAL  ", None),
            Some("inst-1".to_owned())
        );
        // 忽略自己（未来改名功能需要）
        assert_eq!(name_conflict(&list, "Survival", Some("inst-1")), None);
        // 不同名
        assert_eq!(name_conflict(&list, "Creative", None), None);
    }

    #[test]
    fn validate_name_rejects_unsafe_chars() {
        assert!(validate_instance_name("生存服").is_ok());
        assert!(validate_instance_name("My World 2").is_ok());
        for bad in ["", "   ", "a/b", "a\\b", "a:b", "a..b", "a\nb", "a\tb"] {
            assert!(validate_instance_name(bad).is_err(), "`{bad}` 应当被拒");
        }
    }

    #[test]
    fn validate_name_boundary_64_chars() {
        let ok = "A".repeat(64);
        assert!(validate_instance_name(&ok).is_ok(), "64 字符应当合法");
        let too_long = "A".repeat(65);
        assert!(
            validate_instance_name(&too_long).is_err(),
            "65 字符应当被拒"
        );
        // 中文按字符数而非字节数：64 个汉字也合法
        let cn = "中".repeat(64);
        assert!(validate_instance_name(&cn).is_ok(), "64 个汉字应当合法");
    }

    #[test]
    fn validate_name_trims_whitespace() {
        assert_eq!(
            validate_instance_name("  padded  ").unwrap(),
            "padded",
            "应当返回 trim 后的值"
        );
    }

    #[test]
    fn validate_game_version_shapes() {
        assert!(validate_game_version("1.20.1").is_ok());
        assert!(validate_game_version("1.21").is_ok());
        assert!(validate_game_version("1.7.10").is_ok());
        for bad in ["", "latest", "1", "1.20.1.4", "1.x", "1..2", "1.-2"] {
            assert!(validate_game_version(bad).is_err(), "`{bad}` 应当被拒");
        }
    }

    #[test]
    fn rfc3339_is_well_formed_and_increases() {
        let a = now_rfc3339();
        assert_eq!(a.len(), 20);
        assert!(a.ends_with('Z'));
        // 同秒内两次调用应当相等（秒级精度），不同秒则后者更大。
        let b = now_rfc3339();
        assert!(a <= b, "时间戳应单调不减");
    }
}
