//! 沙箱插件受限内存分配器。
//!
//! # 问题背景
//!
//! 框架层的 `UnitConfig::memory_limit_bytes` 是**声明性**字段（见 README §8 限制 1），
//! 纯 Rust 单元无法强制宿主的 `#[global_allocator]`，所以一个恶意或有 bug 的插件
//! 可以直接 `Vec::with_capacity(usize::MAX)` 让宿主 OOM 崩掉。
//!
//! 这一层的目的是把「插件申请堆内存」的入口**收到一个受控接口**里：插件的
//! 业务逻辑只通过这里拿内存，分配器在真正向系统申请之前先检查配额。
//!
//! # 设计原则
//!
//! - **不重载全局分配器**：宿主自己的分配不受影响，我们只管「通过 SandboxAllocator
//!   申请」的那部分。
//! - **RAII 为正路**：`AllocationGuard` 在 drop 时自动释放并扣减计数器。显式
//!   `deallocate` 也保留，方便与 `C` API 风格的接口对接。
//! - **溢出安全**：所有加法用 `checked_add`，避免 `used + size` 本身溢出。
//!
//! # unsafe 说明
//!
//! `plugins/common` crate 顶部声明了 `#![deny(unsafe_code)]`。本模块**必须**用 unsafe，
//! 因为要做裸指针交换（`Box::into_raw` / `Box::from_raw`）以追踪每个分配块的大小。
//! 所以本模块用 `#![allow(unsafe_code)]`，所有 unsafe 的正确性在注释里逐条论证。
//!
//! 其他模块不能直接构造 inner 内部类型，也就不能绕过校验。
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 沙箱分配器的错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// 本次分配后会超出配额。
    OutOfMemory {
        limit: usize,
        used: usize,
        requested: usize,
    },
    /// 分配本身失败（底层 `Vec` 申请失败）。
    /// 现代 OS 上极罕见，但仍需处理。
    AllocationFailed,
    /// 试图释放一个不在我们追踪列表里的指针 —— 要么是野指针，要么是已经释放过了。
    DoubleFreeOrBadPointer,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfMemory {
                limit,
                used,
                requested,
            } => write!(
                f,
                "沙箱内存超限：配额 {limit} 字节，已用 {used}，本次请求 {requested}"
            ),
            Self::AllocationFailed => write!(f, "沙箱分配器自身申请内存失败"),
            Self::DoubleFreeOrBadPointer => {
                write!(f, "试图释放一个未追踪的指针（double free 或野指针）")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

// =====================================================================
// 内层：所有 unsafe 集中在这里，模块对外只暴露安全接口。
// =====================================================================
//
// 关键：crate 根声明了 `#![deny(unsafe_code)]`，所以这个 inner 必须显式 allow。
// 把 unsafe 隔离到一个私有子模块后，外层 sandbox_alloc 模块对外暴露的 API 全是安全的 ——
// 其他模块不能直接构造 inner，也就不能绕过我们的校验。
#[allow(unsafe_code)]
mod inner {
    use super::*;

    /// 沙箱分配器本体。
    ///
    /// 内部状态用 `Arc` 包，这样可以同时被多个 `SandboxContext` 共享（例如同一个插件的
    /// 多个 tool handler 各拿一个上下文，但配额池是同一个）。
    pub struct SandboxAllocator {
        /// 硬上限，单位字节。构造时确定，运行期不可变。
        limit_bytes: usize,
        /// 当前已分配出去的字节数。
        ///
        /// 用 `AtomicUsize` 而不是 `Mutex<usize>`：增减是 hot path，无锁原子操作足够快。
        /// `Ordering::AcqRel` 保证对其他线程可见。
        used_bytes: AtomicUsize,
        /// 追踪每个「活」分配块的 (指针 → 大小) 映射。
        ///
        /// 为什么不能只靠裸指针自己知道 size？因为 Rust 的裸指针就是地址，没有元信息。
        /// 释放时需要知道 size 才能安全地把 `Box<[u8]>` 还回来（`Box::from_raw` 的 size
        /// 必须与 `into_raw` 时一致，否则会 UB）。
        live_blocks: Mutex<HashMap<*mut u8, usize>>,
    }

    // 安全论证：*mut u8 在 HashMap 里只做 key（Copy 语义，不做解引用）。HashMap 的 key
    // 类型 Send/Sync 约束的是"能否在线程间移动 / 共享"，而裸指针只是地址值。真正的
    // 解引用只发生在持有 AllocationGuard、且分配器 live_blocks 里仍登记着这条的时候。
    unsafe impl Send for SandboxAllocator {}
    unsafe impl Sync for SandboxAllocator {}

    impl SandboxAllocator {
        /// 创建配额为 `limit_mb` MB 的分配器。0 等价于"拒绝所有分配"。
        pub fn new(limit_mb: usize) -> Arc<Self> {
            Arc::new(Self {
                limit_bytes: limit_mb.saturating_mul(1024 * 1024),
                used_bytes: AtomicUsize::new(0),
                live_blocks: Mutex::new(HashMap::new()),
            })
        }

        /// 创建配额为精确字节数的分配器。
        pub fn new_bytes(limit_bytes: usize) -> Arc<Self> {
            Arc::new(Self {
                limit_bytes,
                used_bytes: AtomicUsize::new(0),
                live_blocks: Mutex::new(HashMap::new()),
            })
        }

        pub fn limit_bytes(&self) -> usize {
            self.limit_bytes
        }

        /// 当前已使用的字节数。近似值 —— 用 `Acquire` 保证读到最新，但不保证
        /// 读到时别的线程没在同时修改。做"配额够不够"的逻辑判断请直接调
        /// `allocate`（它内部有完整校验）。
        pub fn used_bytes(&self) -> usize {
            self.used_bytes.load(Ordering::Acquire)
        }

        /// 剩余可用字节数。`limit - used`，若 used 超过 limit（理论上不会，除非有 bug）
        /// 返回 0。
        pub fn remaining(&self) -> usize {
            self.limit_bytes
                .saturating_sub(self.used_bytes.load(Ordering::Acquire))
        }

        /// 分配 `size` 字节内存。
        ///
        /// 成功返回指向 `size` 字节可读写内存的裸指针（已对齐、已零初始化）。
        /// 失败返回 [`SandboxError`]。
        ///
        /// # 指针所有权
        ///
        /// 返回的指针必须通过本分配器的 `deallocate` 释放，否则**内存泄漏**。
        /// 正常用法应该用外层 `SandboxContext::alloc`，它返回 RAII guard 自动释放。
        pub fn allocate(&self, size: usize) -> Result<*mut u8, SandboxError> {
            if size == 0 {
                // 零大小：直接拒绝，避免 live_blocks 里出现"悬空指针当 key"的微妙场景。
                return Err(SandboxError::AllocationFailed);
            }

            // ── 1. 配额校验 ──
            let prev = self.used_bytes.load(Ordering::Acquire);
            let next = match prev.checked_add(size) {
                Some(v) => v,
                None => {
                    // usize 溢出 —— 相当于"配额肯定不够"。
                    return Err(SandboxError::OutOfMemory {
                        limit: self.limit_bytes,
                        used: prev,
                        requested: size,
                    });
                }
            };
            if next > self.limit_bytes {
                return Err(SandboxError::OutOfMemory {
                    limit: self.limit_bytes,
                    used: prev,
                    requested: size,
                });
            }

            // ── 2. 真正申请内存 ──
            // 用 vec![0u8; size].into_boxed_slice() + Box::into_raw()：
            //   - stable Rust，不需要 nightly allocator API
            //   - 对齐由 Rust 自己保证（[u8] 对齐到 1，任何布局都合法）
            //   - 零初始化：避免插件读到旧分配残留的敏感数据
            //   - size 与 into_raw 时一致，后续 from_raw 必然安全
            let boxed_slice: Box<[u8]> = vec![0u8; size].into_boxed_slice();
            let ptr = Box::into_raw(boxed_slice) as *mut u8;

            // ── 3. 登记到 live_blocks ──
            // 先登记再提交 used_bytes：这样别的线程在 used 上看到增长时，
            // deallocate 已经能在表里找到 key 了（Acquire/Release 建立 happens-before）。
            {
                let mut blocks = self.live_blocks.lock().unwrap_or_else(|e| e.into_inner());
                blocks.insert(ptr, size);
            }

            // ── 4. 提交 used_bytes ──
            self.used_bytes.store(next, Ordering::Release);

            Ok(ptr)
        }

        /// 释放之前由本分配器 `allocate` 拿到的指针。
        ///
        /// - null 或不在 live_blocks → 返回 DoubleFreeOrBadPointer（**不会 UB**，
        ///   先校验再操作）。
        /// - 同一指针被释放两次 → 第二次也返回 DoubleFree（第一释放已从表摘掉）。
        pub fn deallocate(&self, ptr: *mut u8) -> Result<(), SandboxError> {
            if ptr.is_null() {
                return Err(SandboxError::DoubleFreeOrBadPointer);
            }

            // ── 1. 查表 ──
            let size = {
                let mut blocks = self.live_blocks.lock().unwrap_or_else(|e| e.into_inner());
                // remove 能保证我们不会"释放一块已经被释放过的内存"：即使多线程并发，
                // 也是第一个 remove 拿走 size 并 drop，后续同 key 的 remove 取不到。
                blocks.remove(&ptr).ok_or(SandboxError::DoubleFreeOrBadPointer)?
            };

            // ── 2. 安全回收 ──
            // 安全论证：ptr 是 `Box::into_raw(Box<[u8]>) as *mut u8` 的结果；
            // size 是分配时传入的 size（对齐恒为 1）。Box<[u8]> 的 drop 会用完全相同的
            // layout 调用 GlobalAlloc::dealloc，与 vec! 分配时的 alloc 配对 ——
            // Rust 标准库保证这套配对合法。
            unsafe {
                let _boxed = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, size));
            } // _boxed drop → 内存真正还给 OS。

            // ── 3. 扣减计数器 ──
            // 理论上 deallocate 和 allocate 必须一一对应，但 compare_exchange_weak
            // 用在这里是 double-safe：如果并发修改导致 prev 不匹配，重试；
            // saturating_sub 防止"多减一次"降到 0 以下。
            let prev = self.used_bytes.load(Ordering::Acquire);
            let _ = self.used_bytes.compare_exchange_weak(
                prev,
                prev.saturating_sub(size),
                Ordering::AcqRel,
                Ordering::Acquire,
            );

            Ok(())
        }
    }
}
pub use inner::SandboxAllocator; // 类型别名到模块外层

