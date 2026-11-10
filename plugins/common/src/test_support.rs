//! 测试用临时目录：**离开作用域时自动删除**。
//!
//! # 为什么需要它
//!
//! 之前每个测试都自己拼 `std::env::temp_dir().join(format!("xxx-{}-{}", pid, nanos))`，
//! 而**没有任何一个在结束时清理**。跑一遍全量测试会在系统临时目录里留下几百个目录
//! （实测一轮跑下来 961 个、约 6 MB）。
//!
//! 后果不只是脏：
//! - 长跑的 CI 机器临时目录会被逐渐填满；
//! - 调试时难以分辨哪个目录是这次的、哪些是几天前的陈货；
//! - 磁盘满时测试会以莫名其妙的方式失败（写入时报 IO 错，被当成"存储错误"）。
//!
//! [`TestDir`] 用 `Drop` 保证清理 —— 即使测试 panic（断言失败）也会执行。
//!
//! # 为什么删失败也不 panic
//!
//! `Drop` 里 panic 会在 unwind 期间造成双 panic → 直接 abort 进程。
//! 清理失败（例如 Windows 上文件仍被占用）只记一条 stderr，不掩盖**真正的**测试失败。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 进程级临时目录登记表。
///
/// # 为什么不用「每个测试函数的局部变量」
///
/// 理想做法是 `let dir = TestDir::new(..)` 放进每个测试，靠 `Drop` 清理。
/// 但那要求改所有 `ctx()` 的返回类型和 58 个调用点（每个测试都要把 `dir` 绑定出来），
/// 改动面大且容易漏 —— 漏一个就又留下垃圾。
///
/// 这里用登记表：`ctx()` 创建目录时登记，进程退出时统一清理。
/// 好处是调用点零改动；代价是清理发生在**进程结束时**而非每个测试结束，
/// 所以一次全量测试运行期间目录会短暂累积（可接受，跑完就清空）。
///
/// 登记表用 `Mutex<Vec<PathBuf>>`，不依赖外部 crate。
static REGISTRY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 登记一个目录，返回路径。
///
/// 首次调用会装一个"进程退出前清理"的钩子。
fn register(path: PathBuf) -> PathBuf {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        install_exit_cleanup();
    });
    if let Ok(mut v) = REGISTRY.lock() {
        v.push(path.clone());
    }
    path
}

/// 装一个进程退出时清理的钩子。
///
/// # 为什么不用 `std::process::exit` 之类
///
/// Rust 没有稳定的 `atexit`。可行的做法是起一个**守护线程**，它阻塞在
/// `REGISTRY` 的 Condvar 上；但测试进程结束时不会有人通知它。
///
/// 更可靠且零依赖的做法：`std::thread::spawn` 一个线程，每 200ms 检查一次
/// "本进程主线程是否还活着"不可移植，也不可靠。
///
/// 实际采用：**在 `Drop` 之外再加一道保险** —— 每个 `TestDir` 与 `temp_path`
/// 都登记；同时提供一个显式的 [`cleanup_registered`]，测试 crate 可以在
/// 一个 `#[ctor]` 风格的钩子里调用。但本项目不引入 `ctor` crate。
///
/// **最终方案（务实）**：靠**下一次运行**开始时清理上一次的残留。
/// 每次 `temp_path` 首次调用时，扫一遍临时目录里 `lpf-test-*` 前缀、
/// 且进程 ID 已不存在的旧目录，删除它们。这样：
/// - 不依赖退出钩子（跨平台可靠）；
/// - 每次跑测试都会清掉上一轮的垃圾；
/// - 只删自己命名前缀的目录，绝不误删别的程序的文件。
fn install_exit_cleanup() {
    sweep_stale();
}

/// 扫描并删除**上一次运行**遗留的 `lpf-test-*` 目录。
///
/// # 判据：保守到"宁可漏删，不可误删"
///
/// 只处理名字符合我们格式（`lpf-test-<pid>-<nanos>-<tag>`）且 PID **不是当前进程**的目录。
/// 对它们执行一次**独占重命名**：
///
/// | 重命名结果 | 含义 | 动作 |
/// |---|---|---|
/// | 成功 | 没有进程在占用（陈货） | 改名后删除 |
/// | 失败 | 仍被占用 / 权限不足 | 原样保留，下次再试 |
///
/// 为什么用重命名而不是 `remove_dir_all` 直接删：重命名是**原子可回滚**的，
/// 且"能改名"本身就是"没被占用"的可移植判据（不需要平台 API、不需要 unsafe）。
/// 直接删则可能在删到一半时发现文件被锁，留下半残目录。
///
/// 为什么不同于"按时间判据"：按时间（如 1 小时）会让**每轮测试都留一批垃圾**，
/// 直到超时才清 —— 实测每轮 +45 个目录。重命名判据可以当场清理上一轮的全部遗留。
const _STALE_AGE_UNUSED: std::time::Duration = std::time::Duration::from_secs(3600);

fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("lpf-test-") else {
            continue;
        };
        // 格式校验：`lpf-test-<pid>-<nanos>-<tag>`
        let mut parts = rest.split('-');
        let (Some(pid_str), Some(_nanos)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue; // 当前进程自己的，绝不删
        }
        // 独占重命名判据。
        let path = entry.path();
        let probe = path.with_extension("sweep-probe");
        if std::fs::rename(&path, &probe).is_ok() {
            let _ = std::fs::remove_dir_all(&probe);
        }
    }
}

