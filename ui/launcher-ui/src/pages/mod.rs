//! 六个业务页面。所有页面都只读/写 [`LauncherModel`]，彼此不直接通信，
//! 联动完全靠「唯一当前版本 + 唯一当前账户」这两个字段。

pub mod accounts;
pub mod downloads;
pub mod home;
pub mod logs;
pub mod settings;
pub mod versions;

use crate::domain::LauncherModel;
use crate::shell::Page;
use ui_core::{Backend, Rect, Theme, Ui};

/// 页面分发。返回需要跳转的页面（卡片里的「前往管理」之类）。
pub fn render_page(
    ui: &mut Ui<'_, impl Backend + ?Sized>,
    m: &mut LauncherModel,
    page: Page,
    r: Rect,
    theme: &Theme,
) -> Option<Page> {
    match page.route() {
        Page::Home => home::render(ui, m, r, theme),
        Page::Versions => versions::render(ui, m, r, theme),
        Page::Accounts => accounts::render(ui, m, r, theme),
        Page::Downloads => downloads::render(ui, m, r, theme),
        Page::Settings => settings::render(ui, m, r, theme),
        Page::Logs => logs::render(ui, m, r, theme),
        _ => None,
    }
}