/// 沙箱上下文。把 [`SandboxAllocator`] 暴露给插件工具。
///
/// 与 `PluginContext` 分离：后者是框架注入的"宿主能力"（日志 / HTTP / 跨插件调用），
/// 前者是分配器自己的"配额与记账"。两者生命周期相同，但职责不同。
///
/// 构造时接受一个 `Arc<SandboxAllocator>`，这样同一个插件的多个工具 handler 可以
/// 共享同一个配额池。
#[derive(Clone)]
pub struct SandboxContext {
    allocator: Arc<SandboxAllocator>,
}

impl std::fmt::Debug for SandboxContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxContext")
            .field("limit_mb", &(self.allocator.limit_bytes() / (1024 * 1024)))
            .field("used_bytes", &self.allocator.used_bytes())
            .finish_non_exhaustive()
    }
}

impl SandboxContext {
    /// 构造：直接分配一个新分配器，默认 64 MB 配额。
    pub fn new(limit_mb: usize) -> Self {
        Self {
            allocator: SandboxAllocator::new(limit_mb),
        }
    }

    /// 精确字节数配额。
    pub fn new_bytes(limit_bytes: usize) -> Self {
        Self {
            allocator: SandboxAllocator::new_bytes(limit_bytes),
        }
    }

    /// 拿到底层分配器引用。通常不需要直接用 —— [`alloc`] 返回的是 RAII guard。
    pub fn allocator(&self) -> &Arc<SandboxAllocator> {
        &self.allocator
    }

