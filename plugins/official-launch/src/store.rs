//! 启动配置与状态持久化。
//!
//! # 存储形态（`launch.json`）
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "configs": {
//!     "inst-123-001": {
//!       "jvm_args": ["-XX:+UseG1GC"],
//!       "game_args": ["--fullscreen"],
//!       "memory_mb": 4096
//!     }
//!   }
//! }
//! ```
//!
//! # 为什么配置与实例索引分开存
//!
//! `official-instance` 管"有哪些实例"，`official-launch` 管"怎么启动它们"。
//! 分开存让两个插件可以独立加载/卸载，且改启动参数不会重写实例索引（减少冲突面）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use launcher_plugin_common::result::{biz_codes, business};
use launcher_plugin_common::PluginResult;
use serde::{Deserialize, Serialize};

/// 默认内存（MB）。4GB 是 Minecraft 现代版本的常见起点。
pub const DEFAULT_MEMORY_MB: u32 = 4096;

/// 内存下限：低于 1024 起不来（现代 MC 至少需要约 2GB，1GB 仅够极轻量整合）。
pub const MIN_MEMORY_MB: u32 = 512;
/// 内存上限：32768（32GB）。超过这个值几乎肯定填错（把 MB 当 GB 写了）。
pub const MAX_MEMORY_MB: u32 = 32_768;

/// 单个参数数组的长度上限。防止有人把整个 classpath 塞进来。
pub const MAX_ARGS: usize = 256;
/// 单个参数的长度上限。
pub const MAX_ARG_LEN: usize = 1024;

/// 一个实例的启动配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchConfig {
    /// JVM 参数（例如 `-XX:+UseG1GC`）。
    pub jvm_args: Vec<String>,
    /// 传给游戏本体的参数（例如 `--fullscreen`）。
    pub game_args: Vec<String>,
    /// 最大堆内存（MB）。
    pub memory_mb: u32,
    /// Java 可执行文件绝对路径。`None` 时回退到 `java`（依赖 PATH）。
    #[serde(default)]
    pub java_path: Option<String>,
    /// 游戏本体的标准参数（由 `install_minecraft` 写入：--gameDir/--assetsDir 等）。
    /// 与用户自定义的 `game_args` 分开存，避免设置启动参数时被覆盖丢失。
    #[serde(default)]
    pub mc_args: Vec<String>,
    /// 启动入口：决定 `java [jvm_args]` 之后追加什么参数。
    /// 默认 `None` 表示"探测式启动"（`java -version`），只用来验证真实进程确实能被 spawn。
    #[serde(default)]
    pub entry: LaunchEntry,
    /// natives 解压目录（`install_minecraft` 写入：LWJGL/JNA 的 Windows DLL 都在这里）。
    /// 启动时转成 `-Djava.library.path` / `-Dorg.lwjgl.librarypath` JVM 参数。
    #[serde(default)]
    pub natives_dir: Option<String>,
}

/// 启动入口：决定 `java [jvm_args]` 之后追加什么。
///
/// 默认 `None` 只做一次真实的 `java -version` 探测（返回真实 PID、立即 exit 0），
/// 证明"真实 spawn Java 进程"这条路已打通。要真正进游戏，请把入口配成
/// [`LaunchEntry::Jar`] 或 [`LaunchEntry::MainClass`]，指向真实的客户端 jar / 主类。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", content = "value")]
pub enum LaunchEntry {
    /// 未配置：启动器用 `java -version` 做一次真实但无害的探测启动。
    #[default]
    None,
    /// 运行一个 jar：`java [args] -jar <path>`。
    Jar(String),
    /// 运行主类：`java [args] -cp <classpath> <class>`。
    MainClass { class: String, classpath: String },
    /// 原始参数：直接追加到 `java [jvm_args]` 之后（game_args 也会追加）。
    Raw(Vec<String>),
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            jvm_args: vec!["-XX:+UseG1GC".to_owned()],
            game_args: Vec::new(),
            mc_args: Vec::new(),
            memory_mb: DEFAULT_MEMORY_MB,
            java_path: None,
            entry: LaunchEntry::None,
            natives_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchStore {
    pub schema_version: u32,
    /// instance_id → config。BTreeMap 保证键序稳定。
    pub configs: BTreeMap<String, LaunchConfig>,
}

impl Default for LaunchStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            configs: BTreeMap::new(),
        }
    }
}

const STORE_FILE: &str = "launch.json";
const CURRENT_SCHEMA: u32 = 1;

