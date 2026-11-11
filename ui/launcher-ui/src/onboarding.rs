//! 首次启动引导：欢迎 → Java 扫描/手动指定 → 游戏目录导入 → 配置汇总。
//!
//! 只有 `LauncherPhase::Onboarding` 时渲染。设置里的「重新运行首次启动引导」
//! 会再次把状态机切回引导中，走的是同一套步骤。

use crate::domain::{LauncherModel, ToastKind};
use crate::shell::{pill, section};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WindowAction};

const STEPS: [&str; 4] = ["欢迎", "Java 环境", "游戏目录", "配置汇总"];

pub fn render(ui: &mut Ui<'_, impl Backend + ?Sized>, m: &mut LauncherModel, theme: &Theme) {
    let size = ui.size();
    ui.fill(Rect::new(0, 0, size.w, size.h), theme.bg);
    ui.fill(Rect::new(0, 0, size.w, 6), theme.accent);

    // 右上角关闭（引导期间也可以退出启动器）
    let close = Rect::new(size.w - 46, 8, 38, 30);
    if ui.button(close, "✕", ButtonKind::Ghost, true).clicked {
        ui.request_window_action(WindowAction::Close);
    }

    let panel = Rect::new(
        (size.w - 820).max(20) / 2,
        54,
        (size.w - 80).min(820),
        size.h - 100,
    );
    ui.fill_round(panel, theme.panel, theme.radius.max(12));
    ui.stroke_round(panel, theme.divider, theme.radius.max(12), 1);

    // 步骤指示器
    let step_w = (panel.w - theme.pad * 2) / 4;
    for (i, name) in STEPS.iter().enumerate() {
        let r = Rect::new(
            panel.x + theme.pad + i as i32 * step_w,
            panel.y + 20,
            step_w - 8,
            34,
        );
        let done = i < m.onboarding_step;
        let active = i == m.onboarding_step;
        let color = if active {
            theme.accent
        } else if done {
            theme.card
        } else {
            theme.panel
        };
        ui.fill_round(r, color, theme.radius);
        ui.text_vcenter(
            r,
            format!("{}. {}", i + 1, name),
            if active {
                Color::rgb(0xFF, 0xFF, 0xFF)
            } else {
                theme.text_dim
            },
            theme.font_small,
            TextAlign::Center,
            active,
        );
    }

    let body = Rect::new(
        panel.x + theme.pad,
        panel.y + 68,
        panel.w - theme.pad * 2,
        panel.h - 68 - 70,
    );

    match m.onboarding_step {
        0 => welcome(ui, m, body, theme),
        1 => java_step(ui, m, body, theme),
        2 => dir_step(ui, m, body, theme),
        _ => summary(ui, m, body, theme),
    }

    // 错误提示
    if let Some(err) = m.form_error.clone() {
        ui.text_vcenter(
            Rect::new(body.x, panel.bottom() - 84, body.w, 22),
            err,
            theme.danger,
            theme.font_small,
            TextAlign::Left,
            false,
        );
    }

    // 底部按钮
    let footer_y = panel.bottom() - theme.control_h - theme.pad;
    if m.onboarding_reentry {
        let skip = Rect::new(panel.x + theme.pad, footer_y, 108, theme.control_h);
        if ui.button(skip, "跳过引导", ButtonKind::Ghost, true).clicked {
            m.onboarding_step = 3;
            m.onboarding_next();
            m.log("[INFO] 跳过了首次启动引导（从设置重入）");
            m.toast("已退出引导", ToastKind::Info);
        }
    }
    if m.onboarding_step > 0 {
        let back = Rect::new(panel.right() - 250, footer_y, 108, theme.control_h);
        if ui.button(back, "上一步", ButtonKind::Normal, true).clicked {
            m.onboarding_back();
        }
    }
    let next_label = if m.onboarding_step == 3 {
        "完成并进入启动器"
    } else {
        "下一步"
    };
    let next = Rect::new(
        panel.right() - 132 - theme.pad,
        footer_y,
        132,
        theme.control_h,
    );
    if ui
        .button(next, next_label, ButtonKind::Primary, true)
        .clicked
        && m.onboarding_next()
    {
        m.toast("引导已完成，配置已保存", ToastKind::Success);
    }
}

