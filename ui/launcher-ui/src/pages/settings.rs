//! 设置：游戏 / 下载 / 启动器 / 账户隐私 / 关于。
//!
//! 所有控件改动立即写回 `LauncherModel` 并 `touch()`（即时保存）；
//! 「保存」是显式落盘，「恢复默认」走危险色确认。

use crate::domain::{AfterLaunch, ConfirmRequest, LauncherModel, PendingAction, ToastKind};
use crate::shell::{section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Rect, TextAlign, Theme, Ui, WidgetId};

const TABS: [&str; 5] = ["游戏", "下载", "启动器", "账户隐私", "关于"];

/// 开关项：文案 + 读函数 + 写函数。用类型别名避免内联函数指针类型过长。
type Toggle<'a> = (
    &'a str,
    fn(&LauncherModel) -> bool,
    fn(&mut LauncherModel, bool),
);

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 260, 32), "设置");
    ui.caption(
        Rect::new(r.x + 266, r.y, r.w - 266, 32),
        "改动即时保存，也可显式保存或恢复默认",
    );

    // ---- 分组标签 ----
    let tab_y = r.y + 38;
    let tw = r.w / TABS.len() as i32;
    for (i, name) in TABS.iter().enumerate() {
        let rect = Rect::new(r.x + i as i32 * tw, tab_y, tw - 4, theme.control_h_small);
        if ui
            .button(
                rect,
                name,
                if m.settings_tab == i {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Normal
                },
                true,
            )
            .clicked
        {
            m.settings_tab = i;
        }
    }

    // ---- 内容 ----
    let body = Rect::new(
        r.x,
        tab_y + theme.control_h_small + 8,
        r.w,
        r.bottom() - tab_y - theme.control_h_small - 8 - 46,
    );
    let id = WidgetId::derive(WidgetId::NONE, 0, "settings-body");
    let off = ui.scroll_area(id, body, 560);
    ui.push_clip(body);
    let inner = Rect::new(body.x + 4, body.y - off, body.w - 8, 560);
    let mut goto = None;
    match m.settings_tab {
        0 => game_tab(ui, m, inner, theme),
        1 => download_tab(ui, m, inner, theme),
        2 => launcher_tab(ui, m, inner, theme),
        3 => privacy_tab(ui, m, inner, theme),
        _ => goto = about_tab(ui, m, inner, theme),
    }
    ui.pop_clip();

    // ---- 底部动作 ----
    let fy = r.bottom() - theme.control_h - 4;
    if ui
        .button(
            Rect::new(r.x, fy, 120, theme.control_h),
            "保存",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        m.save();
        m.toast("设置已保存", ToastKind::Success);
    }
    if ui
        .button(
            Rect::new(r.x + 128, fy, 120, theme.control_h),
            "恢复默认",
            ButtonKind::Danger,
            true,
        )
        .clicked
    {
        m.restore_defaults_request();
    }
    if ui
        .button(
            Rect::new(r.x + 256, fy, 180, theme.control_h),
            "重新运行首次启动引导",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.request_confirm(ConfirmRequest {
            title: "重新运行首次启动引导".to_owned(),
            body: "将重新进入欢迎、Java、游戏目录与配置汇总四步。现有数据不会被删除。".to_owned(),
            confirm_label: "开始引导".to_owned(),
            danger: false,
            action: PendingAction::RerunOnboarding,
        });
    }
    ui.caption(
        Rect::new(r.right() - 300, fy, 300, theme.control_h),
        "Ctrl+1..6 切页 · F5 刷新 · Esc 关闭浮层",
    );
    goto
}

fn row(ui: &mut Ui<'_, impl Backend + ?Sized>, x: i32, y: i32, w: i32, h: i32, theme: &Theme) {
    ui.fill_round(Rect::new(x, y, w, h), theme.card, theme.radius);
}