pub struct LaunchIndex {
    path: PathBuf,
}

impl LaunchIndex {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(STORE_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LaunchStore, StoreError> {
        match std::fs::read(&self.path) {
            Ok(b) if b.is_empty() => Ok(LaunchStore::default()),
            Ok(b) => {
                let s: LaunchStore =
                    serde_json::from_slice(&b).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                if s.schema_version > CURRENT_SCHEMA {
                    return Err(StoreError::TooNew(s.schema_version));
                }
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LaunchStore::default()),
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }

    pub fn save(&self, store: &LaunchStore) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(store).map_err(|e| StoreError::Io(e.to_string()))?;
        launcher_plugin_common::fsutil::atomic_write(&self.path, &bytes).map_err(StoreError::Io)
    }
}

// ----------------------------------------------------------------------------
// 当前已启动的游戏进程（进程级槽位）
//
// 真实 spawn 后需要持续持有 `Child` 句柄，以便在 UI 轮询真实退出码。
// 这是单实例启动器的进程级状态，用一个 `static` 槽位持有即可。
// ----------------------------------------------------------------------------

static CURRENT_CHILD: std::sync::LazyLock<std::sync::Mutex<Option<std::process::Child>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// 把刚 spawn 的子进程存入槽位并返回其真实 PID。
pub fn store_child(child: std::process::Child) -> u32 {
    let pid = child.id();
    *CURRENT_CHILD.lock().expect("child slot poisoned") = Some(child);
    pid
}

// ----------------------------------------------------------------------------
// 游戏进程输出环形缓冲
//
// Java 子进程的 stdout/stderr 由后台线程读取后写入这里；UI 每帧 `drain_game_log`
// 取走增量，转发到启动器的日志管理器。容量上限防止内存无限增长。
// ----------------------------------------------------------------------------

static GAME_LOG: std::sync::LazyLock<std::sync::Mutex<std::collections::VecDeque<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::VecDeque::new()));

const GAME_LOG_CAP: usize = 800;

/// 追加一行游戏输出（超出容量丢弃最旧的）。
pub fn push_game_log(line: String) {
    let mut g = GAME_LOG.lock().expect("game log poisoned");
    if g.len() >= GAME_LOG_CAP {
        g.pop_front();
    }
    g.push_back(line);
}

/// 取走全部未读的游戏输出行。
pub fn drain_game_log() -> Vec<String> {
    let mut g = GAME_LOG.lock().expect("game log poisoned");
    g.drain(..).collect()
}

/// 资源对象下载进度（含瞬时速度采样）。
///
/// `install_minecraft` 下载循环中持续更新；结束后置 `None`。
/// UI 的 `tick` 每帧读取，转成下载中心里的任务进度。
#[derive(Debug, Clone)]
pub struct AssetProgress {
    pub instance: String,
    pub done: u64,
    pub total: u64,
    /// 瞬时速度（KB/s）：按 ≥500ms 时间窗对字节数差分采样得到。
    /// 下载循环每完成一个文件都会 set 一次，不能每次都算（会抖动），窗口内沿用上次值。
    pub speed_kb: u64,
    /// 上次速度采样的字节数与时刻（差分基准）。
    sample_done: u64,
    sample_at: std::time::Instant,
}

/// 速度采样窗口：太短会跟着单个文件的字节跳变抖动，太长会迟钝。
const SPEED_SAMPLE_MS: u128 = 500;

static ASSET_PROGRESS: std::sync::LazyLock<std::sync::Mutex<Option<AssetProgress>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

pub fn set_asset_progress(instance: &str, done: u64, total: u64) {
    let now = std::time::Instant::now();
    let mut g = ASSET_PROGRESS.lock().expect("asset progress poisoned");
    let replace = match g.as_mut() {
        // 同一实例：更新进度，并按时间窗差分采样瞬时速度。
        Some(p) if p.instance == instance => {
            let dt = now.duration_since(p.sample_at);
            if dt.as_millis() >= SPEED_SAMPLE_MS && done >= p.sample_done {
                p.speed_kb = (done - p.sample_done)
                    .saturating_mul(1000)
                    / dt.as_millis().max(1) as u64
                    / 1024;
                p.sample_done = done;
                p.sample_at = now;
            }
            p.done = done;
            p.total = total;
            false
        }
        // 首帧或实例切换：重建采样基准，速度归零（不跨实例串速度）。
        _ => true,
    };
    if replace {
        *g = Some(AssetProgress {
            instance: instance.to_owned(),
            done,
            total,
            speed_kb: 0,
            sample_done: done,
            sample_at: now,
        });
    }
}

