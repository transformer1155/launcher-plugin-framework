//! 沙箱执行单元的状态机。
//!
//! 状态集合严格等于规格给出的六个：
//! `Running | Crashed | TimedOut | Restarting | Failed | Stopped`。
//!
//! 我没有额外加 `Created` / `Uninitialized` 之类的中间态：`create_unit` 成功返回的
//! 那一刻单元就已经是 `Running`（首次启动等价于一次"重启成功"的落点），
//! 失败则直接不返回句柄。这样状态的取值域和规格完全一致，UI 不需要为额外状态写分支。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 单元状态。`repr(u8)` 以便放进 `AtomicU8`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[repr(u8)]
pub enum UnitState {
    /// 接受新任务。
    Running = 0,
    /// 沙箱内 handler panic，或超时判定为不可中断而被强制崩溃。
    /// **可以被重启管理器恢复。**
    Crashed = 1,
    /// 空闲回收（idle_timeout_ms）或达到 max_lifetime_ms 被回收。
    /// 不是故障，是正常生命周期终点；下次调用前需要重新拉起（走 Restarting）。
    TimedOut = 2,
    /// 重启管理器正在重建这个单元。此状态下 `call_tool` **立即**返回
    /// `ProcessRestarting`，**不排队**（规格硬要求）。
    Restarting = 3,
    /// 重启失败（例如沙箱初始化失败）。**不再自动重试**，只能手动 `restart_unit`。
    Failed = 4,
    /// 被显式 `destroy`。终态。
    Stopped = 5,
}

impl UnitState {
    pub const ALL: [UnitState; 6] = [
        Self::Running,
        Self::Crashed,
        Self::TimedOut,
        Self::Restarting,
        Self::Failed,
        Self::Stopped,
    ];

    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Running,
            1 => Self::Crashed,
            2 => Self::TimedOut,
            3 => Self::Restarting,
            4 => Self::Failed,
            5 => Self::Stopped,
            _ => return None,
        })
    }

    /// **该状态是否接受新任务。**
    ///
    /// `call_tool` 的第 3 步（执行单元状态可用）直接用它，避免在注册表里散落 `match`。
    pub const fn accepts_tasks(self) -> bool {
        matches!(self, Self::Running)
    }

    /// 是否属于"可以被重启管理器自动恢复"的状态。
    ///
    /// `Failed` 故意**不在**其中（规格：重启失败后不再自动重试）；
    /// `Stopped` 也不在（用户/平台主动停的，不该偷偷拉起来）。
    pub const fn is_auto_restartable(self) -> bool {
        matches!(self, Self::Crashed | Self::TimedOut | Self::Restarting)
    }

    /// 是否是终态（不会再自行迁移）。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// 对外展示用的字符串（与 serde 的 PascalCase 一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Crashed => "Crashed",
            Self::TimedOut => "TimedOut",
            Self::Restarting => "Restarting",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
}

impl fmt::Display for UnitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 状态迁移表：`(from, to) -> 是否允许`。
///
/// 集中定义而不是在业务代码里散落 `if`，好处是：任何状态写入都过这一关，
/// 非法迁移会变成断言失败而不是"悄悄进入一个没人预期的状态"。
///
/// ```text
///                      ┌──────────── destroy ────────────┐
///                      │                                ▼
///   create ──►  Running ── handler panic ──► Crashed ──► Restarting ──► Running
///                 │                          │              │
///                 ├── idle/lifetime ──► TimedOut            └── 初始化失败 ──► Failed
///                 │                        │                                    │
///                 ├── restart 开始 ──► Restarting ◄────────────── manual ───────┘
///                 │                        │
///                 └────────────► Stopped ◄─┴──────────────► Stopped
/// ```
pub const fn is_transition_allowed(from: UnitState, to: UnitState) -> bool {
    use UnitState::*;
    match (from, to) {
        // 同态（幂等写入，允许）
        (Running, Running)
        | (Crashed, Crashed)
        | (TimedOut, TimedOut)
        | (Restarting, Restarting)
        | (Failed, Failed) => true,

        // 运行中出事
        (Running, Crashed) => true,
        (Running, TimedOut) => true,
        (Running, Restarting) => true,
        (Running, Failed) => true, // 例如 submit 期间发现无法初始化
        (Running, Stopped) => true,

        // 崩溃 / 回收后进入重启
        (Crashed, Restarting) => true,
        (Crashed, Stopped) => true,
        (Crashed, Failed) => true,

        (TimedOut, Restarting) => true,
        (TimedOut, Stopped) => true,
        (TimedOut, Failed) => true,

        // 重启结果
        (Restarting, Running) => true,
        (Restarting, Failed) => true,
        (Restarting, Stopped) => true,
        (Restarting, Crashed) => true, // 重启过程中再次崩溃（例如新单元初始化即 panic）

        // Failed 只能被别人推走：手动重启 / destroy
        (Failed, Restarting) => true,
        (Failed, Stopped) => true,

        // Stopped 是终态
        (Stopped, _) => false,

        // 其它组合一律非法
        _ => false,
    }
}

