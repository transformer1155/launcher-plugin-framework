//! UI 构建器：即时模式的核心。
//!
//! # 每帧的工作流程
//!
//! ```text
//! backend 产生 events ──► Ui::begin_frame(events)   // 分配事件给控件
//!                            │
//!                            ├─ 业务代码调用 ui.button(..) 等
//!                            │     └─ 记录布局 + 生成绘制指令 + 返回 Response
//!                            │
//!                            ▼
//!                      Ui::finish() -> DrawList
//!                            │
//!                            ▼
//!                      backend.present(&list)
//! ```
//!
//! # 事件分配（即时模式的关键机制）
//!
//! 事件在 `begin_frame` 里**不做分配**，而是在控件被调用时**按需查询**：
//! 控件知道自己的 `WidgetId` 和矩形，于是能判断"这次点击是不是我的"。
//!
//! 用一个 `hot`（鼠标悬停）/ `active`（被按下）模型：
//!
//! - `hovered(id)`：鼠标是否在**本帧已绘制的最上层**控件上
//! - `clicked(id)`：本帧内是否有 `MouseDown` + `MouseUp` 都落在该控件矩形内
//!
//! 这只在单次点击时正确 —— 拖拽跨越控件的行为未实现（启动器不需要）。

use crate::backend::Backend;
use crate::color::Color;
use crate::draw::{DrawList, TextAlign};
use crate::event::{Event, Key, MouseButton};
use crate::id::{WidgetId, WindowAction};
use crate::layout::{EdgeInsets, Rect, Size};
use crate::theme::Theme;

/// 一次控件调用的结果。
#[derive(Debug, Clone, Copy, Default)]
pub struct Response {
    pub id: WidgetId,
    pub rect: Rect,
    /// 鼠标在本帧位于控件上。
    pub hovered: bool,
    /// 本帧内被点击（按下且在同一控件内抬起）。
    pub clicked: bool,
    /// 本帧内被按下（尚未抬起）。
    pub pressed: bool,
    /// 本帧内刚获得点击（按下瞬间）。
    pub clicked_this_frame: bool,
}

impl Response {
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }
    pub fn is_clicked(&self) -> bool {
        self.clicked
    }
}

/// 控件是否可交互。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetState {
    Normal,
    Hovered,
    Pressed,
    Disabled,
}

/// UI 构建器。
pub struct Ui<'a, B: Backend + ?Sized> {
    backend: &'a B,
    pub(crate) theme: &'a Theme,
    draw: DrawList,
    events: Vec<Event>,
    /// 已消费事件的下标（每个事件只被一个控件消费）。
    consumed: Vec<bool>,
    size: Size,
    /// 当前鼠标位置。
    mouse: Option<(i32, i32)>,
    /// 跨帧持久化的按下态（由调用方持有，见 [`crate::press::PressTracker`]）。
    press: &'a mut crate::press::PressTracker,
    /// 键盘焦点环（由调用方持有，见 [`crate::focus::FocusRing`]）。
    pub(crate) focus_ring: &'a mut crate::focus::FocusRing,
    /// 动画状态（由调用方持有，见 [`crate::anim::AnimationState`]）。
    pub(crate) anim: &'a mut crate::anim::AnimationState,
    /// 右键菜单（由调用方持有，见 [`crate::menu::ContextMenu`]）。
    pub(crate) menu: &'a mut crate::menu::ContextMenu,
    /// 本帧被点击的菜单项下标（`close()` 会清空菜单里的记录，所以在此留存）。
    menu_clicked: Option<usize>,
    /// 跨帧共享槽：把 `finish()` 里判定的菜单点击交给下一帧的业务代码。
    menu_action: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    /// 本帧请求的窗口动作（无边框窗口的标题栏按钮 / 拖动）。
    window_action: std::rc::Rc<std::cell::Cell<Option<WindowAction>>>,
    /// 距上一帧的毫秒数（动画推进用）。
    pub(crate) dt_ms: f32,
    /// 本帧内完成的点击（在 mouse up 时确定）。
    ///
    /// 目前仅用于诊断；控件通过 [`Self::detect_click`] 自行判定点击，
    /// 保留它是为了未来支持"点击穿透/事件冒泡"。
    #[allow(dead_code)]
    clicks: Vec<(WidgetId, i32, i32)>,
    /// 当前布局光标（`ui.vertical` 之类用）。
    cursor: Vec<(i32, i32)>,
    /// 容器裁剪区栈。
    clip_stack: Vec<Rect>,
    /// 本帧控件计数（为 ID 派生提供序号）。
    index: u32,
    /// 当前父 ID（容器栈）。
    parent_stack: Vec<WidgetId>,
    /// 文本输入框的焦点 ID（跨帧保留，由调用方持有）。
    ///
    /// 实际存储在 [`FrameState`] 的共享槽里（`Rc<Cell<..>>`），
    /// 这样 UI 与调用方都能访问，且不产生可变借用的冲突。
    pub focus: std::rc::Rc<std::cell::Cell<Option<WidgetId>>>,
    /// 本帧产生的文本编辑结果（供输入框读取）。
    text_events: Vec<(WidgetId, TextEdit)>,
    /// 滚动偏移（由调用方持有，按 ID 索引）。
    scroll: &'a mut std::collections::HashMap<WidgetId, i32>,
}

