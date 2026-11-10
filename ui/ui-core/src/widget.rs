//! 控件：按钮、输入框、列表、标签等。
//!
//! 每个控件都是 `Ui` 上的一个方法，无状态（状态由调用方持有或由 `Ui` 内部跟踪）。
//! 这是即时模式的标准形态 —— 没有 `struct Button` 需要实例化和存储。

use crate::backend::Backend;
use crate::color::Color;
use crate::draw::TextAlign;
use crate::id::WidgetId;
use crate::layout::Rect;
use crate::ui::{Response, Ui};

/// 按钮样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// 普通按钮（灰底）。
    Normal,
    /// 主按钮（强调色）。
    Primary,
    /// 危险按钮（红色，用于删除）。
    Danger,
    /// 幽灵按钮（无背景，只有 hover 时出现）。
    Ghost,
}

impl<B: Backend + ?Sized> Ui<'_, B> {
    /// 背景色动画：目标变了就启动一段过渡，返回**当前**颜色。
    ///
    /// # 颜色怎么存进浮点动画
    ///
    /// 动画层是浮点数（0..1 的插值），颜色是 3 个 8 位通道。做法是
    /// **每个通道一个动画槽位**？—— 不行，那要 3 个槽位且不同步。
    ///
    /// 实际做法：把颜色打包成一个浮点数（`r<<16 | g<<8 | b` 当整数，转成 f32 存），
    /// 但那样插值会串位。**所以这里用"整体亮度差值"的方式**：
    ///
    /// 1. 动画值存的是"目标颜色的浮点编码"（r*65536 + g*256 + b）；
    /// 2. 插值时把 from/to 都解码回 (r,g,b) 再逐通道插值。
    ///
    /// 为此 `AnimationState` 需要支持"起点/终点是任意浮点，插值后由调用方解码"。
    /// 这正是当前实现：动画层只管浮点插值，颜色编解码在 UI 层做。
    ///
    /// # 返回 `None` 表示"不画背景"（幽灵按钮）
    fn color_anim(
        &mut self,
        id: WidgetId,
        target: Option<Color>,
        in_ms: f32,
        out_ms: f32,
    ) -> Option<Color> {
        use crate::anim::ColorChannel;
        let Some(target) = target else {
            // 幽灵按钮：无背景。停止动画并让值淡出（用 alpha 处理）。
            self.anim_stop(id, crate::anim::Slot::Background);
            return None;
        };
        // 动画开关关闭时，颜色直接落到目标值（不推进也不写入动画轨道）。
        if !self.theme.style.animations {
            return Some(target);
        }

        // ★ 三通道分别插值（避免打包成单个浮点数时的通道串位）。
        let channels = [
            (ColorChannel::Red, target.r() as f32),
            (ColorChannel::Green, target.g() as f32),
            (ColorChannel::Blue, target.b() as f32),
        ];
        let mut out = [0u8; 3];
        for (i, (ch, target_v)) in channels.iter().enumerate() {
            // ★ 判断"目标是否变了"必须对比**上一次的目标**，不能对比当前动画值。
            //   若对比当前值，过渡中间态每帧都算"变了" → 每帧重启动画
            //   → 永远走不到终点（实测：颜色闪一下就弹回）。
            let prev_target = self.anim.target_color(id, *ch);
            let changed = match prev_target {
                None => true,
                Some(p) => (p - *target_v).abs() > 0.5,
            };
            if changed {
                // 起点 = 当前画面上的值（动画的当前位置）。
                let current = self.anim.peek_color(id, *ch, *target_v);
                let dur = if current > *target_v { in_ms } else { out_ms };
                self.anim.start_color(
                    id,
                    *ch,
                    current,
                    *target_v,
                    dur,
                    crate::anim::Easing::OutQuad,
                );
            }
            // 取当前动画值（推进时间后）。
            let v = self.anim.color_value(id, *ch, self.dt_ms, *target_v);
            out[i] = v.clamp(0.0, 255.0).round() as u8;
        }
        Some(Color::rgb(out[0], out[1], out[2]))
    }

    /// 缩放动画：目标变了就启动过渡，返回当前缩放。
    fn scale_anim(&mut self, id: WidgetId, target: f32, in_ms: f32, out_ms: f32) -> f32 {
        if !self.theme.style.animations {
            return target;
        }
        use crate::anim::{Easing, Slot};
        // ★ 与 `color_anim` 同理：对比"上一次的目标"，不是当前动画值。
        let changed = match self.anim.target(id, Slot::Scale) {
            None => true,
            Some(p) => (p - target).abs() > 0.001,
        };
        if changed {
            // 起点 = 当前画面上的缩放值。
            let current = self.anim.peek(id, Slot::Scale, target);
            let dur = if current > target { in_ms } else { out_ms };
            // PCL 用 OutBack 的"回弹"手感
            self.anim_start(id, Slot::Scale, current, target, dur, Easing::OutBack);
        }
        self.anim_value(id, Slot::Scale, target)
    }

    /// 光标闪烁：透明度在 0↔1 之间往返。
    ///
    /// # 实现：两段动画接力
    ///
    /// 动画系统本身是"从 from 到 to 的一次性插值"，没有循环概念。
    /// 循环用**接力**实现：
    ///
    /// ```text
    /// 当前值 >= 1.0  → 启动淡出动画 (1.0 → 0.0, 500ms)
    /// 当前值 <= 0.0  → 启动淡入动画 (0.0 → 1.0, 500ms)
    /// 中间值          → 继续当前动画
    /// ```
    ///
    /// 这样每段动画都是普通的一次性动画，动画系统不需要理解"循环"概念，
    /// 保持简单。
    fn caret_anim(&mut self, id: WidgetId) -> f32 {
        if !self.theme.style.animations {
            return 1.0; // 关动画时光标常亮，而不是恒定熄灭
        }
        use crate::anim::{Easing, Slot};
        const HALF_PERIOD_MS: f32 = 500.0;
        let v = self.anim_value(id, Slot::Caret, 1.0);
        if v >= 0.999 {
            // 亮 → 开始淡出
            self.anim_start(id, Slot::Caret, 1.0, 0.0, HALF_PERIOD_MS, Easing::Linear);
            1.0
        } else if v <= 0.001 {
            // 灭 → 开始淡入
            self.anim_start(id, Slot::Caret, 0.0, 1.0, HALF_PERIOD_MS, Easing::Linear);
            0.0
        } else {
            v
        }
    }

    /// 按钮。
    ///
    /// 返回 [`Response`]，调用方用 `if resp.clicked { .. }` 判断。
    ///
    /// # 交互（对齐 PCL `MyButton`）
    ///
    /// | 行为 | 实现 |
    /// |---|---|
    /// | hover 变色 | ✅ `WidgetState::Hovered` |
    /// | 按下变色 | ✅ `WidgetState::Pressed`（跨帧，靠 `PressTracker`） |
    /// | **按下后拖出按钮再松开 → 不算点击** | ✅ `PressTracker::end` 要求同控件 |
    /// | 点击获得键盘焦点 | ✅ `request_focus` |
    /// | Enter / Space 触发 | ✅ 焦点在自身时 |
    /// | 焦点视觉（强调色边框） | ✅ `input_border_focus` 色 |
    /// | 禁用态不可交互 | ✅ `enabled == false` 时不响应点击 |
    pub fn button(&mut self, rect: Rect, label: &str, kind: ButtonKind, enabled: bool) -> Response {
        let id = self.next_id(label);
        let theme = self.theme.clone();
        // ★ 禁用的按钮**完全不参与**事件检测 —— 不响应点击、不进入按下态、不 hover。
        // PCL 的做法是 `IsEnabled` 时才处理 MouseUp 的 Click 事件。
        let (hovered, clicked, pressed) = if enabled {
            self.detect_click_pub(id, rect)
        } else {
            (false, false, false)
        };

        // ---- 键盘焦点 ----
        // 按钮可聚焦：登记到焦点环，供 Tab 遍历与方向键空间导航。
        self.focus_ring.register_at(id, enabled, rect);
        let has_focus = self.focus_ring.has_focus(id);
        // Enter / Space 触发（仅当焦点在自身）。
        let activated = enabled && has_focus && self.focus_ring.activate_pressed();
        // 鼠标点击后把焦点给按钮（PCL 的 MouseDown 里 Focus()）。
        // 焦点走 `focus_ring`（统一管理），同时同步到 `self.focus`（输入框用的槽位），
        // 避免出现两个互不同步的"当前焦点"。
        if clicked && enabled {
            self.focus_ring.set_focus(Some(id));
            self.request_focus(id);
        }

        // ================= 动画：颜色过渡 + 按压缩放 =================
        //
        // 对标 PCL `MyButton`：
        //   AniStart({AaColor(...ColorBrush3, AnimationColorIn)},  "MyButton Color " & Uuid)
        //   AniStart({AaScaleTransform(..., 0.955, 80, ...)},        "MyButton Scale " & Uuid)
        //
        // PCL 的做法是"状态变了就启动一段动画"，动画自己插值到目标值。
        // 本实现完全一致：目标颜色/缩放由状态决定，动画负责平滑过渡。
        //
        // 颜色用 0xRRGGBB 的浮点表示（只保留 RGB，alpha 单独处理）。
        const COLOR_IN_MS: f32 = 100.0; // PCL 的 AnimationColorIn
        const COLOR_OUT_MS: f32 = 200.0; // PCL 的 AnimationColorOut
        const PRESS_SCALE_MS: f32 = 80.0; // PCL 按下的 80ms
        const RELEASE_SCALE_MS: f32 = 300.0; // PCL 抬起的 300ms

        // ---- 目标背景色 ----
        //
        // ★ 匹配顺序很重要：必须**先按 pressed 分支**，再在 `pressed == false`
        // 里按 hovered 细分。
        //
        // 写成 `(Primary, _, false)` 放在 `(Primary, true, false)` 前面时，
        // 前者会吃掉所有 `pressed == false` 的情况（含 hovered == true），
        // 导致 hover 永远不生效 —— 而且编译器只会给"unreachable pattern"
        // 警告，不会报错。这种 bug 只能靠看图/看警告发现。
        let target_bg: Option<Color> = match (kind, pressed, hovered) {
            (_, _, _) if !enabled => Some(theme.disabled),
            (ButtonKind::Ghost, true, _) => Some(theme.button_active),
            (ButtonKind::Ghost, false, true) => Some(theme.button_hover),
            (ButtonKind::Ghost, false, false) => None,
            (ButtonKind::Primary, true, _) => Some(theme.accent.lighten(-0.12)),
            (ButtonKind::Primary, false, true) => Some(theme.accent_hover),
            (ButtonKind::Primary, false, false) => Some(theme.accent),
            (ButtonKind::Danger, true, _) => Some(theme.danger.lighten(-0.12)),
            (ButtonKind::Danger, false, true) => Some(theme.danger_hover),
            (ButtonKind::Danger, false, false) => Some(theme.danger),
            (ButtonKind::Normal, true, _) => Some(theme.button_active),
            (ButtonKind::Normal, false, true) => Some(theme.button_hover),
            (ButtonKind::Normal, false, false) => Some(theme.button),
        };

        // 背景色动画：目标变了就启动一段过渡。
        // 颜色存成 3 个浮点分量（r/g/b），alpha 单独处理。
        let bg_anim = self.color_anim(id, target_bg, COLOR_IN_MS, COLOR_OUT_MS);

        // ---- 缩放动画 ----
        // PCL：按下缩到 0.955（80ms），抬起弹回 1.0（300ms）。
        let target_scale = if pressed && enabled { 0.955 } else { 1.0 };
        let scale = self.scale_anim(id, target_scale, PRESS_SCALE_MS, RELEASE_SCALE_MS);

        // ================= 绘制 =================
        // 缩放会让内容以按钮中心为锚点收缩。
        let draw_rect = scale_rect(rect, scale);

        let radius = theme.style.radius();
        if let Some(bg) = bg_anim {
            self.fill_round(draw_rect, bg, radius);
        }

        // 焦点框（画在背景之上、文字之下）—— 与按钮同圆角，否则会露出方角。
        if has_focus {
            self.stroke_round(
                Rect::new(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2),
                theme.accent,
                radius + 1,
                1,
            );
        }

        // 文字
        let fg = match (kind, enabled) {
            (_, false) => theme.text_faint,
            (ButtonKind::Primary, _) | (ButtonKind::Danger, _) => Color::rgb(0xFF, 0xFF, 0xFF),
            _ => theme.text,
        };
        let pad = theme.pad_small;
        self.text_vcenter(
            Rect::new(
                draw_rect.x + pad,
                draw_rect.y,
                (draw_rect.w - 2 * pad).max(0),
                draw_rect.h,
            ),
            label,
            fg,
            theme.font_body,
            TextAlign::Center,
            false,
        );

        Response {
            id,
            rect,
            hovered,
            // 鼠标点击或键盘激活（Enter/Space）都算"点击"。
            clicked: clicked || activated,
            pressed,
            clicked_this_frame: clicked || activated,
        }
    }

    /// 输入框。
    ///
    /// `text` 是可变的 —— 控件把编辑动作应用进去（这是即时模式里少见的"就地改"，
    /// 但输入框需要它：调用方持有字符串，控件负责按键盘事件改它）。
    ///
    /// # 光标闪烁
    ///
    /// 获得焦点时显示闪烁光标（`|`）。闪烁由**动画槽位** [`Slot::Caret`] 驱动：
    /// 光标透明度在 0↔1 之间循环，周期 1 秒。
    ///
    /// 用动画而非"按时间取模"的原因：动画系统是**时间驱动**的（`dt_ms` 累积），
    /// 而 `按时间取模` 在没有事件时不会推进（画面会卡住）。动画的 `dt_ms` 由
    /// 后端每帧提供，即使没有输入事件也会推进。
    pub fn text_input(
        &mut self,
        rect: Rect,
        label: &str,
        text: &mut String,
        placeholder: &str,
    ) -> Response {
        let id = self.next_id(label);
        let theme = self.theme.clone();
        let (hovered, clicked, _pressed) = self.detect_click_pub(id, rect);

        // 点击 → 获取焦点
        if clicked {
            self.request_focus(id);
        }
        let focused_now = self.has_focus(id);

        // 背景 + 边框（圆角）
        let radius = theme.style.radius();
        self.fill_round(rect, theme.input_bg, radius);
        let border = if focused_now {
            theme.input_border_focus
        } else {
            theme.input_border
        };
        self.stroke_round(rect, border, radius, theme.border);

        // 应用编辑
        if focused_now {
            self.collect_text_for_pub(id);
            for edit in self.take_text_events(id) {
                match edit {
                    crate::ui::TextEdit::Insert(s) => text.push_str(&s),
                    crate::ui::TextEdit::Backspace => {
                        text.pop();
                    }
                    crate::ui::TextEdit::Delete => {}
                }
            }
        }

        // 文字或占位符
        let pad = theme.pad_small;
        let inner = Rect::new(rect.x + pad, rect.y, (rect.w - 2 * pad).max(0), rect.h);
        if text.is_empty() && !focused_now {
            self.text_vcenter(
                inner,
                placeholder,
                theme.text_faint,
                theme.font_body,
                TextAlign::Left,
                false,
            );
        } else {
            // 光标闪烁：透明度在 0↔1 之间往返。
            // 循环用两个 0.5 秒的动画段实现（淡出 + 淡入）。
            let caret_on = if focused_now {
                let v = self.caret_anim(id);
                v > 0.5
            } else {
                // ★ 失焦必须停掉光标轨道：caret_anim 是"两段接力"的循环动画，
                // 留着轨道会让 any_animating() 永远为 true → 16ms 心跳永不关闭 →
                // 没有动画需求时仍每帧全屏重绘（持续闪烁 + CPU 空转）。
                self.anim_stop(id, crate::anim::Slot::Caret);
                false
            };
            let shown = if focused_now {
                if caret_on {
                    format!("{text}|")
                } else {
                    text.clone()
                }
            } else {
                text.clone()
            };
            self.text_vcenter(
                inner,
                shown,
                theme.text,
                theme.font_body,
                TextAlign::Left,
                false,
            );
        }

        let _ = hovered;
        Response {
            id,
            rect,
            hovered,
            clicked,
            pressed: false,
            clicked_this_frame: clicked,
        }
    }

    /// 单行标签。
    pub fn label(&mut self, rect: Rect, text: impl Into<String>, dim: bool) {
        let theme = self.theme.clone();
        let color = if dim { theme.text_dim } else { theme.text };
        self.text_vcenter(rect, text, color, theme.font_body, TextAlign::Left, false);
    }

    /// 区块标题（略大、加粗）。
    pub fn heading(&mut self, rect: Rect, text: impl Into<String>) {
        let theme = self.theme.clone();
        self.text_vcenter(
            rect,
            text,
            theme.text,
            theme.font_heading,
            TextAlign::Left,
            true,
        );
    }

    /// 副标题/说明文字（小字、暗色）。
    pub fn caption(&mut self, rect: Rect, text: impl Into<String>) {
        let theme = self.theme.clone();
        self.text_vcenter(
            rect,
            text,
            theme.text_dim,
            theme.font_small,
            TextAlign::Left,
            false,
        );
    }

    /// 水平分隔线。
    pub fn separator(&mut self, rect: Rect) {
        let theme = self.theme.clone();
        self.fill(
            Rect::new(rect.x, rect.y + rect.h / 2, rect.w, 1),
            theme.divider,
        );
    }

    /// 侧栏项（可选中）。用于左侧导航。
    pub fn nav_item(&mut self, rect: Rect, label: &str, selected: bool) -> Response {
        let id = self.next_id(label);
        let theme = self.theme.clone();
        let (hovered, clicked, _p) = self.detect_click_pub(id, rect);

        let bg = if selected {
            Some(theme.row_selected)
        } else if hovered {
            Some(theme.row_hover)
        } else {
            None
        };
        if let Some(bg) = bg {
            self.fill_round(rect, bg, theme.style.radius());
        }
        // 左侧选中指示条
        if selected {
            self.fill_round(Rect::new(rect.x, rect.y, 3, rect.h), theme.accent, 2);
        }
        let fg = if selected { theme.text } else { theme.text_dim };
        let pad = theme.pad;
        self.text_vcenter(
            Rect::new(rect.x + pad, rect.y, (rect.w - pad).max(0), rect.h),
            label,
            fg,
            theme.font_body,
            TextAlign::Left,
            selected,
        );
        Response {
            id,
            rect,
            hovered,
            clicked,
            pressed: false,
            clicked_this_frame: clicked,
        }
    }

    /// 卡片容器背景（返回内部可用区域）。
    pub fn card(&mut self, rect: Rect, pad: i32) -> Rect {
        let theme = self.theme.clone();
        let radius = theme.style.radius();
        self.fill_round(rect, theme.card, radius);
        self.stroke_round(rect, theme.divider, radius, theme.border);
        Rect::new(
            rect.x + pad,
            rect.y + pad,
            (rect.w - 2 * pad).max(0),
            (rect.h - 2 * pad).max(0),
        )
    }

    /// 空状态提示（居中、暗色）。
    pub fn empty_state(&mut self, rect: Rect, msg: impl Into<String>) {
        let theme = self.theme.clone();
        self.text_vcenter(
            rect,
            msg,
            theme.text_faint,
            theme.font_body,
            TextAlign::Center,
            false,
        );
    }

    /// 状态徽标（例如 trust 级别）。返回宽度。
    pub fn badge(&mut self, x: i32, y: i32, text: &str, color: Color) -> i32 {
        let theme = self.theme.clone();
        let w = self.measure(text, theme.font_small, false) + theme.pad_small * 2;
        let h = theme.control_h_small - 4;
        let rect = Rect::new(x, y, w, h);
        self.fill_round(rect, color, theme.style.radius());
        self.text_vcenter(
            rect,
            text,
            Color::rgb(0xFF, 0xFF, 0xFF),
            theme.font_small,
            TextAlign::Center,
            false,
        );
        w
    }

    /// 进度条。
    pub fn progress(&mut self, rect: Rect, fraction: f32) {
        let theme = self.theme.clone();
        // 进度条是胶囊形：半径取半高，与窗口圆角体系一致。
        let r = rect.h / 2;
        self.fill_round(rect, theme.input_bg, r);
        let w = ((rect.w as f32) * fraction.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            self.fill_round(Rect::new(rect.x, rect.y, w, rect.h), theme.accent, r);
        }
        self.stroke_round(rect, theme.divider, r, theme.border);
    }

    /// 复选框。返回新状态。
    pub fn checkbox(&mut self, rect: Rect, label: &str, checked: &mut bool) -> Response {
        let id = self.next_id(label);
        let theme = self.theme.clone();
        let (hovered, clicked, _p) = self.detect_click_pub(id, rect);
        if clicked {
            *checked = !*checked;
        }
        let box_size = (rect.h - 4).clamp(12, 18);
        let box_rect = Rect::new(rect.x, rect.y + (rect.h - box_size) / 2, box_size, box_size);
        self.fill_round(
            box_rect,
            if hovered {
                theme.button_hover
            } else {
                theme.input_bg
            },
            3,
        );
        self.stroke_round(box_rect, theme.input_border, 3, theme.border);
        if *checked {
            // 对勾：用一个内缩的圆角方块表示（避免引入图标系统）
            let inner = box_rect.shrink(crate::layout::EdgeInsets::all(4));
            self.fill_round(inner, theme.accent, 2);
        }
        let text_rect = Rect::new(
            box_rect.right() + theme.pad_small,
            rect.y,
            (rect.right() - box_rect.right() - theme.pad_small).max(0),
            rect.h,
        );
        self.text_vcenter(
            text_rect,
            label,
            theme.text,
            theme.font_body,
            TextAlign::Left,
            false,
        );
        Response {
            id,
            rect,
            hovered,
            clicked,
            pressed: false,
            clicked_this_frame: clicked,
        }
    }

    /// 多选列表项。
    ///
    /// 返回 `(Response, MultiSelectAction)`。
    ///
    /// # 交互（对标 PCL `MyListItem` 的 `Checked`）
    ///
    /// | 操作 | 行为 |
    /// |---|---|
    /// | 左键点击复选框 | 切换该项选中状态 |
    /// | 左键点击行 | 切换该项选中状态 |
    /// | Ctrl + 左键 | 切换该项（不影响其他项） |
    /// | Shift + 左键 | 选中从"锚点"到该项的**范围** |
    /// | 右键 | 弹出上下文菜单（由调用方处理） |
    ///
    /// # Shift 范围选择
    ///
    /// 需要一个"锚点"（上一次单独点击的行）。锚点存在 [`Selection`] 里 ——
    /// 它是跨帧状态，且需要被列表与工具栏共享。
    ///
    /// 返回值告诉调用方发生了什么：
    ///
    /// - [`MultiSelectAction::Toggled`]：单项切换
    /// - [`MultiSelectAction::RangeSelected`]：范围选择
    /// - [`MultiSelectAction::None`]：没有变化
    ///
    /// 参数较多是即时模式列表控件的固有形态：一行需要同时拿到几何、标识、
    /// 文案、跨帧选中集、范围锚点与有序 id 列表，拆成结构体反而让调用方更难读。
    #[allow(clippy::too_many_arguments)]
    pub fn multi_select_row(
        &mut self,
        rect: Rect,
        id: &str,
        label: &str,
        detail: &str,
        selection: &mut crate::selection::Selection,
        anchor: &mut Option<String>,
        ordered_ids: &[String],
    ) -> (Response, MultiSelectAction) {
        let theme = self.theme.clone();
        let wid = self.next_id(id);
        let (hovered, clicked, _pressed) = self.detect_click_pub(wid, rect);
        let selected = selection.is_selected(id);

        // ---- 背景 ----
        let bg = if selected {
            Some(theme.row_selected)
        } else if hovered {
            Some(theme.row_hover)
        } else {
            None
        };
        if let Some(bg) = bg {
            self.fill_round(rect, bg, theme.style.radius());
        }

        // ---- 复选框 ----
        let box_size = (rect.h - 8).clamp(12, 18);
        let box_rect = Rect::new(
            rect.x + theme.pad,
            rect.y + (rect.h - box_size) / 2,
            box_size,
            box_size,
        );
        self.fill_round(box_rect, theme.input_bg, 3);
        self.stroke_round(box_rect, theme.input_border, 3, theme.border);
        if selected {
            let inner = box_rect.shrink(crate::layout::EdgeInsets::all(4));
            self.fill_round(inner, theme.accent, 2);
        }

        // ---- 文字 ----
        let text_x = box_rect.right() + theme.pad_small;
        ui_text_vcenter(
            self,
            Rect::new(text_x, rect.y, rect.w - text_x, rect.h),
            label,
            theme.text,
            theme.font_body,
            true,
        );
        let dw = self.measure(detail, theme.font_small, false);
        ui_text_vcenter(
            self,
            Rect::new(rect.right() - dw - theme.pad, rect.y, dw, rect.h),
            detail,
            theme.text_faint,
            theme.font_small,
            false,
        );

        // ---- 点击处理 ----
        let mut action = MultiSelectAction::None;
        if clicked {
            let ctrl = self.ctrl_held();
            let shift = self.shift_held();
            if shift && anchor.is_some() {
                // 范围选择：从锚点到当前项。
                if let Some(a) = anchor.clone() {
                    if let (Some(ai), Some(ti)) = (
                        ordered_ids.iter().position(|x| *x == a),
                        ordered_ids.iter().position(|x| x == id),
                    ) {
                        let (lo, hi) = (ai.min(ti), ai.max(ti));
                        for item in &ordered_ids[lo..=hi] {
                            selection.select(item);
                        }
                        action = MultiSelectAction::RangeSelected;
                    }
                }
            } else if ctrl {
                // Ctrl：切换单项，锚点更新为该项。
                selection.toggle(id);
                *anchor = Some(id.to_owned());
                action = MultiSelectAction::Toggled;
            } else {
                // 普通点击：切换单项。
                selection.toggle(id);
                *anchor = Some(id.to_owned());
                action = MultiSelectAction::Toggled;
            }
        }

        (
            Response {
                id: wid,
                rect,
                hovered,
                clicked,
                pressed: false,
                clicked_this_frame: clicked,
            },
            action,
        )
    }

    /// 复选框（独立控件，用于"全选"）。
    pub fn checkbox2(&mut self, rect: Rect, label: &str, checked: &mut bool) -> Response {
        self.checkbox(rect, label, checked)
    }

    // ---- 供控件模块内部使用的桥接（避免把私有方法暴露给业务代码） ----

    pub(crate) fn detect_click_pub(&mut self, id: WidgetId, rect: Rect) -> (bool, bool, bool) {
        self.detect_click(id, rect)
    }

    pub(crate) fn collect_text_for_pub(&mut self, id: WidgetId) {
        self.collect_text_for(id)
    }
}