/// 非法迁移。属于**框架自身 bug**，不是插件的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: UnitState,
    pub to: UnitState,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "非法的单元状态迁移：{} -> {}", self.from, self.to)
    }
}

impl std::error::Error for IllegalTransition {}

/// 崩溃原因。用于 `on_crash` 回调与重启管理器决策。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CrashReason {
    /// 沙箱 handler 内部 panic（已被 `catch_unwind` 兜住，宿主存活）。
    HandlerPanic { payload: String },
    /// 工具级超时到期且 handler 不可中断 → 升级为单元级崩溃。
    UninterruptibleTimeout { timeout_ms: u64 },
    /// 单元在创建/重启阶段初始化失败。
    InitFailed { detail: String },
}

impl fmt::Display for CrashReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HandlerPanic { payload } => write!(f, "沙箱 handler panic：{payload}"),
            Self::UninterruptibleTimeout { timeout_ms } => {
                write!(f, "超时 {timeout_ms}ms 且 handler 不可中断，已强制崩溃")
            }
            Self::InitFailed { detail } => write!(f, "沙箱初始化失败：{detail}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_u8() {
        for s in UnitState::ALL {
            assert_eq!(UnitState::from_u8(s.as_u8()), Some(s));
        }
        assert_eq!(UnitState::from_u8(99), None);
    }

    #[test]
    fn only_running_accepts_tasks() {
        for s in UnitState::ALL {
            assert_eq!(s.accepts_tasks(), s == UnitState::Running, "{s}");
        }
    }

    #[test]
    fn failed_is_not_auto_restartable() {
        assert!(!UnitState::Failed.is_auto_restartable());
        assert!(UnitState::Crashed.is_auto_restartable());
        assert!(!UnitState::Stopped.is_auto_restartable());
        assert!(!UnitState::Running.is_auto_restartable());
    }

    #[test]
    fn stopped_is_terminal() {
        assert!(UnitState::Stopped.is_terminal());
        for to in UnitState::ALL {
            assert!(!is_transition_allowed(UnitState::Stopped, to));
        }
    }

    #[test]
    fn main_paths_allowed() {
        use UnitState::*;
        let ok = [
            (Running, Crashed),
            (Crashed, Restarting),
            (Restarting, Running),
            (Restarting, Failed),
            (Failed, Restarting),
            (Running, TimedOut),
            (TimedOut, Restarting),
            (Running, Stopped),
            (Failed, Stopped),
        ];
        for (f, t) in ok {
            assert!(is_transition_allowed(f, t), "{f}->{t} 应当允许");
        }
    }

    #[test]
    fn illegal_paths_rejected() {
        use UnitState::*;
        // 不能从 Crashed 直接跳回 Running（必须经过 Restarting）
        assert!(!is_transition_allowed(Crashed, Running));
        // 不能从 Failed 直接跳回 Running
        assert!(!is_transition_allowed(Failed, Running));
        // 不能从 TimedOut 直接跳回 Running
        assert!(!is_transition_allowed(TimedOut, Running));
        // Failed 不能自己变成 Crashed
        assert!(!is_transition_allowed(Failed, Crashed));
    }

    #[test]
    fn full_recovery_cycle_is_walkable() {
        // Running -> Crashed -> Restarting -> Running 是重启管理的核心闭环
        let path = [
            UnitState::Running,
            UnitState::Crashed,
            UnitState::Restarting,
            UnitState::Running,
        ];
        for w in path.windows(2) {
            assert!(is_transition_allowed(w[0], w[1]));
        }
    }
}
