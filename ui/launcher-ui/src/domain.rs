//! 启动器领域模型与**严格状态机**。
//!
//! 四条状态流全部集中在这里，业务页面不允许直接赋值状态字段：
//!
//! | 流 | 合法迁移 |
//! |---|---|
//! | 启动器 | 引导中 → 空闲 → 启动中 → 运行中 → 空闲 |
//! | 账户 | 未登录 → 登录中 → 已登录 / 失败 |
//! | 版本 | 未安装 → 安装中 → 已安装 → 启动中 → 运行中 |
//! | 下载任务 | 等待 → 下载中 ⇄ 暂停 → 完成 / 失败 → 重试 |
//!
//! 所有迁移都必须经 `transition()`，非法迁移返回 [`TransitionError`] 并写日志。
//! 这样「任意时刻只有一个当前版本 / 一个当前账户」「删除必须二次确认」等约束
//! 才有唯一的执行点，页面层只负责表达意图。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::live::LiveDataSource;
use crate::plugins_discovery::DiscoveredPlugin;
use official_launch::java::find_java_installations;

// ---------------------------------------------------------------------------
// 状态机基础设施
// ---------------------------------------------------------------------------

/// 非法状态迁移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub flow: &'static str,
    pub from: String,
    pub to: String,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}：{} → {} 不是合法迁移", self.flow, self.from, self.to)
    }
}

/// 启动器主状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LauncherPhase {
    Onboarding,
    Idle,
    Launching,
    Running,
}

impl LauncherPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Onboarding => "引导中",
            Self::Idle => "空闲",
            Self::Launching => "启动中",
            Self::Running => "运行中",
        }
    }

    /// 合法的迁移表。`Idle → Onboarding` 只用于「设置里重新运行引导」。
    pub fn can(self, to: Self) -> bool {
        use LauncherPhase::*;
        matches!(
            (self, to),
            (Onboarding, Idle)
                | (Idle, Onboarding)
                | (Idle, Launching)
                | (Launching, Running)
                | (Launching, Idle)
                | (Running, Idle)
        )
    }

    pub fn transition(&mut self, to: Self) -> Result<(), TransitionError> {
        if self.can(to) {
            *self = to;
            Ok(())
        } else {
            Err(TransitionError {
                flow: "启动器",
                from: self.label().to_owned(),
                to: to.label().to_owned(),
            })
        }
    }
}

/// 账户登录状态机。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountFlow {
    SignedOut,
    SigningIn,
    SignedIn,
    Failed(String),
}

impl AccountFlow {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SignedOut => "未登录",
            Self::SigningIn => "登录中",
            Self::SignedIn => "已登录",
            Self::Failed(_) => "登录失败",
        }
    }

    pub fn can(&self, to: &Self) -> bool {
        use AccountFlow::*;
        matches!(
            (self, to),
            (SignedOut, SigningIn)
                | (SigningIn, SignedIn)
                | (SigningIn, Failed(_))
                | (Failed(_), SigningIn)
                | (SignedIn, SignedOut)
        )
    }

    pub fn transition(&mut self, to: Self) -> Result<(), TransitionError> {
        if self.can(&to) {
            *self = to;
            Ok(())
        } else {
            Err(TransitionError {
                flow: "账户",
                from: self.label().to_owned(),
                to: to.label().to_owned(),
            })
        }
    }
}

/// 版本生命周期状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionFlow {
    NotInstalled,
    Installing,
    Installed,
    Launching,
    Running,
}

impl VersionFlow {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotInstalled => "未安装",
            Self::Installing => "安装中",
            Self::Installed => "已安装",
            Self::Launching => "启动中",
            Self::Running => "运行中",
        }
    }

    pub fn can(self, to: Self) -> bool {
        use VersionFlow::*;
        matches!(
            (self, to),
            (NotInstalled, Installing)
                | (Installing, Installed)
                | (Installing, NotInstalled)
                | (Installed, Launching)
                | (Launching, Running)
                | (Launching, Installed)
                | (Running, Installed)
        )
    }

    pub fn transition(&mut self, to: Self) -> Result<(), TransitionError> {
        if self.can(to) {
            *self = to;
            Ok(())
        } else {
            Err(TransitionError {
                flow: "版本",
                from: self.label().to_owned(),
                to: to.label().to_owned(),
            })
        }
    }
}

/// 下载任务状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    Waiting,
    Downloading,
    Paused,
    Completed,
    Failed,
}

impl DownloadState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待中",
            Self::Downloading => "下载中",
            Self::Paused => "已暂停",
            Self::Completed => "已完成",
            Self::Failed => "下载失败",
        }
    }

    pub fn can(self, to: Self) -> bool {
        use DownloadState::*;
        matches!(
            (self, to),
            (Waiting, Downloading)
                | (Waiting, Failed)
                | (Downloading, Paused)
                | (Downloading, Completed)
                | (Downloading, Failed)
                | (Paused, Downloading)
                | (Paused, Waiting)
                | (Failed, Waiting)
        )
    }

    pub fn transition(&mut self, to: Self) -> Result<(), TransitionError> {
        if self.can(to) {
            *self = to;
            Ok(())
        } else {
            Err(TransitionError {
                flow: "下载任务",
                from: self.label().to_owned(),
                to: to.label().to_owned(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// 领域数据
// ---------------------------------------------------------------------------

/// 加载器。沿用既有技术名词，不做翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loader {
    Vanilla,
    Forge,
    Fabric,
    Quilt,
    NeoForge,
    OptiFine,
}

impl Loader {
    pub const ALL: [Loader; 6] = [
        Loader::Vanilla,
        Loader::Forge,
        Loader::Fabric,
        Loader::Quilt,
        Loader::NeoForge,
        Loader::OptiFine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vanilla => "Vanilla",
            Self::Forge => "Forge",
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
            Self::NeoForge => "NeoForge",
            Self::OptiFine => "OptiFine",
        }
    }

    /// 插件侧 install_minecraft 的 loader 参数值（小写）。
    pub fn plugin_id(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::Forge => "forge",
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::NeoForge => "neoforge",
            Self::OptiFine => "optifine",
        }
    }

    pub fn from_name(s: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|l| l.label().eq_ignore_ascii_case(s))
            .unwrap_or(Self::Vanilla)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    Microsoft,
    Offline,
}

impl AccountKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microsoft => "微软账户",
            Self::Offline => "离线账户",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JavaSource {
    Scanned,
    Manual,
}

/// 后台下载线程与 UI 之间共享的结果槽位。
type JavaDownloadSlot = Arc<std::sync::Mutex<Option<Result<(String, String), String>>>>;

/// 后台 Java 扫描线程与 UI 之间共享的结果槽位。
type JavaScanSlot = Arc<std::sync::Mutex<Option<Vec<official_launch::java::DetectedJava>>>>;

/// 插件后台任务完成后的「应用闭包」：在 UI 线程的 tick 中执行，把结果写进领域状态。
type PluginApply = Box<dyn FnOnce(&mut LauncherModel) + Send>;

/// 插件异步任务队列：后台线程 push，UI 线程 tick 收割。
///
/// 通用约定：**任何**插件调用都不允许出现在 UI 线程上——插件可能因磁盘慢、
/// 杀毒软件锁文件、网络等原因卡住数秒甚至更久，也可能 panic。`spawn_plugin`
/// 把调用放到独立后台线程，卡死只影响该线程；panic 被捕获并降级为日志提示。
type PluginApplyQueue = Arc<std::sync::Mutex<Vec<PluginApply>>>;

/// Java 下载（Adoptium Temurin）的可见状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JavaDownloadState {
    #[default]
    Idle,
    /// 正在下载指定大版本（分钟级，后台线程执行）。
    Downloading { major: u32 },
    /// 下载并解压完成。
    Done {
        major: u32,
        path: String,
        version: String,
    },
    /// 失败（含中文错误说明）。
    Failed { major: u32, error: String },
}

impl JavaSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Scanned => "自动扫描",
            Self::Manual => "手动指定",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadCategory {
    Version,
    Mod,
    ResourcePack,
    Shader,
    Java,
    /// Minecraft 纹理/音效等资源对象（install_minecraft 资源补齐）。
    Asset,
}

impl DownloadCategory {
    pub const ALL: [DownloadCategory; 6] = [
        DownloadCategory::Version,
        DownloadCategory::Mod,
        DownloadCategory::ResourcePack,
        DownloadCategory::Shader,
        DownloadCategory::Java,
        DownloadCategory::Asset,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Version => "游戏版本",
            Self::Mod => "Mod",
            Self::ResourcePack => "资源包",
            Self::Shader => "光影",
            Self::Java => "Java",
            Self::Asset => "游戏资源",
        }
    }
}

/// 启动游戏之后的启动器行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfterLaunch {
    Keep,
    Minimize,
    Exit,
}

impl AfterLaunch {
    pub fn label(self) -> &'static str {
        match self {
            Self::Keep => "保持窗口",
            Self::Minimize => "最小化到托盘",
            Self::Exit => "退出启动器",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameVersion {
    pub id: String,
    pub name: String,
    pub mc_version: String,
    pub loader: Loader,
    pub java_id: Option<String>,
    pub memory_mb: i32,
    pub size_mb: i32,
    pub mods: i32,
    pub last_played: Option<String>,
    pub flow: VersionFlow,
    pub progress: f32,
    /// 插件侧真实实例 id（create_instance 返回的 inst-xxx）。
    /// UI id（v-xxx）只在 UI 内部使用；对插件的所有调用（启动/删除/读配置）
    /// 必须用这个 id，否则插件查不到该实例的启动入口，会误报"尚未安装"。
    /// F5 刷新后 versions 直接来自插件列表，此时 id 即插件 id，此字段为 Some(id)。
    #[serde(default)]
    pub plugin_inst_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub kind: AccountKind,
    pub flow: AccountFlow,
    pub last_used: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaRuntime {
    pub id: String,
    pub path: String,
    pub version: String,
    pub vendor: String,
    pub bits: i32,
    pub source: JavaSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: String,
    pub name: String,
    pub category: DownloadCategory,
    pub total_mb: i32,
    pub done_mb: i32,
    pub state: DownloadState,
    pub error: Option<String>,
    pub speed_kb: i32,
}

impl DownloadTask {
    pub fn fraction(&self) -> f32 {
        if self.total_mb <= 0 {
            0.0
        } else {
            (self.done_mb as f32 / self.total_mb as f32).clamp(0.0, 1.0)
        }
    }
}

/// 五组设置。改动即时写入本结构，`App` 每帧把 `dirty` 落盘（即时保存）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    // 游戏
    pub memory_mb: i32,
    pub jvm_args: String,
    pub game_args: String,
    pub fullscreen: bool,
    pub width: i32,
    pub height: i32,
    pub after_launch: AfterLaunch,
    // 下载
    pub concurrent: i32,
    pub speed_limit_kb: i32,
    pub mirror: String,
    pub verify_files: bool,
    pub proxy: String,
    // 启动器
    pub dark: bool,
    pub glass: bool,
    /// 启动后隐藏到系统托盘。
    pub minimize_to_tray: bool,
    pub animations: bool,
    pub radius: i32,
    pub font_family: String,

    pub close_to_tray: bool,
    pub auto_update: bool,
    pub language: String,
    // 账户隐私
    pub keep_signed_in: bool,
    pub anonymous_stats: bool,
    pub clear_on_exit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            memory_mb: 4096,
            jvm_args: "-XX:+UseG1GC -XX:MaxGCPauseMillis=100".to_owned(),
            game_args: String::new(),
            fullscreen: false,
            width: 1280,
            height: 720,
            after_launch: AfterLaunch::Minimize,
            concurrent: 4,
            speed_limit_kb: 0,
            mirror: "官方源".to_owned(),
            verify_files: true,
            proxy: String::new(),
            dark: true,
            glass: false,
            minimize_to_tray: true,
            animations: true,
            radius: 8,
            font_family: "Microsoft YaHei UI".to_owned(),

            close_to_tray: false,
            auto_update: true,
            language: "简体中文".to_owned(),
            keep_signed_in: true,
            anonymous_stats: false,
            clear_on_exit: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub kind: ToastKind,
    pub age_ms: u64,
    pub ttl_ms: u64,
}

impl Toast {
    pub fn alpha(&self) -> f32 {
        let fade_in = (self.age_ms as f32 / 180.0).min(1.0);
        let left = self.ttl_ms.saturating_sub(self.age_ms);
        let fade_out = (left as f32 / 320.0).min(1.0);
        fade_in.min(fade_out).clamp(0.0, 1.0)
    }
}

/// 需要二次确认的操作。危险操作（`danger = true`）用警示色确认按钮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRequest {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub danger: bool,
    pub action: PendingAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    DeleteVersion(String),
    DeleteAccount(String),
    DeleteDownload(String),
    RestoreDefaults,
    RerunOnboarding,
    ClearLogs,
    CancelLaunch,
    StopRunning,
}

/// 启动阶段进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchStage {
    Checking,
    Downloading,
    Verifying,
    Launching,
}