pub fn clear_asset_progress() {
    *ASSET_PROGRESS.lock().expect("asset progress poisoned") = None;
}

/// 当前进度：`(实例 id, 已完成字节, 总字节, 瞬时速度 KB/s)`。
pub fn asset_progress() -> Option<(String, u64, u64, u64)> {
    ASSET_PROGRESS
        .lock()
        .expect("asset progress poisoned")
        .as_ref()
        .map(|p| (p.instance.clone(), p.done, p.total, p.speed_kb))
}

/// 当前已启动进程的退出码（若已退出）；`None` 表示仍在运行或从未启动。
pub fn launch_status() -> Option<i32> {
    let mut g = CURRENT_CHILD.lock().expect("child slot poisoned");
    match g.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(Some(status)) => status.code(),
            Ok(None) => None,
            Err(_) => Some(-1),
        },
        None => None,
    }
}

/// 当前已启动进程的真实 PID（若还在运行）。
pub fn current_pid() -> Option<u32> {
    CURRENT_CHILD
        .lock()
        .expect("child slot poisoned")
        .as_ref()
        .map(|c| c.id())
}

/// 尝试杀死当前已启动的游戏进程（取消启动 / 停止运行时用）。
/// 返回是否真的存在一个被杀死的进程。
pub fn kill_child() -> bool {
    let mut g = CURRENT_CHILD.lock().expect("child slot poisoned");
    if let Some(mut child) = g.take() {
        match child.kill() {
            Ok(()) => {
                let _ = child.wait();
                true
            }
            Err(_) => false,
        }
    } else {
        false
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
            Self::Corrupt(m) => write!(f, "launch.json 损坏：{m}"),
            Self::TooNew(v) => write!(
                f,
                "launch.json 的 schema_version={v} 高于当前支持的 {CURRENT_SCHEMA}"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

pub fn store_error_result(e: StoreError) -> PluginResult {
    Ok(business(biz_codes::STORAGE_ERROR, e.to_string()))
}

// ----------------------------------------------------------------------------
// 校验
// ----------------------------------------------------------------------------

/// 校验 `instance_id` 形状（与其它插件同一规则，避免跨插件行为不一致）。
pub fn validate_instance_id(id: &str) -> Result<&str, String> {
    let t = id.trim();
    if t.is_empty() {
        return Err("instance_id 不能为空".to_owned());
    }
    if t.contains('/') || t.contains('\\') || t.contains("..") || t.contains(':') {
        return Err(format!("instance_id `{t}` 含非法字符"));
    }
    if t.len() > 128 {
        return Err("instance_id 过长（>128）".to_owned());
    }
    Ok(t)
}

/// 校验内存值。
pub fn validate_memory_mb(mb: u32) -> Result<u32, String> {
    if !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&mb) {
        return Err(format!(
            "memory_mb 必须在 {MIN_MEMORY_MB}..={MAX_MEMORY_MB} 之间，实际 {mb}"
        ));
    }
    Ok(mb)
}

/// 校验一组启动参数。
///
/// 拒绝项都是有理由的：
/// - 空字符串 → 传给 JVM 会成为幻觉参数
/// - 含换行 → 会被某些 shell 拼成两条命令
/// - 超长/超多 → 防止把命令行撑爆
pub fn validate_args(args: &[String], what: &str) -> Result<(), String> {
    if args.len() > MAX_ARGS {
        return Err(format!("{what} 最多 {MAX_ARGS} 项，实际 {}", args.len()));
    }
    for (i, a) in args.iter().enumerate() {
        if a.trim().is_empty() {
            return Err(format!("{what}[{i}] 不能为空字符串"));
        }
        if a.len() > MAX_ARG_LEN {
            return Err(format!(
                "{what}[{i}] 过长（{} 字节 > {MAX_ARG_LEN}）",
                a.len()
            ));
        }
        if a.contains('\n') || a.contains('\r') {
            return Err(format!("{what}[{i}] 不能包含换行符"));
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// 日志
// ----------------------------------------------------------------------------

/// 实例日志文件路径（相对插件数据目录）。
pub fn log_path_for(instance_id: &str) -> String {
    format!("instances/{instance_id}/logs/latest.log")
}

/// 读取日志尾部 `lines` 行。
///
/// # 为什么不是 `read_to_string` + `split('\n')`
///
/// 日志可以上百 MB。整读会：
/// - 占用大量内存（沙箱有返回值体积配额，读 100MB 再取 50 行是浪费）；
/// - 在沙箱里可能触发 `max_output_bytes`。
///
/// 这里只读文件尾部若干字节，再按行切。为了简单与正确性，采用"读尾部 N KB"策略：
/// 每行按平均 200 字节估算，读 `lines * 200 * 4` 字节（4 倍余量），若不够就翻倍重试。
pub fn read_log_tail(path: &Path, lines: usize) -> Result<(String, bool), String> {
    use std::io::{Read, Seek, SeekFrom};

    const HARD_CAP: u64 = 4 * 1024 * 1024; // 最多读 4MB，防止病态文件

    let mut f = std::fs::File::open(path).map_err(|e| format!("打开日志失败：{e}"))?;
    let len = f
        .metadata()
        .map_err(|e| format!("读取日志元信息失败：{e}"))?
        .len();
    if len == 0 {
        return Ok((String::new(), false));
    }

    let mut want: u64 = (lines as u64).saturating_mul(200).saturating_mul(4);
    want = want.max(4096).min(len).min(HARD_CAP);

    loop {
        let start = len.saturating_sub(want);
        f.seek(SeekFrom::Start(start))
            .map_err(|e| format!("定位日志失败：{e}"))?;
        let mut buf = Vec::with_capacity(want as usize);
        f.read_to_end(&mut buf)
            .map_err(|e| format!("读取日志失败：{e}"))?;

        // 若起点不是文件开头，第一行可能是被截断的半个行，丢弃它。
        let truncated_head = start > 0;
        let text = String::from_utf8_lossy(&buf).to_string();
        let mut collected: Vec<&str> = text.split('\n').collect();
        if truncated_head && collected.len() > 1 {
            collected.remove(0);
        }

        // 末尾的空行（文件以 \n 结尾）不应当占一行。
        while collected.last().is_some_and(|l| l.is_empty()) {
            collected.pop();
        }

        if collected.len() >= lines || start == 0 || want >= HARD_CAP {
            let start_idx = collected.len().saturating_sub(lines);
            let out = collected[start_idx..].join("\n");
            return Ok((out, start == 0));
        }
        // 不够：翻倍再读
        want = (want.saturating_mul(2)).min(len).min(HARD_CAP);
    }
}

/// 当前时间 RFC3339 UTC（三个插件统一格式）。
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

#[macro_export]
macro_rules! load_launch_store {
    ($ctx:expr) => {{
        let idx = $crate::store::LaunchIndex::new($ctx.data_dir());
        match idx.load() {
            ::core::result::Result::Ok(s) => s,
            ::core::result::Result::Err(e) => {
                $ctx.log_error(format!("读取 launch.json 失败：{e}"));
                return $crate::store::store_error_result(e);
            }
        }
    }};
}

#[macro_export]
macro_rules! save_launch_store {
    ($ctx:expr, $store:expr) => {{
        let idx = $crate::store::LaunchIndex::new($ctx.data_dir());
        if let ::core::result::Result::Err(e) = idx.save($store) {
            $ctx.log_error(format!("写入 launch.json 失败：{e}"));
            return $crate::store::store_error_result(e);
        }
    }};
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

    #[test]
    fn missing_is_default() {
        let d = tmp("missing");
        let s = LaunchIndex::new(&d);
        assert!(s.load().unwrap().configs.is_empty());
    }

    #[test]
    fn roundtrip() {
        let d = tmp("roundtrip");
        let s = LaunchIndex::new(&d);
        let mut store = LaunchStore::default();
        store.configs.insert(
            "inst-1".into(),
            LaunchConfig {
                jvm_args: vec!["-Xmx4G".into()],
                game_args: vec!["--fullscreen".into()],
                memory_mb: 4096,
                ..Default::default()
            },
        );
        s.save(&store).unwrap();
        let back = s.load().unwrap();
        assert_eq!(back.configs.get("inst-1").unwrap().memory_mb, 4096);
        assert_eq!(back.configs.get("inst-1").unwrap().jvm_args, vec!["-Xmx4G"]);
    }

    #[test]
    fn memory_bounds() {
        assert!(validate_memory_mb(MIN_MEMORY_MB).is_ok(), "下界合法");
        assert!(validate_memory_mb(MAX_MEMORY_MB).is_ok(), "上界合法");
        assert!(validate_memory_mb(1024).is_ok());
        assert!(
            validate_memory_mb(MIN_MEMORY_MB - 1).is_err(),
            "低于下界应失败"
        );
        assert!(
            validate_memory_mb(MAX_MEMORY_MB + 1).is_err(),
            "高于上界应失败"
        );
        assert!(validate_memory_mb(0).is_err());
        // 典型误填：把 GB 当 MB 写
        assert!(validate_memory_mb(65536).is_err());
    }

    #[test]
    fn arg_validation() {
        assert!(validate_args(&[], "jvm_args").is_ok());
        assert!(validate_args(&["-XX:+UseG1GC".into()], "jvm_args").is_ok());

        assert!(
            validate_args(&["".into()], "jvm_args").is_err(),
            "空串应被拒"
        );
        assert!(
            validate_args(&["  ".into()], "jvm_args").is_err(),
            "纯空白应被拒"
        );
        assert!(
            validate_args(&["-Da=1\n-Db=2".into()], "jvm_args").is_err(),
            "换行应被拒"
        );

        let huge = "x".repeat(MAX_ARG_LEN + 1);
        assert!(validate_args(&[huge], "jvm_args").is_err(), "超长应被拒");

        let many: Vec<String> = (0..=MAX_ARGS).map(|i| format!("-a{i}")).collect();
        assert!(validate_args(&many, "jvm_args").is_err(), "超多应被拒");
    }

    #[test]
    fn instance_id_rules() {
        assert!(validate_instance_id("inst-1").is_ok());
        for bad in ["", "  ", "../x", "a/b", "a\\b", "C:"] {
            assert!(validate_instance_id(bad).is_err(), "`{bad}`");
        }
    }

    #[test]
    fn log_tail_reads_last_lines() {
        // 用足够大的文件，保证尾部读取的起点不是文件开头（否则 reached_start 合理为 true）。
        let d = tmp("logtail");
        let p = d.join("latest.log");
        let content: String = (1..=500)
            .map(|i| format!("a-fairly-long-log-line-{i:04}\n"))
            .collect();
        std::fs::write(&p, content).unwrap();

        let (out, reached_start) = read_log_tail(&p, 5).unwrap();
        let got: Vec<&str> = out.split('\n').collect();
        assert_eq!(got.len(), 5);
        assert_eq!(got[4], "a-fairly-long-log-line-0500");
        assert!(!reached_start, "大文件只读尾部时不应读到文件开头");

        // 小文件 + 请求很多行 → 会读到开头，此时 reached_start 应当为 true
        let small = d.join("small.log");
        std::fs::write(
            &small,
            (1..=10).map(|i| format!("l{i}\n")).collect::<String>(),
        )
        .unwrap();
        let (all, at_start) = read_log_tail(&small, 500).unwrap();
        assert_eq!(all.split('\n').count(), 10, "应返回全部 10 行");
        assert!(at_start, "请求超过总行数时必然读到开头");
    }

    #[test]
    fn log_tail_handles_partial_first_line() {
        // 大文件，尾部读取会把第一行截断；实现必须丢弃那个半行。
        let d = tmp("logpartial");
        let p = d.join("latest.log");
        let mut content = String::new();
        for i in 0..5000 {
            content.push_str(&format!("a-long-log-line-number-{i:05}\n"));
        }
        std::fs::write(&p, content).unwrap();

        let (out, _) = read_log_tail(&p, 10).unwrap();
        let got: Vec<&str> = out.split('\n').collect();
        assert_eq!(got.len(), 10, "必须正好 10 行");
        // 每一行都必须是完整的（以数字结尾，不是被截断的尾巴）
        for line in &got {
            assert!(
                line.matches('-').count() >= 3,
                "每行应是完整的日志行：{line}"
            );
        }
        assert_eq!(got[9], "a-long-log-line-number-04999");
    }

    #[test]
    fn log_tail_empty_or_missing() {
        let d = tmp("logempty");
        let p = d.join("latest.log");
        std::fs::write(&p, "").unwrap();
        let (out, at_start) = read_log_tail(&p, 10).unwrap();
        assert_eq!(out, "");
        assert!(!at_start);

        let missing = d.join("nope.log");
        assert!(read_log_tail(&missing, 10).is_err(), "文件不存在应返回 Err");
    }

    #[test]
    fn log_tail_no_trailing_newline() {
        let d = tmp("lognotrail");
        let p = d.join("latest.log");
        std::fs::write(&p, "only-one-line").unwrap();
        let (out, _) = read_log_tail(&p, 5).unwrap();
        assert_eq!(out, "only-one-line");
    }
}
