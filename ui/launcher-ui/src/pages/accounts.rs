//! 账户管理：微软授权 / 离线添加 / 切换 / 删除不可恢复确认。

use crate::domain::{AccountFlow, LauncherModel, PendingAction, ToastKind};
use crate::shell::{danger_confirm, empty_block, pill, section, Page};
use ui_core::widget::ButtonKind;
use ui_core::{Backend, Color, Rect, TextAlign, Theme, Ui, WidgetId};

pub fn render(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    ui.heading(Rect::new(r.x, r.y, 260, 32), "账户管理");
    ui.caption(
        Rect::new(r.x + 266, r.y, r.w - 266, 32),
        "任意时刻只有一个当前账户，所有页面随它联动",
    );

    // ---- 添加账户 ----
    let bar_y = r.y + 38;
    let add = Rect::new(r.x, bar_y, 180, theme.control_h);
    if ui
        .button(add, "微软账户登录", ButtonKind::Primary, true)
        .clicked
    {
        let _ = m.start_microsoft_auth(false);
    }
    let fail = Rect::new(r.x + 188, bar_y, 148, theme.control_h);
    if ui
        .button(fail, "模拟授权失败", ButtonKind::Ghost, true)
        .clicked
    {
        let _ = m.start_microsoft_auth(true);
        m.toast("已模拟授权失败分支，用于验证重试流程", ToastKind::Warn);
    }
    let input_w = 220;
    let ox = r.right() - input_w - 108 - theme.gap;
    ui.text_input(
        Rect::new(ox, bar_y, input_w, theme.control_h),
        "offline-name",
        &mut m.form_offline_name,
        "离线账户名",
    );
    if ui
        .button(
            Rect::new(r.right() - 108, bar_y, 108, theme.control_h),
            "添加离线",
            ButtonKind::Normal,
            true,
        )
        .clicked
    {
        let name = m.form_offline_name.clone();
        match m.add_offline_account(&name) {
            Ok(_) => m.form_offline_name.clear(),
            Err(e) => {
                m.form_error = Some(e.clone());
                m.toast(e, ToastKind::Warn);
            }
        }
    }

    // ---- 登录状态提示 ----
    let mut y = bar_y + theme.control_h + theme.gap;
    if m.accounts.iter().any(|a| a.flow == AccountFlow::SigningIn) {
        let box_r = Rect::new(r.x, y, r.w, 44);
        ui.fill_round(box_r, theme.card, theme.radius);
        ui.text_vcenter(
            Rect::new(
                box_r.x + theme.pad,
                box_r.y,
                box_r.w - theme.pad * 2,
                box_r.h,
            ),
            "微软授权中… 请在浏览器完成登录后返回启动器。",
            theme.accent,
            theme.font_small,
            TextAlign::Left,
            true,
        );
        y += 52;
    } else if let Some((name, reason)) = m
        .accounts
        .iter()
        .find(|a| matches!(a.flow, AccountFlow::Failed(_)))
        .map(|a| {
            (
                a.name.clone(),
                match &a.flow {
                    AccountFlow::Failed(s) => s.clone(),
                    _ => String::new(),
                },
            )
        })
    {
        let box_r = Rect::new(r.x, y, r.w, 48);
        ui.fill_round(box_r, theme.card, theme.radius);
        ui.stroke_round(box_r, theme.danger, theme.radius, 1);
        ui.text_vcenter(
            Rect::new(box_r.x + theme.pad, box_r.y, box_r.w - 180, box_r.h),
            format!("{name} 登录失败：{reason}"),
            theme.danger,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        if ui
            .button(
                Rect::new(box_r.right() - 164, box_r.y + 8, 148, box_r.h - 16),
                "重试授权",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            m.retry_auth();
        }
        y += 56;
    }

    if let Some(err) = m.form_error.clone() {
        ui.text_vcenter(
            Rect::new(r.x, y, r.w, 22),
            err,
            theme.danger,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        y += 26;
    }

    // ---- 账户列表 ----
    section(ui, Rect::new(r.x, y, r.w, 28), "账户列表", theme);
    let list_y = y + 32;
    let list = Rect::new(r.x, list_y, r.w, (r.bottom() - list_y).max(60));
    let accounts = m.accounts.clone();
    let current = m.current_account.clone();
    if accounts.is_empty() {
        if empty_block(
            ui,
            list,
            "还没有账户，请先微软授权或添加离线账户",
            Some("添加离线账户"),
            theme,
        ) {
            m.form_offline_name = "Steve".to_owned();
        }
        return None;
    }
    let row_h = 56;
    let id = WidgetId::derive(WidgetId::NONE, 0, "accounts-list");
    let off = ui.scroll_area(id, list, accounts.len() as i32 * row_h);
    ui.push_clip(list);
    for (i, a) in accounts.iter().enumerate() {
        let row = Rect::new(list.x, list.y + i as i32 * row_h - off, list.w, row_h - 6);
        let is_current = current.as_deref() == Some(a.id.as_str());
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
            Rect::new(row.x + 12, row.y + 2, 200, 26),
            a.name.as_str(),
            theme.text,
            theme.font_body,
            TextAlign::Left,
            true,
        );
        ui.text_vcenter(
            Rect::new(row.x + 12, row.y + 28, 200, 18),
            a.kind.label(),
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
        let mut px = row.x + 224;
        px += pill(
            ui,
            px,
            row.y + 16,
            a.flow.label(),
            flow_color(&a.flow, theme),
            theme,
        ) + theme.gap;
        if is_current {
            px += pill(ui, px, row.y + 16, "当前账户", theme.accent, theme) + theme.gap;
        }
        let _ = px;
        let mut bx = row.right() - 3 * 88 - theme.gap * 2 - 8;
        if ui
            .button(
                Rect::new(bx, row.y + 9, 88, theme.control_h_small),
                "切换",
                if is_current {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Normal
                },
                !is_current,
            )
            .clicked
        {
            let aid = a.id.clone();
            m.select_account(&aid);
            m.toast(
                format!("已切换当前账户为「{}」", a.name),
                ToastKind::Success,
            );
        }
        bx += 88 + theme.gap;
        if ui
            .button(
                Rect::new(bx, row.y + 9, 88, theme.control_h_small),
                "重新登录",
                ButtonKind::Normal,
                matches!(a.flow, AccountFlow::Failed(_) | AccountFlow::SignedOut),
            )
            .clicked
        {
            let aid = a.id.clone();
            m.select_account(&aid);
            m.retry_auth();
        }
        bx += 88 + theme.gap;
        if ui
            .button(
                Rect::new(bx, row.y + 9, 88, theme.control_h_small),
                "删除",
                ButtonKind::Danger,
                true,
            )
            .clicked
        {
            m.request_confirm(danger_confirm(
                &format!("删除账户「{}」", a.name),
                "该账户的登录凭据与本地配置将被永久删除，此操作不可恢复。",
                PendingAction::DeleteAccount(a.id.clone()),
            ));
        }
    }
    ui.pop_clip();

    ui.caption(
        Rect::new(r.x, r.bottom() - 22, r.w, 20),
        "提示：未登录或登录失败时首页启动按钮会置灰并提示「请先登录账户」。",
    );
    None
}

fn flow_color(flow: &AccountFlow, theme: &Theme) -> Color {
    match flow {
        AccountFlow::SignedIn => Color::rgb(0x3E, 0xA5, 0x6A),
        AccountFlow::SignedOut => theme.text_faint,
        AccountFlow::SigningIn => theme.accent,
        AccountFlow::Failed(_) => theme.danger,
    }
}
