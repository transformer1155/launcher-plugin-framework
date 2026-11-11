//! 远程 / 归档插件安装：GitHub 仓库、任意 tar.gz 直链。
//!
//! 支持三种来源写法（插件页「远程安装」输入框）：
//! - `owner/repo` —— GitHub 仓库，自动下 `codeload.github.com/.../tar.gz/HEAD`；
//! - `https://github.com/owner/repo(/tree/branch)` —— 同上；
//! - `https://任意主机/xxx.tar.gz|.tgz` —— 直链归档（其他远程托管平台）。
//!
//! 解压后递归找 `plugin.json`，整个目录拷进插件目录，即完成安装（第三方插件，
//! 可在插件页禁用/卸载；官方前缀插件无法被冒名覆盖——目标目录已存在时拒绝安装）。

use std::path::Path;

/// 解析来源写法 → `(归档下载直链, 插件名候选)`。
pub fn resolve_remote(source: &str) -> Result<(String, String), String> {
    let s = source.trim();
    if s.is_empty() {
        return Err("请填写来源：owner/repo 或 https://…/xxx.tar.gz".to_owned());
    }

    // 形如 owner/repo（无 scheme、恰好一个 '/'）
    if !s.contains("://") {
        let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            let (o, r) = (parts[0], parts[1].trim_end_matches(".git"));
            return Ok((
                format!("https://codeload.github.com/{o}/{r}/tar.gz/HEAD"),
                r.to_owned(),
            ));
        }
        return Err(format!(
            "无法识别的来源「{s}」：GitHub 仓库填 owner/repo，直链填完整 https URL"
        ));
    }

    if !s.starts_with("https://") {
        return Err("仅支持 https 直链（避免明文下载被篡改）".to_owned());
    }

    // github.com 页面链接 → 转 codeload 归档
    if let Some(rest) = s.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 {
            let (o, r) = (parts[0], parts[1].trim_end_matches(".git"));
            return Ok((
                format!("https://codeload.github.com/{o}/{r}/tar.gz/HEAD"),
                r.to_owned(),
            ));
        }
        return Err("GitHub 链接需要包含 owner/repo".to_owned());
    }

    // 其他直链：必须是 tar.gz / tgz
    let lower = s.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let name = s
            .rsplit('/')
            .next()
            .unwrap_or("plugin")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".tgz")
            .trim_end_matches(".tar")
            .to_owned();
        return Ok((s.to_owned(), name));
    }
    Err("直链目前支持 .tar.gz / .tgz 归档（zip 暂不支持）；GitHub 仓库可直接填 owner/repo".to_owned())
}

/// 把 tar.gz 字节解到 `dest_root`，返回解压根目录。
/// 兼容两种布局：归档顶层就是插件目录（GitHub 默认 `repo-main/`）或直接是插件内容。
pub fn extract_tar_gz(bytes: &[u8], dest_root: &Path) -> Result<std::path::PathBuf, String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(false);
    std::fs::create_dir_all(dest_root).map_err(|e| format!("创建临时目录失败：{e}"))?;
    archive
        .unpack(dest_root)
        .map_err(|e| format!("解压失败（仅支持 tar.gz）：{e}"))?;

    // 顶层如果只有一个目录（GitHub 归档常态），进到它里面。
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dest_root)
        .map_err(|e| format!("读取解压结果失败：{e}"))?
        .flatten()
        .map(|e| e.path())
        .collect();
    if entries.len() == 1 && entries[0].is_dir() {
        let inner = entries.remove(0);
        if inner.join("plugin.json").exists() {
            return Ok(inner);
        }
        // 再下一层找（如 repo/pages/xxx）
        if let Ok(sub) = std::fs::read_dir(&inner) {
            for e in sub.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("plugin.json").exists() {
                    return Ok(p);
                }
            }
        }
        return Ok(inner);
    }

    // 顶层散文件布局：直接在顶层找 plugin.json，否则递归一层找。
    if dest_root.join("plugin.json").exists() {
        return Ok(dest_root.to_path_buf());
    }
    for e in std::fs::read_dir(dest_root)
        .map_err(|e| format!("读取解压结果失败：{e}"))?
        .flatten()
    {
        let p = e.path();
        if p.is_dir() && p.join("plugin.json").exists() {
            return Ok(p);
        }
    }
    Err("归档里没有找到 plugin.json（它不是一个插件包）".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_owner_repo() {
        let (url, name) = resolve_remote("someuser/some-plugin").unwrap();
        assert_eq!(url, "https://codeload.github.com/someuser/some-plugin/tar.gz/HEAD");
        assert_eq!(name, "some-plugin");
    }

    #[test]
    fn resolves_github_page_url() {
        let (url, name) =
            resolve_remote("https://github.com/foo/bar-plugin/tree/main").unwrap();
        assert_eq!(url, "https://codeload.github.com/foo/bar-plugin/tar.gz/HEAD");
        assert_eq!(name, "bar-plugin");
    }

    #[test]
    fn resolves_direct_tarball() {
        let (url, name) =
            resolve_remote("https://example.com/releases/cool-pack.tar.gz").unwrap();
        assert_eq!(url, "https://example.com/releases/cool-pack.tar.gz");
        assert_eq!(name, "cool-pack");
    }

    #[test]
    fn rejects_zip_and_plain_http() {
        assert!(resolve_remote("https://x.com/a.zip").is_err());
        assert!(resolve_remote("http://x.com/a.tar.gz").is_err());
        assert!(resolve_remote("").is_err());
    }
}