/// 输入框内的编辑动作。
#[derive(Debug, Clone, PartialEq)]
pub enum TextEdit {
    Insert(String),
    Backspace,
    Delete,
}

/// 跨帧持久状态的集合。
///
/// # 为什么打包成结构体
///
/// `Ui::begin_frame` 需要 5 个跨帧状态（滚动、按下态、焦点环、动画、右键菜单）。
/// 逐个作为参数会让签名长到 10 个参数，且**顺序容易记错**（都是 `&mut` 借用，
/// 传错顺序编译器不会报错，但语义会乱）。
///
/// 打包成结构体后：
/// - 签名短；
/// - 调用方持有一个 `FrameState` 即可；
/// - 新增状态时不必改所有调用点。
///
/// # 焦点槽
///
/// 焦点需要**双向**流动：业务代码设置（点击按钮）、UI 读取（画焦点框）、
/// 再写回（下一帧继续）。
///
/// 这里用 `Rc<Cell<Option<WidgetId>>>` 作为共享槽：
///
/// - `Ui` 持有它的一个引用（可读可写）；
/// - 调用方持有另一个引用（帧结束后读回）。
///
/// 之所以不是 `&mut`：那会让 `Ui` 和调用方同时可变借用 `FrameState`，
/// Rust 不允许（而这正是"把焦点写回"所必需的）。
///
/// # 生命周期
///
/// 结构体本身是**拥有**这些状态的，调用方通常这样用：
///
/// ```ignore
/// let mut state = FrameState::new();
/// loop {
///     let mut ui = Ui::begin_frame(&backend, &theme, events, &mut state);
///     app.render(&mut ui);
///     ui.finish()
/// }
/// ```
pub struct FrameState {
    /// 各可滚动区域的滚动偏移。
    pub scroll: std::collections::HashMap<WidgetId, i32>,
    /// 按下态。
    pub press: crate::press::PressTracker,
    /// 键盘焦点环。
    pub focus: crate::focus::FocusRing,
    /// 动画状态。
    pub anim: crate::anim::AnimationState,
    /// 右键菜单。
    pub menu: crate::menu::ContextMenu,
    /// 焦点共享槽（UI 与调用方各持一份引用）。
    focused: std::rc::Rc<std::cell::Cell<Option<WidgetId>>>,
    /// 鼠标位置（跨帧）。
    ///
    /// ★ 必须是跨帧的：hover 状态是"鼠标在控件上"，不是"本帧有鼠标事件"。
    ///
    /// 若只用本帧事件推导鼠标位置，那么**没有鼠标事件的帧**（例如用户停住不动、
    /// 或只按了键盘）就会认为"鼠标不在任何地方" → hover 消失 → 按钮闪回原色。
    /// 这正是动画过渡被打断的根因。
    mouse: Option<(i32, i32)>,
    /// 右键菜单的点击结果（跨帧共享槽）。
    ///
    /// 菜单是浮层，命中测试发生在 `finish()` 里（必须画在所有控件之后），
    /// 那时业务代码早就跑完了。所以结果不能靠 `Response` 返回，只能留到
    /// **下一帧**由业务代码用 `Ui::take_menu_click` 取走。
    menu_action: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    /// 本帧的窗口动作（无边框窗口没有系统标题栏，由 UI 发起）。
    ///
    /// 用共享槽而不是 `Ui` 返回值：`Ui` 会被 `finish()` 消耗，而主循环要在
    /// 帧结束后才能执行平台调用（拖动/最小化/关闭）。
    window_action: std::rc::Rc<std::cell::Cell<Option<WindowAction>>>,
}

impl FrameState {
    pub fn new() -> Self {
        Self {
            scroll: std::collections::HashMap::new(),
            press: crate::press::PressTracker::new(),
            focus: crate::focus::FocusRing::new(),
            anim: crate::anim::AnimationState::new(),
            menu: crate::menu::ContextMenu::new(),
            focused: std::rc::Rc::new(std::cell::Cell::new(None)),
            mouse: None,
            menu_action: std::rc::Rc::new(std::cell::Cell::new(None)),
            window_action: std::rc::Rc::new(std::cell::Cell::new(None)),
        }
    }

    /// 当前焦点（读）。
    pub fn focused(&self) -> Option<WidgetId> {
        self.focused.get()
    }

    /// 设置焦点（写）。
    pub fn set_focused(&self, id: Option<WidgetId>) {
        self.focused.set(id);
    }

    /// 取走本帧请求的窗口动作（取完即清空）。
    ///
    /// 主循环在 `ui.finish()` 之后调用，交给后端执行。软件后端 / 离屏渲染
    /// 没有窗口，忽略即可。
    pub fn take_window_action(&self) -> Option<WindowAction> {
        self.window_action.take()
    }
}

