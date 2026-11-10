//! 完整界面与交互的回归测试：四条严格状态机、唯一当前版本/账户、
//! 引导仅首次出现、启动流程、下载任务、全局快捷键与持久化。

use launcher_ui::domain::*;
use launcher_ui::shell::{self, Page};
use std::path::PathBuf;
use ui_core::ShortcutKey;

fn tick_for(m: &mut LauncherModel, ms: f32) {
    let mut left = ms;
    while left > 0.0 {
        let step = left.min(200.0);
        m.tick(step);
        left -= step;
    }
}

// ---------------------------------------------------------------------------
// 四条状态机
// ---------------------------------------------------------------------------

#[test]
fn launcher_phase_only_allows_documented_transitions() {
    let mut p = LauncherPhase::Onboarding;
    assert_eq!(p.transition(LauncherPhase::Idle), Ok(()));
    assert_eq!(p.transition(LauncherPhase::Launching), Ok(()));
    assert_eq!(p.transition(LauncherPhase::Running), Ok(()));
    assert_eq!(p.transition(LauncherPhase::Idle), Ok(()));
    // 空闲不能直接跳到运行中
    assert!(p.transition(LauncherPhase::Running).is_err());
    // 运行中不能直接再启动
    let mut q = LauncherPhase::Running;
    assert!(q.transition(LauncherPhase::Launching).is_err());
    // 设置里重入引导是唯一允许的回流
    assert_eq!(p.transition(LauncherPhase::Onboarding), Ok(()));
}

#[test]
fn account_flow_covers_success_and_failure() {
    let mut a = AccountFlow::SignedOut;
    assert!(
        a.transition(AccountFlow::SignedIn).is_err(),
        "未登录不能直接变已登录"
    );
    assert_eq!(a.transition(AccountFlow::SigningIn), Ok(()));
    assert_eq!(a.transition(AccountFlow::Failed("令牌无效".into())), Ok(()));
    assert_eq!(a.transition(AccountFlow::SigningIn), Ok(()), "失败后可重试");
    assert_eq!(a.transition(AccountFlow::SignedIn), Ok(()));
    assert_eq!(a.transition(AccountFlow::SignedOut), Ok(()), "可退出登录");
}

#[test]
fn version_flow_covers_install_and_launch() {
    let mut v = VersionFlow::NotInstalled;
    assert!(v.transition(VersionFlow::Running).is_err());
    assert_eq!(v.transition(VersionFlow::Installing), Ok(()));
    assert_eq!(v.transition(VersionFlow::Installed), Ok(()));
    assert_eq!(v.transition(VersionFlow::Launching), Ok(()));
    assert_eq!(v.transition(VersionFlow::Running), Ok(()));
    assert_eq!(
        v.transition(VersionFlow::Installed),
        Ok(()),
        "游戏退出回到已安装"
    );
}

#[test]
fn download_state_covers_pause_resume_retry() {
    let mut d = DownloadState::Waiting;
    assert!(d.transition(DownloadState::Completed).is_err());
    assert_eq!(d.transition(DownloadState::Downloading), Ok(()));
    assert_eq!(d.transition(DownloadState::Paused), Ok(()));
    assert_eq!(d.transition(DownloadState::Downloading), Ok(()));
    assert_eq!(d.transition(DownloadState::Failed), Ok(()));
    assert_eq!(d.transition(DownloadState::Waiting), Ok(()), "失败可重试");
    let mut done = DownloadState::Completed;
    assert!(
        done.transition(DownloadState::Downloading).is_err(),
        "完成是终态"
    );
}

// ---------------------------------------------------------------------------
// 启动按钮：未选版本 / 未登录必须置灰并给出对应文案
// ---------------------------------------------------------------------------

#[test]
fn launch_button_is_disabled_with_correct_hint() {
    let mut m = LauncherModel::demo();
    assert_eq!(m.launch_label().0, "启动游戏");
    assert!(m.launch_label().1);

    m.current_version = None;
    assert_eq!(m.launch_label().0, "请先选择一个游戏版本");
    assert!(!m.launch_label().1, "未选版本时启动按钮必须置灰");
    assert!(m.start_launch().is_err());

    m.select_version("v-fabric-1201");
    m.current_account = None;
    assert_eq!(m.launch_label().0, "请先登录账户");
    assert!(!m.launch_label().1, "未登录时启动按钮必须置灰");
    assert!(m.start_launch().is_err());
}

