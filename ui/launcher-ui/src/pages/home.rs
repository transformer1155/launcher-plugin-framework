//! 首页：四张联动卡片 + 启动按钮各状态 + 检查/下载/验证/启动阶段进度。

use crate::domain::{LauncherModel, PendingAction, ToastKind};
use crate::shell::{launch_stages, section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui};

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    let mut goto = None;
    ui.heading(Rect::new(r.x, r.y, 320, 34), "首页");
    ui.caption(
        Rect::new(r.x + 326, r.y, r.w - 326, 34),
        "所有页面随当前版本与当前账户联动",
    );

    // ---- 四张状态卡片 ----
    let cards_y = r.y + 40;
    let gap = theme.gap;
    let cw = (r.w - gap * 3) / 4;
    let ch = 128;
    let version = m
        .current_version()
        .map(|v| {
            (
                v.name.clone(),
                format!("{} · {}", v.mc_version, v.loader.label()),
            )
        })
        .unwrap_or_else(|| ("未选择版本".to_owned(), "请到版本管理选择".to_owned()));
    let account = m
        .current_account()
        .map(|a| {
            (
                a.name.clone(),
                format!("{} · {}", a.kind.label(), a.flow.label()),
            )
        })
        .unwrap_or_else(|| ("未登录账户".to_owned(), "请到账户管理登录".to_owned()));
    let java = m
        .selected_java()
        .map(|j| {
            (
                format!("Java {}", j.version),
                format!("{} 位 · {}", j.bits, j.source.label()),
            )
        })
        .unwrap_or_else(|| ("Java 未配置".to_owned(), "请在设置中指定".to_owned()));
    let memory = (
        format!("{} MB", m.settings.memory_mb),
        format!(
            "JVM 参数 {} 项",
            m.settings.jvm_args.split_whitespace().count()
        ),
    );
    let cards = [
        ("当前版本", version, Page::Versions, theme.accent),
        (
            "当前账户",
            account,
            Page::Accounts,
            Color::rgb(0x8B, 0x6E, 0xD8),
        ),
        (
            "Java 运行时",
            java,
            Page::Settings,
            Color::rgb(0xD1, 0x96, 0x3C),
        ),
        (
            "内存分配",
            memory,
            Page::Settings,
            Color::rgb(0x3E, 0xA5, 0x6A),
        ),
    ];
    for (i, (title, (value, sub), target, color)) in cards.iter().enumerate() {
        let rect = Rect::new(r.x + i as i32 * (cw + gap), cards_y, cw, ch);
        ui.fill_round(rect, theme.card, theme.radius);
        ui.fill_round(Rect::new(rect.x, rect.y, 4, rect.h), *color, 2);
        ui.caption(Rect::new(rect.x + 14, rect.y + 10, rect.w - 28, 22), *title);
        ui.text_vcenter(
            Rect::new(rect.x + 14, rect.y + 34, rect.w - 28, 28),
            value.as_str(),
            theme.text,
            theme.font_heading,
            TextAlign::Left,
            true,
        );
        ui.text_vcenter(
            Rect::new(rect.x + 14, rect.y + 62, rect.w - 28, 22),
            sub.as_str(),
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        if ui
            .button(
                Rect::new(rect.x + 14, rect.y + 88, rect.w - 28, theme.control_h_small),
                "前往管理",
                ButtonKind::Ghost,
                true,
            )
            .clicked
        {
            goto = Some(*target);
        }
    }

    // ---- 启动区 ----
    let launch_y = cards_y + ch + gap;
    let launch_h = 196;
    let launch_rect = Rect::new(r.x, launch_y, r.w, launch_h);
    ui.fill_round(launch_rect, theme.panel, theme.radius);
    section(
        ui,
        Rect::new(
            launch_rect.x + theme.pad,
            launch_rect.y + 6,
            launch_rect.w - theme.pad * 2,
            30,
        ),
        "启动游戏",
        theme,
    );

    let (label, enabled) = m.launch_label();
    let btn = Rect::new(launch_rect.x + theme.pad, launch_rect.y + 44, 260, 48);
    if ui
        .button(btn, label.as_str(), ButtonKind::Primary, enabled)
        .clicked
    {
        match m.start_launch() {
            Ok(()) => {}
            Err(e) => {
                m.toast(e, ToastKind::Warn);
            }
        }
    }
    // 各状态的补充文案
    let hint = match m.phase {
        crate::domain::LauncherPhase::Onboarding => "完成首次启动引导后即可启动游戏。".to_owned(),
        crate::domain::LauncherPhase::Idle => {
            let blockers = m.preflight();
            if blockers.is_empty() {
                format!(
                    "启动后：{}（simulated，不创建 OS 进程）",
                    m.settings.after_launch.label()
                )
            } else {
                format!("启动按钮已置灰：{}", blockers.join("；"))
            }
        }
        crate::domain::LauncherPhase::Launching => m.launch.detail.clone(),
        crate::domain::LauncherPhase::Running => {
            "游戏运行中（simulated），可在此结束运行。".to_owned()
        }
    };
    ui.text_vcenter(
        Rect::new(
            btn.right() + theme.gap,
            btn.y,
            launch_rect.right() - btn.right() - theme.gap - theme.pad,
            48,
        ),
        hint,
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );

    // 进度条与阶段
    let bar = Rect::new(
        launch_rect.x + theme.pad,
        launch_rect.y + 100,
        launch_rect.w - theme.pad * 2,
        12,
    );
    ui.progress(bar, m.launch.pct);
    launch_stages(
        ui,
        Rect::new(
            launch_rect.x + theme.pad,
            launch_rect.y + 120,
            launch_rect.w - theme.pad * 2,
            46,
        ),
        m,
        theme,
    );

    if m.phase == crate::domain::LauncherPhase::Launching
        && ui
            .button(
                Rect::new(
                    launch_rect.right() - 120,
                    launch_rect.y + 8,
                    104,
                    theme.control_h_small,
                ),
                "取消启动",
                ButtonKind::Normal,
                true,
            )
            .clicked
    {
        m.request_confirm(crate::domain::ConfirmRequest {
            title: "取消启动".to_owned(),
            body: "正在进行的资源下载与验证会中止，可稍后重新启动。".to_owned(),
            confirm_label: "确认取消".to_owned(),
            danger: false,
            action: PendingAction::CancelLaunch,
        });
    }
    if m.phase == crate::domain::LauncherPhase::Running {
        if ui
            .button(
                Rect::new(
                    launch_rect.right() - 120,
                    launch_rect.y + 8,
                    104,
                    theme.control_h_small,
                ),
                "结束运行",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            let _ = m.finish_launch(0);
        }
        if ui
            .button(
                Rect::new(
                    launch_rect.right() - 234,
                    launch_rect.y + 8,
                    108,
                    theme.control_h_small,
                ),
                "模拟崩溃",
                ButtonKind::Ghost,
                true,
            )
            .clicked
        {
            let _ = m.finish_launch(-1);
        }
    }

    // ---- 异常退出日志提示 ----
    let mut y = launch_rect.bottom() + gap;
    if m.show_logs_after_crash {
        let warn = Rect::new(r.x, y, r.w, 42);
        ui.fill_round(warn, theme.card, theme.radius);
        ui.stroke_round(warn, theme.danger, theme.radius, 2);
        ui.text_vcenter(
            Rect::new(warn.x + theme.pad, warn.y, warn.w - 170, warn.h),
            "游戏异常退出，已记录日志。查看日志以定位崩溃原因。",
            theme.danger,
            theme.font_small,
            TextAlign::Left,
            true,
        );
        if ui
            .button(
                Rect::new(warn.right() - 158, warn.y + 6, 142, warn.h - 12),
                "查看日志（Ctrl+L）",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            m.show_logs_after_crash = false;
            goto = Some(Page::Logs);
        }
        y += 50;
    }

    // ---- 最近日志 ----
    let log_h = (r.bottom() - y).max(60);
    section(ui, Rect::new(r.x, y, r.w, 28), "最近日志", theme);
    let recent: Vec<String> = m.logs.iter().rev().take(3).cloned().collect();
    let list = Rect::new(r.x, y + 32, r.w, log_h - 32);
    ui.fill_round(list, theme.card, theme.radius);
    for (i, line) in recent.iter().enumerate() {
        let color = if line.contains("[ERROR]") {
            theme.danger
        } else if line.contains("[WARN]") {
            Color::rgb(0xD1, 0x96, 0x3C)
        } else {
            theme.text_dim
        };
        ui.text_vcenter(
            Rect::new(
                list.x + theme.pad,
                list.y + 6 + i as i32 * 24,
                list.w - theme.pad * 2,
                22,
            ),
            line.as_str(),
            color,
            theme.font_small,
            TextAlign::Left,
            false,
        );
    }
    if ui
        .button(
            Rect::new(list.right() - 100, list.y + 4, 88, theme.control_h_small),
            "全部日志",
            ButtonKind::Ghost,
            true,
        )
        .clicked
    {
        goto = Some(Page::Logs);
    }
    goto
}
