//! 原子文件写入助手：所有插件存储共用的「写临时文件 → 改名」落盘逻辑。
//!
//! # 为什么集中在这里
//!
//! 三个插件（instance / launch / mod）各自实现过同一份 `save()`，报错时只有
//! `io::Error` 的字符串 —— 没有**失败的步骤**（建目录 / 写临时文件 / 改名），
//! 也没有**完整路径**。真机上报 `拒绝访问 (os error 5)` 时完全无法定位是哪一步。
//!
//! 本模块统一四件事：
//!
//! 1. **错误带上下文**：`<步骤> <完整路径> 失败：<os 错误>`；os error 5 额外
//!    附带自查提示（杀毒软件 / 受控文件夹访问拦截）；
//! 2. **唯一临时文件名**：`原名.<pid>.<毫秒>.tmp`——固定名 `.json.tmp` 若被
//!    上一次失败残留（只读 / 被锁 / 被沙箱虚拟化），后续所有写入都会以
//!    `os error 5` 永久失败；唯一名让历史残留无法阻塞新写入；
//! 3. **短暂重试**：杀毒软件 / Defender / 同步盘常会把新写的文件锁住几十毫秒；
//! 4. **降级兜底**：临时文件路径始终失败时，退回「直接覆盖写」——原子性换可用性。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 重试参数：杀毒软件的扫描锁通常在几十到几百毫秒内释放。
const RETRIES: usize = 3;
const RETRY_DELAY: Duration = Duration::from_millis(120);

fn io_err(step: &str, path: &Path, e: std::io::Error) -> String {
    let base = format!("{step} {} 失败：{e}", path.display());
    if e.raw_os_error() == Some(5) {
        format!(
            "{base}（Windows 或杀毒软件/防勒索防护拦截了写入：请把启动器加入杀毒软件白名单，或允许它写入 AppData）"
        )
    } else {
        base
    }
}

/// 唯一临时文件路径：`原名.<pid>.<unix毫秒>.tmp`。
fn unique_tmp(path: &Path) -> PathBuf {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{}.{}.tmp", std::process::id(), ms));
    path.with_file_name(name)
}

/// 带重试的一次写入（应对 AV 短暂锁）。
fn write_with_retry(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..=RETRIES {
        match std::fs::write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt < RETRIES {
                    std::thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(io_err("写临时文件", path, last.expect("至少尝试一次")))
}

/// 把 `bytes` 原子写入 `path`（写唯一临时文件 → rename）。
///
/// 失败时错误信息包含**失败的步骤**与**完整目标路径**，os error 5 附带自查提示。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_err("创建目录", parent, e))?;
    }

    let tmp = unique_tmp(path);
    match write_with_retry(&tmp, bytes) {
        Ok(()) => {}
        Err(tmp_err) => {
            // 降级兜底：临时文件路径被系统性拦截（如防勒索软件按目录名拦截新建
            // 文件）时，尝试直接覆盖写目标文件（已有文件通常在白名单内）。
            return match write_with_retry(path, bytes) {
                Ok(()) => Ok(()),
                Err(final_err) => Err(format!(
                    "{tmp_err}；降级直写 {p} 也失败：{f}",
                    p = path.display(),
                    f = final_err
                )),
            };
        }
    }

    // rename 偶发被 AV 锁打断：重试几次。
    let mut last: Option<std::io::Error> = None;
    for _ in 0..RETRIES {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(RETRY_DELAY);
            }
        }
    }
    let e = last.expect("至少重试一次");
    // 降级：直接覆盖写。非原子，但比「永远写不进去」好。
    match std::fs::write(path, bytes) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
        Err(e2) => Err(format!(
            "改名 {} → {} 失败：{e}；降级直写也失败：{e2}",
            tmp.display(),
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_overwrites() {
        let dir = crate::test_support::temp_path("atomic-write");
        let file = dir.join("a/b/store.json");
        atomic_write(&file, b"one").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        atomic_write(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        // 唯一临时文件不应残留（同名前缀检查）
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .collect();
        assert!(leftovers.is_empty(), "不应残留临时文件：{leftovers:?}");
    }

    #[test]
    fn tmp_names_are_unique() {
        let dir = crate::test_support::temp_path("atomic-unique");
        let file = dir.join("store.json");
        let a = unique_tmp(&file);
        std::thread::sleep(Duration::from_millis(2));
        let b = unique_tmp(&file);
        assert_ne!(a, b, "两次生成的临时名应不同");
    }
}
