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
use std::path::PathBuf;

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
}

impl DownloadCategory {
    pub const ALL: [DownloadCategory; 5] = [
        DownloadCategory::Version,
        DownloadCategory::Mod,
        DownloadCategory::ResourcePack,
        DownloadCategory::Shader,
        DownloadCategory::Java,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Version => "游戏版本",
            Self::Mod => "Mod",
            Self::ResourcePack => "资源包",
            Self::Shader => "光影",
            Self::Java => "Java",
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
    pub animations: bool,
    pub radius: i32,
    pub font_family: String,
    pub minimize_to_tray: bool,
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
            animations: true,
            radius: 8,
            font_family: "Microsoft YaHei UI".to_owned(),
            minimize_to_tray: true,
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
}

impl Default for LaunchProgress {
    fn default() -> Self {
        Self {
            stage: LaunchStage::Checking,
            pct: 0.0,
            detail: "准备中".to_owned(),
            exit_code: None,
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

    // 表单与向导的临时输入
    pub form_version_name: String,
    pub form_mc_version: String,
    pub form_loader: Loader,
    pub form_java_path: String,
    pub form_game_dir: String,
    pub form_offline_name: String,
    pub form_download_url: String,
    pub form_error: Option<String>,
    pub versions_filter: String,
    pub download_filter: DownloadCategory,
    pub selected_download: Option<String>,
    pub open_version_detail: Option<String>,
    pub settings_tab: usize,
    /// 日志筛选：0 全部 / 1 INFO / 2 WARN / 3 ERROR。
    pub log_filter: usize,
    pub java_scanning: bool,

    // 计时与身份
    pub time_ms: u64,
    pub next_id: u64,
    /// 运行期开关：测试与离屏渲染不碰磁盘，真实窗口启动时才打开。
    pub persist_enabled: bool,
    pub config_path: PathBuf,
    dirty: bool,
    scan_ms: u64,
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
            form_version_name: String::new(),
            form_mc_version: "1.20.1".to_owned(),
            form_loader: Loader::Fabric,
            form_java_path: String::new(),
            form_game_dir: String::new(),
            form_offline_name: String::new(),
            form_download_url: String::new(),
            form_error: None,
            versions_filter: String::new(),
            download_filter: DownloadCategory::Version,
            selected_download: Some("dl-1".to_owned()),
            open_version_detail: None,
            settings_tab: 0,
            log_filter: 0,
            java_scanning: false,
            time_ms: 0,
            next_id: 100,
            persist_enabled: false,
            config_path: PathBuf::from("launcher-config.json"),
            dirty: false,
            scan_ms: 0,
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
        m
    }

    fn uid(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    pub fn touch(&mut self) {
        self.dirty = true;
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
        };
        self.show_logs_after_crash = false;
        self.error = None;
        self.log("[INFO] 开始启动流程：检查 → 下载 → 验证 → 启动");
        self.toast("已通过前置校验，开始启动", ToastKind::Info);
        self.touch();
        Ok(())
    }

    /// 游戏结束。异常退出码会写日志并弹出日志提示。
    pub fn finish_launch(&mut self, exit_code: i32) -> Result<(), String> {
        if self.phase != LauncherPhase::Running {
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
        }
        self.touch();
        Ok(())
    }

    pub fn cancel_launch(&mut self) {
        if self.phase != LauncherPhase::Launching {
            return;
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
        if self.current_version.as_deref() == Some(id) {
            self.current_version = None;
            if self.phase == LauncherPhase::Launching || self.phase == LauncherPhase::Running {
                let _ = self.phase.transition(LauncherPhase::Idle);
            }
        }
        self.versions.remove(pos);
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
        });
        self.add_download(
            &format!("{mc}-{}.jar", loader.label()),
            DownloadCategory::Version,
            180,
        );
        self.log(format!(
            "[INFO] 开始安装版本「{name}」（{mc} / {}）",
            loader.label()
        ));
        self.toast(format!("开始安装「{name}」"), ToastKind::Info);
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

    pub fn scan_java(&mut self) {
        if self.java_scanning {
            return;
        }
        self.java_scanning = true;
        self.scan_ms = 0;
        self.log("[INFO] 开始扫描本机 Java 运行时…");
        self.toast("正在扫描 Java…", ToastKind::Info);
    }

    fn finish_java_scan(&mut self) {
        self.java_scanning = false;
        let candidates = [
            (
                "java-17",
                r"C:\Program Files\Java\jdk-17\bin\java.exe",
                "17.0.9",
                "Eclipse Temurin",
            ),
            (
                "java-21",
                r"C:\Program Files\Eclipse Adoptium\jdk-21\bin\java.exe",
                "21.0.1",
                "Eclipse Adoptium",
            ),
            (
                "java-8",
                r"C:\Program Files\Java\jre1.8.0_391\bin\java.exe",
                "1.8.0_391",
                "Oracle",
            ),
        ];
        let mut found = 0;
        for (id, path, ver, vendor) in candidates {
            if self.java_runtimes.iter().any(|j| j.id == id) {
                continue;
            }
            self.java_runtimes.push(JavaRuntime {
                id: id.to_owned(),
                path: path.to_owned(),
                version: ver.to_owned(),
                vendor: vendor.to_owned(),
                bits: 64,
                source: JavaSource::Scanned,
            });
            found += 1;
        }
        if self.selected_java.is_none() {
            self.selected_java = self.java_runtimes.first().map(|j| j.id.clone());
        }
        self.log(format!("[INFO] Java 扫描完成，新增 {found} 个运行时"));
        self.toast(
            format!("Java 扫描完成，发现 {} 个", self.java_runtimes.len()),
            ToastKind::Success,
        );
        self.touch();
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
        self.toast("已刷新", ToastKind::Info);
    }

    // -----------------------------------------------------------------------
    // 每帧推进（动画、状态机、即时保存）
    // -----------------------------------------------------------------------

    pub fn tick(&mut self, dt_ms: f32) {
        let dt = dt_ms.clamp(0.0, 200.0);
        self.time_ms = self.time_ms.saturating_add(dt as u64);

        for t in &mut self.toasts {
            t.age_ms = t.age_ms.saturating_add(dt as u64);
        }
        self.toasts.retain(|t| t.age_ms < t.ttl_ms);

        if self.java_scanning {
            self.scan_ms += dt as u64;
            if self.scan_ms >= 900 {
                self.finish_java_scan();
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
                    AfterLaunch::Minimize => {
                        if self.settings.minimize_to_tray {
                            self.tray_hidden = true;
                            Some(ui_core::WindowAction::MinimizeToTray)
                        } else {
                            Some(ui_core::WindowAction::Minimize)
                        }
                    }
                    AfterLaunch::Exit => Some(ui_core::WindowAction::Close),
                };
                self.dirty = true;
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
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.config_path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, &self.config_path);
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