fn welcome(ui: &mut Ui<'_, impl Backend + ?Sized>, _m: &mut LauncherModel, r: Rect, theme: &Theme) {
    ui.heading(
        Rect::new(r.x, r.y + 10, r.w, 40),
        "欢迎使用 Aether Launcher",
    );
    ui.text_vcenter(
        Rect::new(r.x, r.y + 58, r.w, 28),
        "只需三步即可完成初始化，之后随时可以在设置中修改。",
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    let items = [
        (
            "Java 环境",
            "自动扫描本机 Java，或手动指定 java.exe。支持 Java 8 / 17 / 21。",
        ),
        (
            "游戏目录",
            "导入已有的 .minecraft 目录，版本与 Mod 会被识别。",
        ),
        (
            "版本与账户",
            "支持 Forge / Fabric / Quilt / NeoForge / OptiFine，账户可微软授权或离线添加。",
        ),
        (
            "下载与启动",
            "下载中心管理任务，启动前自动检查 Java 与 JVM 参数。",
        ),
    ];
    let mut y = r.y + 108;
    for (k, v) in items {
        let card = Rect::new(r.x, y, r.w, 62);
        ui.fill_round(card, theme.card, theme.radius);
        ui.text_vcenter(
            Rect::new(card.x + theme.pad, card.y + 6, 140, 24),
            k,
            theme.accent,
            theme.font_body,
            TextAlign::Left,
            true,
        );
        ui.text_vcenter(
            Rect::new(card.x + theme.pad + 146, card.y + 6, card.w - 160, 48),
            v,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        y += 70;
    }
}

fn java_step(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) {
    section(
        ui,
        Rect::new(r.x, r.y, r.w, 30),
        "Java 扫描与手动指定",
        theme,
    );
    let scan = Rect::new(r.right() - 132, r.y, 132, theme.control_h_small);
    if ui
        .button(
            scan,
            if m.java_scanning {
                "扫描中…"
            } else {
                "自动扫描 Java"
            },
            ButtonKind::Primary,
            !m.java_scanning,
        )
        .clicked
    {
        m.scan_java();
    }

    let list = Rect::new(r.x, r.y + 38, r.w, 200);
    // 先克隆一份，避免遍历 m.java_runtimes 时又可变借用 m 去写 selected_java。
    let runtimes = m.java_runtimes.clone();
    let selected_id = m.selected_java.clone();
    if runtimes.is_empty() {
        ui.empty_state(list, "尚未发现 Java，请扫描或手动指定 java.exe 路径");
    } else {
        let row_h = 44;
        let content_h = runtimes.len() as i32 * row_h;
        let id = ui_core::WidgetId::derive(ui_core::WidgetId::NONE, 0, "onboarding-java-list");
        let off = ui.scroll_area(id, list, content_h);
        ui.push_clip(list);
        for (i, j) in runtimes.iter().enumerate() {
            let row = Rect::new(list.x, list.y + i as i32 * row_h - off, list.w, row_h - 4);
            let selected = selected_id.as_deref() == Some(j.id.as_str());
            ui.fill_round(
                row,
                if selected {
                    theme.row_selected
                } else {
                    theme.card
                },
                theme.radius,
            );
            if ui
                .button(
                    Rect::new(row.x + 6, row.y + 5, 72, row_h - 14),
                    if selected { "已选择" } else { "选择" },
                    if selected {
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Normal
                    },
                    true,
                )
                .clicked
            {
                let id = j.id.clone();
                m.select_java(&id);
            }
            ui.text_vcenter(
                Rect::new(row.x + 86, row.y, 120, row_h - 4),
                format!("Java {}", j.version),
                theme.text,
                theme.font_small,
                TextAlign::Left,
                true,
            );
            ui.text_vcenter(
                Rect::new(row.x + 210, row.y, row.w - 220, row_h - 4),
                format!("{} · {} 位 · {}", j.path, j.bits, j.source.label()),
                theme.text_dim,
                theme.font_small,
                TextAlign::Left,
                false,
            );
        }
        ui.pop_clip();
    }

    let y = list.bottom() + theme.gap;
    section(ui, Rect::new(r.x, y, r.w, 30), "手动指定 Java", theme);
    let input = Rect::new(r.x, y + 38, r.w - 132 - theme.gap, theme.control_h);
    ui.text_input(
        input,
        "java-path",
        &mut m.form_java_path,
        r"C:\Program Files\Java\jdk-17\bin\java.exe",
    );
    if ui
        .button(
            Rect::new(r.right() - 132, y + 38, 132, theme.control_h),
            "指定并使用",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        let path = m.form_java_path.clone();
        if let Err(e) = m.add_java_manual(&path) {
            m.form_error = Some(e);
        } else {
            m.form_java_path.clear();
        }
    }
    ui.caption(
        Rect::new(r.x, y + 38 + theme.control_h + 6, r.w, 22),
        "提示：1.20.1 及以上建议 Java 17，1.21 建议 Java 21；旧版 Forge 需要 Java 8。",
    );
}

fn dir_step(ui: &mut Ui<'_, impl Backend + ?Sized>, m: &mut LauncherModel, r: Rect, theme: &Theme) {
    section(ui, Rect::new(r.x, r.y, r.w, 30), "游戏目录导入", theme);
    ui.text_vcenter(
        Rect::new(r.x, r.y + 38, r.w, 26),
        "指定 .minecraft 目录，已有的版本、Mod、资源包会被自动识别。",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let y = r.y + 76;
    let input = Rect::new(r.x, y, r.w - 132 - theme.gap, theme.control_h);
    if m.form_game_dir.is_empty() {
        m.form_game_dir = m.game_dir.clone();
    }
    ui.text_input(
        input,
        "game-dir",
        &mut m.form_game_dir,
        r"D:\Minecraft\.minecraft",
    );
    if ui
        .button(
            Rect::new(r.right() - 132, y, 132, theme.control_h),
            "使用此目录",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        let dir = m.form_game_dir.clone();
        m.set_game_dir(&dir);
    }
    let y2 = y + theme.control_h + theme.gap;
    if ui
        .button(
            Rect::new(r.x, y2, 168, theme.control_h_small),
            "使用默认目录",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.form_game_dir = r"D:\Minecraft\.minecraft".to_owned();
        m.set_game_dir(&m.form_game_dir.clone());
    }
    if ui
        .button(
            Rect::new(r.x + 176, y2, 168, theme.control_h_small),
            "导入已有版本",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.log("[INFO] 扫描游戏目录中的已有版本…");
        m.toast(
            format!("已识别 {} 个版本", m.versions.len()),
            ToastKind::Info,
        );
    }
    let y3 = y2 + theme.control_h_small + theme.gap;
    ui.fill_round(Rect::new(r.x, y3, r.w, 96), theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + theme.pad, y3 + 8, r.w - theme.pad * 2, 24),
        format!(
            "当前目录：{}",
            if m.game_dir.is_empty() {
                "（未设置）"
            } else {
                &m.game_dir
            }
        ),
        theme.text,
        theme.font_small,
        TextAlign::Left,
        true,
    );
    ui.text_vcenter(
        Rect::new(r.x + theme.pad, y3 + 36, r.w - theme.pad * 2, 24),
        format!(
            "已识别版本 {} 个 · 可用 Java {} 个",
            m.versions.len(),
            m.java_runtimes.len()
        ),
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    ui.text_vcenter(
        Rect::new(r.x + theme.pad, y3 + 62, r.w - theme.pad * 2, 24),
        "也可以直接把整合包或 .jar 拖到窗口上完成安装。",
        theme.text_faint,
        theme.font_small,
        TextAlign::Left,
        false,
    );
}

fn summary(ui: &mut Ui<'_, impl Backend + ?Sized>, m: &LauncherModel, r: Rect, theme: &Theme) {
    section(ui, Rect::new(r.x, r.y, r.w, 30), "配置汇总", theme);
    let y = r.y + 40;
    let rows = [
        (
            "Java",
            m.selected_java()
                .map(|j| format!("Java {}（{}）", j.version, j.vendor))
                .unwrap_or_else(|| "未配置".to_owned()),
        ),
        (
            "游戏目录",
            if m.game_dir.is_empty() {
                "未设置".to_owned()
            } else {
                m.game_dir.clone()
            },
        ),
        (
            "当前版本",
            m.current_version()
                .map(|v| v.name.clone())
                .unwrap_or_else(|| "未选择".to_owned()),
        ),
        (
            "当前账户",
            m.current_account()
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "未登录".to_owned()),
        ),
        ("内存分配", format!("{} MB", m.settings.memory_mb)),
        ("JVM 参数", m.settings.jvm_args.clone()),
    ];
    for (i, (k, v)) in rows.iter().enumerate() {
        let row = Rect::new(r.x, y + i as i32 * 40, r.w, 36);
        ui.fill_round(row, theme.card, theme.radius);
        crate::shell::kv(ui, row, k, v, theme);
    }
    let tip_y = y + rows.len() as i32 * 40 + theme.gap;
    ui.fill_round(Rect::new(r.x, tip_y, r.w, 58), theme.panel, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + theme.pad, tip_y, r.w - theme.pad * 2, 28),
        "完成后进入主界面。所有配置都可随时在「设置」中修改，也可以重新运行本引导。",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    pill(
        ui,
        r.x + theme.pad,
        tip_y + 30,
        "仅首次出现",
        theme.accent,
        theme,
    );
}
