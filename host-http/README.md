# 网络实现（host-http）

本文说明宿主侧 HTTP 实现的边界、技术选型与安全设计。

---

## 1. 为什么需要它，以及它为什么不在框架 `src/` 里

框架 README 第 3 行写着：

> 单 OS 进程、无子进程、**无 HTTP / socket / 管道**。插件间通信全部是宿主进程内的异步函数调用。

真实现网络**必然要建出站 TCP/TLS 连接**，看起来与这句话冲突。

**裁决（用户已明确确认）：允许出站 TCP，实现为宿主侧独立 crate。**

理由：

1. 那句话的落点是**后半句** —— 约束的是**插件之间**怎么通信（不许用 HTTP/socket/管道互调），
   不是"整个进程永不允许出站连接"。
2. 插件规格原文要求 `ctx.http`，并说"框架层已声明域名白名单机制"——**设计上就预期有真实网络**。
3. `plugins/common/src/context.rs` 的 `HttpPort` 注释写着"**宿主必须实现**（框架层没有网络栈）"，
   这正是留给宿主的接口。
4. 出站 TCP 不产生额外 OS 进程，不破坏"单进程"这一核心约束。

因此依赖方向是：

```
launcher-plugin-framework/src/     框架核心（零网络）
        ↑ 依赖
plugins/common/                    插件共享层（定义 HttpPort 抽象，零网络）
        ↑ 依赖
host-http/          ← 唯一允许建出站 TCP
        ↑ 被依赖（仅 dev-dependency）
plugins/official-mod/              业务插件（通过 ctx.http() 间接使用）
```

**插件拿不到 `std::net`**，只能通过 `HttpPort` 这个受控端口访问网络。

---

## 2. 技术选型：为什么手写 HTTP/1.1

本机环境限制（实测逐条验证）：

| 方案 | 结果 | 原因 |
|---|---|---|
| `ureq` + `tls` feature | ❌ | 硬依赖 `ring` → 需要 C 编译器（本机无 MSVC/gcc） |
| `native-tls`（schannel） | ❌ | 依赖 `windows-sys` → 需要 `dlltool`，本机 rustup 自带的 `dlltool` 有 `CreateProcess` 缺陷 |
| `ureq` + `rustls` + aws-lc-rs | ❌ | `aws-lc-sys` 同样需要 C 工具链 |
| **`rustls` + `rustls-rustcrypto`** | ✅ | **纯 Rust**，无 C 依赖 |
| **`webpki-roots`** | ✅ | 内置根证书，不依赖系统证书库 |

最终实现：`rustls`（TLS 协议）+ `rustls-rustcrypto`（纯 Rust 加密）+ `webpki-roots`（根证书）
+ **手写最小 HTTP/1.1 客户端**（约 150 行）。

**已验证真实可用**：与 `api.modrinth.com` 完成 TLS 握手并取回 `HTTP/1.1 200 OK`（3675 字节 JSON）。

---

## 3. 安全设计（按重要性）

### 3.1 白名单在最外层，且**不跟随重定向**

本实现**完全不实现重定向跟随**：收到 3xx 就把状态码原样交给调用方。

| 配置 | 重定向行为 | 白名单是否被绕过 |
|---|---|---|
| 本实现（不跟随） | 返回 3xx，不跳转 | **不会** |
| `ureq` 默认（跟随） | 自动跳到新域名 | **会**（新域名未经校验） |

场景：一个**在白名单内**的域名返回 `302 → evil.com`。若跟随重定向，实际请求就落到了
未经校验的域名，白名单形同虚设。

### 3.2 URL 解析：userinfo 必须在切端口**之前**检查

`https://user:pass@evil.com/x` 的 authority 是 `user:pass@evil.com`。

如果先 `split(':').next()`：

```
host_port = "user:pass@evil.com"
split(':').next() = "user"          ← 错误！把 userinfo 当主机名
```

后果是：白名单校验认为要连 `user`，而实际语义指向 `evil.com` —— **这是一条 SSRF 绕过**。

正确做法：先在整个 authority 上查 `@`，再切端口。实现里已修正，并有测试
`rejects_userinfo_shape` 覆盖。