impl Default for FrameState {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, B: Backend + ?Sized> Ui<'a, B> {
    /// 开始一帧。
    ///
    /// # 跨帧持久状态
    ///
    /// 即时模式 UI 每帧重建控件，**所有跨帧状态必须由外部保管**，否则会丢。
    /// 这些状态打包在 [`FrameState`] 里（滚动偏移、按下态、焦点环、动画、右键菜单）。
    ///
    /// `dt_ms` 是距上一帧的毫秒数（动画推进用）。传 0 表示"不推进时间"
    /// （用于离屏渲染快照）。
    pub fn begin_frame(
        backend: &'a B,
        theme: &'a Theme,
        events: Vec<Event>,
        state: &'a mut FrameState,
        dt_ms: f32,
    ) -> Self {
        let size = backend.size();
        // 每帧把外观配置交给后端（后端自己判断变化与否，配置没变时是 no-op）。
        // 放在 begin_frame 里，业务代码改 Theme::style 后下一帧就生效，
        // 不需要在页面里到处传平台句柄。
        backend.configure(&theme.style);
        // ★ 鼠标位置跨帧保持：只有收到鼠标事件时才更新。
        //   （本帧没有鼠标事件 ≠ 鼠标不在那里）
        if let Some(p) = events.iter().rev().find_map(|e| e.pos()) {
            state.mouse = Some(p);
        }
        let mouse = state.mouse;
        // 焦点环：本帧清空登记表、保留焦点；并处理键盘事件。
        //
        // ★ Tab 的处理**推迟到帧末**（见 `finish`）：焦点环的遍历顺序来自
        // "本帧登记了哪些可聚焦控件"，而登记发生在控件绘制时。
        // 若在帧首处理 Tab，焦点环还是空的，Tab 就会失效。
        state.focus.begin_frame();
        state.focus.handle_events(&events);
        // 右键菜单：每帧清空瞬时状态并处理事件（Escape 关闭、右键按下位置）。
        state.menu.begin_frame();
        state.menu.handle_events(&events);
        // 拆开 `state` 的各个字段（Rust 不允许同时借用同一个 struct 的多个字段）。
        let FrameState {
            scroll,
            press,
            focus: focus_ring,
            anim,
            menu,
            focused,
            mouse: _,
            menu_action,
            window_action,
        } = state;
        let focused = std::rc::Rc::clone(focused);
        let menu_action = std::rc::Rc::clone(menu_action);
        let window_action = std::rc::Rc::clone(window_action);
        Self {
            backend,
            theme,
            draw: DrawList::new(),
            events,
            consumed: Vec::new(),
            size,
            mouse,
            press,
            focus_ring,
            anim,
            menu,
            menu_clicked: None,
            menu_action,
            window_action,
            dt_ms,
            clicks: Vec::new(),
            cursor: vec![(0, 0)],
            clip_stack: Vec::new(),
            index: 0,
            parent_stack: vec![WidgetId::NONE],
            focus: focused,
            text_events: Vec::new(),
            scroll,
        }
    }

    /// 结束一帧，返回绘制列表。
    ///
    /// # Tab 的处理在这里
    ///
    /// Tab 需要"本帧登记了哪些可聚焦控件"才能决定往哪跳，而登记发生在控件绘制时
    /// （也就是本帧的中间）。因此 Tab 的处理必须**推迟到帧末**：
    ///
    /// - 在帧首处理 → 焦点环还是空的 → Tab 失效（第一帧按 Tab 没反应）。
    /// - 在帧末处理 → 焦点环已填好 → Tab 正确跳到下一个控件。
    ///
    /// 焦点在**下一帧**生效（视觉上晚一帧），这是可接受的：焦点框的移动本来
    /// 就该在下一帧才画出来。
    pub fn finish(mut self) -> DrawList {
        // 关闭所有未闭合的裁剪（保证配对）
        while self.clip_stack.pop().is_some() {
            self.draw.pop_clip();
        }
        // Tab：移动焦点。
        if self.focus_ring.tab_pressed() {
            if let Some(next) = self.focus_ring.focus_next() {
                self.focus_ring.set_focus(Some(next));
                self.focus.set(Some(next));
            }
        }
        // 方向键：空间导航（需要控件位置，所以同样在帧末处理）。
        if let Some(dir) = self.focus_ring.arrow() {
            if let Some(next) = self.focus_ring.focus_arrow(dir) {
                self.focus_ring.set_focus(Some(next));
                self.focus.set(Some(next));
            }
        }
        // ★ 焦点环与共享槽保持一致。
        //   焦点环是"权威"（负责 Tab/方向键遍历），共享槽是给业务代码读的。
        //   两者必须在帧末同步，否则业务代码读到的焦点是过期的。
        if let Some(cur) = self.focus_ring.current() {
            if self.focus.get() != Some(cur) {
                self.focus.set(Some(cur));
            }
        }

        // ★ 右键菜单画在**所有控件之后** —— 它是浮层，必须在最上层。
        if self.menu.is_open() {
            self.draw_menu();
        }
        std::mem::take(&mut self.draw)
    }

