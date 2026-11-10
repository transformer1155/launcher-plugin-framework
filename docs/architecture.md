# 目录结构与模块划分

```
launcher-plugin-framework/
├── Cargo.toml
├── README.md
├── docs/
│   ├── architecture.md           架构说明（进程模型 / 信任分级 / 调用链）
│   ├── security-boundary.md      安全边界声明（可执行的、写进代码注释的同一套话）
│   └── open-questions.md         需要你确认的遗留问题
├── src/
│   ├── lib.rs                    crate 根：模块声明、顶层 re-export、安全边界声明
│   ├── error.rs                  错误码表 ToolError + MCP 同构映射
│   ├── types.rs                  ToolMeta / TrustLevel / ToolResult / TaskId / UnitId
│   ├── json.rs                   极简 JSONSchema 校验（InvalidArguments 的来源）
│   ├── catch_unwind.rs           异步 panic 捕获：包住 Future::poll 的 CatchUnwind
│   ├── scheduler/
│   │   ├── mod.rs                Scheduler trait + UnitConfig / TaskOutcome
│   │   ├── state.rs              单元状态机 UnitState + 合法迁移表
│   │   ├── unit.rs               虚拟进程执行单元：心跳、配额、超时、取消
│   │   └── reaper.rs             空闲回收 / max_lifetime 回收
│   ├── registry/
│   │   ├── mod.rs                ToolRegistry trait
│   │   └── impl_registry.rs      call_tool 完整分流逻辑（四步检查 + 分流执行）
│   ├── restart.rs                RestartManager：指数退避 + 滑动窗口熔断 + 重启流程
│   ├── signature/
│   │   ├── mod.rs                SignatureVerifier trait + PluginPackage + 校验结果
│   │   ├── ed25519_verifier.rs   Ed25519 实现（公钥内置、私钥官方持有）
│   │   └── revocation.rs         吊销列表（自身签名，未验签 fail-closed）
│   ├── loader.rs                 插件加载：验签 → 决定 trust_level（失败降级到沙箱）
│   └── mcp.rs                    MCP tools/list、tools/call 适配层
├── examples/
│   └── host_demo.rs              端到端演示：一个可信插件 + 一个沙箱插件 + 崩溃重启
└── tests/
    ├── registry_acl.rs           注册表 ACL / 错误码 / 分流
    ├── panic_capture.rs          可信插件 HandlerPanic，宿主存活
    ├── sandbox_timeout.rs        超时可中断 vs 不可中断升级崩溃
    ├── restart_backoff.rs        退避序列 1/2/4/8/…/30 与滑动窗口熔断
    └── signature_downgrade.rs    验签失败降级到沙箱、吊销列表 fail-closed
```

## 分层

| 层 | 模块 | 职责 |
|---|---|---|
| L0 基础 | `error` `types` `json` | 错误码、元数据、schema 校验。无异步、无锁 |
| L1 执行 | `catch_unwind` `scheduler` | 虚拟进程执行单元、超时、取消、配额、状态机 |
| L2 编排 | `registry` `restart` | 工具登记与 ACL、四步检查分流、退避重启与熔断 |
| L3 信任 | `signature` `loader` | 包整体验签、吊销列表、降级决策 |
| L4 对外 | `mcp` | 与 MCP 无状态语义对齐（无 initialize 握手） |

依赖方向严格单向：L4 → L2 → L1 → L0，L3 只被 L2/入口使用，L1 不认识 L3。
`scheduler` 不知道"签名"存在，`restart` 不知道"工具表"存在（通过回调与注册表解耦）。

## 关键设计取舍（先看这三条）

1. **沙箱单元不是线程，也不是线程池条目。** 它是一份 `UnitConfig` + 一组原子计数器（状态、
   心跳、并发配额、调用速率窗口）+ 一个代数（generation，用于让旧单元的迟到回调失效）。
   执行 handler 的仍是宿主的同一套 tokio 线程池——这正是"单 OS 进程内逻辑隔离"的物理含义。
2. **超时计时器与被测 handler 不在同一个 future 里。** handler 跑在独立 spawn 的 task 上，
   计时器在调用方 task 上 race，因此 handler 阻塞 poll 时计时器仍然能触发。这是能区分
   "可中断超时"和"不可中断超时"的前提。
3. **可中断性靠心跳判定，不靠"abort 之后它死没死"。** abort 只能在下一次 await 点生效，
   所以 `heartbeat` 在超时时刻是否前进，就是"可中断 / 不可中断"的判据。
