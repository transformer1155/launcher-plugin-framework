//! 插件管理页：真实扫描出的插件列表 + 安装 / 卸载。
//!
//! 这里的「插件」来自 [`crate::plugins_discovery::scan_plugins`] 对插件目录的真实扫描，
//! 不再是硬编码。内置插件（official-* / 社区示例）已编译进启动器，状态为「已加载」；
//! 第三方插件会被发现并展示，但单进程单二进制下没有动态加载能力，标注「需重新编译启动器」。

use std::path::Path;

use crate::domain::{LauncherModel, ToastKind};
use crate::shell::{section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WidgetId};

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 240, 32), "插件");
    ui.caption(
        Rect::new(r.x + 246, r.y, r.w - 246, 32),
        "已安装的插件来自插件目录的真实扫描；内置插件已编译进启动器，第三方插件仅登记、需重新编译才能加载",
    );

    let mut y = r.y + 38;

    // ---- 安装区 ----
    section(ui, Rect::new(r.x, y, r.w, 28), "安装插件", theme);
    y += 36;
    ui.fill_round(Rect::new(r.x, y, r.w, 42), theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + 12, y + 7, 80, 28),
        "插件目录",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let before = m.form_plugin_path.clone();
    ui.text_input(
        Rect::new(r.x + 100, y + 6, r.w - 220, 30),
        "plugin-path",
        &mut m.form_plugin_path,
        r"C:\path\to\plugin",
    );
    if m.form_plugin_path != before {
        m.touch();
    }
    if ui
        .button(
            Rect::new(r.x + r.w - 110, y + 6, 98, 30),
            "安装",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        let p = m.form_plugin_path.clone();
        match m.install_plugin_from(Path::new(&p)) {
            Ok(id) => {
                m.form_plugin_path.clear();
                m.toast(format!("已安装插件 {id}"), ToastKind::Success);
            }
            Err(e) => m.toast(e, ToastKind::Warn),
        }
        m.touch();
    }
    y += 54;

    // ---- 远程安装区（GitHub / tar.gz 直链）----
    section(ui, Rect::new(r.x, y, r.w, 28), "远程安装（GitHub / 直链）", theme);
    y += 36;
    ui.fill_round(Rect::new(r.x, y, r.w, 42), theme.card, theme.radius);
    ui.text_vcenter(
        Rect::new(r.x + 12, y + 7, 110, 28),
        "插件来源",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let remote_before = m.form_plugin_remote.clone();
    ui.text_input(
        Rect::new(r.x + 130, y + 6, r.w - 260, 30),
        "plugin-remote",
        &mut m.form_plugin_remote,
        "owner/repo 或 https://…/plugin.tar.gz",
    );
    if m.form_plugin_remote != remote_before {
        m.touch();
    }
    let remote_busy = m.remote_install_busy;
    if ui
        .button(
            Rect::new(r.x + r.w - 120, y + 6, 108, 30),
            if remote_busy { "下载中…" } else { "远程安装" },
            ButtonKind::Primary,
            !remote_busy,
        )
        .clicked
    {
        let src = m.form_plugin_remote.clone();
        match m.install_plugin_from_remote(&src) {
            Ok(()) => {
                m.form_plugin_remote.clear();
                m.toast("已开始下载安装，完成后会提示", ToastKind::Info);
            }
            Err(e) => m.toast(e, ToastKind::Warn),
        }
        m.touch();
    }
    ui.caption(
        Rect::new(r.x + r.w - 120, y + 38, 200, 20),
        "支持 owner/repo / .tar.gz",
    );
    y += 54;

    // ---- 列表 ----
    section(
        ui,
        Rect::new(r.x, y, r.w, 28),
        &format!("已安装（{}）", m.plugins.len()),
        theme,
    );
    y += 36;
    let list_rect = Rect::new(r.x, y, r.w, r.bottom() - y - 8);
    let row_h = 88;
    let content_h = (m.plugins.len() as i32 * row_h).max(row_h);
    let id = WidgetId::derive(WidgetId::NONE, 0, "plugins-list");
    let off = ui.scroll_area(id, list_rect, content_h);
    ui.push_clip(list_rect);
    let inner = Rect::new(list_rect.x, list_rect.y - off, list_rect.w - 8, content_h);

    // 先把展示数据拷出来，避免迭代 m.plugins 时又调用需要 &mut self 的卸载/选择。
    let items: Vec<(String, String, String, String, String, usize, bool, bool)> = m
        .plugins
        .iter()
        .map(|p| {
            (
                p.id.clone(),
                p.name.clone(),
                p.version.clone(),
                p.trust_badge().to_owned(),
                p.status().to_owned(),
                p.tools.len(),
                p.builtin,
                p.disabled,
            )
        })
        .collect();

    for (i, (pid, name, version, badge, status, ntools, builtin, disabled)) in
        items.iter().enumerate()
    {
        let cy = inner.y + i as i32 * row_h;
        let card = Rect::new(inner.x, cy, inner.w, row_h - 8);
        ui.fill_round(card, theme.card, theme.radius);

        ui.text_vcenter(
            Rect::new(card.x + 12, cy + 8, card.w - 200, 26),
            format!("{name}  v{version}"),
            theme.text,
            theme.font_small,
            TextAlign::Left,
            true,
        );
        ui.text_vcenter(
            Rect::new(card.x + 12, cy + 36, card.w - 200, 22),
            pid,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        ui.text_vcenter(
            Rect::new(card.x + 12, cy + 58, card.w - 200, 22),
            format!("{ntools} 个工具 · {status}"),
            theme.text_faint,
            theme.font_small,
            TextAlign::Left,
            false,
        );

        // 信任徽章
        let badge_color: Color = if badge.contains("可信") {
            theme.accent
        } else if badge.contains("未审核") {
            theme.danger
        } else {
            theme.text_dim
        };
        ui.fill_round(
            Rect::new(card.x + card.w - 180, cy + 10, 96, 24),
            badge_color,
            theme.radius,
        );
        ui.text_vcenter(
            Rect::new(card.x + card.w - 180, cy + 10, 96, 24),
            badge,
            theme.bg,
            theme.font_small,
            TextAlign::Center,
            true,
        );

        // 选择 / 卸载 / 禁用
        if *builtin {
            ui.text_vcenter(
                Rect::new(card.x + card.w - 78, cy + 10, 66, 24),
                "内置",
                theme.text_dim,
                theme.font_small,
                TextAlign::Center,
                false,
            );
        } else {
            // 禁用 / 启用（官方默认插件由模型层硬拒绝，这里也按官方前缀隐藏按钮）。
            let is_official = pid.starts_with("com.example.official");
            if !is_official {
                let (btn_text, want_disabled) = if *disabled {
                    ("启用", false)
                } else {
                    ("禁用", true)
                };
                if ui
                    .button(
                        Rect::new(card.x + card.w - 176, cy + 10, 88, 24),
                        btn_text,
                        ButtonKind::Normal,
                        true,
                    )
                    .clicked
                {
                    if let Err(e) = m.set_plugin_disabled(pid, want_disabled) {
                        m.toast(e, ToastKind::Warn);
                    }
                    m.touch();
                }
            }
            if ui
                .button(
                    Rect::new(card.x + card.w - 78, cy + 10, 66, 24),
                    "卸载",
                    ButtonKind::Danger,
                    true,
                )
                .clicked
            {
                if let Err(e) = m.uninstall_plugin(pid) {
                    m.toast(e, ToastKind::Warn);
                } else {
                    m.toast(format!("已卸载 {pid}"), ToastKind::Info);
                }
                m.touch();
            }
        }
    }
    ui.pop_clip();
    None
}