fn label(ui: &mut Ui<'_, impl Backend + ?Sized>, x: i32, y: i32, text: &str, theme: &Theme) {
    ui.text_vcenter(
        Rect::new(x, y, 150, 28),
        text,
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
}

fn game_tab(ui: &mut Ui<'_, impl Backend + ?Sized>, m: &mut LauncherModel, r: Rect, theme: &Theme) {
    section(ui, Rect::new(r.x, r.y, r.w, 28), "游戏设置", theme);
    let mut y = r.y + 36;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "内存分配", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 200, 30),
            &format!("{} MB（点击 ±512）", m.settings.memory_mb),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.memory_mb = if m.settings.memory_mb >= 16384 {
            512
        } else {
            m.settings.memory_mb + 512
        };
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "JVM 参数", theme);
    let before = m.settings.jvm_args.clone();
    ui.text_input(
        Rect::new(r.x + 170, y + 6, r.w - 182, 30),
        "jvm-args",
        &mut m.settings.jvm_args,
        "-XX:+UseG1GC",
    );
    if m.settings.jvm_args != before {
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "游戏参数", theme);
    let before = m.settings.game_args.clone();
    ui.text_input(
        Rect::new(r.x + 170, y + 6, r.w - 182, 30),
        "game-args",
        &mut m.settings.game_args,
        "可选",
    );
    if m.settings.game_args != before {
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "启动后行为", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 220, 30),
            m.settings.after_launch.label(),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.after_launch = match m.settings.after_launch {
            AfterLaunch::Keep => AfterLaunch::Minimize,
            AfterLaunch::Minimize => AfterLaunch::Exit,
            AfterLaunch::Exit => AfterLaunch::Keep,
        };
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "全屏与分辨率", theme);
    let mut fs = m.settings.fullscreen;
    if ui
        .checkbox(Rect::new(r.x + 170, y + 6, 120, 30), "全屏启动", &mut fs)
        .clicked
    {
        m.settings.fullscreen = fs;
        m.touch();
    }
    if ui
        .button(
            Rect::new(r.x + 300, y + 6, 160, 30),
            &format!("{} × {}", m.settings.width, m.settings.height),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.width = if m.settings.width == 1280 { 1920 } else { 1280 };
        m.settings.height = if m.settings.width == 1920 { 1080 } else { 720 };
        m.touch();
    }
}

fn download_tab(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) {
    section(ui, Rect::new(r.x, r.y, r.w, 28), "下载设置", theme);
    let mut y = r.y + 36;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "并发任务数", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 180, 30),
            &format!("{} 个（点击 +1）", m.settings.concurrent),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.concurrent = (m.settings.concurrent % 8) + 1;
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "下载限速", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 180, 30),
            &format!("{} KB/s（0 = 不限速）", m.settings.speed_limit_kb),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.speed_limit_kb = match m.settings.speed_limit_kb {
            0 => 1024,
            1024 => 4096,
            _ => 0,
        };
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "镜像源", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 220, 30),
            m.settings.mirror.as_str(),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.mirror = if m.settings.mirror == "官方源" {
            "镜像源（BMCLAPI）".to_owned()
        } else {
            "官方源".to_owned()
        };
        m.toast("镜像源已切换（即时生效）", ToastKind::Info);
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "代理地址", theme);
    let before = m.settings.proxy.clone();
    ui.text_input(
        Rect::new(r.x + 170, y + 6, r.w - 182, 30),
        "proxy",
        &mut m.settings.proxy,
        "留空表示不使用代理",
    );
    if m.settings.proxy != before {
        m.touch();
    }
    y += 50;
    let mut verify = m.settings.verify_files;
    if ui
        .checkbox(
            Rect::new(r.x + 12, y + 6, r.w - 24, 30),
            "下载后校验文件完整性（推荐开启）",
            &mut verify,
        )
        .clicked
    {
        m.settings.verify_files = verify;
        m.touch();
    }
}

fn launcher_tab(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) {
    section(ui, Rect::new(r.x, r.y, r.w, 28), "启动器外观与行为", theme);
    let mut y = r.y + 36;
    let toggles: [Toggle; 6] = [
        (
            "暗色主题（关闭为亮色）",
            |s| s.settings.dark,
            |s, v| s.settings.dark = v,
        ),
        (
            "磨玻璃效果",
            |s| s.settings.glass,
            |s, v| s.settings.glass = v,
        ),
        (
            "控件动画",
            |s| s.settings.animations,
            |s, v| s.settings.animations = v,
        ),
        (
            "最小化到托盘",
            |s| s.settings.minimize_to_tray,
            |s, v| s.settings.minimize_to_tray = v,
        ),
        (
            "关闭时隐藏到托盘",
            |s| s.settings.close_to_tray,
            |s, v| s.settings.close_to_tray = v,
        ),
        (
            "自动检查更新",
            |s| s.settings.auto_update,
            |s, v| s.settings.auto_update = v,
        ),
    ];
    for (text, get, set) in toggles {
        row(ui, r.x, y, r.w, 38, theme);
        let mut value = get(m);
        if ui
            .checkbox(Rect::new(r.x + 12, y + 4, r.w - 24, 30), text, &mut value)
            .clicked
        {
            set(m, value);
            m.touch();
        }
        y += 44;
    }
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "控件圆角", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 180, 30),
            &format!("{} px（点击切换）", m.settings.radius),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.radius = match m.settings.radius {
            0 => 8,
            8 => 16,
            _ => 0,
        };
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "界面字体", theme);
    let before = m.settings.font_family.clone();
    ui.text_input(
        Rect::new(r.x + 170, y + 6, r.w - 182, 30),
        "font-family",
        &mut m.settings.font_family,
        "Microsoft YaHei UI",
    );
    if m.settings.font_family != before {
        m.touch();
    }
    y += 50;
    row(ui, r.x, y, r.w, 42, theme);
    label(ui, r.x + 12, y + 7, "语言", theme);
    if ui
        .button(
            Rect::new(r.x + 170, y + 6, 180, 30),
            m.settings.language.as_str(),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.language = if m.settings.language == "简体中文" {
            "English".to_owned()
        } else {
            "简体中文".to_owned()
        };
        m.touch();
    }
    ui.caption(
        Rect::new(r.x + 12, y + 50, r.w - 24, 22),
        "外观设置与首页「外观设置」入口共用同一份配置，改动下一帧生效。",
    );
}