/// 多选列表的一次交互结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiSelectAction {
    /// 无变化。
    None,
    /// 单项被切换。
    Toggled,
    /// 范围被选中（Shift + 点击）。
    RangeSelected,
}

/// 文字垂直居中绘制（供 `multi_select_row` 借用，避免与 `Ui` 方法名冲突）。
fn ui_text_vcenter<B: Backend + ?Sized>(
    ui: &mut Ui<'_, B>,
    rect: Rect,
    text: impl Into<String>,
    color: Color,
    size_px: i32,
    bold: bool,
) {
    ui.text_vcenter(rect, text, color, size_px, TextAlign::Left, bold);
}

/// 按压缩放：把矩形按比例缩小（以中心为锚点）。
pub(crate) fn scale_rect(rect: Rect, scale: f32) -> Rect {
    if (scale - 1.0).abs() < 0.0005 {
        return rect;
    }
    let w = ((rect.w as f32) * scale).round() as i32;
    let h = ((rect.h as f32) * scale).round() as i32;
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    Rect::new(cx - w / 2, cy - h / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_rect_expands_around_center() {
        let r = Rect::new(0, 0, 100, 40);
        let s = scale_rect(r, 1.5);
        assert_eq!((s.w, s.h), (150, 60));
        assert_eq!((s.x, s.y), (-25, -10), "放大时中心必须保持不变");
    }

    #[test]
    fn scale_rect_zero_collapses_to_center() {
        let r = Rect::new(10, 20, 100, 40);
        let s = scale_rect(r, 0.0);
        assert_eq!((s.w, s.h), (0, 0));
        assert_eq!((s.x, s.y), (60, 40));
    }

    #[test]
    fn scale_rect_shrinks_around_center() {
        let r = Rect::new(0, 0, 100, 40);
        let s = scale_rect(r, 0.5);
        assert_eq!(s.w, 50);
        assert_eq!(s.h, 20);
        assert_eq!(s.x, 25, "应保持中心不变");
        assert_eq!(s.y, 10);
    }

    #[test]
    fn scale_rect_identity() {
        let r = Rect::new(10, 20, 100, 40);
        assert_eq!(scale_rect(r, 1.0), r);
    }
}
