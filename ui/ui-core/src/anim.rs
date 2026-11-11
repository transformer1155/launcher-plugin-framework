//! 动画状态：**跨帧**的数值插值。
//!
//! # 为什么需要它
//!
//! 即时模式 UI 每帧重建控件描述。要做"颜色渐变""按下缩放""光标闪烁"这类效果，
//! 就必须有**跨帧存活的状态** —— 当前颜色是多少、当前缩放是多少。
//!
//! 这与 [`crate::press::PressTracker`] 是同一类问题：交互/动画状态不能放在
//! 每帧重建的 `Ui` 里，必须由调用方持有。
//!
//! # 对标 PCL 的 `AniStart` / `AniStop`
//!
//! PCL 的做法：
//!
//! ```vb
//! AniStart({AaColor(PanFore, Border.BorderBrushProperty, "ColorBrush3", AnimationColorIn)},
//!          "MyButton Color " & Uuid)
//! AniStop("MyButton Color " & Uuid)
//! ```
//!
//! 三个要点：
//!
//! 1. **动画有名字**（这里用 `WidgetId` + 槽位名），同名动画会**覆盖**旧的 ——
//!    快速连点时不会出现两段动画打架。
//! 2. `AniStop` 能按名字**打断**动画。
//! 3. 动画有**时长**，到期后落到目标值。
//!
//! 本实现把这三点映射为：
//!
//! | PCL | 本实现 |
//! |---|---|
//! | 动画名 `"MyButton Color " & Uuid` | `(WidgetId, Slot)` 二元组 |
//! | `AniStart` 同名覆盖 | [`AnimationState::start`] 同槽位覆盖 |
//! | `AniStop` | [`AnimationState::stop`] |
//! | 缓动函数 `AniEaseOutFluent` | [`Easing`] 枚举 |
//!
//! # 缓动函数
//!
//! PCL 用 `AniEaseOutFluent` / `AniEasePower.ExtraStrong` 等。
//! 这里实现三种够用的（并说明差异）：
//!
//! - [`Easing::Linear`]：匀速。
//! - [`Easing::OutQuad`]：减速（快起慢停）。PCL 的 `AniEaseOutFluent` 接近这个。
//! - [`Easing::OutBack`]：带回弹（末端轻微过冲）。PCL 按压缩放的手感接近这个。

use std::collections::HashMap;

use crate::id::WidgetId;

/// 动画槽位。同一控件的不同属性用不同槽位，互不干扰。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// 背景色过渡。
    Background,
    /// 前景/文字色过渡。
    Foreground,
    /// 缩放（1.0 = 原始大小）。
    Scale,
    /// 边框色过渡。
    Border,
    /// 光标闪烁（0..1 的透明度）。
    Caret,
    /// 颜色过渡的 R 通道。
    ColorR,
    /// 颜色过渡的 G 通道。
    ColorG,
    /// 颜色过渡的 B 通道。
    ColorB,
}

/// 颜色过渡用的三个通道槽位。
///
/// # 为什么颜色要拆成三个槽位
///
/// 颜色有 R/G/B 三个通道。若把颜色打包成一个浮点数再插值，会出现**通道串位**：
/// R 减小会让 G 突然变大（因为打包值的高位是 R）。
///
/// 实测：普通灰 `#33333A` 过渡到 hover 灰时，中间态是 `#34E576` —— 明显偏绿，
/// 就是串位造成的。
///
/// 拆成三个独立槽位后，每个通道独立插值，**逐通道精确**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorChannel {
    Red,
    Green,
    Blue,
}

/// 缓动函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    OutQuad,
    OutBack,
}

impl Easing {
    /// 把进度 `t`（0..=1）映射为缓动后的进度。
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::OutBack => {
                // 回弹：末端轻微过冲后回落。c1=1.70158 是常用常数。
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                let u = t - 1.0;
                1.0 + C3 * u * u * u + C1 * u * u
            }
        }
    }
}

/// 一段正在进行的动画。
#[derive(Debug, Clone)]
struct Track {
    from: f32,
    to: f32,
    /// 已过毫秒。
    elapsed: f32,
    duration: f32,
    easing: Easing,
}

/// 动画状态容器。由调用方跨帧持有。
#[derive(Debug, Default)]
pub struct AnimationState {
    tracks: HashMap<(WidgetId, Slot), Track>,
    /// 当前动画值。
    values: HashMap<(WidgetId, Slot), f32>,
    /// **上一次的目标值**。
    ///
    /// ★ 这与 `values` 必须是两个独立的槽位。
    ///
    /// `values` 存"当前画面上的值"（可能是过渡中间态），
    /// `targets` 存"上一次请求的目标"。
    ///
    /// 判断"目标是否变了"必须对比 `targets`：
    /// 若拿 `values`（中间态）去比，每一帧都会认为"目标变了"，
    /// 于是每帧重启动画 —— 过渡永远走不到终点（实测表现为颜色闪一下就弹回）。
    targets: HashMap<(WidgetId, Slot), f32>,
}

