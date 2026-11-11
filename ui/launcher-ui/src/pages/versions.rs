//! 版本管理：列表（加载/空/错误态）、安装、删除二次确认、详情。

use crate::domain::{LauncherModel, Loader, PendingAction, ToastKind, VersionFlow};
use crate::shell::{
    danger_confirm, empty_block, error_block, kv, loading_block, pill, section, Page,
};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WidgetId};

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 260, 32), "版本管理");
    ui.caption(
        Rect::new(r.x + 266, r.y, r.w - 266, 32),
        "任意时刻只有一个当前版本，所有页面随它联动",
    );

    // ---- 工具栏 ----
    let bar_y = r.y + 38;
    let search_w = (r.w - 3 * 108 - theme.gap * 3).min(360);
    ui.text_input(
        Rect::new(r.x, bar_y, search_w, theme.control_h),
        "version-filter",
        &mut m.versions_filter,
        "搜索版本 / 加载器",
    );
    let mut bx = r.x + search_w + theme.gap;
    if ui
        .button(
            Rect::new(bx, bar_y, 108, theme.control_h),
            if m.form_error.is_some() {
                "填写中…"
            } else {
                "安装版本"
            },
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        m.open_version_detail = None;
        m.form_version_name.clear();
        m.form_error = None;
        m.toast("请填写版本信息后确认安装", ToastKind::Info);
        // 用详情面板的位置展示安装表单：复用 open_version_detail 的特殊值。
        m.open_version_detail = Some("__install__".to_owned());
    }
    bx += 108 + theme.gap;
    if ui
        .button(
            Rect::new(bx, bar_y, 108, theme.control_h),
            "刷新",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.refresh();
    }
    bx += 108 + theme.gap;
    if ui
        .button(
            Rect::new(bx, bar_y, 108, theme.control_h),
            "Java 设置",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings_tab = 0;
        return Some(Page::Settings);
    }

    // ---- 安装表单 ----
    let mut list_y = bar_y + theme.control_h + theme.gap;
    if m.open_version_detail.as_deref() == Some("__install__") {
        // 打开表单时加载官方版本目录（幂等，失败可重试）。
        m.load_mc_catalog();
        let form_h = 298;
        let form = Rect::new(r.x, list_y, r.w, form_h);
        ui.fill_round(form, theme.panel, theme.radius);
        ui.text_input(
            Rect::new(form.x + theme.pad, form.y + 10, 260, theme.control_h_small),
            "new-version-name",
            &mut m.form_version_name,
            "版本名称（留空自动生成）",
        );
        if ui
            .button(
                Rect::new(
                    form.x + theme.pad + 268,
                    form.y + 10,
                    150,
                    theme.control_h_small,
                ),
                m.form_loader.label(),
                ButtonKind::Normal,
                true,
            )
            .clicked
        {
            let idx = Loader::ALL
                .iter()
                .position(|l| *l == m.form_loader)
                .unwrap_or(0);
            m.form_loader = Loader::ALL[(idx + 1) % Loader::ALL.len()];
        }
        ui.caption(
            Rect::new(
                form.x + theme.pad + 426,
                form.y + 10,
                340,
                theme.control_h_small,
            ),
            "加载器（当前真实下载仅支持原版）",
        );

        // ---- 版本选择：搜索 + 点选（不再手输版本号）----
        ui.caption(Rect::new(form.x + theme.pad, form.y + 46, 200, 22), "选择游戏版本");
        ui.text_input(
            Rect::new(form.x + theme.pad, form.y + 70, 260, theme.control_h_small),
            "catalog-filter",
            &mut m.catalog_filter,
            "搜索版本，如 1.21",
        );
        let picked = Rect::new(
            form.x + theme.pad + 268,
            form.y + 70,
            300,
            theme.control_h_small,
        );
        ui.fill_round(picked, theme.card, theme.radius);
        ui.text_vcenter(
            picked,
            format!("已选：{}", m.form_mc_version),
            theme.accent,
            theme.font_small,
            TextAlign::Left,
            true,
        );

        let list_rect = Rect::new(
            form.x + theme.pad,
            form.y + 106,
            form.w - theme.pad * 2,
            140,
        );
        match (&m.mc_catalog_state, m.mc_catalog.is_empty()) {
            (Some(Err(e)), _) => {
                if error_block(ui, list_rect, &format!("版本目录加载失败：{e}"), theme) {
                    m.mc_catalog_state = None;
                    m.load_mc_catalog();
                }
            }
            (Some(Ok(true)), _) => {
                loading_block(ui, list_rect, "正在拉取官方版本清单…", theme);
            }
            _ if m.mc_catalog.is_empty() => {
                ui.caption(list_rect, "暂无版本目录，可直接确认安装（默认 1.21.1）");
            }
            _ => {
                let f = m.catalog_filter.trim().to_ascii_lowercase();
                let shown: Vec<_> = m
                    .mc_catalog
                    .iter()
                    .filter(|(id, _, _)| f.is_empty() || id.contains(&f))
                    .take(60)
                    .cloned()
                    .collect();
                let row_h = 28;
                let cid = WidgetId::derive(WidgetId::NONE, 1, "mc-catalog");
                let off = ui.scroll_area(cid, list_rect, shown.len() as i32 * row_h);
                ui.push_clip(list_rect);
                for (i, (id, kind, rel)) in shown.iter().enumerate() {
                    let row = Rect::new(
                        list_rect.x,
                        list_rect.y + i as i32 * row_h - off,
                        list_rect.w,
                        row_h - 4,
                    );
                    let selected = m.form_mc_version == *id;
                    ui.fill_round(
                        row,
                        if selected { theme.row_selected } else { theme.card },
                        theme.radius,
                    );
                    ui.text_vcenter(
                        Rect::new(row.x + 10, row.y, 160, row.h),
                        id.as_str(),
                        if selected { theme.accent } else { theme.text },
                        theme.font_small,
                        TextAlign::Left,
                        true,
                    );
                    ui.text_vcenter(
                        Rect::new(row.x + 180, row.y, 220, row.h),
                        rel.as_str(),
                        theme.text_dim,
                        theme.font_small,
                        TextAlign::Left,
                        false,
                    );
                    ui.text_vcenter(
                        Rect::new(row.right() - 120, row.y, 110, row.h),
                        kind.as_str(),
                        theme.text_faint,
                        theme.font_small,
                        TextAlign::Right,
                        false,
                    );
                    let rid = WidgetId::derive(cid, i as u32, "catalog-row");
                    if ui.detect_click_pub(rid, row).1 {
                        m.pick_mc_version(id);
                    }
                }
                ui.pop_clip();
            }
        }

        if let Some(err) = m.form_error.clone() {
            ui.text_vcenter(
                Rect::new(form.x + theme.pad, form.y + 254, form.w - 320, 22),
                err,
                theme.danger,
                theme.font_small,
                TextAlign::Left,
                false,
            );
        }
        if ui
            .button(
                Rect::new(form.right() - 224, form.y + 254, 100, theme.control_h_small),
                "取消",
                ButtonKind::Normal,
                true,
            )
            .clicked
        {
            m.open_version_detail = None;
            m.form_error = None;
        }
        if ui
            .button(
                Rect::new(form.right() - 116, form.y + 254, 100, theme.control_h_small),
                "确认安装",
                ButtonKind::Primary,
                true,
            )
            .clicked
        {
            let name = m.form_version_name.clone();
            let mc = m.form_mc_version.clone();
            let loader = m.form_loader;
            match m.install_version(&name, &mc, loader) {
                Ok(id) => {
                    m.select_version(&id);
                    m.open_version_detail = None;
                    m.form_error = None;
                }
                Err(e) => m.form_error = Some(e),
            }
        }
        ui.caption(
            Rect::new(form.x + theme.pad, form.y + 276, form.w - theme.pad * 2, 22),
            "确认安装 = 创建实例 + 从 Mojang 官方源真实下载客户端（约 100MB，几分钟）。",
        );
        list_y = form.bottom() + theme.gap;
    }

    // ---- 详情面板 ----
    if let Some(id) = m.open_version_detail.clone() {
        if id != "__install__" {
            if let Some(v) = m.versions.iter().find(|v| v.id == id).cloned() {
                let panel = Rect::new(r.x, list_y, r.w, 116);
                ui.fill_round(panel, theme.panel, theme.radius);
                section(
                    ui,
                    Rect::new(
                        panel.x + theme.pad,
                        panel.y + 4,
                        panel.w - theme.pad * 2,
                        28,
                    ),
                    &format!("版本详情 · {}", v.name),
                    theme,
                );
                let rows = [
                    ("游戏版本", v.mc_version.clone()),
                    ("加载器", v.loader.label().to_owned()),
                    ("状态", v.flow.label().to_owned()),
                    (
                        "Java",
                        m.selected_java()
                            .map(|j| j.version.clone())
                            .unwrap_or_else(|| "未配置".to_owned()),
                    ),
                    ("内存", format!("{} MB", v.memory_mb)),
                    ("Mod 数", format!("{} 个", v.mods)),
                ];
                for (i, (k, val)) in rows.iter().enumerate() {
                    let col = i % 3;
                    let row = i / 3;
                    let cell = Rect::new(
                        panel.x + theme.pad + col as i32 * ((panel.w - theme.pad * 2) / 3),
                        panel.y + 36 + row as i32 * 34,
                        (panel.w - theme.pad * 2) / 3,
                        30,
                    );
                    kv(ui, cell, k, val.as_str(), theme);
                }
                if ui
                    .button(
                        Rect::new(panel.right() - 100, panel.y + 8, 84, theme.control_h_small),
                        "关闭",
                        ButtonKind::Ghost,
                        true,
                    )
                    .clicked
                {
                    m.open_version_detail = None;
                }
                list_y = panel.bottom() + theme.gap;
            }
        }
    }

    // ---- 列表 ----
    section(ui, Rect::new(r.x, list_y, r.w, 28), "已安装版本", theme);
    let list = Rect::new(r.x, list_y + 32, r.w, r.bottom() - list_y - 32);
    if m.loading {
        loading_block(ui, list, "版本列表", theme);
        return None;
    }
    if let Some(err) = m.error.clone() {
        if error_block(ui, list, &err, theme) {
            m.refresh();
        }
        return None;
    }

    let filter = m.versions_filter.trim().to_ascii_lowercase();
    let versions: Vec<_> = m
        .versions
        .iter()
        .filter(|v| {
            filter.is_empty()
                || v.name.to_ascii_lowercase().contains(&filter)
                || v.mc_version.contains(&filter)
                || v.loader.label().to_ascii_lowercase().contains(&filter)
        })
        .cloned()
        .collect();
    let current = m.current_version.clone();
    if versions.is_empty() {
        if empty_block(
            ui,
            list,
            "没有匹配的版本，试试其它关键词或安装新版本",
            Some("安装版本"),
            theme,
        ) {
            m.open_version_detail = Some("__install__".to_owned());
        }
        return None;
    }

    let row_h = 54;
    let id = WidgetId::derive(WidgetId::NONE, 0, "versions-list");
    let off = ui.scroll_area(id, list, versions.len() as i32 * row_h);
    ui.push_clip(list);
    for (i, v) in versions.iter().enumerate() {
        let row = Rect::new(list.x, list.y + i as i32 * row_h - off, list.w, row_h - 6);
        let is_current = current.as_deref() == Some(v.id.as_str());
        ui.fill_round(
            row,
            if is_current {
                theme.row_selected
            } else {
                theme.card
            },
            theme.radius,
        );
        if is_current {
            ui.stroke_round(row, theme.accent, theme.radius, 1);
        }
        ui.text_vcenter(
            Rect::new(row.x + 12, row.y + 2, 220, 24),
            v.name.as_str(),
            theme.text,
            theme.font_body,
            TextAlign::Left,
            true,
        );
        ui.text_vcenter(
            Rect::new(row.x + 12, row.y + 26, 220, 18),
            format!(
                "{} · {} · {} MB · {} 个 Mod",
                v.mc_version,
                v.loader.label(),
                v.size_mb,
                v.mods
            ),
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        let mut px = row.x + 244;
        px += pill(
            ui,
            px,
            row.y + 14,
            v.flow.label(),
            flow_color(v.flow, theme),
            theme,
        ) + theme.gap;
        if is_current {
            px += pill(ui, px, row.y + 14, "当前版本", theme.accent, theme) + theme.gap;
        }
        let mut bx = row.right() - 3 * 84 - theme.gap * 2 - 8;
        if ui
            .button(
                Rect::new(bx, row.y + 8, 84, theme.control_h_small),
                "设为当前",
                if is_current {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Normal
                },
                !is_current,
            )
            .clicked
        {
            let vid = v.id.clone();
            m.select_version(&vid);
            m.toast(
                format!("已切换当前版本为「{}」", v.name),
                ToastKind::Success,
            );
        }
        bx += 84 + theme.gap;
        if ui
            .button(
                Rect::new(bx, row.y + 8, 84, theme.control_h_small),
                "详情",
                ButtonKind::Normal,
                true,
            )
            .clicked
        {
            m.open_version_detail = Some(v.id.clone());
        }
        bx += 84 + theme.gap;
        if ui
            .button(
                Rect::new(bx, row.y + 8, 84, theme.control_h_small),
                "删除",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            m.request_confirm(danger_confirm(
                &format!("删除版本「{}」", v.name),
                "该版本的文件、Mod 与配置将被永久删除，此操作不可恢复。建议先备份游戏目录。",
                PendingAction::DeleteVersion(v.id.clone()),
            ));
        }
        let _ = px;
    }
    ui.pop_clip();
    None
}

fn flow_color(flow: VersionFlow, theme: &Theme) -> Color {
    match flow {
        VersionFlow::Installed => Color::rgb(0x3E, 0xA5, 0x6A),
        VersionFlow::NotInstalled => theme.text_faint,
        VersionFlow::Installing => theme.accent,
        VersionFlow::Launching => Color::rgb(0xD1, 0x96, 0x3C),
        VersionFlow::Running => Color::rgb(0x8B, 0x6E, 0xD8),
    }
}
