//! 实例管理页：多选列表 + 右键菜单 + 滚动 + 真正的增删改。

use ui_core::backend::Backend;
use ui_core::draw::TextAlign;
use ui_core::layout::{EdgeInsets, Rect};
use ui_core::widget::ButtonKind;
use ui_core::{Theme, Ui, WidgetId};

use crate::app::{App, Dialog};
use crate::data::Snapshot;

/// 列表滚动区域 ID（跨帧稳定）。
fn scroll_id() -> WidgetId {
    WidgetId::derive(WidgetId::NONE, 7101, "instance-list")
}

pub fn render(app: &mut App, ui: &mut Ui<'_, impl Backend + ?Sized>, area: Rect, snap: &Snapshot) {
    let theme = app.theme().clone();
    let row_h = theme.control_h + 16;
    let gap = theme.gap;
    let radius = theme.style.radius();

    // ---- 标题行 ----
    let header_h = theme.font_heading + 8;
    ui.heading(area.take_top(header_h), "实例列表");
    ui.fill(
        Rect::new(area.x, area.y + header_h, area.w, 1),
        theme.divider,
    );

    // ---- 操作行 ----
    let actions_y = area.y + header_h + theme.gap;
    let actions = Rect::new(area.x, actions_y, area.w, theme.control_h);
    let btn_w = 96;
    let mut x = actions.x;
    if ui
        .button(
            Rect::new(x, actions.y, btn_w, actions.h),
            "新建实例",
            ButtonKind::Primary,
            true,
        )
        .clicked
    {
        app.dialog = Some(Dialog::NewInstance);
        app.form_error = None;
    }
    x += btn_w + gap;

    let selected_count = app.instance_selection.len();
    let can_delete = selected_count > 0 || app.selected_instance.is_some();
    if ui
        .button(
            Rect::new(x, actions.y, btn_w, actions.h),
            if selected_count > 0 {
                "删除选中"
            } else {
                "删除"
            },
            ButtonKind::Danger,
            can_delete,
        )
        .clicked
    {
        let ids = if selected_count > 0 {
            app.instance_selection.selected()
        } else {
            app.selected_instance.clone().into_iter().collect()
        };
        app.dialog = Some(Dialog::ConfirmDelete(ids));
    }
    x += btn_w + gap;
    if ui
        .button(
            Rect::new(x, actions.y, btn_w, actions.h),
            "全选",
            ButtonKind::Normal,
            !snap.instances.is_empty(),
        )
        .clicked
    {
        let ids: Vec<String> = snap.instances.iter().map(|i| i.id.clone()).collect();
        if app.instance_selection.len() == ids.len() {
            app.instance_selection.clear();
        } else {
            app.instance_selection.select_all(&ids);
        }
    }

    let stats = format!(
        "{} 个实例 · 已选 {} · {} 个 Mod",
        snap.instances.len(),
        selected_count,
        snap.mods.len()
    );
    let tw = ui.measure(&stats, theme.font_small, false);
    ui.text_vcenter(
        Rect::new(actions.right() - tw, actions.y, tw, actions.h),
        stats,
        theme.text_dim,
        theme.font_small,
        TextAlign::Right,
        false,
    );

    // ---- 列表 ----
    let list_top = actions_y + theme.control_h + theme.gap;
    let list_area = Rect::new(area.x, list_top, area.w, (area.bottom() - list_top).max(0));
    if snap.instances.is_empty() {
        ui.empty_state(list_area, "还没有实例 —— 点击「新建实例」开始");
        return;
    }

    let head_h = 28;
    let content_h = head_h + 1 + snap.instances.len() as i32 * (row_h + 1);
    let offset = ui.scroll_area(scroll_id(), list_area, content_h);

    // 右键菜单：只在真的发生右键按下时记录上下文。
    if let Some((mx, my)) = ui.right_click_pos() {
        if list_area.contains(mx, my) {
            let rel = my - (list_area.y + head_h + 1) + offset;
            let idx = rel / (row_h + 1);
            if idx >= 0 && (idx as usize) < snap.instances.len() {
                app.open_instance_menu(ui, list_area, &snap.instances[idx as usize].id);
            }
        }
    }

    // 表头（固定，不随滚动）
    let head = list_area.take_top(head_h);
    ui.fill_round(head, theme.card, radius);
    let cols = column_rects(head, theme.pad);
    for (i, name) in ["名称", "版本", "加载器", "上次游玩"].iter().enumerate() {
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
    let ordered: Vec<String> = snap.instances.iter().map(|i| i.id.clone()).collect();

    ui.push_clip(rows);
    let mut y = rows.y - offset;
    for inst in &snap.instances {
        let row = Rect::new(rows.x, y, rows.w, row_h);
        if row.bottom() > rows.y && row.y < rows.bottom() {
            let selected = app.instance_selection.is_selected(&inst.id)
                || (app.instance_selection.is_empty()
                    && app.selected_instance.as_deref() == Some(inst.id.as_str()));
            // 先做命中测试（nav_item 会画 hover/selected 底纹），
            // 再画列内容 —— 顺序反了底纹会把文字盖掉。
            // 点击：普通=单选，Ctrl=切换，Shift=范围选择。
            let hit = ui.nav_item(row, "", selected);
            if hit.clicked {
                if ui.shift_held() {
                    let anchor = app.selection_anchor.clone();
                    if let Some(a) = anchor {
                        if let (Some(ai), Some(ti)) = (
                            ordered.iter().position(|x| *x == a),
                            ordered.iter().position(|x| *x == inst.id),
                        ) {
                            let (lo, hi) = (ai.min(ti), ai.max(ti));
                            for id in &ordered[lo..=hi] {
                                app.instance_selection.select(id);
                            }
                        }
                    }
                } else if ui.ctrl_held() {
                    app.instance_selection.toggle(&inst.id);
                    app.selection_anchor = Some(inst.id.clone());
                } else {
                    app.instance_selection.clear();
                    app.selected_instance = Some(inst.id.clone());
                    app.selection_anchor = Some(inst.id.clone());
                }
            }
            draw_row(ui, &theme, row, inst, selected);
        }
        y += row_h + 1;
    }
    ui.pop_clip();

    // 滚动条提示
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
}

fn column_rects(rect: Rect, pad: i32) -> [Rect; 4] {
    // 左侧留 26px 给复选框
    let inner = rect.shrink(EdgeInsets::symmetric(pad, 0));
    let inner = Rect::new(inner.x + 26, inner.y, (inner.w - 26).max(0), inner.h);
    let w_name = inner.w * 40 / 100;
    let rest = inner.w - w_name;
    let w_rest = rest / 3;
    [
        Rect::new(inner.x, inner.y, w_name, inner.h),
        Rect::new(inner.x + w_name, inner.y, w_rest, inner.h),
        Rect::new(inner.x + w_name + w_rest, inner.y, w_rest, inner.h),
        Rect::new(
            inner.x + w_name + w_rest * 2,
            inner.y,
            rest - w_rest * 2,
            inner.h,
        ),
    ]
}

/// 画一行的静态内容（点击命中由调用方用 `nav_item` 处理）。
fn draw_row(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    theme: &Theme,
    row: Rect,
    inst: &crate::data::InstanceView,
    selected: bool,
) {
    // 行底纹 + 圆角
    if selected {
        ui.fill_round(row, theme.row_selected, theme.style.radius());
    }
    // 复选框
    let box_size = 16;
    let box_r = Rect::new(
        row.x + theme.pad_small,
        row.y + (row.h - box_size) / 2,
        box_size,
        box_size,
    );
    ui.fill_round(box_r, theme.input_bg, 4);
    ui.stroke_round(box_r, theme.input_border, 4, 1);
    if selected {
        ui.fill_round(box_r.shrink(EdgeInsets::all(4)), theme.accent, 2);
    }

    let cols = column_rects(row, theme.pad);
    ui.text_vcenter(
        cols[0],
        inst.name.clone(),
        theme.text,
        theme.font_body,
        TextAlign::Left,
        selected,
    );
    ui.text_vcenter(
        cols[1],
        inst.game_version.clone(),
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    ui.text_vcenter(
        cols[2],
        inst.loader.clone(),
        theme.text_dim,
        theme.font_body,
        TextAlign::Left,
        false,
    );
    let played = inst
        .last_played
        .as_deref()
        .map(short_time)
        .unwrap_or_else(|| "从未".to_owned());
    ui.text_vcenter(
        cols[3],
        played,
        theme.text_faint,
        theme.font_small,
        TextAlign::Left,
        false,
    );
    ui.fill(Rect::new(row.x, row.bottom(), row.w, 1), theme.divider);
}

/// `2026-11-08T10:00:00Z` → `11-08 10:00`。
fn short_time(s: &str) -> String {
    if s.len() >= 16 {
        let d = &s[5..10];
        let t = &s[11..16];
        format!("{d} {t}")
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_time_formats_iso() {
        assert_eq!(short_time("2026-11-08T10:00:00Z"), "11-08 10:00");
        assert_eq!(short_time("bad"), "bad");
    }

    #[test]
    fn columns_sum_to_inner_width() {
        let r = Rect::new(0, 0, 1000, 30);
        let cols = column_rects(r, 12);
        let inner = r.shrink(EdgeInsets::symmetric(12, 0));
        // 左侧 26px 被复选框占用，四列加起来等于剩余宽度。
        assert_eq!(cols[0].w + cols[1].w + cols[2].w + cols[3].w, inner.w - 26);
        assert_eq!(cols[3].right(), inner.right());
    }
}