    /// 本帧结束后的当前焦点（供调用方持久化）。
    pub fn current_focus(&self) -> Option<WidgetId> {
        self.focus.get()
    }

    // ---------------------------------------------------------------
    // 动画辅助
    // ---------------------------------------------------------------

    /// 读取某槽位的动画值（并推进时间）。
    pub fn anim_value(&mut self, id: WidgetId, slot: crate::anim::Slot, default: f32) -> f32 {
        self.anim.value(id, slot, self.dt_ms, default)
    }

    /// 启动一段动画（同槽位覆盖）。
    pub fn anim_start(
        &mut self,
        id: WidgetId,
        slot: crate::anim::Slot,
        from: f32,
        to: f32,
        duration_ms: f32,
        easing: crate::anim::Easing,
    ) {
        self.anim.start(id, slot, from, to, duration_ms, easing);
    }

    /// 打断某槽位的动画。
    pub fn anim_stop(&mut self, id: WidgetId, slot: crate::anim::Slot) {
        self.anim.stop(id, slot);
    }

    /// 当前是否有动画在进行。
    pub fn animating(&self) -> bool {
        self.anim.any_animating()
    }

    /// 距上一帧的毫秒数。
    pub fn dt_ms(&self) -> f32 {
        self.dt_ms
    }

    // ---------------------------------------------------------------
    // 全局事件（快捷键 / 拖拽安装 / 托盘）
    // ---------------------------------------------------------------