impl AnimationState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动（或覆盖）一段动画。
    ///
    /// 同槽位的新动画会**覆盖**旧的（对应 PCL 的 `AniStart` 同名覆盖语义），
    /// 因此快速连续触发不会打架。
    pub fn start(
        &mut self,
        id: WidgetId,
        slot: Slot,
        from: f32,
        to: f32,
        duration_ms: f32,
        easing: Easing,
    ) {
        // 记录目标（判断"目标是否变了"靠它）。
        self.targets.insert((id, slot), to);
        if duration_ms <= 0.0 {
            // 时长为 0：直接落到目标值。
            self.tracks.remove(&(id, slot));
            self.values.insert((id, slot), to);
            return;
        }
        self.tracks.insert(
            (id, slot),
            Track {
                from,
                to,
                elapsed: 0.0,
                duration: duration_ms,
                easing,
            },
        );
    }

    /// 上一次请求的目标值（`None` 表示从未设置过）。
    pub fn target(&self, id: WidgetId, slot: Slot) -> Option<f32> {
        self.targets.get(&(id, slot)).copied()
    }

    /// 打断某槽位的动画（对应 PCL 的 `AniStop`）。
    ///
    /// 打断后该槽位**冻结在当前值**，不会被拉回目标值 —— 这正是"停止动画"的意思。
    pub fn stop(&mut self, id: WidgetId, slot: Slot) {
        self.tracks.remove(&(id, slot));
    }

    /// 停止某控件的所有动画。
    pub fn stop_all(&mut self, id: WidgetId) {
        self.tracks.retain(|(i, _), _| *i != id);
    }

    /// 推进时间并返回该槽位当前的动画值。
    ///
    /// `dt_ms` 是距上一帧的毫秒数。
    ///
    /// 若该槽位没有动画，返回**上一次的值**（若从未有过则返回 `default`）。
    pub fn value(
        &mut self,
        id: WidgetId,
        slot: Slot,
        dt_ms: f32,
        default: f32,
    ) -> f32 {
        let key = (id, slot);
        // 先推进并算出值与目标值，再决定是否结束动画（避免同时持有两个可变借用）。
        let (v, finished, target) = match self.tracks.get_mut(&key) {
            Some(track) => {
                track.elapsed += dt_ms;
                let t = (track.elapsed / track.duration).clamp(0.0, 1.0);
                let eased = track.easing.apply(t);
                let v = track.from + (track.to - track.from) * eased;
                (v, t >= 1.0, track.to)
            }
            None => (self.values.get(&key).copied().unwrap_or(default), false, 0.0),
        };
        if finished {
            // 动画结束：落到目标值并移除轨道。
            self.tracks.remove(&key);
            self.values.insert(key, target);
        } else {
            self.values.insert(key, v);
        }
        v
    }

    /// 只读当前值（不推进时间）。
    ///
    /// # 首次读取会"落盘" default
    ///
    /// 若该槽位从未有过值，返回 `default` **并把它记下来**。这样下一次读取就能
    /// 拿到上次的值 —— 动画系统据此判断"目标是否变了"。
    ///
    /// 这解决了即时模式的一个固有问题：控件没有持久实例，"上一次的值"必须
    /// 由动画状态代为保管。
    pub fn peek(&mut self, id: WidgetId, slot: Slot, default: f32) -> f32 {
        let key = (id, slot);
        match self.values.get(&key).copied() {
            Some(v) => v,
            None => {
                self.values.insert(key, default);
                default
            }
        }
    }

    /// 该槽位当前是否正在动画。
    pub fn is_animating(&self, id: WidgetId, slot: Slot) -> bool {
        self.tracks.contains_key(&(id, slot))
    }

    /// 是否有任何动画在进行（后端可用它决定是否需要连续重绘）。
    pub fn any_animating(&self) -> bool {
        !self.tracks.is_empty()
    }

    /// 清理某控件的全部状态（控件销毁时调用，避免泄漏）。
    pub fn forget(&mut self, id: WidgetId) {
        self.tracks.retain(|(i, _), _| *i != id);
        self.values.retain(|(i, _), _| *i != id);
    }

    // -----------------------------------------------------------------
    // 颜色专用：三通道独立插值
    // -----------------------------------------------------------------

    /// 颜色的单个通道的动画状态。
    fn color_key(id: WidgetId, ch: ColorChannel) -> (WidgetId, Slot) {
        // 用 Slot 的不同变体承载三个通道，与通用动画互不干扰。
        let slot = match ch {
            ColorChannel::Red => Slot::ColorR,
            ColorChannel::Green => Slot::ColorG,
            ColorChannel::Blue => Slot::ColorB,
        };
        (id, slot)
    }

    /// 读取颜色的某通道当前值（0..255），并推进时间。
    ///
    /// 首次读取时用 `default` 落盘，作为后续过渡的起点。
    pub fn color_value(
        &mut self,
        id: WidgetId,
        ch: ColorChannel,
        dt_ms: f32,
        default: f32,
    ) -> f32 {
        let key = Self::color_key(id, ch);
        self.value(id, key.1, dt_ms, default)
    }

    /// 启动颜色的某通道过渡。
    pub fn start_color(
        &mut self,
        id: WidgetId,
        ch: ColorChannel,
        from: f32,
        to: f32,
        duration_ms: f32,
        easing: Easing,
    ) {
        let key = Self::color_key(id, ch);
        self.start(id, key.1, from, to, duration_ms, easing);
    }

    /// 只读颜色的某通道值（不推进时间）；首次会落盘 `default`。
    pub fn peek_color(&mut self, id: WidgetId, ch: ColorChannel, default: f32) -> f32 {
        let key = Self::color_key(id, ch);
        self.peek(id, key.1, default)
    }

    /// 颜色的某通道上一次的目标值。
    pub fn target_color(&self, id: WidgetId, ch: ColorChannel) -> Option<f32> {
        let key = Self::color_key(id, ch);
        self.target(id, key.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> WidgetId {
        WidgetId(n)
    }

    #[test]
    fn value_without_animation_returns_default() {
        let mut a = AnimationState::new();
        assert_eq!(a.value(id(1), Slot::Scale, 16.0, 1.0), 1.0);
    }

    #[test]
    fn animation_interpolates_to_target() {
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Scale, 1.0, 0.955, 100.0, Easing::Linear);
        // 过一半
        let v = a.value(id(1), Slot::Scale, 50.0, 1.0);
        assert!((v - 0.9775).abs() < 0.001, "线性插值应在中点，实际 {v}");
        // 走完
        let v2 = a.value(id(1), Slot::Scale, 50.0, 1.0);
        assert!((v2 - 0.955).abs() < 0.0001, "应到达目标值，实际 {v2}");
        assert!(!a.is_animating(id(1), Slot::Scale), "结束后不应再有轨道");
    }

    #[test]
    fn same_slot_new_animation_overwrites_old() {
        // PCL 的 AniStart 同名覆盖语义
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Background, 0.0, 1.0, 1000.0, Easing::Linear);
        a.start(id(1), Slot::Background, 0.5, 0.0, 100.0, Easing::Linear);
        // 应当从新的起点 0.5 出发，而不是旧的 0.0
        let v = a.value(id(1), Slot::Background, 0.0, 0.0);
        assert!((v - 0.5).abs() < 0.001, "应从新起点开始，实际 {v}");
    }

    #[test]
    fn different_slots_are_independent() {
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Background, 0.0, 1.0, 100.0, Easing::Linear);
        a.start(id(1), Slot::Scale, 1.0, 0.5, 100.0, Easing::Linear);
        let bg = a.value(id(1), Slot::Background, 50.0, 0.0);
        let sc = a.value(id(1), Slot::Scale, 50.0, 1.0);
        assert!((bg - 0.5).abs() < 0.001);
        assert!((sc - 0.75).abs() < 0.001);
    }

    #[test]
    fn stop_freezes_at_current_value() {
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Scale, 1.0, 0.0, 1000.0, Easing::Linear);
        let v = a.value(id(1), Slot::Scale, 100.0, 1.0);
        a.stop(id(1), Slot::Scale);
        let after = a.value(id(1), Slot::Scale, 1000.0, 1.0);
        assert!((after - v).abs() < 0.001, "打断后应冻结在当前值，实际 {after} vs {v}");
    }

    #[test]
    fn zero_duration_jumps_to_target() {
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Scale, 1.0, 0.5, 0.0, Easing::Linear);
        assert!((a.value(id(1), Slot::Scale, 0.0, 1.0) - 0.5).abs() < 0.0001);
    }

    #[test]
    fn easing_shapes() {
        assert_eq!(Easing::Linear.apply(0.5), 0.5);
        // OutQuad：中点应 > 0.5（减速）
        assert!(Easing::OutQuad.apply(0.5) > 0.5);
        // OutBack：末端轻微过冲
        let peak = Easing::OutBack.apply(0.8);
        assert!(peak > 1.0, "OutBack 在末端应过冲，实际 {peak}");
        assert!((Easing::OutBack.apply(1.0) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn easing_clamps_out_of_range() {
        assert_eq!(Easing::Linear.apply(-1.0), 0.0);
        assert_eq!(Easing::Linear.apply(2.0), 1.0);
    }

    #[test]
    fn forget_removes_all_state_for_widget() {
        let mut a = AnimationState::new();
        a.start(id(1), Slot::Scale, 1.0, 0.5, 100.0, Easing::Linear);
        a.start(id(2), Slot::Scale, 1.0, 0.5, 100.0, Easing::Linear);
        a.forget(id(1));
        assert!(!a.is_animating(id(1), Slot::Scale));
        assert!(a.is_animating(id(2), Slot::Scale), "别的控件不受影响");
    }

    #[test]
    fn any_animating_reflects_activity() {
        let mut a = AnimationState::new();
        assert!(!a.any_animating());
        a.start(id(1), Slot::Caret, 0.0, 1.0, 1000.0, Easing::Linear);
        assert!(a.any_animating());
        a.value(id(1), Slot::Caret, 2000.0, 0.0);
        assert!(!a.any_animating(), "动画结束后不应再有活动");
    }
}