#[test]
fn preflight_checks_java_memory_and_game_dir() {
    let mut m = LauncherModel::demo();
    m.selected_java = None;
    assert!(m.preflight().iter().any(|s| s.contains("Java")));
    m.selected_java = Some("java-17".into());
    m.settings.memory_mb = 256;
    assert!(m.preflight().iter().any(|s| s.contains("512 MB")));
    m.settings.memory_mb = 4096;
    m.game_dir.clear();
    assert!(m.preflight().iter().any(|s| s.contains("游戏目录")));
}

// ---------------------------------------------------------------------------
// 启动流程：检查 → 下载 → 验证 → 启动 → 运行 → 空闲
// ---------------------------------------------------------------------------

#[test]
fn full_launch_flow_follows_the_state_machine() {
    let mut m = LauncherModel::demo();
    assert_eq!(m.phase, LauncherPhase::Idle);
    m.start_launch().unwrap();
    assert_eq!(m.phase, LauncherPhase::Launching);
    assert_eq!(m.launch.stage, LaunchStage::Checking);
    assert!(m.start_launch().is_err(), "启动中不能重复启动");

    tick_for(&mut m, 1400.0);
    assert_eq!(m.launch.stage, LaunchStage::Downloading, "阶段应推进到下载");
    tick_for(&mut m, 1200.0);
    assert_eq!(m.launch.stage, LaunchStage::Verifying, "阶段应推进到验证");
    tick_for(&mut m, 2200.0);
    assert_eq!(m.phase, LauncherPhase::Running);
    assert_eq!(m.current_version().unwrap().flow, VersionFlow::Running);
    assert!(
        m.pending_window_action.is_some(),
        "启动后应按设置最小化或退出"
    );

    m.finish_launch(0).unwrap();
    assert_eq!(m.phase, LauncherPhase::Idle);
    assert_eq!(m.current_version().unwrap().flow, VersionFlow::Installed);
}

#[test]
fn abnormal_exit_records_log_and_opens_log_hint() {
    let mut m = LauncherModel::demo();
    m.start_launch().unwrap();
    tick_for(&mut m, 5000.0);
    assert_eq!(m.phase, LauncherPhase::Running);
    m.finish_launch(-1).unwrap();
    assert!(m.show_logs_after_crash);
    assert!(m.error.as_deref().unwrap().contains("异常退出"));
    assert!(m.logs.iter().any(|l| l.contains("[ERROR] 游戏异常退出")));
    assert!(m.toasts.iter().any(|t| t.kind == ToastKind::Danger));
}

#[test]
fn cancel_launch_returns_to_idle() {
    let mut m = LauncherModel::demo();
    m.start_launch().unwrap();
    m.cancel_launch();
    assert_eq!(m.phase, LauncherPhase::Idle);
    assert_eq!(m.current_version().unwrap().flow, VersionFlow::Installed);
}

// ---------------------------------------------------------------------------
// 唯一当前版本 / 当前账户，删除必须二次确认且不可恢复
// ---------------------------------------------------------------------------

#[test]
fn deleting_current_version_clears_selection_everywhere() {
    let mut m = LauncherModel::demo();
    let id = m.current_version.clone().unwrap();
    assert_eq!(m.current_version().unwrap().id, id);
    m.request_confirm(shell::danger_confirm(
        "删除版本",
        "该操作不可恢复",
        PendingAction::DeleteVersion(id.clone()),
    ));
    assert!(m.confirm.as_ref().unwrap().danger, "删除必须用危险色确认");
    m.confirm_action();
    assert!(
        m.current_version.is_none(),
        "删除当前版本后所有页面都查不到当前版本"
    );
    assert!(m.current_version().is_none());
    assert!(!m.versions.iter().any(|v| v.id == id));
    assert!(m.logs.iter().any(|l| l.contains("不可恢复")));
}

#[test]
fn deleting_current_account_clears_login_state() {
    let mut m = LauncherModel::demo();
    assert!(m.is_logged_in());
    let id = m.current_account.clone().unwrap();
    m.request_confirm(shell::danger_confirm(
        "删除账户",
        "该操作不可恢复",
        PendingAction::DeleteAccount(id),
    ));
    m.confirm_action();
    assert!(m.current_account.is_none());
    assert!(!m.is_logged_in());
    assert_eq!(m.launch_label().0, "请先登录账户");
}

