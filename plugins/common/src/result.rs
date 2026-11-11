//! 结果构造与错误分类。
//!
//! # 两类失败的边界（这是本文件唯一真正重要的事）
//!
//! 框架的 [`launcher_plugin_framework::types::ToolResult`] 已经区分了两种 `isError: true`：
//!
//! | 来源 | 形状 | 是否带 `code` | 用途 |
//! |---|---|---|---|
//! | **业务失败** | `Ok(ToolResult::business_error(..))` | 否（或带插件自定义 code） | "实例不存在""Mod 未安装" |
//! | **框架失败** | `Err(ToolError)` | 是（10 个固定码之一） | 参数非法 / 权限不足 / 沙箱崩溃 |
//!
//! 规格的 handler 签名是 `Result<ToolResult, ToolError>`，所以：
//!
//! - **成功** → `Ok(success(..))`
//! - **业务失败** → `Ok(business(..))`（用 [`biz_fail!`] 宏，读起来像 `return Err`）
//! - **框架失败** → `Err(ctx.err(InvalidArguments, ..))`
//!
//! 为什么业务失败不塞进 `Err(ToolError)`：错误码表是**框架**的封闭枚举
//! （`ToolNotFound`/`PermissionDenied`/…），"实例 ID 不存在"套哪个码都是错的 ——
//! 套 `ToolNotFound` 会让 UI 以为插件没装，套 `ProcessFailed` 会让用户去重启插件。
//! 硬塞进去就等于在协议层撒谎。

use launcher_plugin_framework::error::{ToolError, ToolErrorCode};
use launcher_plugin_framework::types::ToolResult;
use serde_json::{json, Value};

/// 插件 handler 的返回类型（与规格签名一致）。
pub type PluginResult = Result<ToolResult, ToolError>;

/// 成功：返回结构化 JSON。
pub fn success(value: Value) -> ToolResult {
    ToolResult::success(value)
}

/// 成功：返回一段文本（同时带 `structuredContent: {text}`，纯文本客户端也能用）。
pub fn success_text(text: impl Into<String>) -> ToolResult {
    ToolResult::success_text(text)
}

/// 成功：只返回 `{ ok: true }` 的常见形状。
pub fn ok() -> ToolResult {
    ToolResult::success(json!({ "ok": true }))
}

/// **业务失败**。`code` 是插件自己的词汇（例如 `"instance_not_found"`），
/// 与框架的 10 个错误码是**两套词表**，不要混用。
///
/// 返回的 `ToolResult` 里 `isError: true`，`structuredContent.code` 为插件自定义值。
/// MCP 调用方按 `isError` 判断失败、按 `code` 分辨原因 —— 与沙箱崩溃的协议形状同构。
pub fn business(code: &str, message: impl Into<String>) -> ToolResult {
    let message = message.into();
    ToolResult {
        content: vec![launcher_plugin_framework::types::ContentBlock::Text {
            text: message.clone(),
        }],
        structured_content: Some(json!({
            "code": code,
            "message": message,
        })),
        is_error: true,
    }
}

/// 便捷枚举：常用业务错误码。
///
/// 集中定义是为了让 `list_instances`/`switch_instance` 等工具对同一个失败
/// 使用**同一个 code** —— 否则 UI 就得猜两套拼写。
pub mod biz_codes {
    /// 请求的实体不存在。
    pub const NOT_FOUND: &str = "not_found";
    /// 实体已存在（创建重名）。
    pub const ALREADY_EXISTS: &str = "already_exists";
    /// 前置条件不满足（例如实例没有安装 Mod 加载器却要装 Mod）。
    pub const PRECONDITION_FAILED: &str = "precondition_failed";
    /// 状态冲突（例如正在运行时不允许切换）。
    pub const CONFLICT: &str = "conflict";
    /// 存储读写失败（IO / 序列化）。
    pub const STORAGE_ERROR: &str = "storage_error";
    /// 外部服务失败（网络 / 上游 5xx）。
    pub const UPSTREAM_ERROR: &str = "upstream_error";
    /// 配额或数量上限。
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
    /// 功能未实现（明确未实现的占位，不能假装成功）。
    pub const UNSUPPORTED: &str = "unsupported";
    /// 启动失败（spawn 进程失败 / Java 不可用）。
    pub const LAUNCH_FAILED: &str = "launch_failed";
}