impl LaunchStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Checking => "检查运行环境",
            Self::Downloading => "下载缺失资源",
            Self::Verifying => "验证文件完整性",
            Self::Launching => "启动游戏",
        }
    }

    pub const ORDER: [LaunchStage; 4] = [
        LaunchStage::Checking,
        LaunchStage::Downloading,
        LaunchStage::Verifying,
        LaunchStage::Launching,
    ];
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchProgress {
    pub stage: LaunchStage,
    pub pct: f32,
    pub detail: String,
    pub exit_code: Option<i32>,
    /// 真实启动时的 OS 进程 PID（live 模式下由插件 spawn 返回；demo 下为 None）。
    pub pid: Option<u32>,
}

impl Default for LaunchProgress {
    fn default() -> Self {
        Self {
            stage: LaunchStage::Checking,
            pct: 0.0,
            detail: "准备中".to_owned(),
            exit_code: None,
            pid: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 持久化形状
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistState {
    onboarding_done: bool,
    game_dir: String,
    selected_java: Option<String>,
    current_version: Option<String>,
    current_account: Option<String>,
    versions: Vec<GameVersion>,
    accounts: Vec<Account>,
    java_runtimes: Vec<JavaRuntime>,
    downloads: Vec<DownloadTask>,
    settings: Settings,
    logs: Vec<String>,
}

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

/// 整个启动器的唯一真相源。
pub struct LauncherModel {
    pub phase: LauncherPhase,
    pub onboarding_step: usize,
    pub onboarding_done: bool,
    pub onboarding_reentry: bool,
    pub game_dir: String,
    pub java_runtimes: Vec<JavaRuntime>,
    pub selected_java: Option<String>,
    pub versions: Vec<GameVersion>,
    pub accounts: Vec<Account>,
    /// 任意时刻唯一的当前版本。
    pub current_version: Option<String>,
    /// 任意时刻唯一的当前账户。
    pub current_account: Option<String>,
    pub downloads: Vec<DownloadTask>,
    /// 已发现的插件（真实扫描插件目录得到；演示模式下为内置插件清单）。
    pub plugins: Vec<DiscoveredPlugin>,
    pub settings: Settings,
    pub logs: Vec<String>,
    pub toasts: Vec<Toast>,
    pub confirm: Option<ConfirmRequest>,
    pub launch: LaunchProgress,
    pub update_available: Option<String>,
    pub update_dismissed: bool,
    pub tray_hidden: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub show_logs_after_crash: bool,
    pub pending_window_action: Option<ui_core::WindowAction>,

    /// 实时数据源：接入 official-* 插件工具后由其持有（生产形态）。
    /// `None` 表示仍使用内置演示数据（测试与离屏截图默认如此）。
    pub live: Option<Arc<LiveDataSource>>,

    // 表单与向导的临时输入
    pub form_version_name: String,
    pub form_mc_version: String,
    pub form_loader: Loader,
    pub form_java_path: String,
    /// Java 下载（Adoptium）的 UI 可见状态。
    pub java_download: JavaDownloadState,
    /// 要下载的 JDK 大版本（设置页选择用，默认 21）。
    pub java_download_major: u32,
    /// 后台下载线程写、`tick` 读的结果槽位。
    pub(crate) java_download_slot: Option<JavaDownloadSlot>,
    /// 插件异步任务队列（通用机制，见类型注释）。
    plugin_queue: PluginApplyQueue,
    pub form_plugin_path: String,
    pub form_game_dir: String,
    pub form_offline_name: String,
    pub form_download_url: String,
    pub form_error: Option<String>,
    pub versions_filter: String,
    /// 可安装版本目录（Mojang 官方清单，仅 release）。空 = 尚未加载或加载失败。
    pub mc_catalog: Vec<(String, String, String)>,
    /// 版本目录加载状态：`None`=未开始；`Some(true)`=加载中；`Some(false)`=失败（含原因）。
    pub mc_catalog_state: Option<Result<bool, String>>,
    /// 安装表单里版本列表的搜索关键字。
    pub catalog_filter: String,
    pub download_filter: DownloadCategory,
    pub selected_download: Option<String>,
    pub open_version_detail: Option<String>,
    pub settings_tab: usize,
    /// 日志筛选：0 全部 / 1 INFO / 2 WARN / 3 ERROR。
    pub log_filter: usize,
    pub java_scanning: bool,
    /// 后台扫描线程写、`tick` 读的结果槽位（UI 线程零阻塞）。
    pub(crate) java_scan_slot: Option<JavaScanSlot>,
    /// 资源补齐进行中（启动失败后自动触发）。期间禁止重复启动：
    /// 补齐正在写 assets 目录，此时启动会读到半成品文件。
    pub repair_pending: bool,
    /// "启动失败 → 补齐 → 自动重试启动"的连续重试次数（防死循环，上限 2）。
    pub(crate) auto_repair_retries: u8,
    /// 请求跳转的页面（如安装版本时跳到下载中心看进度）。App 每帧取走并消费。
    pub pending_nav: Option<crate::shell::Page>,

    /// 声明式页面插件（热加载自 plugins/pages/*/page.json）。
    pub page_plugins: Vec<crate::page_plugin::PagePluginDecl>,
    pub(crate) page_plugins_dir: Option<PathBuf>,
    pub(crate) page_plugins_stamp: u64,
    /// 外接 MCP 服务器（plugins/mcp.json）。
    pub mcp_servers: Vec<crate::mcp::McpServer>,
    /// MCP 工具缓存：`(服务器名, 工具名, 描述)`。
    pub mcp_tools: Vec<(String, String, String)>,
    pub mcp_status: Option<String>,
    pub mcp_busy: bool,
    /// 插件工坊表单。
    pub form_ws_name: String,
    pub form_ws_path: String,
    /// 远程插件安装（GitHub owner/repo 或 tar.gz 直链）。
    pub form_plugin_remote: String,
    pub remote_install_busy: bool,

    // 计时与身份
    pub time_ms: u64,
    pub next_id: u64,
    /// 运行期开关：测试与离屏渲染不碰磁盘，真实窗口启动时才打开。
    pub persist_enabled: bool,
    pub config_path: PathBuf,
    dirty: bool,
    auth_ms: u64,
    auth_in_flight: Option<String>,
    auth_should_fail: bool,
}

impl Default for LauncherModel {
    fn default() -> Self {
        Self::demo()
    }
}

impl LauncherModel {
    /// 内存示例数据。测试与首次运行都用它，不碰磁盘。
    pub fn demo() -> Self {
        let java = vec![
            JavaRuntime {
                id: "java-17".to_owned(),
                path: r"C:\Program Files\Java\jdk-17\bin\java.exe".to_owned(),
                version: "17.0.9".to_owned(),
                vendor: "Eclipse Temurin".to_owned(),
                bits: 64,
                source: JavaSource::Scanned,
            },
            JavaRuntime {
                id: "java-21".to_owned(),
                path: r"C:\Program Files\Eclipse Adoptium\jdk-21\bin\java.exe".to_owned(),
                version: "21.0.1".to_owned(),
                vendor: "Eclipse Adoptium".to_owned(),
                bits: 64,
                source: JavaSource::Scanned,
            },
        ];
        let versions = vec![
            GameVersion {
                id: "v-fabric-1201".to_owned(),
                name: "1.20.1 Fabric".to_owned(),
                mc_version: "1.20.1".to_owned(),
                loader: Loader::Fabric,
                java_id: Some("java-17".to_owned()),
                memory_mb: 4096,
                size_mb: 386,
                mods: 42,
                last_played: Some("今天 14:32".to_owned()),
                flow: VersionFlow::Installed,
                progress: 1.0,
                plugin_inst_id: None,
            },
            GameVersion {
                id: "v-forge-1202".to_owned(),
                name: "1.20.2 Forge".to_owned(),
                mc_version: "1.20.2".to_owned(),
                loader: Loader::Forge,
                java_id: Some("java-17".to_owned()),
                memory_mb: 6144,
                size_mb: 512,
                mods: 18,
                last_played: Some("昨天 21:05".to_owned()),
                flow: VersionFlow::Installed,
                progress: 1.0,
                plugin_inst_id: None,
            },
            GameVersion {
                id: "v-neoforge-121".to_owned(),
                name: "1.21 NeoForge".to_owned(),
                mc_version: "1.21".to_owned(),
                loader: Loader::NeoForge,
                java_id: Some("java-21".to_owned()),
                memory_mb: 4096,
                size_mb: 421,
                mods: 0,
                last_played: None,
                flow: VersionFlow::Installed,
                progress: 1.0,
                plugin_inst_id: None,
            },
        ];
        let accounts = vec![Account {
            id: "acc-offline".to_owned(),
            name: "Steve".to_owned(),
            kind: AccountKind::Offline,
            flow: AccountFlow::SignedIn,
            last_used: Some("今天 14:32".to_owned()),
        }];
        let downloads = vec![
            DownloadTask {
                id: "dl-1".to_owned(),
                name: "fabric-loader-0.15.11.jar".to_owned(),
                category: DownloadCategory::Version,
                total_mb: 12,
                done_mb: 7,
                state: DownloadState::Paused,
                error: None,
                speed_kb: 2048,
            },
            DownloadTask {
                id: "dl-2".to_owned(),
                name: "sodium-fabric-1.20.1.jar".to_owned(),
                category: DownloadCategory::Mod,
                total_mb: 3,
                done_mb: 0,
                state: DownloadState::Waiting,
                error: None,
                speed_kb: 0,
            },
            DownloadTask {
                id: "dl-3".to_owned(),
                name: "ComplementaryShaders.zip".to_owned(),
                category: DownloadCategory::Shader,
                total_mb: 28,
                done_mb: 28,
                state: DownloadState::Completed,
                error: None,
                speed_kb: 0,
            },
            DownloadTask {
                id: "dl-4".to_owned(),
                name: "OptiFine_1.20.1_HD_U_I6.jar".to_owned(),
                category: DownloadCategory::Mod,
                total_mb: 9,
                done_mb: 4,
                state: DownloadState::Failed,
                error: Some("连接超时（镜像源无响应）".to_owned()),
                speed_kb: 0,
            },
        ];
        Self {
            phase: LauncherPhase::Idle,
            onboarding_step: 0,
            onboarding_done: true,
            onboarding_reentry: false,
            game_dir: r"D:\Minecraft\.minecraft".to_owned(),
            java_runtimes: java,
            selected_java: Some("java-17".to_owned()),
            versions,
            accounts,
            current_version: Some("v-fabric-1201".to_owned()),
            current_account: Some("acc-offline".to_owned()),
            downloads,
            settings: Settings::default(),
            logs: vec![
                "[INFO] 启动器已就绪，版本 1.9.1".to_owned(),
                "[INFO] Java 17.0.9 已就绪（64 位）".to_owned(),
                "[INFO] 当前版本：1.20.1 Fabric".to_owned(),
                "[INFO] 当前账户：Steve（离线账户）".to_owned(),
            ],
            toasts: Vec::new(),
            confirm: None,
            launch: LaunchProgress::default(),
            update_available: None,
            update_dismissed: false,
            tray_hidden: false,
            loading: false,
            error: None,
            show_logs_after_crash: false,
            pending_window_action: None,
            live: None,
            plugins: crate::plugins_discovery::builtin_plugins(),
            form_version_name: String::new(),
            form_mc_version: "1.20.1".to_owned(),
            form_loader: Loader::Fabric,
            form_java_path: String::new(),
            java_download: JavaDownloadState::Idle,
            java_download_major: 21,
            java_download_slot: None,
            plugin_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            form_plugin_path: String::new(),
            form_game_dir: String::new(),
            form_offline_name: String::new(),
            form_download_url: String::new(),
            form_error: None,
            versions_filter: String::new(),
            mc_catalog: Vec::new(),
            mc_catalog_state: None,
            catalog_filter: String::new(),
            download_filter: DownloadCategory::Version,
            selected_download: Some("dl-1".to_owned()),
            open_version_detail: None,
            settings_tab: 0,
            log_filter: 0,
            java_scanning: false,
            java_scan_slot: None,
            repair_pending: false,
            auto_repair_retries: 0,
            pending_nav: None,
            page_plugins: Vec::new(),
            page_plugins_dir: None,
            page_plugins_stamp: 0,
            mcp_servers: Vec::new(),
            mcp_tools: Vec::new(),
            mcp_status: None,
            mcp_busy: false,
            form_ws_name: String::new(),
            form_ws_path: String::new(),
            form_plugin_remote: String::new(),
            remote_install_busy: false,
            time_ms: 0,
            next_id: 100,
            persist_enabled: false,
            config_path: PathBuf::from("launcher-config.json"),
            dirty: false,
            auth_ms: 0,
            auth_in_flight: None,
            auth_should_fail: false,
        }
    }

    /// 首次启动：进入引导。真实启动时由 `App` 根据持久化状态调用。
    pub fn first_run() -> Self {
        let mut m = Self::demo();
        m.phase = LauncherPhase::Onboarding;
        m.onboarding_done = false;
        m.onboarding_step = 0;
        m.game_dir.clear();
        m.selected_java = None;
        m.java_runtimes.clear();
        m.current_version = Some("v-fabric-1201".to_owned());
        m.current_account = None;
        m.accounts.clear();
        m.logs.clear();
        m.logs.push("[INFO] 首次启动，进入初始化引导".to_owned());
        m.live = None;
        m
    }

    fn uid(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// 通用后台任务：在独立线程执行，产出对模型的修改经队列在 tick 收割。
    /// 与 `spawn_plugin` 的区别：不要求 LiveDataSource（MCP 调用等纯网络任务用）。
    pub fn spawn_bg<J>(&self, job: J)
    where
        J: FnOnce() -> PluginApply + Send + 'static,
    {
        let queue = self.plugin_queue.clone();
        std::thread::spawn(move || {
            let apply: PluginApply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Box::new(|m: &mut LauncherModel| m.log("[ERROR] 后台任务异常崩溃".to_owned())));
            if let Ok(mut q) = queue.lock() {
                q.push(apply);
            }
        });
    }

    /// 打开声明式页面插件：铺示例 + 加载 + 记录目录（供 tick 热重载检测）。
    pub fn enable_page_plugins(&mut self, root: &std::path::Path) {
        let plugins_dir = root.join("plugins");
        let _ = std::fs::create_dir_all(&plugins_dir);
        crate::mcp::ensure_config_template(&plugins_dir);
        self.mcp_servers = crate::mcp::load_servers(&plugins_dir);
        let pages_dir = plugins_dir.join("pages");
        crate::page_plugin::seed_examples(&pages_dir);
        let (pages, errors) = crate::page_plugin::load_pages(&pages_dir);
        for e in errors {
            self.log(format!("[WARN] 页面插件加载失败：{e}"));
        }
        self.page_plugins_stamp = crate::page_plugin::dir_stamp(&pages_dir);
        self.page_plugins_dir = Some(pages_dir);
        self.page_plugins = pages;
        self.log(format!(
            "[INFO] 页面插件就绪：{} 个（目录 plugins/pages，改文件即热重载）",
            self.page_plugins.len()
        ));
    }

    /// 每帧热重载检测：目录指纹变了就重扫页面插件与 MCP 配置。
    fn poll_page_plugins(&mut self) {
        let Some(dir) = self.page_plugins_dir.clone() else {
            return;
        };
        let stamp = crate::page_plugin::dir_stamp(&dir);
        if stamp == self.page_plugins_stamp {
            return;
        }
        self.page_plugins_stamp = stamp;
        let plugins_dir = dir.parent().map(|p| p.to_path_buf());
        let (pages, errors) = crate::page_plugin::load_pages(&dir);
        for e in errors {
            self.log(format!("[WARN] 页面插件加载失败：{e}"));
        }
        let n = pages.len();
        self.page_plugins = pages;
        if let Some(pd) = plugins_dir {
            self.mcp_servers = crate::mcp::load_servers(&pd);
        }
        self.log(format!("[INFO] 已热重载页面插件（当前 {n} 个）"));
        self.toast("页面插件已热重载", ToastKind::Success);
    }

    /// 刷新外接 MCP 工具列表（后台线程，结果经队列回收）。
    pub fn refresh_mcp_tools(&mut self) {
        if self.mcp_busy {
            return;
        }
        if self.mcp_servers.is_empty() {
            self.mcp_status = Some("未配置 MCP 服务器：请编辑 plugins/mcp.json".to_owned());
            return;
        }
        self.mcp_busy = true;
        self.mcp_status = Some("正在连接 MCP 服务器…".to_owned());
        let servers = self.mcp_servers.clone();
        self.spawn_bg(move || {
            Box::new(move |m: &mut LauncherModel| {
                let mut tools = Vec::new();
                let mut msgs = Vec::new();
                for s in &servers {
                    match crate::mcp::list_tools(s) {
                        Ok(list) => {
                            msgs.push(format!("{}：{} 个工具", s.name, list.len()));
                            for (name, desc) in list {
                                tools.push((s.name.clone(), name, desc));
                            }
                        }
                        Err(e) => msgs.push(format!("{}：失败（{e}）", s.name)),
                    }
                }
                let total = tools.len();
                m.mcp_tools = tools;
                m.mcp_busy = false;
                m.mcp_status = Some(msgs.join("；"));
                m.log(format!("[MCP] 工具列表刷新完成，共 {total} 个"));
                m.touch();
            })
        });
    }

    /// 插件工坊：从剪贴板导入 page.json 创建页面插件（写入即热加载）。
    /// 返回 Ok(插件名) / Err(错误信息)。
    pub fn create_page_plugin_from_text(&mut self, name: &str, json_text: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("请先填写插件名称".to_owned());
        }
        crate::page_plugin::parse(json_text.as_bytes())
            .map_err(|e| format!("JSON 无效：{e}"))?;
        let Some(dir) = self.page_plugins_dir.clone() else {
            return Err("页面插件目录未初始化（真实启动模式才有）".to_owned());
        };
        // 目录名用用户名转安全 id；decl.id 若与目录名不一致会被加载器矫正为目录名。
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let dest = dir.join(&safe);
        if dest.exists() {
            return Err(format!("已存在同名插件目录：{safe}"));
        }
        std::fs::create_dir_all(&dest).map_err(|e| format!("创建目录失败：{e}"))?;
        // 保持用户 JSON 里的 id，但加载时目录名优先；这里直接统一成目录名更直观。
        let final_json = match serde_json::from_str::<serde_json::Value>(json_text) {
            Ok(mut v) => {
                v["id"] = serde_json::Value::String(safe.clone());
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| json_text.to_owned())
            }
            Err(_) => json_text.to_owned(),
        };
        std::fs::write(dest.join("page.json"), final_json)
            .map_err(|e| format!("写入失败：{e}"))?;
        self.log(format!("[INFO] 已创建页面插件「{name}」（{safe}），即将热重载"));
        Ok(name.to_owned())
    }

    /// 删除一个页面插件目录（热重载后导航自动消失）。
    pub fn remove_page_plugin(&mut self, id: &str) -> Result<(), String> {
        let Some(dir) = self.page_plugins_dir.clone() else {
            return Err("页面插件目录未初始化".to_owned());
        };
        std::fs::remove_dir_all(dir.join(id)).map_err(|e| format!("删除失败：{e}"))
    }

    /// 页面插件目录（工坊页「打开目录」用）。
    pub fn page_plugins_dir(&self) -> Option<std::path::PathBuf> {
        self.page_plugins_dir.clone()
    }

    /// 接入真实数据源。一旦接入，`refresh()` 会从插件拉取实例、启动会真实 spawn Java。
    pub fn attach_live(&mut self, live: Arc<LiveDataSource>) {
        self.live = Some(live);
        self.log(
            "[INFO] 已接入官方插件数据源（official-instance / official-launch / official-mod）",
        );
        // 接入后立即从插件同步一次真实实例列表，替换可能过期的本地版本数据，
        // 避免"界面上有版本但插件侧查不到实例"的错位。
        self.refresh();
    }

    /// 是否接入了真实数据源。
    pub fn has_live(&self) -> bool {
        self.live.is_some()
    }

    /// 把插件返回的 loader 名映射成领域枚举（插件只支持 vanilla/fabric/forge，
    /// 其余未知值归到 Vanilla）。
    fn map_loader(s: &str) -> Loader {
        match s.to_ascii_lowercase().as_str() {
            "fabric" => Loader::Fabric,
            "forge" => Loader::Forge,
            "quilt" => Loader::Quilt,
            "neoforge" => Loader::NeoForge,
            "optifine" => Loader::OptiFine,
            _ => Loader::Vanilla,
        }
    }

    /// 领域的 loader 枚举 → 插件接受的字符串（插件只支持 vanilla/fabric/forge）。
    fn loader_to_plugin(s: Loader) -> &'static str {
        match s {
            Loader::Fabric => "fabric",
            Loader::Forge => "forge",
            _ => "vanilla",
        }
    }