fn privacy_tab(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) {
    section(ui, Rect::new(r.x, r.y, r.w, 28), "账户与隐私", theme);
    let mut y = r.y + 36;
    let items: [Toggle; 3] = [
        (
            "保持登录状态",
            |s| s.settings.keep_signed_in,
            |s, v| s.settings.keep_signed_in = v,
        ),
        (
            "发送匿名使用统计",
            |s| s.settings.anonymous_stats,
            |s, v| s.settings.anonymous_stats = v,
        ),
        (
            "退出时清除缓存",
            |s| s.settings.clear_on_exit,
            |s, v| s.settings.clear_on_exit = v,
        ),
    ];
    for (text, get, set) in items {
        row(ui, r.x, y, r.w, 38, theme);
        let mut value = get(m);
        if ui
            .checkbox(Rect::new(r.x + 12, y + 4, r.w - 24, 30), text, &mut value)
            .clicked
        {
            set(m, value);
            m.touch();
        }
        y += 44;
    }
    y += 8;
    row(ui, r.x, y, r.w, 76, theme);
    ui.text_vcenter(
        Rect::new(r.x + 12, y + 8, r.w - 24, 26),
        "删除账户或版本都会弹出不可恢复确认，并写入运行日志。",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    if ui
        .button(
            Rect::new(r.x + 12, y + 40, 160, 28),
            "清除运行日志",
            ButtonKind::Danger,
            true,
        )
        .clicked
    {
        m.request_confirm(ConfirmRequest {
            title: "清除运行日志".to_owned(),
            body: "所有日志记录会被永久删除，此操作不可恢复。".to_owned(),
            confirm_label: "确认清除".to_owned(),
            danger: true,
            action: PendingAction::ClearLogs,
        });
    }
}

fn about_tab(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    section(ui, Rect::new(r.x, r.y, r.w, 28), "关于", theme);
    let y = r.y + 36;
    row(ui, r.x, y, r.w, 196, theme);
    let lines = [
        ("启动器版本", "1.9.1（自研即时模式 UI）"),
        ("界面框架", "ui-core + ui-backend-soft / ui-backend-win32"),
        ("第三方 GUI 依赖", "无（手写 Win32 FFI 与 5×7 点阵）"),
        ("插件框架", "launcher-plugin-framework（单进程）"),
        ("运行约束", "simulated 启动，不创建任何 OS 进程"),
    ];
    for (i, (k, v)) in lines.iter().enumerate() {
        ui.text_vcenter(
            Rect::new(r.x + 12, y + 10 + i as i32 * 34, 160, 28),
            *k,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        ui.text_vcenter(
            Rect::new(r.x + 176, y + 10 + i as i32 * 34, r.w - 188, 28),
            *v,
            theme.text,
            theme.font_small,
            TextAlign::Left,
            true,
        );
    }
    let by = y + 206;
    if ui
        .button(
            Rect::new(r.x, by, 160, theme.control_h_small),
            "检查更新",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.update_available = Some("1.9.2".to_owned());
        m.update_dismissed = false;
        m.toast("检测到新版本 1.9.2", ToastKind::Info);
        m.touch();
    }
    if ui
        .button(
            Rect::new(r.x + 168, by, 160, theme.control_h_small),
            "打开运行日志",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        return Some(Page::Logs);
    }
    ui.caption(
        Rect::new(r.x, by + 34, r.w, 22),
        "支持 Ctrl+1..6 切页、F5 刷新、Esc 关闭浮层，也可把整合包拖到窗口上安装。",
    );
    None
}
