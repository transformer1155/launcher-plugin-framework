//! 下载中心：资源分类、详情、任务面板与下载设置。

use crate::domain::{DownloadCategory, DownloadState, LauncherModel, ToastKind};
use crate::shell::{empty_block, kv, pill, section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WidgetId};

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 260, 32), "下载中心");
    ui.caption(
        Rect::new(r.x + 266, r.y, r.w - 266, 32),
        "等待 → 下载中 ⇄ 暂停 → 完成 / 失败 → 重试",
    );

    // ---- 新建任务 ----
    let bar_y = r.y + 38;
    let input_w = (r.w - 2 * 108 - theme.gap * 2).min(520);
    ui.text_input(
        Rect::new(r.x, bar_y, input_w, theme.control_h),
        "download-url",
        &mut m.form_download_url,
        "粘贴资源链接或直接拖入 .jar / 整合包",
    );
    if ui
        .button(
            Rect::new(r.x + input_w + theme.gap, bar_y, 108, theme.control_h),
            "新建任务",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        let url = m.form_download_url.trim().to_owned();
        if url.is_empty() {
            m.toast("请先填写资源名称或链接", ToastKind::Warn);
        } else {
            let name = url.rsplit(['/', '\\']).next().unwrap_or(&url).to_owned();
            m.add_download(&name, m.download_filter, 96);
            m.form_download_url.clear();
        }
    }
    if ui
        .button(
            Rect::new(r.x + input_w + 116 + theme.gap, bar_y, 108, theme.control_h),
            "全部暂停",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        let ids: Vec<String> = m
            .downloads
            .iter()
            .filter(|d| d.state == DownloadState::Downloading)
            .map(|d| d.id.clone())
            .collect();
        for id in ids {
            m.pause_download(&id);
        }
        m.toast("已暂停所有进行中的任务", ToastKind::Info);
    }

    let body_y = bar_y + theme.control_h + theme.gap;
    let left_w = 136;
    let right_w = 268;
    let gap = theme.gap;
    let left = Rect::new(r.x, body_y, left_w, (r.bottom() - body_y).max(120));
    let mid = Rect::new(
        r.x + left_w + gap,
        body_y,
        (r.w - left_w - right_w - gap * 2).max(200),
        (r.bottom() - body_y).max(120),
    );
    let right = Rect::new(
        r.right() - right_w,
        body_y,
        right_w,
        (r.bottom() - body_y).max(120),
    );

    // ---- 资源分类 ----
    section(ui, Rect::new(left.x, left.y, left.w, 28), "资源分类", theme);
    let mut cy = left.y + 34;
    for cat in DownloadCategory::ALL {
        let active = m.download_filter == cat;
        let item = Rect::new(left.x, cy, left.w, theme.control_h_small);
        if ui
            .button(
                item,
                cat.label(),
                if active {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Ghost
                },
                true,
            )
            .clicked
        {
            m.download_filter = cat;
        }
        cy += theme.control_h_small + 4;
    }
    cy += gap;
    let counts = m.downloads.len();
    let active = m
        .downloads
        .iter()
        .filter(|d| d.state == DownloadState::Downloading)
        .count();
    let failed = m
        .downloads
        .iter()
        .filter(|d| d.state == DownloadState::Failed)
        .count();
    ui.caption(
        Rect::new(left.x, cy, left.w, 20),
        format!("任务 {counts} 个"),
    );
    ui.caption(
        Rect::new(left.x, cy + 22, left.w, 20),
        format!("下载中 {active} 个"),
    );
    ui.caption(
        Rect::new(left.x, cy + 44, left.w, 20),
        format!("失败 {failed} 个"),
    );

    // ---- 任务列表 ----
    section(ui, Rect::new(mid.x, mid.y, mid.w, 28), "任务面板", theme);
    let list = Rect::new(mid.x, mid.y + 32, mid.w, mid.h - 32);
    let tasks = m.downloads.clone();
    let selected = m.selected_download.clone();
    if tasks.is_empty() {
        empty_block(
            ui,
            list,
            "暂无下载任务，粘贴链接或拖入文件即可创建",
            None,
            theme,
        );
    } else {
        let row_h = 72;
        let id = WidgetId::derive(WidgetId::NONE, 0, "downloads-list");
        let off = ui.scroll_area(id, list, tasks.len() as i32 * row_h);
        ui.push_clip(list);
        for (i, t) in tasks.iter().enumerate() {
            let row = Rect::new(list.x, list.y + i as i32 * row_h - off, list.w, row_h - 6);
            let is_sel = selected.as_deref() == Some(t.id.as_str());
            ui.fill_round(
                row,
                if is_sel {
                    theme.row_selected
                } else {
                    theme.card
                },
                theme.radius,
            );
            if ui
                .button(
                    Rect::new(row.x + 8, row.y + 6, row.w - 16, 24),
                    t.name.as_str(),
                    ButtonKind::Ghost,
                    true,
                )
                .clicked
            {
                m.selected_download = Some(t.id.clone());
            }
            ui.text_vcenter(
                Rect::new(row.x + 12, row.y + 32, 120, 20),
                format!("{} · {} / {} MB", t.category.label(), t.done_mb, t.total_mb),
                theme.text_dim,
                theme.font_small,
                TextAlign::Left,
                false,
            );
            pill(
                ui,
                row.x + 138,
                row.y + 33,
                t.state.label(),
                state_color(t.state, theme),
                theme,
            );
            let bar = Rect::new(row.x + 12, row.y + 56, row.w - 190, 8);
            ui.progress(bar, t.fraction());

            let mut bx = row.right() - 3 * 56 - 12;
            let label = match t.state {
                DownloadState::Downloading => Some("暂停"),
                DownloadState::Paused => Some("继续"),
                DownloadState::Failed => Some("重试"),
                _ => None,
            };
            if let Some(l) = label {
                if ui
                    .button(
                        Rect::new(bx, row.y + 36, 56, theme.control_h_small),
                        l,
                        ButtonKind::Normal,
                        true,
                    )
                    .clicked
                {
                    let tid = t.id.clone();
                    match t.state {
                        DownloadState::Downloading => m.pause_download(&tid),
                        DownloadState::Paused => m.resume_download(&tid),
                        DownloadState::Failed => m.retry_download(&tid),
                        _ => {}
                    }
                }
            }
            bx += 60;
            if ui
                .button(
                    Rect::new(bx, row.y + 36, 56, theme.control_h_small),
                    "移除",
                    ButtonKind::Danger,
                    true,
                )
                .clicked
            {
                let tid = t.id.clone();
                m.delete_download(&tid);
            }
        }
        ui.pop_clip();
    }

    // ---- 详情 + 下载设置 ----
    section(
        ui,
        Rect::new(right.x, right.y, right.w, 28),
        "任务详情",
        theme,
    );
    let detail = Rect::new(right.x, right.y + 34, right.w, 200);
    ui.fill_round(detail, theme.card, theme.radius);
    if let Some(t) = m
        .downloads
        .iter()
        .find(|d| Some(d.id.as_str()) == selected.as_deref())
        .cloned()
    {
        let rows = [
            ("名称", t.name.clone()),
            ("分类", t.category.label().to_owned()),
            ("状态", t.state.label().to_owned()),
            ("进度", format!("{} / {} MB", t.done_mb, t.total_mb)),
            ("速度", format!("{} KB/s", t.speed_kb)),
            ("错误", t.error.clone().unwrap_or_else(|| "无".to_owned())),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let cell = Rect::new(
                detail.x + 10,
                detail.y + 8 + i as i32 * 30,
                detail.w - 20,
                28,
            );
            kv(ui, cell, k, v.as_str(), theme);
        }
    } else {
        ui.text_vcenter(
            detail,
            "选择一个任务查看详情",
            theme.text_dim,
            theme.font_small,
            TextAlign::Center,
            false,
        );
    }

    let set_y = detail.bottom() + gap;
    section(
        ui,
        Rect::new(right.x, set_y, right.w, 28),
        "下载设置（即时保存）",
        theme,
    );
    let mut sy = set_y + 34;
    ui.text_vcenter(
        Rect::new(right.x, sy, 96, 24),
        "并发数",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    let before = m.settings.concurrent;
    if ui
        .button(
            Rect::new(right.x + 96, sy, right.w - 96, 24),
            &format!("{} 个任务（点击 +1）", m.settings.concurrent),
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        m.settings.concurrent = (m.settings.concurrent % 8) + 1;
        m.touch();
    }
    let _ = before;
    sy += 30;
    ui.text_vcenter(
        Rect::new(right.x, sy, 96, 24),
        "镜像源",
        theme.text_dim,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    if ui
        .button(
            Rect::new(right.x + 96, sy, right.w - 96, 24),
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
        m.touch();
    }
    sy += 30;
    let mut verify = m.settings.verify_files;
    if ui
        .checkbox(
            Rect::new(right.x, sy, right.w, 26),
            "下载后校验文件完整性",
            &mut verify,
        )
        .clicked
    {
        m.settings.verify_files = verify;
        m.touch();
    }
    sy += 30;
    let mut auto = m.settings.auto_update;
    if ui
        .checkbox(
            Rect::new(right.x, sy, right.w, 26),
            "自动检查启动器更新",
            &mut auto,
        )
        .clicked
    {
        m.settings.auto_update = auto;
        m.touch();
    }
    None
}

fn state_color(s: DownloadState, theme: &Theme) -> Color {
    match s {
        DownloadState::Completed => Color::rgb(0x3E, 0xA5, 0x6A),
        DownloadState::Downloading => theme.accent,
        DownloadState::Paused => Color::rgb(0xD1, 0x96, 0x3C),
        DownloadState::Failed => theme.danger,
        DownloadState::Waiting => theme.text_faint,
    }
}