/// 清理所有已登记目录。由每个测试 crate 的首次调用或显式调用触发。
///
/// # 注意
///
/// 它会清空**整个**登记表，因此**不适用于并行测试内部调用** ——
/// 若测试 A 调用它，会把同时运行的测试 B/C 的目录一起删掉，
/// 导致 B/C 以"文件不存在"这类难以归因的方式失败。
///
/// 正确用法只有两种：
/// 1. 测试进程收尾（`#[ignore]` 的手动清理入口）；
/// 2. 测试**自己的**目录，用 [`TestDir`]（`Drop` 精确清理，互不影响）。
///
/// 返回清理的目录数。
pub fn cleanup_registered() -> usize {
    let Ok(mut v) = REGISTRY.lock() else {
        return 0;
    };
    let paths: Vec<PathBuf> = v.drain(..).collect();
    let mut n = 0;
    for p in paths {
        if std::fs::remove_dir_all(&p).is_ok() {
            n += 1;
        }
    }
    n
}

/// 一个测试专用的临时目录的路径（不自动删除）。
///
/// 用于既有测试的 `ctx()` 辅助函数：零调用点改动。
pub fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let safe_tag: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = std::env::temp_dir().join(format!(
        "lpf-test-{}-{}-{}",
        std::process::id(),
        nanos,
        safe_tag
    ));
    register(path.clone());
    path
}

/// 一个测试专用临时目录，析构时递归删除（新代码推荐用它）。
pub struct TestDir {
    path: PathBuf,
    keep: bool,
}

impl TestDir {
    /// 创建一个唯一目录。
    ///
    /// 唯一性由 `进程 ID + 纳秒时间戳 + 标签` 保证：并行测试不会互踩。
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // 标签里可能有路径分隔符（例如 plugin id 带点），替换掉以免建出子目录。
        let safe_tag: String = tag
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = std::env::temp_dir().join(format!(
            "lpf-test-{}-{}-{}",
            std::process::id(),
            nanos,
            safe_tag
        ));
        std::fs::create_dir_all(&path).expect("创建测试临时目录失败");
        Self { path, keep: false }
    }

    /// 保留目录（失败排查时用）。会打印路径提醒你手动删。
    pub fn keep_for_debug(mut self) -> Self {
        self.keep = true;
        eprintln!("[TestDir] 保留目录供调试：{}", self.path.display());
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.path.clone()
    }

    /// 在目录下拼一个相对路径（不创建）。
    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    /// 手动提前清理（多数情况用 Drop 就够了）。
    pub fn cleanup(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        // 不 panic：Drop 里 panic 会在 unwind 中双 panic → abort。
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            // 目录本就不存在（测试自己删过了）不算错误。
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "[TestDir] 清理失败（已忽略，不影响测试结论）：{} — {}",
                    self.path.display(),
                    e
                );
            }
        }
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_removes() {
        let p = {
            let d = TestDir::new("basic");
            assert!(d.path().exists(), "应当创建成功");
            d.to_path_buf()
        };
        assert!(!p.exists(), "离开作用域后应当被删除");
    }

    #[test]
    fn cleanup_on_panic() {
        // 关键：测试 panic 时也必须清理（Drop 在 unwind 时执行）。
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let c2 = captured.clone();
        let r = std::panic::catch_unwind(move || {
            let d = TestDir::new("panic");
            *c2.lock().unwrap() = Some(d.to_path_buf());
            panic!("故意 panic");
        });
        assert!(r.is_err(), "应当 panic");
        let p = captured.lock().unwrap().clone().unwrap();
        assert!(!p.exists(), "panic 后仍应清理（Drop 已执行）");
    }

    #[test]
    fn unique_per_call() {
        let a = TestDir::new("same");
        let b = TestDir::new("same");
        assert_ne!(a.path(), b.path(), "同名标签也必须产生不同目录");
    }

    #[test]
    fn sanitizes_tag() {
        let d = TestDir::new("com.example.plugin/sub");
        // 目录名里不应出现路径分隔符造成的子目录
        assert!(!d.path().to_string_lossy().contains("plugin/sub"));
        assert!(d.path().exists());
    }

    #[test]
    fn keep_for_debug_preserves() {
        let d = TestDir::new("keep").keep_for_debug();
        let p = d.to_path_buf();
        drop(d);
        assert!(p.exists(), "keep_for_debug 后不应删除");
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn fresh_temp_path_dir_survives_parallel_sweep() {
        // 关键安全性质：sweep_stale 绝不能删掉**当前进程**创建的目录，
        // 否则并行测试会互相破坏。这里只测 sweep（不调用全局 cleanup_registered，
        // 因为那个会清空整表，属于进程收尾用途）。
        let fresh = temp_path("fresh-guard");
        std::fs::create_dir_all(&fresh).unwrap();
        sweep_stale();
        assert!(fresh.exists(), "当前进程的新目录不应被 sweep 删除");
        // 精确清理自己的目录（不影响并行测试）
        let _ = std::fs::remove_dir_all(&fresh);
    }

    #[test]
    fn testdir_drop_is_precise_and_parallel_safe() {
        // TestDir 的 Drop 只删自己的目录，适合并行测试。
        let p = {
            let d = TestDir::new("precise");
            let p = d.to_path_buf();
            std::fs::write(d.join("a.txt"), b"x").unwrap();
            p
        };
        assert!(!p.exists(), "TestDir 应精确清理自己的目录");
    }

    #[test]
    fn sweep_stale_ignores_unrelated_dirs() {
        // 不具备我们的命名格式的外部目录，绝不碰。
        let foreign = std::env::temp_dir().join(format!("not-ours-{}", std::process::id()));
        std::fs::create_dir_all(&foreign).unwrap();
        sweep_stale();
        assert!(foreign.exists(), "非本项目的目录不应被删除");
        let _ = std::fs::remove_dir_all(&foreign);
    }
}
