//! `detect_java`：扫描本机已安装的 Java 运行时。
//!
//! 无参数。直接调用 [`crate::java::find_java_installations`]，把每个候选的
//! 路径 / 版本 / 厂商 / 架构序列化返回。UI 用它填充 Java 选择下拉框。

use launcher_plugin_common::result::success;
use launcher_plugin_common::{tool_args, tool_def, PluginContext, PluginResult};
use serde_json::json;

tool_args! {
    /// `detect_java` 不需要任何参数。
    pub struct DetectJavaArgs {}
}

pub async fn handle(_ctx: PluginContext, _args: DetectJavaArgs) -> PluginResult {
    let runtimes = crate::java::find_java_installations();
    let list: Vec<serde_json::Value> = runtimes
        .iter()
        .map(|j| {
            json!({
                "path": j.path,
                "home": j.home,
                "version": j.version,
                "vendor": j.vendor,
                "arch": j.arch,
                "display": j.display(),
            })
        })
        .collect();
    Ok(success(json!({
        "count": list.len(),
        "runtimes": list,
    })))
}

pub fn def() -> launcher_plugin_common::ToolDef {
    tool_def! {
        name: "detect_java",
        description: "扫描本机已安装的 Java 运行时（PATH / JAVA_HOME / 常见安装目录 / 注册表）",
        destructive: false,
        timeout_ms: 30_000,
        args: DetectJavaArgs,
        handler: crate::tools::detect_java::handle
    }
}