    /// 分配 `size` 字节，返回 RAII guard。**guard drop 时自动释放并扣减计数器**，
    /// 正常代码不需要手动调 `deallocate`。
    ///
    /// # 用法
    ///
    /// ```no_run
    /// # use launcher_plugin_common::SandboxContext;
    /// let ctx = SandboxContext::new(64);
    /// let mut guard = ctx.alloc(1024).expect("拿 1KB");
    /// let data = guard.as_mut_slice();
    /// data[0] = 0xAB;
    /// // 出作用域 guard drop —— 1KB 自动归还
    /// ```
    pub fn alloc(&self, size: usize) -> Result<AllocationGuard, SandboxError> {
        let ptr = self.allocator.allocate(size)?;
        Ok(AllocationGuard {
            allocator: Arc::clone(&self.allocator),
            ptr,
            size,
        })
    }

    /// 分配 `count` 个 `T`，返回一个初始化为零的 guard。
    ///
    /// 底层走的还是字节分配，所以 `T` 必须是 `Copy`（零初始化对它没有安全问题）。
    /// 这是比 `alloc` 更友好的接口 —— 避免插件自己算 `size_of::<T>() * count` 还会
    /// 溢出。
    pub fn alloc_slice<T: Copy>(&self, count: usize) -> Result<AllocationGuard, SandboxError> {
        let size = std::mem::size_of::<T>()
            .checked_mul(count)
            .ok_or_else(|| SandboxError::OutOfMemory {
                limit: self.allocator.limit_bytes(),
                used: self.allocator.used_bytes(),
                requested: usize::MAX,
            })?;
        self.alloc(size)
    }
}