/// 提前返回一个业务失败。
///
/// ```ignore
/// biz_fail!(biz_codes::NOT_FOUND, "实例 {id} 不存在");
/// ```
///
/// 展开为 `return Ok(business(..))` —— 读起来像 `return Err`，但走的是业务失败通道。
#[macro_export]
macro_rules! biz_fail {
    ($code:expr, $($msg:tt)*) => {
        return ::core::result::Result::Ok($crate::result::business($code, format!($($msg)*)))
    };
}

/// 提前返回一个**框架级**失败（参数非法 / 权限 / 状态）。
///
/// ```ignore
/// framework_fail!(ctx, ToolErrorCode::InvalidArguments, "loader 必须是 vanilla/fabric/forge");
/// ```
#[macro_export]
macro_rules! framework_fail {
    ($ctx:expr, $code:expr, $($msg:tt)*) => {
        return ::core::result::Result::Err($ctx.err($code, format!($($msg)*)))
    };
}

/// 存储层错误 → 框架无关的业务错误。
///
/// IO 失败是**插件自己的问题**（数据目录坏了），不该表现为框架的 `ProcessFailed`
/// （那会让重启管理器去重启插件，但重启并不能修好磁盘）。
pub fn storage_error(e: impl std::fmt::Display) -> ToolResult {
    business(biz_codes::STORAGE_ERROR, format!("存储读写失败：{e}"))
}

/// 反序列化失败 → 业务错误（数据文件损坏）。
pub fn corrupt_data(what: &str, e: impl std::fmt::Display) -> ToolResult {
    business(
        biz_codes::STORAGE_ERROR,
        format!("{what} 数据损坏，无法解析：{e}"),
    )
}

/// 参数非法 → 框架级错误（`InvalidArguments`）。规格的 10 码里有这一条，应当用它。
pub fn invalid_args(ctx: &crate::context::PluginContext, message: impl Into<String>) -> ToolError {
    ctx.err(ToolErrorCode::InvalidArguments, message)
}

/// 把 `PluginResult` 收敛成框架要的 `ToolResult`（框架 handler 只有这一个出口）。
///
/// 这是 `Err(ToolError)` 变成 `isError: true` 的**唯一**位置，保证不会漏掉某条错误路径。
pub fn into_framework_result(r: PluginResult) -> ToolResult {
    match r {
        Ok(tr) => tr,
        Err(e) => ToolResult::error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_error_is_isomorphic_with_business_shape() {
        let r = business(biz_codes::NOT_FOUND, "实例不存在");
        assert!(r.is_error);
        let sc = r.structured_content.unwrap();
        assert_eq!(sc["code"], "not_found");
        assert_eq!(sc["message"], "实例不存在");
        // 内容块必须是 Text，纯文本客户端可用
        assert!(matches!(
            r.content[0],
            launcher_plugin_framework::types::ContentBlock::Text { .. }
        ));
    }

    #[test]
    fn framework_error_carries_framework_code() {
        let ctx = crate::context::PluginContext::for_test("demo");
        let e = invalid_args(&ctx, "缺少 name");
        assert_eq!(e.code, ToolErrorCode::InvalidArguments);
        assert_eq!(e.plugin_id, "demo");
    }

    #[test]
    fn into_framework_result_maps_err_to_iserror() {
        let ctx = crate::context::PluginContext::for_test("demo");
        let r = into_framework_result(Err(invalid_args(&ctx, "x")));
        assert!(r.is_error);
        assert_eq!(
            r.structured_content.as_ref().unwrap()["code"],
            "InvalidArguments"
        );
    }

    #[test]
    fn into_framework_result_passes_success_through() {
        let r = into_framework_result(Ok(ok()));
        assert!(!r.is_error);
        assert_eq!(r.structured_content.unwrap()["ok"], true);
    }

    #[test]
    fn biz_codes_are_distinct() {
        // 防手滑：两个常量写成同一个字符串会让 UI 无法区分。
        let all = [
            biz_codes::NOT_FOUND,
            biz_codes::ALREADY_EXISTS,
            biz_codes::PRECONDITION_FAILED,
            biz_codes::CONFLICT,
            biz_codes::STORAGE_ERROR,
            biz_codes::UPSTREAM_ERROR,
            biz_codes::LIMIT_EXCEEDED,
            biz_codes::UNSUPPORTED,
        ];
        let mut set = std::collections::BTreeSet::new();
        for c in all {
            assert!(set.insert(c), "业务错误码重复：{c}");
        }
    }
}
