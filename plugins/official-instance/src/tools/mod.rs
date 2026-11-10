//! 工具模块。每个工具一个文件，导出 `handle`（业务）与 `def()`（元数据 + schema）。

pub mod create_instance;
pub mod delete_instance;
pub mod list_instances;
pub mod switch_instance;