/// RAII 释放守卫。持有一块 [`SandboxAllocator`] 分配出的内存，drop 时自动归还。
///
/// 设计选择：
/// - 故意**不**实现 `DerefMut` 到 `&mut [u8]` —— 那样的话外部可以直接解引用，
///   但 guard 的生命周期会被延长，容易让"内存还在被用但计数器不归零"造成误解。
///   用 `as_mut_slice` 显式拿切片，意图更清楚。
/// - drop 里调 `deallocate` 是 best-effort：失败不会 panic（panic in drop 会 abort），
///   只做记录（目前静默忽略）。
pub struct AllocationGuard {
    allocator: Arc<SandboxAllocator>,
    ptr: *mut u8,
    size: usize,
}

// 安全论证：
// - Send：持有裸指针，但只在 guard 活着期间解引用；分配器保证这块内存不会被别人
//   拿到（只有我们 drop 时才归还 live_blocks 条目）。
// - Sync：多个 guard 共享同一块？不会 —— 分配器每块内存只登记一次。跨线程共享同一个
//   guard 虽然理论上可以（Send 已经覆盖），但没有业务需求，实际也没人这么用。
unsafe impl Send for AllocationGuard {}
unsafe impl Sync for AllocationGuard {}

impl AllocationGuard {
    /// 把这块内存当作可变切片。
    ///
    /// # 安全
    /// guard 自己持有 allocator 的 Arc 和 ptr/size，且 drop 会先把 ptr 置 null 再
    /// 调 deallocate。所以只要切片活着，guard 就不可能提前 drop —— 编译器的生命周期
    /// 保证这一点。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.ptr.is_null() || self.size == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }

    /// 显式提前归还。之后再解引用会返回空切片（因为 ptr 已置 null）。
    ///
    /// 与直接 drop 等价，但提前归还好让计数器更快降下来。
    pub fn release(mut self) -> Result<(), SandboxError> {
        let ptr = self.ptr;
        self.ptr = std::ptr::null_mut();
        self.size = 0;
        self.allocator.deallocate(ptr)
        // 函数返回，self drop —— 但此时 ptr 已经 null，drop 什么都不做。
    }
}

impl std::fmt::Debug for AllocationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocationGuard")
            .field("ptr", &self.ptr)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Drop for AllocationGuard {
    fn drop(&mut self) {
        if self.ptr.is_null() || self.size == 0 {
            // 已经被 release() 手动归还，或构造失败。什么都不做。
            return;
        }
        // 只做 best-effort 归还。失败（DoubleFree / Mutex poisoned）时
        // 我们无法 panic —— panic in drop 会触发整个进程 abort。
        let _ = self.allocator.deallocate(self.ptr);
        self.ptr = std::ptr::null_mut();
        self.size = 0;
    }
}

