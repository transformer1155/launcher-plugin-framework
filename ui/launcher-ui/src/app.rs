//! 应用装配：全局事件 → 状态机推进 → 主题同步 → 外壳/页面 → 全局反馈。
//!
//! `App` 持有唯一真相源 [`LauncherModel`]。`Theme` 也由它持有，Windows 消息循环
//! 每帧从 `App` 读一份新主题（不要在闭包外缓存克隆）。

use crate::domain::{LauncherModel, LauncherPhase};
use crate::{onboarding, pages, shell};
use std::path::PathBuf;
use ui_core::{Backend, Rect, ShortcutKey, StyleConfig, Theme, Ui};

pub use crate::shell::Page;

const PAGE_ANIM_MS: f32 = 320.0;

pub struct App {
    pub model: LauncherModel,
    theme: Theme,
    dark: bool,
    page: Page,
    page_anim_ms: f32,
    /// 上一次同步过外观的 Settings 快照：只有设置真的变了才回写 Theme，
    /// 否则 `style_mut()` 的直接修改会被设置页的默认值覆盖。
    last_settings: crate::domain::Settings,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let model = LauncherModel::demo();
        let last_settings = model.settings.clone();
        Self {
            model,
            theme: Theme::dark(),
            dark: true,
            page: Page::Home,
            page_anim_ms: PAGE_ANIM_MS,
            last_settings,
        }
    }

    /// 首次启动（无配置文件）时由 `enable_persistence` 自动切换，也可用 `--first-run` 强制。
    pub fn first_run() -> Self {
        let mut app = Self::new();
        app.model = LauncherModel::first_run();
        app.last_settings = app.model.settings.clone();
        app
    }

    /// 打开配置持久化。文件不存在时自动进入首次启动引导。
    pub fn enable_persistence(&mut self, path: impl Into<PathBuf>) {
        self.model.enable_persistence(path.into());
        self.last_settings = self.model.settings.clone();
        self.sync_theme(true);
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn model(&self) -> &LauncherModel {
        &self.model
    }

    pub fn model_mut(&mut self) -> &mut LauncherModel {
        &mut self.model
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn set_page(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.page = page;
        self.page_anim_ms = if self.theme.style.animations {
            0.0
        } else {
            PAGE_ANIM_MS
        };
    }

    /// 命令行外观参数入口。
    pub fn set_style(&mut self, style: StyleConfig) {
        self.theme.style = style.clone();
        self.model.settings.glass = style.glass.enabled;
        self.model.settings.animations = style.animations;
        self.model.settings.radius = style.control_radius;
        self.model.settings.font_family = style.font.family.clone();
        self.last_settings = self.model.settings.clone();
        if !style.animations {
            self.page_anim_ms = PAGE_ANIM_MS;
        }
    }

    pub fn style_mut(&mut self) -> &mut StyleConfig {
        &mut self.theme.style
    }

    pub fn set_dark(&mut self, dark: bool) {
        let saved = self.theme.style.clone();
        self.dark = dark;
        self.theme = if dark { Theme::dark() } else { Theme::light() };
        self.theme.style = saved;
        self.model.settings.dark = dark;
        self.last_settings.dark = dark;
    }

    /// 是否需要 16ms 动画心跳。
    pub fn animating(&self) -> bool {
        if !self.theme.style.animations {
            return false;
        }
        if self.page_anim_ms < PAGE_ANIM_MS {
            return true;
        }
        if self.model.phase == LauncherPhase::Launching {
            return true;
        }
        if !self.model.toasts.is_empty() || self.model.java_scanning {
            return true;
        }
        self.model
            .downloads
            .iter()
            .any(|d| d.state == crate::domain::DownloadState::Downloading)
            || self
                .model
                .versions
                .iter()
                .any(|v| v.flow == crate::domain::VersionFlow::Installing)
            || self
                .model
                .accounts
                .iter()
                .any(|a| a.flow == crate::domain::AccountFlow::SigningIn)
    }

    /// 把设置页改动同步到 Theme。只在 Settings 真的变化时回写。
    fn sync_theme(&mut self, force: bool) {
        if !force && self.model.settings == self.last_settings {
            return;
        }
        let s = self.model.settings.clone();
        if s.dark != self.dark {
            let saved = self.theme.style.clone();
            self.dark = s.dark;
            self.theme = if s.dark {
                Theme::dark()
            } else {
                Theme::light()
            };
            self.theme.style = saved;
        }
        self.theme.style.glass.enabled = s.glass;
        self.theme.style.control_radius = s.radius;
        self.theme.style.animations = s.animations;
        self.theme.style.font.family = s.font_family.clone();
        if !s.animations {
            self.page_anim_ms = PAGE_ANIM_MS;
        }
        self.last_settings = s;
    }

    // -----------------------------------------------------------------------
    // 全局事件
    // -----------------------------------------------------------------------

    fn handle_global(&mut self, ui: &mut Ui<'_, impl Backend + ?Sized>) {
        // 拖拽安装
        let dropped = ui.dropped_files();
        if !dropped.is_empty() {
            self.model.install_dropped_files(&dropped);
        }

        // 托盘恢复
        if ui.tray_restore_requested() && self.model.tray_hidden {
            self.model.tray_hidden = false;
            self.model.log("[INFO] 从系统托盘恢复窗口");
        }

        let keys = ui.shortcuts();
        for k in &keys {
            match *k {
                ShortcutKey::Escape => {
                    if self.model.confirm.is_some() {
                        self.model.cancel_confirm();
                    } else if self.model.open_version_detail.is_some() {
                        self.model.open_version_detail = None;
                    } else if !self.model.update_dismissed && self.model.update_available.is_some()
                    {
                        self.model.update_dismissed = true;
                    } else {
                        ui.close_menu();
                    }
                }
                ShortcutKey::F5 => self.model.refresh(),
                _ => {}
            }
        }
        if let Some(p) = shell::shortcut_page(&keys) {
            self.set_page(p);
        }

        // 启动流程结束后按设置最小化或退出
        if let Some(action) = self.model.pending_window_action.take() {
            ui.request_window_action(action);
        }
    }

    // -----------------------------------------------------------------------
    // 主渲染
    // -----------------------------------------------------------------------

    pub fn render(&mut self, ui: &mut Ui<'_, impl Backend + ?Sized>) {
        self.handle_global(ui);
        let dt = ui.dt_ms();
        self.model.tick(dt);
        self.page_anim_ms = (self.page_anim_ms + dt).min(PAGE_ANIM_MS);

        // 引导是全屏独占页面
        if self.model.phase == LauncherPhase::Onboarding {
            onboarding::render(ui, &mut self.model, &self.theme);
            shell::render_toasts(ui, &self.model, &self.theme);
            shell::render_confirm(ui, &mut self.model, &self.theme);
            self.sync_theme(false);
            return;
        }

        // 磨玻璃背景：开启时产生 BlurRegion 指令（软件后端做真像素模糊）
        if self.theme.style.glass.enabled {
            let wr = ui.window_rect();
            ui.blur(
                wr,
                self.theme.style.glass.radius(),
                self.theme.style.glass.tint,
            );
        }

        if let Some(p) = shell::render_topbar(ui, &mut self.model, &self.theme) {
            self.set_page(p);
        }
        if let Some(p) = shell::render_sidebar(ui, &self.model, self.page, &self.theme) {
            self.set_page(p);
        }

        let banner = self.model.update_available.is_some() && !self.model.update_dismissed;
        let content = shell::content_rect(ui, &self.theme, banner);
        if banner {
            let banner_rect = Rect::new(
                content.x,
                self.theme.topbar_h + self.theme.pad,
                content.w,
                34,
            );
            shell::render_update_banner(ui, &mut self.model, banner_rect, &self.theme);
        }

        // 页面进入动画：内容区从左到右展开（裁剪实现，不改布局坐标）
        let t = (self.page_anim_ms / PAGE_ANIM_MS).min(1.0);
        let animated = self.theme.style.animations && t < 1.0;
        if animated {
            let ease = 1.0 - (1.0 - t) * (1.0 - t);
            let visible = Rect::new(
                content.x,
                content.y,
                (content.w as f32 * ease) as i32,
                content.h,
            );
            ui.push_clip(visible);
        }
        if let Some(p) = pages::render_page(ui, &mut self.model, self.page, content, &self.theme) {
            self.set_page(p);
        }
        if animated {
            ui.pop_clip();
        }

        // 浮层必须最后画
        shell::render_toasts(ui, &self.model, &self.theme);
        shell::render_confirm(ui, &mut self.model, &self.theme);
        self.sync_theme(false);
    }
}