### 3.3 只接受 `https://`

明文 `http://` 与其他 scheme 一律拒绝，避免"白名单通了但走的是明文"这种降级。

### 3.4 响应体上限，且**不信任 `Content-Length`**

- 按 `DEFAULT_MAX_BODY_BYTES`（4 MB）截断；
- `Content-Length` 可以是伪造的：声称 999999999 字节但实际只有 6 字节，实现按 `max_body` 截断；
- `chunked` 编码按块累加，累计超限即中止。

### 3.5 超时

连接与读写都有超时（`TcpStream::set_read_timeout/set_write_timeout`）。
请求跑在 `spawn_blocking` 里，所以**必须**靠 socket 超时而不是 tokio timeout
（后者无法中止阻塞线程）。

### 3.6 `spawn_blocking`

整个 TLS 握手是同步阻塞的。若直接在 async 上下文里跑，会占死一个 worker 线程，
让同进程里的沙箱超时计时器停摆（框架的"逻辑隔离"名存实亡）。

---

## 4. 真实网络暴露出的一个 bug（值得记录）

`official-mod` 的 `search_mods` 此前只用 mock `HttpPort` 测试，全部通过。
接入真实网络后立刻返回 **HTTP 400**。

根因：**facet 结构未做百分号编码**。

```rust
// 错误（导致 400）
facets=[["categories:fabric"],["versions:1.20.1"]]

// 正确（返回 200）
facets=%5B%5B%22categories%3Afabric%22%5D%5B%22versions%3A1.20.1%22%5D%5D
```

实测对比：

| URL | Modrinth 响应 |
|---|---|
| 未编码 facet | `400 Bad Request` |
| 编码 facet | `200 OK` |
| 无 facet | `200 OK` |

**这个 bug mock 测试永远测不出来** —— 因为 mock 不解析 URL，直接返回固定 JSON。
已修复，并加了回归测试 `facets_are_percent_encoded_end_to_end`（断言 URL 里不含裸 `[` `]` `"`）。

---

## 5. 已知限制

| 限制 | 说明 |
|---|---|
| **不做证书固定** | 依赖 `webpki-roots` 内置根证书（可信来源，但非固定到特定证书） |
| **不支持 HTTP/2** | 只做 HTTP/1.1（Modrinth 两者都支持） |
| **不做 per-host 限流** | 429 原样交给调用方，由上层决定退避策略 |
| **不支持非 ASCII 请求路径** | 当前调用方只打 Modrinth 的 ASCII API |
| **无连接池** | 每次请求新建 TCP+TLS（Modrinth 搜索频率低，可接受） |
| **`rustls-rustcrypto` 是 alpha 版** | `0.0.2-alpha`。生产前应评估替换为成熟 provider（届时需要 C 工具链） |

---

## 6. 运行真实网络测试

默认全部 `#[ignore]`（CI 无网络时不应失败）。手动运行：

```bash
# host-http 自身的真实网络测试（3 个）
cargo test -p host-http -- --ignored --nocapture

# official-mod 的真实端到端（5 个）
cargo test -p official-mod --test real_network -- --ignored --nocapture
```

后者覆盖：真实搜索、limit 生效、vanilla loader、白名单拦截、**跨插件校验 + 真实网络共存**。

---

## 7. 宿主如何接入

```rust
use launcher_plugin_common::PluginContext;
use official_mod::build_with_logger;

// 1) 从插件清单读白名单（唯一真相源）
let manifest = PluginManifestV1::parse(include_bytes!("official-mod/plugin.json"))?;
let decl = manifest.network.expect("该插件需要网络");

// 2) 真实传输层 + 白名单包装
let port = host_http::allowlisted(&decl);

// 3) 注入插件上下文
let (ctx, toolset) = official_mod::build_with_logger(data_dir, logger);
let ctx = ctx.with_http(port);

// 4) 启动时可选做一次可达性自检
if let Err(e) = host_http::probe("api.modrinth.com") {
    eprintln!("网络不可用，Mod 搜索将失败：{e}");
}
```