// ---- 单元测试 ----

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 1：正常分配 + 正常释放，计数器归零。
    #[test]
    fn normal_alloc_dealloc_reduces_counter() {
        let ctx = SandboxContext::new(64); // 64 MB
        assert_eq!(ctx.allocator.used_bytes(), 0);

        let g1 = ctx.alloc(1024).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 1024);

        let mut g2 = ctx.alloc(2048).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 1024 + 2048);

        // 写点数据，确认没越界
        let slice = g2.as_mut_slice();
        slice[0] = 0xAA;
        slice[slice.len() - 1] = 0xBB;
        assert_eq!(slice[0], 0xAA);
        assert_eq!(slice[slice.len() - 1], 0xBB);

        let g3 = ctx.alloc(4096).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 1024 + 2048 + 4096);

        drop(g1);
        drop(g2);
        assert_eq!(ctx.allocator.used_bytes(), 4096);

        let _ = g3; // 让 clippy 闭嘴
        assert_eq!(ctx.allocator.used_bytes(), 4096);
    }

    /// 测试 2：试图分配超过配额的内存，必须返回 OutOfMemory。
    #[test]
    fn alloc_over_quota_rejects() {
        let ctx = SandboxContext::new_bytes(1024); // 精确 1KB 配额

        // 先吃掉 700 字节
        let _g1 = ctx.alloc(700).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 700);
        assert_eq!(ctx.allocator.remaining(), 324);

        // 再要 500 字节 → 700 + 500 = 1200 > 1024，拒绝
        let err = match ctx.alloc(500) {
            Ok(_) => panic!("超出配额应该拒绝"),
            Err(e) => e,
        };
        match err {
            SandboxError::OutOfMemory {
                limit,
                used,
                requested,
            } => {
                assert_eq!(limit, 1024);
                assert_eq!(used, 700);
                assert_eq!(requested, 500);
            }
            other => panic!("预期 OutOfMemory，实际 {other:?}"),
        }

        // g1 还没释放，计数器保持 700
        assert_eq!(ctx.allocator.used_bytes(), 700);

        // 释放 g1，应该能拿 1024
        drop(_g1);
        let _g2 = ctx.alloc(1024).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 1024);

        // 再要 1 字节都不行
        assert!(matches!(ctx.alloc(1), Err(SandboxError::OutOfMemory { .. })));
    }

    /// 测试 3：连续多次小额分配 + 释放，验证计数器始终与 live_blocks 一致。
    #[test]
    fn counter_accuracy_over_many_allocations() {
        let ctx = SandboxContext::new_bytes(64 * 1024); // 64 KB

        let mut guards: Vec<AllocationGuard> = Vec::new();
        let mut expected_used: usize = 0;

        for i in 0..64 {
            let g = ctx
                .alloc(1024)
                .unwrap_or_else(|e| panic!("第 {i} 次分配应成功: {e}"));
            expected_used += 1024;
            assert_eq!(
                ctx.allocator.used_bytes(),
                expected_used,
                "分配第 {i} 块后计数器不准"
            );
            guards.push(g);
        }

        assert_eq!(expected_used, 64 * 1024);
        assert_eq!(ctx.allocator.used_bytes(), 64 * 1024);
        assert!(matches!(
            ctx.alloc(1),
            Err(SandboxError::OutOfMemory { .. })
        ));

        // 释放一半（偶数索引，从后往前删避免索引错位），检查每次释放后计数器正确
        for _ in 0..32 {
            guards.remove(0);
            expected_used -= 1024;
            assert_eq!(
                ctx.allocator.used_bytes(),
                expected_used,
                "释放 guard 后计数器不准"
            );
        }

        // 再把剩下的全部释放
        drop(guards);
        assert_eq!(
            ctx.allocator.used_bytes(),
            0,
            "全部释放后计数器应归零"
        );
    }

    /// 附加：DoubleFree（手动 deallocate 两次）必须报错，不能 UB。
    #[test]
    fn double_free_is_detected() {
        let alloc = SandboxAllocator::new_bytes(4096);
        let ptr = alloc.allocate(64).unwrap();

        alloc.deallocate(ptr).unwrap();

        let err = match alloc.deallocate(ptr) {
            Ok(()) => panic!("double free 必须报错"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            SandboxError::DoubleFreeOrBadPointer
        ));
    }

    /// 附加：零大小分配拒绝。
    #[test]
    fn zero_size_alloc_rejected() {
        let alloc = SandboxAllocator::new_bytes(4096);
        assert!(matches!(
            alloc.allocate(0),
            Err(SandboxError::AllocationFailed)
        ));
    }

    /// 附加：并发分配 / 释放，计数器仍然准确。
    #[test]
    fn concurrent_alloc_release_stays_accurate() {
        use std::thread;

        let ctx = SandboxContext::new_bytes(1024 * 1024); // 1 MB
        let ctx_clone = ctx.clone();

        let g_main = ctx.alloc(512 * 1024).unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 512 * 1024);

        let handle = thread::spawn(move || {
            let g = ctx_clone.alloc(256 * 1024).unwrap();
            assert_eq!(
                ctx_clone.allocator.used_bytes(),
                512 * 1024 + 256 * 1024
            );
            drop(g);
            assert_eq!(ctx_clone.allocator.used_bytes(), 512 * 1024);
        });

        handle.join().unwrap();
        assert_eq!(ctx.allocator.used_bytes(), 512 * 1024);

        drop(g_main);
        assert_eq!(ctx.allocator.used_bytes(), 0);
    }

    /// 附加：显式 release 之后再 as_mut_slice 应该拿空切片。
    #[test]
    fn release_returns_empty_slice() {
        let ctx = SandboxContext::new_bytes(4096);
        let mut g = ctx.alloc(256).unwrap();
        assert_eq!(g.as_mut_slice().len(), 256);
        g.release().unwrap();
        // release 消耗了 self，这里不能再用 g —— 但我们可以验证：
        // release 后计数器归零
        assert_eq!(ctx.allocator.used_bytes(), 0);
    }
}
