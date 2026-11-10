//! Mod 管理页：实时本地搜索 + 安装/移除 + 右键菜单 + 滚动。

use ui_core::backend::Backend;
use ui_core::draw::TextAlign;
use ui_core::layout::{EdgeInsets, Rect};
use ui_core::widget::ButtonKind;
use ui_core::{Ui, WidgetId};

use crate::app::{App, Dialog};
use crate::data::{ModView, Snapshot};

fn scroll_id() -> WidgetId {
    WidgetId::derive(WidgetId::NONE, 7102, "mod-list")
}

pub fn render(app: &mut App, ui: &mut Ui<'_, impl Backend + ?Sized>, area: Rect, snap: &Snapshot) {
    let theme = app.theme().clone();
    let gap = theme.gap;
    let radius = theme.style.radius();

    // ---- 标题 ----
    let header_h = theme.font_heading + 8;
    ui.heading(area.take_top(header_h), "Mod 管理");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    // ---- 搜索行 ----
    let y = area.y + header_h + gap;
    let search_row = Rect::new(area.x, y, area.w, theme.control_h);
    let input_w = (search_row.w - 3 * gap - 90 - 90).max(120);
    let input_rect = Rect::new(search_row.x, search_row.y, input_w, search_row.h);
    ui.text_input(
        input_rect,
        "mod_search",
        &mut app.mod_search,
        "搜索已安装 Mod（实时过滤）",
    );

    let mut x = input_rect.right() + gap;
    if ui
        .button(
            Rect::new(x, search_row.y, 90, search_row.h),
            "搜索",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        let q = app.mod_search.trim();
        if q.is_empty() {
            app.toast("输入关键词可过滤已安装 Mod", false);
        } else {
            app.toast(
                format!("本地过滤「{q}」完成；在线搜索需接入 host-http"),
                false,
            );
        }
    }
    x += 90 + gap;
    if ui
        .button(
            Rect::new(x, search_row.y, 90, search_row.h),
            "安装",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        app.dialog = Some(Dialog::InstallMod);
        app.form_error = None;
    }

    // ---- 已安装列表 ----
    let list_top = y + theme.control_h + gap;
    let list_area = Rect::new(area.x, list_top, area.w, (area.bottom() - list_top).max(0));
    let query = app.mod_search.trim().to_lowercase();
    let shown: Vec<&ModView> = snap
        .mods
        .iter()
        .filter(|m| {
            query.is_empty()
                || m.filename.to_lowercase().contains(&query)
                || m.name.to_lowercase().contains(&query)
        })
        .collect();

    if shown.is_empty() {
        ui.empty_state(
            list_area,
            if query.is_empty() {
                "该实例还没有安装 Mod"
            } else {
                "没有匹配的 Mod —— 换个关键词试试"
            },
        );
        return;
    }

    let head_h = 28;
    let row_h = theme.control_h;
    let content_h = head_h + 1 + shown.len() as i32 * (row_h + 1);
    let offset = ui.scroll_area(scroll_id(), list_area, content_h);

    if let Some((mx, my)) = ui.right_click_pos() {
        if list_area.contains(mx, my) {
            let rel = my - (list_area.y + head_h + 1) + offset;
            let idx = rel / (row_h + 1);
            if idx >= 0 && (idx as usize) < shown.len() {
                app.open_mod_menu(ui, list_area, &shown[idx as usize].filename);
            }
        }
    }

    let head = list_area.take_top(head_h);
    ui.fill_round(head, theme.card, radius);
    let cols = columns3(head, theme.pad);
    for (i, name) in ["文件名", "名称", "操作"].iter().enumerate() {
        ui.text_vcenter(
            cols[i],
            *name,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            true,
        );
    }

    let rows = Rect::new(
        list_area.x,
        list_area.y + head_h + 1,
        list_area.w,
        (list_area.h - head_h - 1).max(0),
    );
    ui.push_clip(rows);
    let mut ry = rows.y - offset;
    let mut to_remove: Option<String> = None;
    let mut shown_count = 0;
    for m in &shown {
        let row = Rect::new(rows.x, ry, rows.w, row_h);
        if row.bottom() > rows.y && row.y < rows.bottom() {
            shown_count += 1;
            let c = columns3(row, theme.pad);
            ui.fill(Rect::new(row.x, row.bottom(), row.w, 1), theme.divider);
            ui.text_vcenter(
                c[0],
                m.filename.clone(),
                theme.text,
                theme.font_body,
                TextAlign::Left,
                false,
            );
            ui.text_vcenter(
                c[1],
                format!("{}  {}", m.name, m.version),
                theme.text_dim,
                theme.font_body,
                TextAlign::Left,
                false,
            );
            let btn_w = 72;
            let btn = Rect::new(
                c[2].right() - btn_w,
                row.y + (row_h - theme.control_h_small) / 2,
                btn_w,
                theme.control_h_small,
            );
            if ui.button(btn, "移除", ButtonKind::Ghost, true).clicked {
                to_remove = Some(m.filename.clone());
            }
        }
        ry += row_h + 1;
    }
    ui.pop_clip();

    if let Some(f) = to_remove {
        app.store().remove_mod(&f);
        app.toast(format!("已移除 {f}"), false);
    }

    let max_scroll = (content_h - list_area.h).max(0);
    if max_scroll > 0 {
        let track = Rect::new(list_area.right() - 4, list_area.y, 3, list_area.h);
        ui.fill_round(track, theme.input_bg, 2);
        let thumb_h = (track.h * list_area.h / content_h).max(24);
        let thumb_y = track.y + (track.h - thumb_h) * offset / max_scroll;
        ui.fill_round(
            Rect::new(track.x, thumb_y, track.w, thumb_h),
            theme.accent,
            2,
        );
    }
    ui.caption(
        Rect::new(area.x, area.bottom() - 18, area.w, 18),
        format!("显示 {shown_count}/{} 个 Mod · 右键可移除", snap.mods.len()),
    );
}

fn columns3(rect: Rect, pad: i32) -> [Rect; 3] {
    let inner = rect.shrink(EdgeInsets::symmetric(pad, 0));
    let w0 = inner.w * 42 / 100;
    let w1 = inner.w * 35 / 100;
    let w2 = inner.w - w0 - w1;
    [
        Rect::new(inner.x, inner.y, w0, inner.h),
        Rect::new(inner.x + w0, inner.y, w1, inner.h),
        Rect::new(inner.x + w0 + w1, inner.y, w2, inner.h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns3_sum_to_inner() {
        let r = Rect::new(0, 0, 900, 30);
        let c = columns3(r, 10);
        let inner = r.shrink(EdgeInsets::symmetric(10, 0));
        assert_eq!(c[0].w + c[1].w + c[2].w, inner.w);
        assert_eq!(c[2].right(), inner.right());
    }
}