#[test]
fn escape_cancels_confirmation() {
    let mut m = LauncherModel::demo();
    let id = m.current_version.clone().unwrap();
    m.request_confirm(shell::danger_confirm(
        "x",
        "y",
        PendingAction::DeleteVersion(id),
    ));
    m.cancel_confirm();
    assert!(m.confirm.is_none());
    assert!(m.current_version().is_some(), "取消确认不能删任何东西");
}

#[test]
fn only_one_current_version_and_account_exist() {
    let mut m = LauncherModel::demo();
    m.select_version("v-forge-1202");
    assert_eq!(m.current_version().unwrap().id, "v-forge-1202");
    assert_eq!(
        m.versions
            .iter()
            .filter(|v| Some(v.id.as_str()) == m.current_version.as_deref())
            .count(),
        1
    );
    let acc = m.add_offline_account("Alex").unwrap();
    assert_eq!(m.current_account.as_deref(), Some(acc.as_str()));
    assert_eq!(
        m.accounts
            .iter()
            .filter(|a| Some(a.id.as_str()) == m.current_account.as_deref())
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// 引导：仅首次出现、四步、设置可重入
// ---------------------------------------------------------------------------

#[test]
fn onboarding_walks_four_steps_and_enters_idle() {
    let mut m = LauncherModel::first_run();
    assert_eq!(m.phase, LauncherPhase::Onboarding);
    assert!(!m.preflight().is_empty(), "引导中不能启动游戏");
    assert!(!m.onboarding_next()); // 欢迎 → Java
    assert_eq!(m.onboarding_step, 1);
    assert!(!m.onboarding_next(), "没有 Java 时不能前进");
    m.scan_java();
    tick_for(&mut m, 1200.0);
    assert!(m.selected_java().is_some(), "扫描应找到 Java");
    assert!(!m.onboarding_next()); // → 游戏目录
    assert_eq!(m.onboarding_step, 2);
    assert!(!m.onboarding_next(), "没填游戏目录不能前进");
    m.set_game_dir(r"D:\Minecraft\.minecraft");
    assert!(!m.onboarding_next()); // → 配置汇总
    assert_eq!(m.onboarding_step, 3);
    assert!(m.onboarding_next(), "最后一步完成引导");
    assert_eq!(m.phase, LauncherPhase::Idle);
    assert!(m.onboarding_done);
}

#[test]
fn onboarding_can_be_reentered_from_settings() {
    let mut m = LauncherModel::demo();
    assert_eq!(m.phase, LauncherPhase::Idle);
    m.request_confirm(ConfirmRequest {
        title: "重新运行首次启动引导".into(),
        body: "现有数据不会被删除".into(),
        confirm_label: "开始引导".into(),
        danger: false,
        action: PendingAction::RerunOnboarding,
    });
    m.confirm_action();
    assert_eq!(m.phase, LauncherPhase::Onboarding);
    assert!(m.onboarding_reentry);
    assert_eq!(m.onboarding_step, 0);
}

#[test]
fn persistence_marks_first_run_only_when_config_is_missing() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("flow-persist-test.json");
    let _ = std::fs::remove_file(&path);

    let mut first = LauncherModel::demo();
    first.enable_persistence(path.clone());
    assert_eq!(
        first.phase,
        LauncherPhase::Onboarding,
        "无配置文件时进入引导"
    );
    first.onboarding_done = true;
    first.phase = LauncherPhase::Idle;
    first.settings.memory_mb = 8192;
    first.save();

    let mut second = LauncherModel::demo();
    second.enable_persistence(path.clone());
    assert_eq!(second.phase, LauncherPhase::Idle, "再次启动不再出现引导");
    assert_eq!(second.settings.memory_mb, 8192);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 下载任务 / 版本安装 / 账户登录
// ---------------------------------------------------------------------------

#[test]
fn download_tasks_pause_resume_and_retry() {
    let mut m = LauncherModel::demo();
    m.resume_download("dl-1"); // Paused → Downloading
    assert_eq!(
        m.downloads.iter().find(|d| d.id == "dl-1").unwrap().state,
        DownloadState::Downloading
    );
    m.pause_download("dl-1");
    assert_eq!(
        m.downloads.iter().find(|d| d.id == "dl-1").unwrap().state,
        DownloadState::Paused
    );
    m.retry_download("dl-4"); // Failed → Waiting → Downloading
    let t = m.downloads.iter().find(|d| d.id == "dl-4").unwrap();
    assert_eq!(t.state, DownloadState::Downloading);
    assert!(t.error.is_none(), "重试要清掉旧错误");
    let before = m.logs.len();
    m.retry_download("dl-3"); // Completed 不该被重试
    assert!(m.logs.len() > before, "非法迁移必须写日志");
}

#[test]
fn installing_a_version_follows_version_flow() {
    let mut m = LauncherModel::demo();
    let id = m
        .install_version("Test 1.20.4", "1.20.4", Loader::Quilt)
        .unwrap();
    assert_eq!(
        m.versions.iter().find(|v| v.id == id).unwrap().flow,
        VersionFlow::Installing
    );
    assert!(m
        .downloads
        .iter()
        .any(|d| d.category == DownloadCategory::Version));
    tick_for(&mut m, 7000.0);
    assert_eq!(
        m.versions.iter().find(|v| v.id == id).unwrap().flow,
        VersionFlow::Installed
    );
    assert!(m.preflight().is_empty());
}

#[test]
fn microsoft_auth_success_failure_and_retry() {
    let mut m = LauncherModel::demo();
    m.start_microsoft_auth(true).unwrap();
    assert!(m.accounts.iter().any(|a| a.flow == AccountFlow::SigningIn));
    tick_for(&mut m, 1800.0);
    let failed = m.current_account().unwrap();
    assert!(matches!(failed.flow, AccountFlow::Failed(_)));
    assert!(!m.is_logged_in());
    m.retry_auth();
    tick_for(&mut m, 1800.0);
    assert!(m.is_logged_in());
}

#[test]
fn invalid_inputs_are_rejected() {
    let mut m = LauncherModel::demo();
    assert!(m.install_version("", "1.20.1", Loader::Vanilla).is_err());
    assert!(m.install_version("X", "abc", Loader::Vanilla).is_err());
    assert!(m.add_offline_account("").is_err());
    assert!(m.add_offline_account("bad name!").is_err());
    assert!(m.add_java_manual("").is_err());
    assert!(m.add_java_manual("C:\\not-java.txt").is_err());
}

// ---------------------------------------------------------------------------
// 全局快捷键 / 拖拽安装 / 设置恢复默认
// ---------------------------------------------------------------------------

#[test]
fn ctrl_shortcuts_map_to_pages() {
    assert_eq!(
        shell::shortcut_page(&[ShortcutKey::Ctrl('1')]),
        Some(Page::Home)
    );
    assert_eq!(
        shell::shortcut_page(&[ShortcutKey::Ctrl('2')]),
        Some(Page::Versions)
    );
    assert_eq!(
        shell::shortcut_page(&[ShortcutKey::Ctrl('6')]),
        Some(Page::Logs)
    );
    assert_eq!(
        shell::shortcut_page(&[ShortcutKey::Ctrl('L')]),
        Some(Page::Logs)
    );
    assert_eq!(shell::shortcut_page(&[ShortcutKey::F5]), None);
}

#[test]
fn dropped_files_join_install_queue() {
    let mut m = LauncherModel::demo();
    let before = m.downloads.len();
    m.install_dropped_files(&[r"D:\packs\Fabric-1.20.1.zip".to_owned()]);
    assert!(m.downloads.len() > before);
    assert!(m.logs.iter().any(|l| l.contains("拖入文件")));
    assert!(m.versions.iter().any(|v| v.name.contains("Fabric")));
}

#[test]
fn restore_defaults_goes_through_danger_confirm() {
    let mut m = LauncherModel::demo();
    m.settings.memory_mb = 1024;
    m.restore_defaults_request();
    assert!(m.confirm.as_ref().unwrap().danger);
    m.confirm_action();
    assert_eq!(m.settings.memory_mb, Settings::default().memory_mb);
}

#[test]
fn app_pages_stay_compatible_with_old_route_names() {
    assert_eq!(Page::Instances.route(), Page::Versions);
    assert_eq!(Page::Mods.route(), Page::Downloads);
    assert_eq!(Page::Launch.route(), Page::Home);
    assert_eq!(Page::from_arg("instances"), Page::Versions);
    assert_eq!(Page::from_arg("mods"), Page::Downloads);
    assert_eq!(Page::NAV.len(), 6);
}
