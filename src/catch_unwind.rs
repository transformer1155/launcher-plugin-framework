//! 异步 panic 捕获：把 `catch_unwind` 包在 `Future::poll` 外面。
//!
//! # 规格要求
//!
//! > 可信插件直接跑在宿主进程主执行上下文里。注册表调用其工具 handler 时直接调用，
//! > 无调度器介入。调用边界必须做异常捕获（Rust 用 `catch_unwind` 包住 Future poll，
//! > 或按实际异步运行时能力实现）。
//!
//! # 这个实现做了什么
//!
//! [`CatchUnwind`] 是一个 `Future` 适配器：它持有内层 future 和一段 `AssertUnwindSafe`，
//! 在**每一次 `poll`** 外面包 `std::panic::catch_unwind`。因此 panic 无论发生在
//! 1) 构造 future 的同步阶段，2) 第一次 poll，还是 3) 第 N 次 poll，都能被捕获。
//!
//! # 它做不到什么（必须明说）
//!
//! 1. **`panic = "abort"` 下无效。** `catch_unwind` 依赖 unwind 表。本 crate 的 `Cargo.toml`
//!    不设置 `panic = "abort"`，且文档声明宿主必须用默认 `panic = "unwind"`。若宿主用
//!    abort profile，任何 panic 都会直接杀死进程 —— 此时禁用可信插件机制才是正确选择。
//! 2. **无法捕获 `abort()` / 段错误 / OOM / `std::process::exit()`。**
//!    `catch_unwind` 只处理 Rust 的 unwind。这正是"可信插件与宿主同等权限"的物理体现：
//!    签名校验是唯一防线。见 `docs/security-boundary.md`。
//! 3. **`UnwindSafe` 是人工承诺，不是编译器保证。** 被捕获的 panic 之后，future 内部
//!    可能留下不一致状态（例如 `Mutex` 被毒化、`Vec` 长度与容量错位）。框架的处理是：
//!    panic 后**丢弃**整个 future 与 handler 调用上下文，不重试、不观察其内部状态。
//!    `AssertUnwindSafe` 在这里是安全可用的，因为我们对"事后状态"的唯一操作是 drop。
//! 4. **panic hook 会先执行。** `catch_unwind` 不阻止 `panic::set_hook` 的输出，
//!    宿主应当用 hook 做日志归因（见 [`install_panic_hook`]）。
//! 5. **不捕获 `extern "C"` 边界传来的 panic**（Rust 里这本身就是 UB，无法可靠捕获）。

use std::any::Any;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};

/// 捕获到的 panic 载荷摘要。用于填进 `ToolError::panic_payload`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanicPayload {
    /// `&str` / `String` 载荷原文；非字符串载荷给类型名。
    pub text: String,
}

impl PanicPayload {
    pub fn from_any(payload: &(dyn Any + Send)) -> Self {
        let text = if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_owned()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_owned()
        };
        Self { text }
    }
}

/// 把 handler 求值 + 每次 poll 都包进 `catch_unwind` 的适配器。
///
/// 用法：`CatchUnwind::new(fut)`，然后像普通 future 一样 `.await`。
pub struct CatchUnwind<F> {
    inner: Pin<Box<AssertUnwindSafe<F>>>,
    /// 一旦捕获到 panic 就置 true，后续 poll 直接返回 `None`（future 永不完成）。
    poisoned: bool,
}

impl<F: Future> CatchUnwind<F> {
    pub fn new(fut: F) -> Self {
        Self {
            inner: Box::pin(AssertUnwindSafe(fut)),
            poisoned: false,
        }
    }
}

impl<F> Future for CatchUnwind<F>
where
    F: Future<Output = crate::types::ToolResult>,
{
    /// `Ok(Ok(result))` → 正常；`Ok(Err(panic))` → 捕获到 panic。
    type Output = Result<crate::types::ToolResult, PanicPayload>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // 安全性：我们从不移动 `inner` 之外的字段，且 `inner` 已是 Pin<Box>。
        let this = self.get_mut();
        if this.poisoned {
            // 已被 panic 毒化：不再 poll。返回 Pending 会让调用方永远挂住，
            // 所以更安全的做法是返回一个"永不完成"的 pending —— 但调用方
            // 已经在 `catch_unwind_call` 里拿到 Err 并直接返回了，不会二次 poll。
            // 这里保留 Pending 只为满足类型系统。
            return Poll::Pending;
        }

        let poll_result = catch_unwind(AssertUnwindSafe(|| this.inner.as_mut().poll(cx)));

        match poll_result {
            Ok(Poll::Ready(r)) => Poll::Ready(Ok(r)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => {
                this.poisoned = true;
                let p = match payload.downcast_ref::<&'static str>() {
                    Some(s) => PanicPayload {
                        text: (*s).to_owned(),
                    },
                    None => match payload.downcast_ref::<String>() {
                        Some(s) => PanicPayload { text: s.clone() },
                        None => PanicPayload {
                            text: "<non-string panic payload>".to_owned(),
                        },
                    },
                };
                Poll::Ready(Err(p))
            }
        }
    }
}