    /// 本帧是否按下了指定全局快捷键。
    ///
    /// **只读、不消费**：同一帧里多个模块都要响应 Esc / F5（关闭浮层、刷新页面），
    /// 若第一个查询者把事件吃掉，后面的就永远收不到。全局快捷键是「意图」，
    /// 与控件点击的「归属」语义不同。
    pub fn shortcut_pressed(&self, key: crate::id::ShortcutKey) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, Event::Shortcut(k) if *k == key))
    }

    /// 本帧的全部全局快捷键（按事件顺序）。
    pub fn shortcuts(&self) -> Vec<crate::id::ShortcutKey> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Shortcut(k) => Some(*k),
                _ => None,
            })
            .collect()
    }

    /// 本帧拖入窗口的文件路径（拖拽安装整合包 / `.jar` / 游戏目录）。
    pub fn dropped_files(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.events {
            if let Event::FilesDropped { paths } = e {
                out.extend(paths.iter().cloned());
            }
        }
        out
    }

    /// 托盘图标是否请求恢复窗口。
    pub fn tray_restore_requested(&self) -> bool {
        self.events.iter().any(|e| matches!(e, Event::TrayRestore))
    }

    // ---------------------------------------------------------------
    // 修饰键
    // ---------------------------------------------------------------

    /// 本帧是否有 Ctrl 被按住。
    ///
    /// 判定方式：扫描本帧所有带修饰键的事件，任一个带 `ctrl` 即可。
    ///
    /// 为什么不用"全局键盘状态"：那需要后端每帧查询键盘（`GetKeyState`），
    /// 而且在事件驱动模型下，修饰键的正确用法是"跟随产生它的那次点击"。
    /// 实践中用户在按下 Ctrl 之前通常已经移动过鼠标（产生 MouseMove 事件），
    /// 因此最近一次带 Ctrl 的事件能正确反映意图。
    pub fn ctrl_held(&self) -> bool {
        self.events.iter().any(|e| match e {
            Event::MouseDown { mods, .. }
            | Event::MouseUp { mods, .. }
            | Event::Wheel { mods, .. }
            | Event::KeyDown { mods, .. } => mods.ctrl,
            _ => false,
        })
    }

    /// 本帧是否有 Shift 被按住。
    pub fn shift_held(&self) -> bool {
        self.events.iter().any(|e| match e {
            Event::MouseDown { mods, .. }
            | Event::MouseUp { mods, .. }
            | Event::Wheel { mods, .. }
            | Event::KeyDown { mods, .. } => mods.shift,
            _ => false,
        })
    }

    // ---------------------------------------------------------------
    // 右键菜单
    // ---------------------------------------------------------------

    /// 在控件上弹出右键菜单。
    ///
    /// 若本帧在该控件上发生了右键按下，就在此处打开菜单。
    /// 菜单的定位由调用方决定（通常是鼠标位置）。
    pub fn open_context_menu(&mut self, rect: Rect, items: Vec<crate::menu::MenuItem>, width: i32) {
        let Some((x, y)) = self.menu.right_down() else {
            return;
        };
        if !rect.contains(x, y) {
            return;
        }
        // 菜单定位：鼠标位置；若会超出窗口右/下边界，则向左/上翻转。
        let (mut mx, mut my) = (x, y);
        let mh = items.len() as i32 * self.menu.item_height();
        if mx + width > self.size.w {
            mx = (mx - width).max(0);
        }
        if my + mh > self.size.h {
            my = (my - mh).max(0);
        }
        self.menu.open_at(mx, my, items, width);
    }

    /// 当前是否有右键菜单打开。
    pub fn menu_open(&self) -> bool {
        self.menu.is_open()
    }

    /// 本帧右键按下的位置（没有则为 `None`）。
    ///
    /// 业务代码用它判断"右键落在第几行"，再决定菜单内容与上下文。
    pub fn right_click_pos(&self) -> Option<(i32, i32)> {
        self.menu.right_down()
    }

    /// 取走**上一帧**的右键菜单点击结果（取完即清空）。
    ///
    /// 菜单命中测试在 `finish()` 里发生，结果只能留到下一帧消费。
    /// 业务代码应在 `render` 开头调用一次，配合自己保存的"当前菜单上下文"
    /// 决定这个下标对应哪个动作。
    pub fn take_menu_click(&self) -> Option<usize> {
        self.menu_action.take()
    }

    /// 关闭右键菜单。
    pub fn close_menu(&mut self) {
        self.menu.close();
    }

    // ---------------------------------------------------------------
    // 窗口动作（无边框窗口的自绘标题栏）
    // ---------------------------------------------------------------

    /// 请求一个窗口级动作（标题栏按钮用）。同帧多个请求以后者为准。
    pub fn request_window_action(&self, action: WindowAction) {
        self.window_action.set(Some(action));
    }

    /// 把矩形登记为**可拖动的标题栏**。
    ///
    /// 只消费尚未被任何控件消费的 `MouseDown`，所以标题栏上的按钮不会被误判成
    /// 拖动 —— 但前提是**先画完按钮再调用本方法**（按钮会通过 `detect_click`
    /// 认领落在自己身上的按下事件）。
    ///
    /// 拖动本身由后端用系统级 `WM_NCLBUTTONDOWN / HTCAPTION` 完成，内核只发意图。
    pub fn title_bar(&mut self, rect: Rect) {
        // 菜单是最高优先级浮层：它打开时点击顶栏应该用于关闭菜单，而不是拖窗口。
        if self.menu.is_open() {
            return;
        }
        self.sync_consumed();
        let mut hit = None;
        for i in 0..self.events.len() {
            if self.consumed[i] {
                continue;
            }
            if let Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } = &self.events[i]
            {
                if rect.contains(*x, *y) {
                    hit = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = hit {
            self.consumed[i] = true;
            self.window_action.set(Some(WindowAction::Drag));
        }
    }

    /// 绘制右键菜单（浮层，在所有控件之后）。
    fn draw_menu(&mut self) {
        let theme = self.theme.clone();

        // ---- 先做命中测试（左键点击菜单项）----
        //
        // 菜单是浮层，优先级高于底层控件：菜单打开时，点击应先给菜单。
        // 因此这里**主动消费**事件，底层控件本帧不会再收到它。
        self.sync_consumed();
        let mut mouse_down: Option<(i32, i32)> = None;
        for i in 0..self.events.len() {
            if self.consumed[i] {
                continue;
            }
            if let Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } = self.events[i]
            {
                mouse_down = Some((x, y));
                self.consumed[i] = true;
            }
        }
        if let Some((x, y)) = mouse_down {
            // 点击菜单项 → 记录并关闭；点击菜单外 → 关闭。
            if self.menu.hit_test(x, y).is_some() {
                // ★ 先取走点击结果，再关闭 —— `close()` 会清空 `clicked`。
                let picked = self.menu.clicked();
                if let Some(idx) = picked {
                    // finish() 之后 Ui 就被消耗了，结果必须写进跨帧共享槽。
                    self.menu_action.set(Some(idx));
                }
                self.menu_clicked = picked;
                self.menu.request_close();
            }
        }
        if self.menu.close_requested() {
            self.menu.close();
            return;
        }

        let rect = self.menu.rect();

        let radius = theme.style.radius();
        // 阴影（用偏移的暗色圆角矩形近似）
        self.fill_round(
            Rect::new(rect.x + 3, rect.y + 3, rect.w, rect.h),
            Color::argb(0x60, 0, 0, 0),
            radius,
        );
        // 背景 + 边框
        self.fill_round(rect, theme.card, radius);
        self.stroke_round(rect, theme.divider, radius, theme.border);

        // 逐项绘制
        let items: Vec<(String, bool, bool, bool)> = self
            .menu
            .items()
            .iter()
            .map(|i| (i.label.clone(), i.disabled, i.separator, i.checked))
            .collect();
        let pad = theme.pad_small;

        for (idx, (label, disabled, separator, checked)) in items.into_iter().enumerate() {
            let ir = self.menu.item_rect(idx);
            if separator {
                self.fill(
                    Rect::new(ir.x + pad, ir.y + ir.h / 2, (ir.w - pad * 2).max(0), 1),
                    theme.divider,
                );
                continue;
            }
            // hover 高亮：菜单项也要有反馈，且禁用项不响应。
            let hot = !disabled && self.mouse.is_some_and(|(mx, my)| ir.contains(mx, my));
            if hot {
                self.fill_round(ir.shrink(EdgeInsets::all(2)), theme.row_hover, 4);
            }
            // 勾选态与普通态的主文字颜色相同，只有禁用项变淡。
            let fg = if disabled {
                theme.text_faint
            } else {
                theme.text
            };
            // 勾选标记
            let text_x = ir.x + pad * 2;
            if checked {
                self.text_vcenter(
                    Rect::new(ir.x + pad, ir.y, pad, ir.h),
                    "✓",
                    theme.accent,
                    theme.font_body,
                    TextAlign::Center,
                    false,
                );
            }
            self.text_vcenter(
                Rect::new(text_x, ir.y, (ir.right() - text_x - pad).max(0), ir.h),
                label,
                fg,
                theme.font_body,
                TextAlign::Left,
                false,
            );
        }
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    pub fn backend(&self) -> &B {
        self.backend
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn window_rect(&self) -> Rect {
        Rect::new(0, 0, self.size.w, self.size.h)
    }

    /// 鼠标当前位置。
    pub fn mouse_pos(&self) -> Option<(i32, i32)> {
        self.mouse
    }

    // ---------------------------------------------------------------
    // 基础绘制（业务代码也能直接用，做自定义控件）
    // ---------------------------------------------------------------

    /// 直接向绘制列表追加填充矩形。
    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.draw.fill_rect(rect, color);
    }

    pub fn stroke(&mut self, rect: Rect, color: Color, thickness: i32) {
        self.draw.stroke_rect(rect, color, thickness);
    }

    /// 圆角填充。半径取自 [`Theme::style`]，业务代码一般不必传。
    pub fn fill_round(&mut self, rect: Rect, color: Color, radius: i32) {
        self.draw.fill_round_rect(rect, color, radius);
    }

    /// 圆角描边。
    pub fn stroke_round(&mut self, rect: Rect, color: Color, radius: i32, thickness: i32) {
        self.draw.stroke_round_rect(rect, color, radius, thickness);
    }

    /// 磨玻璃后处理：模糊该区域已绘制的内容，再叠 tint。
    pub fn blur(&mut self, rect: Rect, radius: i32, tint: Color) {
        self.draw.blur_region(rect, radius, tint);
    }

    /// 当前主题里的控件圆角半径。
    pub fn radius(&self) -> i32 {
        self.theme.style.radius()
    }

    pub fn text(&mut self, rect: Rect, text: impl Into<String>, color: Color, size_px: i32) {
        self.draw
            .text(rect, text, color, size_px, TextAlign::Left, false);
    }

    /// 带对齐与加粗的文字。
    pub fn text_aligned(
        &mut self,
        rect: Rect,
        text: impl Into<String>,
        color: Color,
        size_px: i32,
        align: TextAlign,
        bold: bool,
    ) {
        self.draw.text(rect, text, color, size_px, align, bold);
    }

    /// 把文字在矩形内**垂直居中**绘制（最常用的形态）。
    pub fn text_vcenter(
        &mut self,
        rect: Rect,
        text: impl Into<String>,
        color: Color,
        size_px: i32,
        align: TextAlign,
        bold: bool,
    ) {
        let lh = self.backend.line_height(size_px, bold);
        let y = rect.y + (rect.h - lh) / 2;
        self.draw.text(
            Rect::new(rect.x, y, rect.w, lh),
            text,
            color,
            size_px,
            align,
            bold,
        );
    }

    pub fn line(&mut self, rect: Rect, color: Color, horizontal: bool) {
        self.draw.line(rect, color, horizontal);
    }

    pub fn push_clip(&mut self, rect: Rect) {
        self.draw.push_clip(rect);
        self.clip_stack.push(rect);
    }

    pub fn pop_clip(&mut self) {
        if self.clip_stack.pop().is_some() {
            self.draw.pop_clip();
        }
    }

    /// 测量文字宽度。
    pub fn measure(&self, text: &str, size_px: i32, bold: bool) -> i32 {
        self.backend.text_width(text, size_px, bold)
    }

    // ---------------------------------------------------------------
    // ID / 事件分派
    // ---------------------------------------------------------------

    pub(crate) fn next_id(&mut self, label: &str) -> WidgetId {
        let parent = *self.parent_stack.last().unwrap_or(&WidgetId::NONE);
        let idx = self.index;
        self.index += 1;
        WidgetId::derive(parent, idx, label)
    }

    /// 把事件消费标记同步到 events 长度。
    fn sync_consumed(&mut self) {
        if self.consumed.len() < self.events.len() {
            self.consumed.resize(self.events.len(), false);
        }
    }

    /// 查询：本帧内该控件是否被点击、悬停、按下。
    ///
    /// # 语义（对齐 PCL 的 `MyButton` 范式）
    ///
    /// PCL 的做法：
    ///
    /// ```vb
    /// Private IsMouseDown As Boolean = False
    /// Private Sub Button_MouseDown(...) Handles Me.MouseLeftButtonDown
    ///     IsMouseDown = True
    /// End Sub
    /// Private Sub Button_MouseUp(...) Handles Me.MouseLeftButtonUp
    ///     If Not IsMouseDown Then Return
    ///     RaiseEvent Click(sender, e)
    /// End Sub
    /// Private Sub Button_MouseLeave(...) Handles Me.MouseLeave
    ///     If Not IsMouseDown Then Return
    ///     IsMouseDown = False   ' 拖出按钮 → 取消按下态
    /// End Sub
    /// ```
    ///
    /// 关键点：**按下与抬起必须在同一个控件上才算点击**。按下后把鼠标拖出按钮
    /// 再松开，不应该触发点击。
    ///
    /// 本实现用 [`PressState`] 记录每个控件的按下态，语义与 PCL 一致：
    ///
    /// - `MouseDown` 落在控件内 → 进入 `Pressed`；
    /// - `MouseUp` 落在**同一控件**内 → 触发点击，退出 `Pressed`；
    /// - `MouseUp` 落在别处 → **不触发点击**，退出 `Pressed`；
    /// - 鼠标移出控件 → 退出 `Pressed`（取消按下视觉）。
    ///
    /// # 为什么用 per-widget 状态而不是全局 `active`
    ///
    /// 全局"当前按下的控件"在多控件场景下有两个问题：
    /// 1. 帧间不持久 —— `Ui` 每帧重建，`active` 存在 `Ui` 里会丢；
    /// 2. 无法表达"按下后在控件 A 内移动到控件 B"这种真实行为。
    ///
    /// 按下态必须**跨帧持久**，因此放在调用方持有的 [`PressTracker`] 里。
    pub(crate) fn detect_click(&mut self, id: WidgetId, rect: Rect) -> (bool, bool, bool) {
        self.sync_consumed();
        let mut hovered = self.mouse.is_some_and(|(x, y)| rect.contains(x, y));
        let mut clicked = false;
        let mut pressed = false;

        for i in 0..self.events.len() {
            if self.consumed[i] {
                continue;
            }
            let ev = self.events[i].clone();
            match ev {
                Event::MouseDown {
                    x,
                    y,
                    button: MouseButton::Left,
                    ..
                } => {
                    if rect.contains(x, y) {
                        // 记录按下位置：后续帧即使没有鼠标事件，
                        // 也能知道鼠标最后在哪里（用于判断"是否已移出控件"）。
                        self.press.begin_at(id, x, y);
                        pressed = true;
                        hovered = true;
                        self.consumed[i] = true;
                    }
                }
                Event::MouseUp {
                    x,
                    y,
                    button: MouseButton::Left,
                    ..
                } => {
                    // ★ 抬起事件只归"当初按下的那个控件"。
                    //
                    // 旧实现无条件 `consumed[i] = true`，于是**第一个**调用
                    // detect_click 的控件（顶栏按钮）会吞掉所有 MouseUp，
                    // 排在其后的控件永远收不到抬起 → 整个界面点不动。
                    // 而且对非目标控件调用 `press.end(id)` 虽返回 false，
                    // 事件却已被吞，真正的目标就没机会清掉按下态了。
                    //
                    // 先用只读的 `is_pressed` 认领，再 `end` 收尾：抬起落在
                    // 别处时也算"这次按下结束"，但不触发点击（拖出取消）。
                    if self.press.is_pressed(id) {
                        let inside = rect.contains(x, y);
                        self.press.end(id);
                        clicked = inside;
                        self.consumed[i] = true;
                    }
                }
                Event::MouseLeave => {
                    // 鼠标离开窗口：清掉所有按下态（PCL 的 MouseLeave 处理）。
                    self.press.clear_all();
                }
                _ => {}
            }
        }

        // 鼠标移出控件 → 取消该控件的按下态（视觉上不再显示"按下"）。
        //
        // ★ 位置来源优先用**本帧**鼠标位置；本帧无鼠标事件时退回到"按下时的位置"。
        // 这样"按住不放"（中间帧没有鼠标事件）不会误清按下态，
        // 而真正的"移出"（有 MouseMove 到别处）仍会取消。
        let mouse_pos = self.mouse.or_else(|| self.press.press_origin());
        if let Some((mx, my)) = mouse_pos {
            if !rect.contains(mx, my) {
                self.press.cancel(id);
            }
        }
        if self.press.is_pressed(id) {
            pressed = true;
        }

        (hovered, clicked, pressed)
    }

    /// 组装一个 `Response`（当前控件内联构造；保留此方法供自定义控件复用）。
    #[allow(dead_code)]
    fn response_for(&mut self, id: WidgetId, rect: Rect) -> Response {
        let (hovered, clicked, pressed) = self.detect_click(id, rect);
        Response {
            id,
            rect,
            hovered,
            clicked,
            pressed,
            clicked_this_frame: clicked,
        }
    }

    // ---------------------------------------------------------------
    // 布局辅助：垂直/水平流
    // ---------------------------------------------------------------

    /// 在指定区域内开启一个垂直流容器，返回其内部矩形。
    ///
    /// 调用方负责在结束时 [`Self::end_container`]。
    pub fn begin_vertical(&mut self, area: Rect, pad: EdgeInsets) -> Rect {
        let inner = area.shrink(pad);
        self.cursor.push((inner.x, inner.y));
        self.parent_stack.push(WidgetId::derive(
            *self.parent_stack.last().unwrap_or(&WidgetId::NONE),
            self.index,
            "v",
        ));
        self.index += 1;
        self.clip_stack.push(inner);
        self.draw.push_clip(inner);
        inner
    }

    pub fn end_container(&mut self) {
        self.cursor.pop();
        self.parent_stack.pop();
        if self.clip_stack.pop().is_some() {
            self.draw.pop_clip();
        }
    }

    /// 在当前垂直流里取下一块区域（高度固定）。
    /// 在垂直流里取下一块固定高度区域（供自定义控件使用）。
    #[allow(dead_code)]
    fn vtake(&mut self, h: i32) -> Rect {
        let (x, y) = *self.cursor.last().unwrap_or(&(0, 0));
        // 可用宽度取当前裁剪区宽度
        let w = self.clip_stack.last().map(|c| c.w).unwrap_or(self.size.w);
        let r = Rect::new(x, y, w, h);
        if let Some(c) = self.cursor.last_mut() {
            c.1 += h;
        }
        r
    }

    /// 手动推进光标（用于自定义间距）。
    pub fn advance(&mut self, dy: i32) {
        if let Some(c) = self.cursor.last_mut() {
            c.1 += dy;
        }
    }

    pub fn cursor_y(&self) -> i32 {
        self.cursor.last().map(|c| c.1).unwrap_or(0)
    }

    /// 垂直流里剩余的高度是否可以放下 `h`。
    pub fn has_room(&self, h: i32) -> bool {
        match self.clip_stack.last() {
            Some(c) => self.cursor_y() < c.bottom() - h,
            None => true,
        }
    }

    // ---------------------------------------------------------------
    // 焦点与文本输入
    // ---------------------------------------------------------------

    pub fn has_focus(&self, id: WidgetId) -> bool {
        self.focus.get() == Some(id)
    }

    pub fn request_focus(&mut self, id: WidgetId) {
        self.focus.set(Some(id));
    }

    pub fn clear_focus(&mut self) {
        self.focus.set(None);
    }

    /// 取出本帧输入到某个输入框的编辑动作。
    pub fn take_text_events(&mut self, id: WidgetId) -> Vec<TextEdit> {
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for (wid, edit) in self.text_events.drain(..) {
            if wid == id {
                out.push(edit);
            } else {
                keep.push((wid, edit));
            }
        }
        self.text_events = keep;
        out
    }

    /// 采集键盘/文本事件到某个已聚焦的输入框。
    pub(crate) fn collect_text_for(&mut self, id: WidgetId) {
        self.sync_consumed();
        for i in 0..self.events.len() {
            if self.consumed[i] {
                continue;
            }
            match self.events[i].clone() {
                Event::Text { text } => {
                    self.text_events.push((id, TextEdit::Insert(text)));
                    self.consumed[i] = true;
                }
                Event::KeyDown {
                    key: Key::Backspace,
                    ..
                } => {
                    self.text_events.push((id, TextEdit::Backspace));
                    self.consumed[i] = true;
                }
                Event::KeyDown {
                    key: Key::Delete, ..
                } => {
                    self.text_events.push((id, TextEdit::Delete));
                    self.consumed[i] = true;
                }
                _ => {}
            }
        }
    }

    // ---------------------------------------------------------------
    // 滚动
    // ---------------------------------------------------------------

    /// 某滚动区域当前的偏移（逻辑像素）。
    pub fn scroll_offset(&self, id: WidgetId) -> i32 {
        self.scroll.get(&id).copied().unwrap_or(0)
    }

    /// 处理滚轮并返回**夹到合法范围**后的偏移。
    ///
    /// `content_h` 是内容总高，`rect` 是可视区域。页面用返回值把内容上移，
    /// 并配合 [`Self::push_clip`] 裁掉溢出部分。
    pub fn scroll_area(&mut self, id: WidgetId, rect: Rect, content_h: i32) -> i32 {
        self.apply_wheel(id, rect, content_h);
        self.scroll_offset(id)
    }

    fn apply_wheel(&mut self, id: WidgetId, rect: Rect, content_h: i32) {
        let max_scroll = (content_h - rect.h).max(0);
        let cur = self.scroll_offset(id);
        let mut delta = 0;
        self.sync_consumed();
        for i in 0..self.events.len() {
            if self.consumed[i] {
                continue;
            }
            if let Event::Wheel { x, y, delta: d, .. } = self.events[i].clone() {
                if rect.contains(x, y) {
                    // 后端把 WHEEL_DELTA(120) 归一化成 ±1，一个 notch 约等于一行。
                    // 直接用 ±1 会让滚轮几乎没有反应。
                    const WHEEL_STEP_PX: i32 = 48;
                    delta += -d * WHEEL_STEP_PX; // 向下滚 = 增大偏移
                    self.consumed[i] = true;
                }
            }
        }
        if delta != 0 {
            let next = (cur + delta).clamp(0, max_scroll);
            self.scroll.insert(id, next);
        } else {
            // 内容缩短时也要把偏移收进范围
            let clamped = cur.clamp(0, max_scroll);
            if clamped != cur {
                self.scroll.insert(id, clamped);
            }
        }
    }
}