    // -----------------------------------------------------------------------
    // 反馈
    // -----------------------------------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.next_id += 1;
        let id = self.next_id;
        self.toasts.push(Toast {
            id,
            text: text.into(),
            kind,
            age_ms: 0,
            ttl_ms: 3600,
        });
        // 最多保留 4 条，避免遮挡界面。
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub fn log(&mut self, line: impl Into<String>) {
        self.logs.push(line.into());
        if self.logs.len() > 400 {
            let drop = self.logs.len() - 400;
            self.logs.drain(0..drop);
        }
    }

    fn illegal(&mut self, err: TransitionError) {
        let msg = format!("[WARN] 非法状态迁移：{err}");
        self.log(msg.clone());
        self.toast(msg, ToastKind::Warn);
    }

    // -----------------------------------------------------------------------
    // 联动查询：所有页面都从这里读当前版本 / 当前账户
    // -----------------------------------------------------------------------

    pub fn current_version(&self) -> Option<&GameVersion> {
        self.current_version
            .as_ref()
            .and_then(|id| self.versions.iter().find(|v| &v.id == id))
    }

    pub fn current_account(&self) -> Option<&Account> {
        self.current_account
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|a| &a.id == id))
    }

    pub fn selected_java(&self) -> Option<&JavaRuntime> {
        self.selected_java
            .as_ref()
            .and_then(|id| self.java_runtimes.iter().find(|j| &j.id == id))
    }

    pub fn is_logged_in(&self) -> bool {
        self.current_account()
            .is_some_and(|a| a.flow == AccountFlow::SignedIn)
    }

    /// 把以空白分隔的参数串拆成 `Vec<String>`（用于把 `Settings.jvm_args` 等传给插件）。
    fn split_args(s: &str) -> Vec<String> {
        s.split_whitespace().map(|x| x.to_owned()).collect()
    }

    /// 启动前置校验。返回**中文阻断文案**，为空才允许启动。
    pub fn preflight(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.phase == LauncherPhase::Onboarding {
            out.push("请先完成首次启动引导".to_owned());
            return out;
        }
        match self.current_version() {
            None => out.push("请先选择一个游戏版本".to_owned()),
            Some(v) => match v.flow {
                VersionFlow::NotInstalled => out.push("当前版本尚未安装，请先安装".to_owned()),
                VersionFlow::Installing => out.push("当前版本正在安装，请稍候".to_owned()),
                VersionFlow::Launching | VersionFlow::Running => {}
                VersionFlow::Installed => {}
            },
        }
        if self.current_account().is_none() {
            out.push("请先登录账户".to_owned());
        } else if !self.is_logged_in() {
            out.push("账户尚未登录完成，请等待或重新登录".to_owned());
        }
        if self.selected_java().is_none() {
            out.push("Java 不可用，请在设置中指定 Java".to_owned());
        }
        if self.settings.memory_mb < 512 {
            out.push("内存分配不能低于 512 MB，请在设置中调整".to_owned());
        }
        if self.game_dir.trim().is_empty() {
            out.push("请先设置游戏目录".to_owned());
        }
        out
    }

    /// 启动按钮的文案与可用状态。所有页面共用。
    pub fn launch_label(&self) -> (String, bool) {
        match self.phase {
            LauncherPhase::Onboarding => ("完成首次启动引导后即可启动".to_owned(), false),
            LauncherPhase::Launching => (
                format!("启动中… {}%", (self.launch.pct * 100.0) as i32),
                false,
            ),
            LauncherPhase::Running => ("游戏运行中".to_owned(), false),
            LauncherPhase::Idle => {
                let blockers = self.preflight();
                if blockers.is_empty() {
                    ("启动游戏".to_owned(), true)
                } else {
                    (blockers[0].clone(), false)
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 启动流程
    // -----------------------------------------------------------------------

    pub fn start_launch(&mut self) -> Result<(), String> {
        if self.phase != LauncherPhase::Idle {
            return Err("当前不在空闲状态，无法重复启动".to_owned());
        }
        let blockers = self.preflight();
        if !blockers.is_empty() {
            return Err(blockers.join("；"));
        }
        // 资源补齐（失败后自动触发）尚未完成时不重复启动：
        // 补齐正在写 assets 目录，此时启动会读到半成品文件。
        if self.repair_pending {
            return Err(
                "资源补齐正在进行中（首次约需几分钟下载纹理/音效），完成后会自动重试启动。"
                    .to_owned(),
            );
        }
        let id = self.current_version.clone().unwrap_or_default();
        match self.phase.transition(LauncherPhase::Launching) {
            Ok(()) => {}
            Err(e) => {
                self.illegal(e);
                return Err("启动器状态不允许启动".to_owned());
            }
        }
        if let Some(v) = self.versions.iter_mut().find(|v| v.id == id) {
            match v.flow.transition(VersionFlow::Launching) {
                Ok(()) => {}
                Err(e) => {
                    self.illegal(e);
                    let _ = self.phase.transition(LauncherPhase::Idle);
                    return Err("版本状态不允许启动".to_owned());
                }
            }
        }
        self.launch = LaunchProgress {
            stage: LaunchStage::Checking,
            pct: 0.0,
            detail: "检查 Java、账户与游戏目录".to_owned(),
            exit_code: None,
            pid: None,
        };
        self.show_logs_after_crash = false;
        self.error = None;
        self.log("[INFO] 开始启动流程：检查 → 下载 → 验证 → 启动");
        self.toast("已通过前置校验，开始启动", ToastKind::Info);

        // 实时模式：真正 spawn Java 进程，并记录真实 PID。
        // 插件调用（设置配置 + spawn）整体放到后台线程，UI 永不阻塞；
        // `tick` 收割后按结果收尾，进程退出时由 `finish_launch` 处理。
        if self.live.is_some() {
            let jvm_args = Self::split_args(&self.settings.jvm_args);
            let game_args = Self::split_args(&self.settings.game_args);
            let memory_mb = self.settings.memory_mb as u32;
            let after_launch = self.settings.after_launch;

            let vid = id.clone();
            let vname = self
                .versions
                .iter()
                .find(|v| v.id == id)
                .map(|v| v.name.clone())
                .unwrap_or_default();
            let vmc = self
                .versions
                .iter()
                .find(|v| v.id == id)
                .map(|v| v.mc_version.clone())
                .unwrap_or_default();
            // 插件侧真实实例 id：安装完成后记录；刷新后的版本 id 本身即插件 id。
            // 拿 UI id 去问插件会查不到启动入口，误报"尚未安装 Minecraft"。
            let plugin_id = self
                .versions
                .iter()
                .find(|v| v.id == id)
                .and_then(|v| v.plugin_inst_id.clone())
                .unwrap_or_else(|| id.clone());
            self.spawn_plugin(move |live| {
                // 兜底：旧版 exe 安装的版本没有记录 plugin_inst_id，此时按版本名
                // 到插件侧反查真实实例 id 并采用（create_instance 用的就是版本名）。
                let mut plugin_id = plugin_id;
                if plugin_id == vid {
                    if let Ok(instances) = live.list_instances() {
                        // 优先按版本名匹配（create_instance 用的就是版本名）；
                        // 找不到再按游戏版本号唯一匹配（旧数据/改过名的兜底）。
                        let by_name = instances.iter().find(|i| i.name == vname);
                        let resolved = match by_name {
                            Some(i) => Some(i.id.clone()),
                            None => {
                                let by_mc: Vec<_> = instances
                                    .iter()
                                    .filter(|i| i.game_version == vmc)
                                    .collect();
                                if by_mc.len() == 1 {
                                    Some(by_mc[0].id.clone())
                                } else {
                                    None
                                }
                            }
                        };
                        if let Some(pid) = resolved {
                            plugin_id = pid;
                        }
                    }
                }
                // 就绪检查：真实 Minecraft 入口没装配（install_minecraft 未跑或失败）
                // 时拒绝启动，否则只会静默跑一次探测式 `java -version`，用户看不到窗口。
                let ready = live.launch_readiness(&plugin_id);
                let resolved = plugin_id.clone();
                let result = match ready {
                    Ok((k, _)) if k == "main_class" => live
                        .set_launch_config(&plugin_id, &jvm_args, &game_args, memory_mb)
                        .and_then(|()| live.launch_game(&plugin_id)),
                    Ok((_kind, _java)) => {
                        // 注意：`java` 为 None 只表示该实例未固定 Java 路径，
                        // 启动时仍会回退到 PATH 上的 java——不能断言"没有 Java"。
                        Err("该版本尚未安装 Minecraft，请先在版本管理删除该版本并重新安装（确认安装会真实下载客户端并自动挂载 Java）。".to_owned())
                    }
                    Err(e) => Err(format!("读取启动配置失败：{e}")),
                };
                Box::new(move |d: &mut LauncherModel| {
                    // 后台执行期间状态可能已变（用户取消/切版本），只在仍在启动态时生效。
                    if d.phase != LauncherPhase::Launching {
                        return;
                    }
                    // 把反查到的插件实例 id 回填到版本上，后续启动/删除直接使用。
                    if let Some(v) = d.versions.iter_mut().find(|v| v.id == vid) {
                        v.plugin_inst_id = Some(resolved.clone());
                    }
                    match result {
                        Ok(pid) => {
                            d.launch.pid = Some(pid);
                            d.auto_repair_retries = 0; // 启动成功即重置自动重试计数
                            d.launch.stage = LaunchStage::Launching;
                            d.launch.detail = format!("已启动真实 Java 进程（PID {pid}）");
                            d.log(format!("[INFO] 游戏已启动（真实 Java 进程，PID {pid}）"));
                            d.toast("游戏已启动", ToastKind::Success);
                            // 按"启动后行为"立即对窗口生效（保持 / 最小化 / 退出）。
                            d.pending_window_action = match after_launch {
                                AfterLaunch::Keep => None,
                                // 最小化一律隐藏到系统托盘。
                                AfterLaunch::Minimize => {
                                    d.tray_hidden = true;
                                    Some(ui_core::WindowAction::MinimizeToTray)
                                }
                                AfterLaunch::Exit => Some(ui_core::WindowAction::Close),
                            };
                            d.touch();
                        }
                        Err(e) => {
                            d.log(format!("[ERROR] 启动游戏失败：{e}"));
                            let _ = d.phase.transition(LauncherPhase::Idle);
                            if let Some(v) = d.versions.iter_mut().find(|v| v.id == vid) {
                                let _ = v.flow.transition(VersionFlow::Installed);
                            }
                            // 启动失败先补齐再自动重试（用户策略：先启动，失败再补齐）。
                            // 常见失败：入口未装配（从未跑过 install_minecraft）。
                            let loader = d
                                .versions
                                .iter()
                                .find(|v| v.id == vid)
                                .map(|v| v.loader.plugin_id().to_owned())
                                .unwrap_or_default();
                            d.begin_asset_repair(
                                vec![(resolved.clone(), vmc.clone(), loader)],
                                true,
                                "启动就绪检查未通过",
                            );
                            d.touch();
                        }
                    }
                })
            });
            self.touch();
            return Ok(());
        }

        // 演示模式：模拟推进（`tick` 会在约 4 秒后自动进入"运行中"）。
        self.touch();
        Ok(())
    }

    /// 游戏结束。异常退出码会写日志并弹出日志提示。
    ///
    /// 演示模式由 `Running` 收尾；实时模式进程退出时仍处于 `Launching`，故两者都允许。
    pub fn finish_launch(&mut self, exit_code: i32) -> Result<(), String> {
        if self.phase != LauncherPhase::Running && self.phase != LauncherPhase::Launching {
            return Err("游戏未在运行中".to_owned());
        }
        let _ = self.phase.transition(LauncherPhase::Idle);
        let id = self.current_version.clone();
        if let Some(id) = id {
            if let Some(v) = self.versions.iter_mut().find(|v| v.id == id) {
                let _ = v.flow.transition(VersionFlow::Installed);
            }
        }
        self.launch.exit_code = Some(exit_code);
        if exit_code == 0 {
            self.log("[INFO] 游戏正常退出（exit code 0）");
            self.toast("游戏已正常退出", ToastKind::Success);
        } else {
            self.log(format!("[ERROR] 游戏异常退出，exit code {exit_code}"));
            self.log("[ERROR] 建议查看日志定位崩溃原因（Ctrl+L 打开日志）");
            self.error = Some(format!("游戏异常退出（exit code {exit_code}）"));
            self.show_logs_after_crash = true;
            self.toast(
                "游戏异常退出，已记录日志，请查看日志定位原因",
                ToastKind::Danger,
            );
            // 崩溃日志指向资源缺失（assets 对象等）→ 自动补齐后重试启动。
            if self.crash_looks_like_missing_assets() {
                let target = self
                    .current_version
                    .clone()
                    .and_then(|id| {
                        self.versions.iter().find(|v| v.id == id).map(|v| {
                            (
                                v.plugin_inst_id.clone().unwrap_or_else(|| v.id.clone()),
                                v.mc_version.clone(),
                                v.loader.plugin_id().to_owned(),
                            )
                        })
                    });
                if let Some(t) = target {
                    self.begin_asset_repair(vec![t], true, "崩溃日志含资源缺失特征");
                }
            }
        }
        self.touch();
        Ok(())
    }

    pub fn cancel_launch(&mut self) {
        if self.phase != LauncherPhase::Launching {
            return;
        }
        // 实时模式：真正终止已 spawn 的 Java 进程。
        if let Some(live) = self.live.clone() {
            if live.kill_game() {
                self.log("[WARN] 已终止真实游戏进程");
            }
        }
        let _ = self.phase.transition(LauncherPhase::Idle);
        let id = self.current_version.clone();
        if let Some(id) = id {
            if let Some(v) = self.versions.iter_mut().find(|v| v.id == id) {
                let _ = v.flow.transition(VersionFlow::Installed);
            }
        }
        self.log("[WARN] 用户取消了启动流程");
        self.toast("已取消启动", ToastKind::Warn);
    }

    // -----------------------------------------------------------------------
    // 版本 / 账户 / Java 操作（唯一执行点）
    // -----------------------------------------------------------------------

    pub fn select_version(&mut self, id: &str) {
        if self.versions.iter().any(|v| v.id == id) {
            self.current_version = Some(id.to_owned());
            // 实时模式：在插件侧切换"当前实例"（持久化到 instances.json）。后台线程执行。
            // 只有插件侧真实存在的实例才切换（演示 id 如 v-109 不存在，跳过）。
            if let Some(live) = self.live.clone() {
                if live.has_instance(id) {
                    let sid = id.to_owned();
                    self.spawn_plugin(move |live| {
                        let res = live.switch_instance(&sid);
                        Box::new(move |d: &mut LauncherModel| {
                            if let Err(e) = res {
                                d.log(format!("[ERROR] 在插件侧切换实例失败：{e}"));
                            }
                        })
                    });
                }
            }
            self.touch();
        }
    }

    pub fn select_account(&mut self, id: &str) {
        if self.accounts.iter().any(|a| a.id == id) {
            self.current_account = Some(id.to_owned());
            self.touch();
        }
    }

    pub fn select_java(&mut self, id: &str) {
        if self.java_runtimes.iter().any(|j| j.id == id) {
            self.selected_java = Some(id.to_owned());
            self.touch();
        }
    }

    pub fn delete_version(&mut self, id: &str) {
        let Some(pos) = self.versions.iter().position(|v| v.id == id) else {
            return;
        };
        let name = self.versions[pos].name.clone();
        // 移除前先取插件侧实例 id（删除插件实例要用它）。
        let plugin_inst_id = self.versions[pos].plugin_inst_id.clone();
        if self.current_version.as_deref() == Some(id) {
            self.current_version = None;
            if self.phase == LauncherPhase::Launching || self.phase == LauncherPhase::Running {
                let _ = self.phase.transition(LauncherPhase::Idle);
            }
        }
        self.versions.remove(pos);
        // 实时模式：在插件侧真正删除实例（已二次确认）。后台线程执行。
        // 优先用插件侧实例 id（安装后记录），否则退回 UI id。
        if self.live.is_some() {
            let did = plugin_inst_id.unwrap_or_else(|| id.to_owned());
            self.spawn_plugin(move |live| {
                let res = live.delete_instance(&did);
                Box::new(move |d: &mut LauncherModel| {
                    if let Err(e) = res {
                        d.log(format!("[ERROR] 在插件侧删除实例失败：{e}"));
                    }
                })
            });
        }
        self.log(format!("[WARN] 已删除版本「{name}」，该操作不可恢复"));
        self.toast(
            format!("版本「{name}」已删除（不可恢复）"),
            ToastKind::Danger,
        );
        self.touch();
    }

    pub fn delete_account(&mut self, id: &str) {
        let Some(pos) = self.accounts.iter().position(|a| a.id == id) else {
            return;
        };
        let name = self.accounts[pos].name.clone();
        if self.current_account.as_deref() == Some(id) {
            self.current_account = None;
        }
        self.accounts.remove(pos);
        self.log(format!("[WARN] 已删除账户「{name}」，该操作不可恢复"));
        self.toast(
            format!("账户「{name}」已删除（不可恢复）"),
            ToastKind::Danger,
        );
        self.touch();
    }

    /// 加载可安装版本目录（Mojang 官方清单）。后台线程执行，`tick` 收割。
    /// 只在"从未加载"或"上次失败"时发起请求；表单打开期间每帧调用本函数
    /// 必须幂等，否则会无限重复拉取清单（列表永远刷不出来）。
    /// 失败重试：先把 `mc_catalog_state` 置为 `None` 再调用本函数。
    pub fn load_mc_catalog(&mut self) {
        if self.mc_catalog_state == Some(Ok(true)) {
            return; // 加载中
        }
        if self.mc_catalog_state == Some(Ok(false)) {
            return; // 已加载完成，不自动重拉
        }
        self.mc_catalog_state = Some(Ok(true));
        self.spawn_plugin(|live| {
            let res = live.list_versions();
            Box::new(move |d: &mut LauncherModel| match res {
                Ok(list) => {
                    d.log(format!("[INFO] 已加载官方版本目录（{} 个正式版）", list.len()));
                    d.mc_catalog = list;
                    d.mc_catalog_state = Some(Ok(false)); // false = 已完成（非加载中）
                }
                Err(e) => {
                    d.log(format!("[ERROR] 加载版本目录失败：{e}"));
                    d.mc_catalog_state = Some(Err(e));
                }
            })
        });
    }

    /// 从目录点选一个版本：填充游戏版本号（名称留空则自动生成）。
    pub fn pick_mc_version(&mut self, id: &str) {
        self.form_mc_version = id.to_owned();
        if self.form_version_name.trim().is_empty() {
            self.form_version_name = format!("Minecraft {id}");
        }
    }

    pub fn install_version(
        &mut self,
        name: &str,
        mc: &str,
        loader: Loader,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("版本名称不能为空".to_owned());
        }
        let mc = mc.trim();
        if mc.is_empty() || !mc.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err("游戏版本号格式不正确".to_owned());
        }
        // 目录已加载时校验版本真实存在（点选的 id 一定在目录里；手输的可能拼错）。
        if !self.mc_catalog.is_empty() && !self.mc_catalog.iter().any(|(id, _, _)| id == mc) {
            return Err(format!(
                "游戏版本 {mc} 不在 Mojang 官方清单中，请从版本列表中点选"
            ));
        }
        if self.versions.iter().any(|v| v.name == name) {
            return Err("已存在同名版本".to_owned());
        }
        let id = self.uid("v");
        self.versions.push(GameVersion {
            id: id.clone(),
            name: name.to_owned(),
            mc_version: mc.to_owned(),
            loader,
            java_id: self.selected_java.clone(),
            memory_mb: self.settings.memory_mb,
            size_mb: 0,
            mods: 0,
            last_played: None,
            flow: VersionFlow::Installing,
            progress: 0.0,
            // 插件侧实例 id 由 create_instance 返回后在后台闭包里回填。
            plugin_inst_id: None,
        });
        // 注意：不再创建假的 "{mc}-xxx.jar 0/180MB" 下载任务——真实下载进度
        // 由 install_minecraft 写入 ASSET_PROGRESS，自动出现在下载中心
        // （"游戏资源补齐"任务），假任务只会永远卡在"下载中"误导用户。
        self.log(format!(
            "[INFO] 开始安装版本「{name}」（{mc} / {}）",
            loader.label()
        ));
        if loader != Loader::Vanilla {
            if matches!(loader, Loader::Fabric) {
                self.log("[INFO] Fabric 将从官方 meta 真实安装（加载器库 + main_class 合并）".to_owned());
            } else {
                self.log("[WARN] Forge/NeoForge/Quilt/OptiFine 安装暂未支持；所选加载器将按原版客户端安装与启动".to_owned());
            }
        }
        self.toast(format!("开始安装「{name}」"), ToastKind::Info);
        // 安装过程的下载全部在下载中心可见——直接跳过去让用户看到进度。
        self.pending_nav = Some(crate::shell::Page::Downloads);
        // 实时模式：在插件侧真正创建实例（持久化到 instances.json），随后从
        // Mojang 官方源下载真实 Minecraft 客户端（分钟级）。全部在后台线程执行。
        if self.live.is_some() {
            let iname = name.to_owned();
            let imc = mc.to_owned();
            let loader_id = loader.plugin_id().to_owned();
            let plugin_loader = Self::loader_to_plugin(loader).to_owned();
            let ui_id = id.clone();
            self.spawn_plugin(move |live| {
                let created = live.create_instance(&iname, &imc, &plugin_loader);
                let installed = match &created {
                    Ok(inst_id) => Some(live.install_minecraft(inst_id, &imc, &loader_id)),
                    Err(_) => None,
                };
                Box::new(move |d: &mut LauncherModel| match created {
                    Ok(inst_id) => {
                        // 记录插件侧实例 id：启动/删除都必须用它对准插件，
                        // 否则查不到该实例的启动入口（entry_kind=none）。
                        if let Some(v) = d
                            .versions
                            .iter_mut()
                            .find(|v| v.id == ui_id)
                        {
                            v.plugin_inst_id = Some(inst_id.clone());
                        }
                        d.log(format!(
                            "[INFO] 已在插件侧创建实例 {inst_id}（{imc} / {plugin_loader}）"
                        ));
                        match installed {
                            Some(Ok(info)) => {
                                let libs = info.get("libraries").and_then(|x| x.as_u64()).unwrap_or(0);
                                let jpath = info.get("java").and_then(|x| x.as_str()).unwrap_or("PATH 上的 java");
                                d.log(format!(
                                    "[INFO] Minecraft {imc} 下载完成（{libs} 个库，Java：{jpath}），可以启动了"
                                ));
                                d.toast(format!("Minecraft {imc} 安装完成"), ToastKind::Success);
                            }
                            Some(Err(e)) => {
                                d.log(format!("[ERROR] Minecraft {imc} 下载失败：{e}"));
                                d.toast(format!("Minecraft 下载失败：{e}"), ToastKind::Warn);
                            }
                            None => {}
                        }
                    }
                    Err(e) => {
                        d.log(format!("[ERROR] 在插件侧创建实例失败：{e}"));
                        d.toast(format!("插件创建实例失败：{e}"), ToastKind::Warn);
                    }
                })
            });
        }
        self.touch();
        Ok(id)
    }

    /// 拖拽安装：文件/目录落到窗口上的入口。
    pub fn install_dropped_files(&mut self, paths: &[String]) {
        for p in paths {
            let name = p.rsplit(['\\', '/']).next().unwrap_or(p).trim().to_owned();
            if name.is_empty() {
                continue;
            }
            let loader = if name.to_ascii_lowercase().contains("forge") {
                Loader::Forge
            } else if name.to_ascii_lowercase().contains("fabric") {
                Loader::Fabric
            } else if name.to_ascii_lowercase().contains("optifine") {
                Loader::OptiFine
            } else {
                Loader::Vanilla
            };
            self.add_download(&name, DownloadCategory::Version, 64);
            self.log(format!(
                "[INFO] 拖入文件「{name}」→ 已加入安装队列（{}）",
                loader.label()
            ));
        }
        self.toast(
            format!("已接收 {} 个文件，加入安装队列", paths.len()),
            ToastKind::Info,
        );
        if let Some(name) = paths
            .first()
            .and_then(|p| p.rsplit(['\\', '/']).next())
            .map(|s| s.to_owned())
        {
            let _ = self.install_version(&name, &self.form_mc_version.clone(), Loader::Vanilla);
        }
        self.touch();
    }

    pub fn add_offline_account(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("账户名不能为空".to_owned());
        }
        if name.len() > 16 {
            return Err("账户名不能超过 16 个字符".to_owned());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err("账户名只能包含字母、数字与下划线".to_owned());
        }
        if self.accounts.iter().any(|a| a.name == name) {
            return Err("已存在同名账户".to_owned());
        }
        let id = self.uid("acc");
        self.accounts.push(Account {
            id: id.clone(),
            name: name.to_owned(),
            kind: AccountKind::Offline,
            flow: AccountFlow::SignedIn,
            last_used: None,
        });
        self.current_account = Some(id.clone());
        self.log(format!("[INFO] 已添加离线账户「{name}」并设为当前账户"));
        self.toast(format!("已添加离线账户「{name}」"), ToastKind::Success);
        self.touch();
        Ok(id)
    }

    /// 微软授权入口。`fail_next` 用于演示/测试「登录失败」分支。
    pub fn start_microsoft_auth(&mut self, fail_next: bool) -> Result<(), String> {
        let id = self.uid("acc");
        let name = format!("Microsoft_{}", self.next_id);
        self.accounts.push(Account {
            id: id.clone(),
            name,
            kind: AccountKind::Microsoft,
            flow: AccountFlow::SigningIn,
            last_used: None,
        });
        self.current_account = Some(id.clone());
        self.auth_in_flight = Some(id);
        self.auth_ms = 0;
        self.auth_should_fail = fail_next;
        self.log("[INFO] 打开微软授权流程，等待回调…");
        self.toast("正在等待微软授权…", ToastKind::Info);
        self.touch();
        Ok(())
    }

    fn finish_auth(&mut self, ok: bool) {
        let Some(id) = self.auth_in_flight.take() else {
            return;
        };
        let Some(pos) = self.accounts.iter().position(|a| a.id == id) else {
            return;
        };
        let to = if ok {
            AccountFlow::SignedIn
        } else {
            AccountFlow::Failed("授权被取消或令牌无效".to_owned())
        };
        match self.accounts[pos].flow.transition(to) {
            Ok(()) => {
                if ok {
                    self.log("[INFO] 微软账户授权成功");
                    self.toast("微软账户登录成功", ToastKind::Success);
                } else {
                    self.log("[ERROR] 微软账户授权失败：授权被取消或令牌无效");
                    self.toast("微软授权失败，可重试", ToastKind::Danger);
                }
            }
            Err(e) => self.illegal(e),
        }
        self.touch();
    }

    pub fn retry_auth(&mut self) {
        let Some(id) = self.current_account.clone() else {
            return;
        };
        let Some(pos) = self.accounts.iter().position(|a| a.id == id) else {
            return;
        };
        match self.accounts[pos].flow.transition(AccountFlow::SigningIn) {
            Ok(()) => {
                self.auth_in_flight = Some(id);
                self.auth_ms = 0;
                self.auth_should_fail = false;
                self.log("[INFO] 重新发起微软授权");
            }
            Err(e) => self.illegal(e),
        }
    }

    /// 开始扫描本机 Java 运行时。真实探测（PATH / JAVA_HOME / 安装目录 / 注册表 +
    /// 逐个 spawn `java -version`）耗时数百毫秒到数秒——**放到后台线程执行**，
    /// `tick` 收割结果，UI 线程零阻塞。
    pub fn scan_java(&mut self) {
        if self.java_scanning {
            return;
        }
        let slot: JavaScanSlot = Arc::new(std::sync::Mutex::new(None));
        let slot2 = slot.clone();
        std::thread::spawn(move || {
            let detected = find_java_installations();
            if let Ok(mut g) = slot2.lock() {
                *g = Some(detected);
            }
        });
        self.java_scan_slot = Some(slot);
        self.java_scanning = true;
        self.log("[INFO] 开始扫描本机 Java 运行时…");
        self.toast("正在扫描 Java…", ToastKind::Info);
    }

    fn finish_java_scan(&mut self, detected: Vec<official_launch::java::DetectedJava>) {
        self.java_scanning = false;
        self.java_scan_slot = None;
        // `find_java_installations` 已按版本降序返回，第一个即最新。
        let mut found = 0;
        for dj in &detected {
            if self.java_runtimes.iter().any(|j| j.path == dj.path) {
                continue;
            }
            self.java_runtimes.push(JavaRuntime {
                id: dj.path.clone(),
                path: dj.path.clone(),
                version: dj.version.clone(),
                vendor: dj.vendor.clone(),
                bits: if dj.arch == "x86" { 32 } else { 64 },
                source: JavaSource::Scanned,
            });
            found += 1;
        }
        if self.selected_java.is_none() && !self.java_runtimes.is_empty() {
            self.selected_java = self.java_runtimes.first().map(|j| j.id.clone());
        }
        self.log(format!(
            "[INFO] Java 扫描完成，发现 {} 个运行时（新增 {found}）",
            self.java_runtimes.len()
        ));
        if found == 0 {
            self.toast("未在本机发现 Java 运行时", ToastKind::Warn);
        } else {
            self.toast(
                format!("Java 扫描完成，发现 {} 个", self.java_runtimes.len()),
                ToastKind::Success,
            );
        }
        self.touch();
    }

    /// 从 Adoptium（Temurin）下载 JDK。真实 HTTPS 下载 + tar 解压，分钟级耗时
    /// —— 放到后台线程执行，`tick` 收割结果；UI 线程零阻塞。
    pub fn start_java_download(&mut self, major: u32) {
        if matches!(self.java_download, JavaDownloadState::Downloading { .. }) {
            return;
        }
        let Some(live) = self.live.clone() else {
            self.toast("演示模式下不下载 Java", ToastKind::Warn);
            return;
        };
        let slot = Arc::new(std::sync::Mutex::new(None));
        let slot2 = slot.clone();
        std::thread::spawn(move || {
            let r = live.download_java(major);
            if let Ok(mut g) = slot2.lock() {
                *g = Some(r);
            }
        });
        self.java_download_slot = Some(slot);
        self.java_download = JavaDownloadState::Downloading { major };
        self.log(format!(
            "[INFO] 开始从 Adoptium 下载 JDK {major}（后台进行，约 1~3 分钟）"
        ));
        self.toast(format!("开始下载 JDK {major}"), ToastKind::Info);
        self.touch();
    }

    /// 清除下载结果（回到空闲，允许再次下载）。
    pub fn clear_java_download(&mut self) {
        if !matches!(self.java_download, JavaDownloadState::Downloading { .. }) {
            self.java_download = JavaDownloadState::Idle;
            self.touch();
        }
    }

    pub fn add_java_manual(&mut self, path: &str) -> Result<(), String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("请填写 java.exe 的完整路径".to_owned());
        }
        if !path.to_ascii_lowercase().ends_with("java.exe")
            && !path.to_ascii_lowercase().ends_with("java")
        {
            return Err("路径看起来不是 Java 可执行文件".to_owned());
        }
        if self.java_runtimes.iter().any(|j| j.path == path) {
            return Err("该 Java 已在列表中".to_owned());
        }
        let id = self.uid("java");
        self.java_runtimes.push(JavaRuntime {
            id: id.clone(),
            path: path.to_owned(),
            version: "未知（手动指定）".to_owned(),
            vendor: "手动指定".to_owned(),
            bits: 64,
            source: JavaSource::Manual,
        });
        self.selected_java = Some(id);
        self.log(format!("[INFO] 手动指定 Java：{path}"));
        self.toast("已指定 Java 运行时", ToastKind::Success);
        self.touch();
        Ok(())
    }

    pub fn set_game_dir(&mut self, dir: &str) {
        self.game_dir = dir.trim().to_owned();
        if !self.game_dir.is_empty() {
            self.log(format!("[INFO] 游戏目录已设为 {}", self.game_dir));
            self.touch();
        }
    }

    // -----------------------------------------------------------------------
    // 下载任务
    // -----------------------------------------------------------------------

    pub fn add_download(
        &mut self,
        name: &str,
        category: DownloadCategory,
        total_mb: i32,
    ) -> String {
        let id = self.uid("dl");
        self.downloads.push(DownloadTask {
            id: id.clone(),
            name: name.to_owned(),
            category,
            total_mb,
            done_mb: 0,
            state: DownloadState::Waiting,
            error: None,
            speed_kb: 0,
        });
        self.selected_download = Some(id.clone());
        self.log(format!(
            "[INFO] 新建下载任务「{name}」（{}）",
            category.label()
        ));
        self.touch();
        id
    }

    fn download_transition(&mut self, id: &str, to: DownloadState) {
        let Some(pos) = self.downloads.iter().position(|d| d.id == id) else {
            return;
        };
        match self.downloads[pos].state.transition(to) {
            Ok(()) => {
                if to == DownloadState::Waiting {
                    self.downloads[pos].error = None;
                }
                self.touch();
            }
            Err(e) => self.illegal(e),
        }
    }

    pub fn pause_download(&mut self, id: &str) {
        self.download_transition(id, DownloadState::Paused);
    }

    pub fn resume_download(&mut self, id: &str) {
        self.download_transition(id, DownloadState::Downloading);
    }

    pub fn retry_download(&mut self, id: &str) {
        // 失败 → 等待 → 下载中，两步都要过状态机。
        let Some(pos) = self.downloads.iter().position(|d| d.id == id) else {
            return;
        };
        if self.downloads[pos].state != DownloadState::Failed {
            let from = self.downloads[pos].state.label().to_owned();
            self.illegal(TransitionError {
                flow: "下载任务",
                from,
                to: "重试".to_owned(),
            });
            return;
        }
        self.download_transition(id, DownloadState::Waiting);
        self.downloads[pos].error = None;
        self.download_transition(id, DownloadState::Downloading);
        self.log("[INFO] 下载任务已重试");
        self.toast("已重新加入下载队列", ToastKind::Info);
    }

    pub fn delete_download(&mut self, id: &str) {
        let Some(pos) = self.downloads.iter().position(|d| d.id == id) else {
            return;
        };
        let name = self.downloads[pos].name.clone();
        self.downloads.remove(pos);
        if self.selected_download.as_deref() == Some(id) {
            self.selected_download = None;
        }
        self.log(format!("[INFO] 移除下载任务「{name}」"));
        self.touch();
    }

    // -----------------------------------------------------------------------
    // 确认对话框
    // -----------------------------------------------------------------------

    pub fn request_confirm(&mut self, req: ConfirmRequest) {
        self.confirm = Some(req);
    }

    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    pub fn confirm_action(&mut self) {
        let Some(req) = self.confirm.take() else {
            return;
        };
        match req.action {
            PendingAction::DeleteVersion(id) => self.delete_version(&id),
            PendingAction::DeleteAccount(id) => self.delete_account(&id),
            PendingAction::DeleteDownload(id) => self.delete_download(&id),
            PendingAction::RestoreDefaults => {
                self.settings = Settings::default();
                self.log("[INFO] 设置已恢复默认");
                self.toast("已恢复默认设置", ToastKind::Success);
                self.touch();
            }
            PendingAction::RerunOnboarding => self.start_onboarding(true),
            PendingAction::ClearLogs => {
                self.logs.clear();
                self.log("[INFO] 日志已清空");
                self.toast("日志已清空", ToastKind::Info);
            }
            PendingAction::CancelLaunch => self.cancel_launch(),
            PendingAction::StopRunning => {
                let _ = self.finish_launch(0);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 引导
    // -----------------------------------------------------------------------

    pub fn start_onboarding(&mut self, reentry: bool) {
        if self.phase == LauncherPhase::Idle {
            if let Err(e) = self.phase.transition(LauncherPhase::Onboarding) {
                self.illegal(e);
                return;
            }
        }
        self.onboarding_step = 0;
        self.onboarding_reentry = reentry;
        self.form_error = None;
        self.log(if reentry {
            "[INFO] 从设置重新进入首次启动引导"
        } else {
            "[INFO] 进入首次启动引导"
        });
    }

    pub fn onboarding_back(&mut self) {
        self.onboarding_step = self.onboarding_step.saturating_sub(1);
        self.form_error = None;
    }

    /// 返回 `true` 表示引导完成并进入空闲态。
    pub fn onboarding_next(&mut self) -> bool {
        self.form_error = None;
        match self.onboarding_step {
            0 => {
                self.onboarding_step = 1;
                false
            }
            1 => {
                if self.selected_java().is_none() {
                    self.form_error = Some("请扫描或手动指定一个 Java 运行时".to_owned());
                    return false;
                }
                self.onboarding_step = 2;
                false
            }
            2 => {
                if self.game_dir.trim().is_empty() {
                    self.form_error = Some("请填写或导入游戏目录".to_owned());
                    return false;
                }
                self.onboarding_step = 3;
                false
            }
            _ => {
                self.onboarding_done = true;
                if self.phase == LauncherPhase::Onboarding {
                    if let Err(e) = self.phase.transition(LauncherPhase::Idle) {
                        self.illegal(e);
                        return false;
                    }
                }
                self.log("[INFO] 首次启动引导完成，配置已保存");
                self.toast("配置完成，欢迎使用启动器", ToastKind::Success);
                self.touch();
                true
            }
        }
    }

    // -----------------------------------------------------------------------
    // 设置
    // -----------------------------------------------------------------------

    pub fn restore_defaults_request(&mut self) {
        self.request_confirm(ConfirmRequest {
            title: "恢复默认设置".to_owned(),
            body: "所有游戏、下载、启动器、账户隐私设置都会恢复为默认值。此操作不可撤销。"
                .to_owned(),
            confirm_label: "恢复默认".to_owned(),
            danger: true,
            action: PendingAction::RestoreDefaults,
        });
    }

    pub fn refresh(&mut self) {
        self.loading = true;
        self.error = None;
        self.log("[INFO] F5：刷新版本、账户与下载状态");

        // 实时模式：从插件刷新实例列表，并真实扫描已安装的插件。
        // 全部插件调用放后台线程，UI 永不阻塞。
        if self.live.is_some() {
            let mem = self.settings.memory_mb;
            let sel_java = self.selected_java().map(|j| j.id.clone());
            self.spawn_plugin(move |live| {
                let list = live.list_instances();
                let mut mods_count: std::collections::HashMap<String, i32> =
                    std::collections::HashMap::new();
                if let Ok(instances) = &list {
                    for i in instances {
                        if let Ok(mods) = live.list_mods(&i.id) {
                            mods_count.insert(i.id.clone(), mods.len() as i32);
                        }
                    }
                }
                let plugins = live.discover_plugins();
                Box::new(move |d: &mut LauncherModel| match list {
                    Ok(instances) => {
                        let versions: Vec<GameVersion> = instances
                            .iter()
                            .map(|i| GameVersion {
                                id: i.id.clone(),
                                name: i.name.clone(),
                                mc_version: i.game_version.clone(),
                                loader: Self::map_loader(&i.loader),
                                java_id: sel_java.clone(),
                                memory_mb: mem,
                                size_mb: 0,
                                mods: mods_count.get(&i.id).copied().unwrap_or(0),
                                last_played: i.last_played.clone(),
                                flow: VersionFlow::Installed,
                                progress: 1.0,
                                // 刷新来源即插件列表，id 本身就是插件侧实例 id。
                                plugin_inst_id: Some(i.id.clone()),
                            })
                            .collect();
                        let cur = d
                            .current_version
                            .clone()
                            .filter(|c| versions.iter().any(|v| &v.id == c));
                        d.versions = versions;
                        d.plugins = plugins;
                        d.current_version =
                            cur.or_else(|| d.versions.first().map(|v| v.id.clone()));
                        // 注意：这里**不再**主动补齐资源。启动流程是"先尝试启动，
                        // 失败（未安装就绪 / 崩溃日志含资源缺失特征）才自动补齐并
                        // 重试启动"——见 start_launch 与 finish_launch。
                        d.log(format!(
                            "[INFO] 已从插件刷新实例列表（{} 个）",
                            d.versions.len()
                        ));
                        d.toast("已从插件刷新实例", ToastKind::Info);
                        d.touch();
                        d.loading = false;
                    }
                    Err(e) => {
                        d.log(format!("[ERROR] 从插件刷新实例失败：{e}"));
                        d.toast(format!("刷新失败：{e}"), ToastKind::Warn);
                        d.loading = false;
                        d.touch();
                    }
                })
            });
            return;
        }

        // 演示数据模式：仅本地状态刷新提示。
        self.toast("已刷新", ToastKind::Info);
    }

    // -----------------------------------------------------------------------
    // 插件安装 / 卸载（真实发现 + 目录复制；单二进制约束下非内置插件仅登记不加载）
    // -----------------------------------------------------------------------

    /// 从指定目录安装一个插件：复制其 `plugin.json` 与内容到插件目录，然后重新发现。
    ///
    /// 返回安装后的插件 id；若目录没有合法 `plugin.json` 则返回错误。
    /// **单进程单二进制约束**：只有内置 id 对应的代码被编译进启动器，其余插件会被发现并展示，
    /// 但标注「需重新编译启动器才能加载」——这是框架「无动态加载」的现实，不是缺陷。
    pub fn install_plugin_from(&mut self, src_dir: &Path) -> Result<String, String> {
        let manifest = src_dir.join("plugin.json");
        let bytes = std::fs::read(&manifest)
            .map_err(|e| format!("读取 {} 失败：{e}", manifest.display()))?;
        let parsed = crate::plugins_discovery::parse_manifest(&bytes, src_dir.to_path_buf())
            .ok_or_else(|| "plugin.json 非法（无法解析为插件清单）".to_owned())?;
        let id = parsed.id.clone();

        if let Some(live) = self.live.clone() {
            let dest = live.plugins_dir().join(&id);
            if dest.exists() {
                return Err(format!("插件 {id} 已存在于插件目录"));
            }
            // 复制目录 + 重新发现可能耗时（大插件目录），放后台线程执行。
            let src = src_dir.to_path_buf();
            let pid = id.clone();
            self.spawn_plugin(move |live| {
                let res = copy_dir(&src, &live.plugins_dir().join(&pid))
                    .map_err(|e| format!("复制插件失败：{e}"))
                    .map(|()| live.discover_plugins());
                Box::new(move |d: &mut LauncherModel| match res {
                    Ok(discovered) => {
                        d.plugins = discovered;
                        d.log(format!("[INFO] 已安装插件 {pid}"));
                        d.toast(format!("已安装插件 {pid}"), ToastKind::Success);
                        d.touch();
                    }
                    Err(e) => {
                        d.log(format!("[ERROR] 安装插件 {pid} 失败：{e}"));
                        d.toast(format!("安装插件失败：{e}"), ToastKind::Warn);
                        d.touch();
                    }
                })
            });
        } else {
            // 演示模式：仅在内存里登记（落不到磁盘）。
            if !self.plugins.iter().any(|p| p.id == id) {
                self.plugins.push(parsed);
            }
            self.log(format!(
                "[INFO] 已安装插件 {id}（来自 {}）",
                src_dir.display()
            ));
            self.toast(format!("已安装插件 {id}"), ToastKind::Success);
        }
        self.touch();
        Ok(id)
    }

    /// 卸载一个插件。内置插件不可卸载；非内置插件删除其目录并重新发现。
    /// 禁用 / 启用一个第三方插件：插件目录写/删 `disabled.flag`。
    /// 官方（com.example.official-*）与内置插件硬性拒绝。
    pub fn set_plugin_disabled(&mut self, id: &str, disabled: bool) -> Result<(), String> {
        let pos = self
            .plugins
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("未找到插件 {id}"))?;
        if self.plugins[pos].builtin || id.starts_with("com.example.official") {
            return Err("官方默认插件不可禁用或卸载".to_owned());
        }
        let dir = self.plugins[pos].dir.clone();
        let flag = dir.join("disabled.flag");
        let res = if disabled {
            std::fs::write(&flag, "disabled by user")
                .map_err(|e| format!("写入禁用标记失败：{e}"))
        } else {
            match std::fs::remove_file(&flag) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除禁用标记失败：{e}")),
            }
        };
        if let Err(e) = res {
            self.log(format!("[ERROR] 切换插件 {id} 禁用状态失败：{e}"));
            return Err(e);
        }
        self.plugins[pos].disabled = disabled;
        let verb = if disabled { "已禁用" } else { "已启用" };
        self.log(format!("[WARN] {verb}插件 {id}"));
        self.toast(format!("{verb}插件 {id}"), ToastKind::Info);
        self.touch();
        Ok(())
    }

    /// 远程安装插件：GitHub `owner/repo`、github.com 链接或任意 tar.gz 直链。
    /// 下载 + 解压 + 拷入插件目录全程在后台线程，结果经队列回收。
    pub fn install_plugin_from_remote(&mut self, source: &str) -> Result<(), String> {
        if self.remote_install_busy {
            return Err("已有远程安装任务进行中".to_owned());
        }
        let (url, _name) = crate::remote_plugin::resolve_remote(source)?;
        if self.live.is_none() {
            return Err("演示模式下无法远程安装（需要真实插件目录）".to_owned());
        }
        self.remote_install_busy = true;
        self.log(format!("[INFO] 开始下载插件包：{url}"));
        self.spawn_plugin(move |live| {
            let plugins_dir = live.plugins_dir().to_path_buf();
            let res = (|| -> Result<String, String> {
                let bytes = host_http::get_bytes(&url)?;
                let tmp = std::env::temp_dir().join(format!(
                    "aether-plugin-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0)
                ));
                let src = crate::remote_plugin::extract_tar_gz(&bytes, &tmp)?;
                let manifest = std::fs::read(src.join("plugin.json"))
                    .map_err(|e| format!("读取 plugin.json 失败：{e}"))?;
                let parsed = crate::plugins_discovery::parse_manifest(
                    &manifest,
                    src.clone(),
                )
                .ok_or("plugin.json 非法（无法解析为插件清单）")?;
                let id = parsed.id.clone();
                if id.starts_with("com.example.official") {
                    let _ = std::fs::remove_dir_all(&tmp);
                    return Err("不允许安装伪装官方插件的包".to_owned());
                }
                let dest = plugins_dir.join(&id);
                if dest.exists() {
                    let _ = std::fs::remove_dir_all(&tmp);
                    return Err(format!("插件 {id} 已存在于插件目录"));
                }
                copy_dir(&src, &dest).map_err(|e| format!("拷入插件目录失败：{e}"))?;
                let _ = std::fs::remove_dir_all(&tmp);
                Ok(id)
            })();
            let discovered = live.discover_plugins();
            Box::new(move |d: &mut LauncherModel| {
                d.remote_install_busy = false;
                match res {
                    Ok(id) => {
                        d.plugins = discovered;
                        d.log(format!("[INFO] 已从远程安装插件 {id}"));
                        d.toast(format!("已安装插件 {id}"), ToastKind::Success);
                    }
                    Err(e) => {
                        d.log(format!("[ERROR] 远程安装失败：{e}"));
                        d.toast(format!("远程安装失败：{e}"), ToastKind::Warn);
                    }
                }
                d.touch();
            })
        });
        self.touch();
        Ok(())
    }

    /// 后台补齐指定实例的资源对象；`then_relaunch` 时补齐完成自动重试启动。
    /// 这是"先启动、失败再补齐"策略的执行点（启动就绪检查失败 / 崩溃日志含
    /// 资源缺失特征时调用）。已有补齐在跑则忽略。
    pub fn begin_asset_repair(
        &mut self,
        targets: Vec<(String, String, String)>,
        then_relaunch: bool,
        reason: &str,
    ) {
        if self.repair_pending || targets.is_empty() {
            return;
        }
        if then_relaunch && self.auto_repair_retries >= 2 {
            self.log("[WARN] 自动补齐重试已达上限（2 次），不再自动启动；请手动启动或查看日志".to_owned());
            return;
        }
        self.repair_pending = true;
        self.log(format!(
            "[INFO] 检测到资源问题（{reason}），开始自动补齐 {} 个实例的资源（完成后{}）",
            targets.len(),
            if then_relaunch { "自动重试启动" } else { "提示" }
        ));
        self.toast("正在自动补齐游戏资源，完成后自动继续", ToastKind::Info);
        self.spawn_plugin(move |live| {
            let mut ok = 0usize;
            let mut failed: Vec<String> = Vec::new();
            for (id, mc, ld) in &targets {
                match live.install_minecraft(id, mc, ld) {
                    Ok(_) => ok += 1,
                    Err(e) => failed.push(format!("{mc}（{e}）")),
                }
            }
            let relaunch = then_relaunch && failed.is_empty();
            Box::new(move |d: &mut LauncherModel| {
                d.repair_pending = false;
                if failed.is_empty() {
                    d.log(format!(
                        "[INFO] 资源补齐完成（{ok} 个实例，含纹理/音效/natives）"
                    ));
                } else {
                    d.log(format!(
                        "[WARN] 资源补齐：{ok} 个成功，失败：{}",
                        failed.join("；")
                    ));
                    d.toast("资源补齐有失败项，详见日志", ToastKind::Warn);
                }
                if relaunch {
                    d.auto_repair_retries += 1;
                    d.log("[INFO] 补齐完成，自动重试启动…".to_owned());
                    if let Err(e) = d.start_launch() {
                        d.log(format!("[ERROR] 自动重试启动失败：{e}"));
                        d.toast(format!("自动重试启动失败：{e}"), ToastKind::Warn);
                    }
                } else if failed.is_empty() {
                    d.toast("资源补齐完成，可以启动游戏了", ToastKind::Success);
                }
                d.touch();
            })
        });
    }

    /// 判断游戏崩溃日志是否指向资源缺失（assets 对象 / natives 缺文件）。
    /// 是则启动失败后的"补齐并重试"才有意义。
    fn crash_looks_like_missing_assets(&self) -> bool {
        const MARKERS: [&str; 3] = [
            "NoSuchFileException",
            r"assets\objects",
            "assets/objects",
        ];
        self.logs
            .iter()
            .rev()
            .take(400)
            .any(|line| MARKERS.iter().any(|mk| line.contains(mk)))
    }

    pub fn uninstall_plugin(&mut self, id: &str) -> Result<(), String> {
        let pos = self
            .plugins
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("未找到插件 {id}"))?;
        if self.plugins[pos].builtin || id.starts_with("com.example.official") {
            return Err("官方默认插件不可卸载".to_owned());
        }
        if self.live.is_some() {
            // 删除插件目录 + 重新发现可能耗时，放后台线程执行。
            let uid = id.to_owned();
            self.spawn_plugin(move |live| {
                let dir = live.plugins_dir().join(&uid);
                let res = if dir.exists() {
                    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除插件目录失败：{e}"))
                } else {
                    Ok(())
                };
                let discovered = res.is_ok().then(|| live.discover_plugins());
                Box::new(move |d: &mut LauncherModel| match res {
                    Ok(()) => {
                        if let Some(ps) = discovered {
                            d.plugins = ps;
                        }
                        d.log(format!("[WARN] 已卸载插件 {uid}"));
                        d.toast(format!("已卸载插件 {uid}"), ToastKind::Warn);
                        d.touch();
                    }
                    Err(e) => {
                        d.log(format!("[ERROR] 卸载插件 {uid} 失败：{e}"));
                        d.toast(format!("卸载插件失败：{e}"), ToastKind::Warn);
                        d.touch();
                    }
                })
            });
        } else {
            self.plugins.remove(pos);
            self.log(format!("[WARN] 已卸载插件 {id}"));
            self.toast(format!("已卸载插件 {id}"), ToastKind::Warn);
        }
        self.touch();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 每帧推进（动画、状态机、即时保存）
    // -----------------------------------------------------------------------

    /// 把一次插件调用放到后台线程执行（通用异步化入口）。
    ///
    /// `job` 在后台线程完成真实的插件调用，并返回一个「应用闭包」描述结果如何写进
    /// 领域状态；UI 在每帧 `tick` 中收割执行。后台线程 panic 会被捕获并降级为
    /// 一条日志与 toast——UI 线程永不阻塞、永不因插件崩溃而受影响。
    fn spawn_plugin<J>(&self, job: J)
    where
        J: FnOnce(&crate::live::LiveDataSource) -> PluginApply + Send + 'static,
    {
        let Some(live) = self.live.clone() else {
            return;
        };
        let queue = self.plugin_queue.clone();
        std::thread::spawn(move || {
            let apply: PluginApply =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&live))).unwrap_or_else(
                    |_| {
                        Box::new(|d: &mut LauncherModel| {
                            d.log("[ERROR] 插件后台任务异常崩溃（panic），已安全忽略".to_owned());
                            d.toast("插件后台任务崩溃", ToastKind::Warn);
                        })
                    },
                );
            if let Ok(mut q) = queue.lock() {
                q.push(apply);
            }
        });
    }

    pub fn tick(&mut self, dt_ms: f32) {
        let dt = dt_ms.clamp(0.0, 200.0);
        self.time_ms = self.time_ms.saturating_add(dt as u64);

        // 页面插件热重载检测（每 ~500ms 一次，避免每帧 read_dir）。
        if self.time_ms % 500 < dt as u64 {
            self.poll_page_plugins();
        }

        // 插件异步任务收割：后台线程的插件调用结果在这里应用到 UI 状态（零阻塞）。
        let applies: Vec<PluginApply> = self
            .plugin_queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for apply in applies {
            apply(self);
        }

        for t in &mut self.toasts {
            t.age_ms = t.age_ms.saturating_add(dt as u64);
        }
        self.toasts.retain(|t| t.age_ms < t.ttl_ms);

        // Java 扫描：后台线程完成后在这里收割结果（UI 线程零阻塞）。
        if self.java_scanning {
            let done = self
                .java_scan_slot
                .as_ref()
                .and_then(|s| s.lock().ok().map(|mut g| g.take()))
                .flatten();
            if let Some(detected) = done {
                self.finish_java_scan(detected);
            }
        }

        // 资源补齐进度 → 下载中心任务（每帧同步，下载任何东西都在下载中心可见）。
        if let Some(live) = self.live.as_ref() {
            match live.asset_progress() {
                Some((inst, done, total, speed_kb)) => {
                    let name = self
                        .versions
                        .iter()
                        .find(|v| v.id == inst)
                        .map(|v| v.name.clone())
                        .unwrap_or_else(|| inst.clone());
                    let done = done.min(total);
                    let total_mb = ((total / 1024).max(1)) as i32;
                    let done_mb = (done / 1024).min(total_mb as u64) as i32;
                    let speed_kb = speed_kb.min(i32::MAX as u64) as i32;
                    if let Some(t) = self
                        .downloads
                        .iter_mut()
                        .find(|t| t.id == "asset-repair" && t.state == DownloadState::Downloading)
                    {
                        t.name = format!("游戏资源补齐（{name}）");
                        t.total_mb = total_mb;
                        t.done_mb = done_mb;
                        t.speed_kb = speed_kb;
                    } else {
                        // 不存在活动任务则新建（完成后旧任务保留在列表里）。
                        self.downloads.retain(|t| t.id != "asset-repair");
                        self.downloads.insert(
                            0,
                            DownloadTask {
                                id: "asset-repair".to_owned(),
                                name: format!("游戏资源补齐（{name}）"),
                                category: DownloadCategory::Asset,
                                total_mb,
                                done_mb,
                                state: DownloadState::Downloading,
                                error: None,
                                speed_kb,
                            },
                        );
                    }
                }
                None => {
                    // 补齐任务整体结束（repair_pending 已清）才标记完成；
                    // 实例之间的短暂空档不算结束，否则任务会提前"已完成"。
                    if !self.repair_pending {
                        if let Some(t) = self
                            .downloads
                            .iter_mut()
                            .find(
                                |t| t.id == "asset-repair" && t.state == DownloadState::Downloading,
                            )
                        {
                            t.done_mb = t.total_mb;
                            t.state = DownloadState::Completed;
                        }
                    }
                }
            }
        }

        // Java 下载：后台线程完成后在这里收割结果（UI 线程零阻塞）。
        if matches!(self.java_download, JavaDownloadState::Downloading { .. }) {
            let done = self
                .java_download_slot
                .as_ref()
                .and_then(|s| s.lock().ok().map(|mut g| g.take()))
                .flatten();
            if let Some(result) = done {
                let major = match self.java_download {
                    JavaDownloadState::Downloading { major } => major,
                    _ => 0,
                };
                match result {
                    Ok((path, version)) => {
                        // 自动把 bin/java.exe 注册进运行时列表。
                        let exe = std::path::Path::new(&path)
                            .join("bin")
                            .join("java.exe")
                            .display()
                            .to_string();
                        if let Err(e) = self.add_java_manual(&exe) {
                            self.log(format!("[WARN] 下载的 JDK 注册失败：{e}"));
                        }
                        self.java_download = JavaDownloadState::Done {
                            major,
                            path: path.clone(),
                            version: version.clone(),
                        };
                        self.log(format!("[INFO] JDK {major} 下载完成：{version} → {path}"));
                        self.toast(
                            format!("JDK {major}（{version}）下载完成"),
                            ToastKind::Success,
                        );
                    }
                    Err(e) => {
                        self.java_download = JavaDownloadState::Failed {
                            major,
                            error: e.clone(),
                        };
                        self.log(format!("[WARN] JDK {major} 下载失败：{e}"));
                        self.toast(format!("JDK {major} 下载失败：{e}"), ToastKind::Warn);
                    }
                }
                self.java_download_slot = None;
                self.touch();
            }
        }

        if self.auth_in_flight.is_some() {
            self.auth_ms += dt as u64;
            if self.auth_ms >= 1400 {
                let ok = !self.auth_should_fail;
                self.finish_auth(ok);
                self.auth_should_fail = false;
            }
        }

        // 版本安装进度。
        // 注意：循环里持有 &mut self.versions，不能同时调用 &mut self 的方法，
        // 所以先把「刚装完的版本名 / 非法迁移」收集起来，出了循环再处理。
        let mut installed = Vec::new();
        let mut version_errors = Vec::new();
        for v in &mut self.versions {
            if v.flow == VersionFlow::Installing {
                v.progress = (v.progress + dt / 6000.0).min(1.0);
                v.size_mb = (v.progress * 380.0) as i32;
                if v.progress >= 1.0 {
                    match v.flow.transition(VersionFlow::Installed) {
                        Ok(()) => installed.push(v.name.clone()),
                        Err(e) => version_errors.push(e),
                    }
                }
            }
        }
        for e in version_errors {
            self.illegal(e);
        }
        for name in installed {
            self.log(format!("[INFO] 版本「{name}」安装完成"));
            self.toast(format!("「{name}」安装完成"), ToastKind::Success);
            self.dirty = true;
        }

        // 下载任务推进（同样先改任务字段，再统一标记脏）。
        let mut download_changed = false;
        for task in &mut self.downloads {
            match task.state {
                DownloadState::Waiting => {
                    let _ = task.state.transition(DownloadState::Downloading);
                }
                DownloadState::Downloading => {
                    let inc = (dt / 1000.0) * (task.speed_kb.max(512) as f32) / 1024.0;
                    task.done_mb = (task.done_mb as f32 + inc).min(task.total_mb as f32) as i32;
                    if task.done_mb >= task.total_mb {
                        let _ = task.state.transition(DownloadState::Completed);
                        download_changed = true;
                    }
                }
                _ => {}
            }
        }
        if download_changed {
            self.dirty = true;
        }

        // 启动阶段推进：检查 → 下载 → 验证 → 启动
        if self.phase == LauncherPhase::Launching {
            if let Some(live) = self.live.clone() {
                // 实时模式：进度条仅作阶段提示并封顶，真正的"完成"由真实退出码驱动。
                self.launch.pct = (self.launch.pct + dt / 4200.0).min(0.94);
                let (stage, detail) = if self.launch.pct < 0.25 {
                    (LaunchStage::Checking, "检查 Java、账户与游戏目录")
                } else if self.launch.pct < 0.55 {
                    (LaunchStage::Downloading, "下载缺失的资源与依赖库")
                } else if self.launch.pct < 0.82 {
                    (LaunchStage::Verifying, "验证文件完整性与 JVM 参数")
                } else {
                    (LaunchStage::Launching, "正在运行真实游戏进程")
                };
                if stage != self.launch.stage {
                    self.launch.stage = stage;
                    self.launch.detail = detail.to_owned();
                    self.log(format!("[INFO] 启动阶段：{}", stage.label()));
                }
                // 拉取游戏进程输出到日志管理器（增量，不阻塞）。
                for line in live.drain_game_log() {
                    self.log(line);
                }
                // 轮询真实进程退出码；就绪则收尾（`finish_launch` 会回到空闲态）。
                if let Some(code) = live.launch_status() {
                    let _ = self.phase.transition(LauncherPhase::Running);
                    let id = self.current_version.clone();
                    if let Some(id) = id {
                        if let Some(v) = self.versions.iter_mut().find(|v| v.id == id) {
                            let _ = v.flow.transition(VersionFlow::Running);
                        }
                    }
                    let _ = self.finish_launch(code);
                }
            } else {
                // 演示模式：模拟推进，约 4 秒后自动进入"运行中"。
                self.launch.pct = (self.launch.pct + dt / 4200.0).min(1.0);
                let (stage, detail) = if self.launch.pct < 0.25 {
                    (LaunchStage::Checking, "检查 Java、账户与游戏目录")
                } else if self.launch.pct < 0.55 {
                    (LaunchStage::Downloading, "下载缺失的资源与依赖库")
                } else if self.launch.pct < 0.82 {
                    (LaunchStage::Verifying, "验证文件完整性与 JVM 参数")
                } else {
                    (LaunchStage::Launching, "正在启动游戏进程")
                };
                if stage != self.launch.stage {
                    self.launch.stage = stage;
                    self.launch.detail = detail.to_owned();
                    self.log(format!("[INFO] 启动阶段：{}", stage.label()));
                }
                if self.launch.pct >= 1.0 {
                    let _ = self.phase.transition(LauncherPhase::Running);
                    let id = self.current_version.clone();
                    if let Some(id) = id {
                        if let Some(v) = self.versions.iter_mut().find(|v| v.id == id) {
                            let _ = v.flow.transition(VersionFlow::Running);
                        }
                    }
                    self.log("[INFO] 游戏已启动（simulated：未创建任何 OS 进程）");
                    self.toast("游戏已启动", ToastKind::Success);
                    self.pending_window_action = match self.settings.after_launch {
                        AfterLaunch::Keep => None,
                        // 最小化一律隐藏到系统托盘。
                        AfterLaunch::Minimize => {
                            self.tray_hidden = true;
                            Some(ui_core::WindowAction::MinimizeToTray)
                        }
                        AfterLaunch::Exit => Some(ui_core::WindowAction::Close),
                    };
                    self.dirty = true;
                }
            }
        }

        if self.loading && self.time_ms > 450 {
            self.loading = false;
        }

        // 更新提示（模拟检查更新，3.5 秒后出现）
        if self.settings.auto_update
            && !self.update_dismissed
            && self.update_available.is_none()
            && self.time_ms > 3500
        {
            self.update_available = Some("1.9.2".to_owned());
            self.log("[INFO] 检测到新版本 1.9.2");
        }

        // 即时保存：改动由 touch() 标记，帧末统一落盘。
        if self.dirty {
            self.save();
            self.dirty = false;
        }
    }

    // -----------------------------------------------------------------------
    // 持久化
    // -----------------------------------------------------------------------

    pub fn enable_persistence(&mut self, path: PathBuf) {
        self.config_path = path;
        self.persist_enabled = true;
        self.load();
    }

    fn snapshot(&self) -> PersistState {
        PersistState {
            onboarding_done: self.onboarding_done,
            game_dir: self.game_dir.clone(),
            selected_java: self.selected_java.clone(),
            current_version: self.current_version.clone(),
            current_account: self.current_account.clone(),
            versions: self.versions.clone(),
            accounts: self.accounts.clone(),
            java_runtimes: self.java_runtimes.clone(),
            downloads: self.downloads.clone(),
            settings: self.settings.clone(),
            logs: self.logs.clone(),
        }
    }

    pub fn save(&mut self) {
        if !self.persist_enabled {
            return;
        }
        let Ok(json) = serde_json::to_string_pretty(&self.snapshot()) else {
            return;
        };
        if let Err(e) =
            launcher_plugin_common::fsutil::atomic_write(&self.config_path, json.as_bytes())
        {
            // 写不进去必须让用户看见：完整路径 + 失败步骤，日志页直接可查。
            self.log(format!("[ERROR] 保存启动器配置失败：{e}"));
        }
    }

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.config_path) else {
            // 配置文件不存在 = 首次启动。切换到引导态并立即落盘，
            // 这样「引导仅首次出现」由持久化状态决定，而不是靠进程内存。
            let path = self.config_path.clone();
            let enabled = self.persist_enabled;
            *self = Self::first_run();
            self.config_path = path;
            self.persist_enabled = enabled;
            self.save();
            return;
        };
        let Ok(s) = serde_json::from_str::<PersistState>(&text) else {
            self.log("[WARN] 配置文件无法解析，已使用默认配置");
            return;
        };
        self.onboarding_done = s.onboarding_done;
        self.game_dir = s.game_dir;
        self.selected_java = s.selected_java;
        self.current_version = s.current_version;
        self.current_account = s.current_account;
        self.versions = s.versions;
        self.accounts = s.accounts;
        self.java_runtimes = s.java_runtimes;
        self.downloads = s.downloads;
        self.settings = s.settings;
        self.logs = s.logs;
        if self.onboarding_done {
            self.phase = LauncherPhase::Idle;
        } else {
            self.phase = LauncherPhase::Onboarding;
            self.onboarding_step = 0;
        }
        // 落盘的状态里可能残留启动中/运行中，回到空闲态避免卡死。
        if self.phase == LauncherPhase::Launching || self.phase == LauncherPhase::Running {
            self.phase = LauncherPhase::Idle;
        }
        self.log("[INFO] 已从配置文件恢复设置");
    }
}

/// 递归复制目录（用于插件安装）。`fs_extra` 之类依赖会增加体积，这里用标准库手写。
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