/// 一次完整的"受保护的 handler 调用"。
///
/// 覆盖 **构造 future** 与 **驱动 future** 两个阶段。可信插件路径必须走这个函数。
pub async fn catch_unwind_call<F>(
    plugin_id: &str,
    tool_name: &str,
    make_future: impl FnOnce() -> F,
) -> Result<crate::types::ToolResult, PanicPayload>
where
    F: Future<Output = crate::types::ToolResult>,
{
    // 阶段 1：构造 future 本身 panic（例如 handler 在返回前就 panic!()）。
    let fut = match catch_unwind(AssertUnwindSafe(make_future)) {
        Ok(f) => f,
        Err(payload) => {
            log_panic(plugin_id, tool_name, &payload);
            return Err(PanicPayload::from_any(&*payload));
        }
    };

    // 阶段 2：poll 期间 panic。
    let r = CatchUnwind::new(fut).await;
    if let Err(ref p) = r {
        log_panic(plugin_id, tool_name, &p.text);
    }
    r
}

fn log_panic(plugin_id: &str, tool_name: &str, payload: &(dyn Any + Send)) {
    log_panic_text(plugin_id, tool_name, &PanicPayload::from_any(payload).text);
}

/// 统一的 panic 日志格式。可信插件崩溃时宿主仍存活，因此必须有这条日志，
/// 否则用户只会看到"某个功能突然报错"。
pub fn log_panic_text(plugin_id: &str, tool_name: &str, text: &str) {
    tracing_log(plugin_id, tool_name, text);
}

/// 极简日志：不引入 `tracing`/`log` crate（避免规格外依赖抽象），
/// 由宿主通过 [`install_panic_hook`] 接管输出。
fn tracing_log(plugin_id: &str, tool_name: &str, text: &str) {
    // 这里用 eprintln 作为保底输出。宿主可以用 panic hook + 自己的日志器替换。
    eprintln!("[plugin-panic] plugin={plugin_id} tool={tool_name} payload={text:?}");
}

/// 安装一个 panic hook，把 panic 位置信息带上，便于归因到插件。
///
/// **注意**：这不会把信使变成防火墙。hook 执行后 panic 继续 unwind，
/// 仍然必须由 `catch_unwind` 兜住。hook 只负责"记录"。
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 先输出结构化前缀，再交给默认 hook 打印 backtrace。
            eprintln!(
                "[plugin-panic-location] thread={:?} location={:?}",
                std::thread::current().name(),
                info.location()
            );
            default_hook(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolResult;

    #[tokio::test]
    async fn catches_panic_in_first_poll() {
        let r = catch_unwind_call("demo", "demo.echo", || async {
            panic!("boom-in-poll");
        })
        .await;
        assert!(r.is_err(), "应当捕获到 panic");
        assert!(r.unwrap_err().text.contains("boom-in-poll"));
    }

    #[tokio::test]
    async fn catches_panic_before_future_created() {
        let r: Result<ToolResult, PanicPayload> =
            catch_unwind_call("demo", "demo.echo", || -> std::future::Ready<ToolResult> {
                panic!("boom-before");
            })
            .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().text.contains("boom-before"));
    }

    #[tokio::test]
    async fn catches_panic_in_nth_poll() {
        // 前两次 poll 返回 Pending，第三次 panic。
        struct LatePanic {
            polls: usize,
        }
        impl Future for LatePanic {
            type Output = ToolResult;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                self.polls += 1;
                if self.polls < 3 {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    panic!("boom-third");
                }
            }
        }

        let r = catch_unwind_call("demo", "demo.echo", || LatePanic { polls: 0 }).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().text.contains("boom-third"));
    }

    #[tokio::test]
    async fn passes_through_normal_result() {
        let r = catch_unwind_call("demo", "demo.echo", || async {
            ToolResult::success_text("ok")
        })
        .await;
        assert!(r.is_ok());
        assert!(!r.unwrap().is_error);
    }

    #[tokio::test]
    async fn non_string_payload_is_reported() {
        let r = catch_unwind_call("demo", "demo.echo", || async {
            std::panic::panic_any(42_u32);
        })
        .await;
        let p = r.unwrap_err();
        assert!(p.text.contains("non-string"), "{}", p.text);
    }
}
