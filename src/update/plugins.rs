//! 插件在线更新的下载逻辑；导入复用宿主现有严格导入入口。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{download_to, DownloadProgress, HashMismatch, PluginEntry, PluginIndex};

pub fn download_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("updates").join("plugins")
}

/// 插件索引本地缓存：网络失败时未安装列表仍可展示上次成功结果。
pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins-index-cache.json")
}

pub fn load_index_cache(data_dir: &Path) -> Option<PluginIndex> {
    std::fs::read_to_string(cache_path(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

pub fn save_index_cache(data_dir: &Path, index: &PluginIndex) -> Result<()> {
    let text = serde_json::to_string_pretty(index).context("序列化插件索引缓存失败")?;
    crate::jsonl::write_atomic(&cache_path(data_dir), &text).context("写入插件索引缓存失败")
}

/// 重新获取在线索引并取回指定插件条目；zip_url 按当前索引地址展开。
pub fn refresh_entry(id: &str) -> Result<PluginEntry> {
    let index_url = crate::update::plugin_index_url();
    let index = crate::update::check_plugins(&index_url)?;
    let mut resolved = index;
    crate::update::resolve_plugin_index(&index_url, &mut resolved)?;
    resolved
        .plugins
        .into_iter()
        .find(|plugin| plugin.id == id)
        .with_context(|| format!("插件索引中没有 {id}"))
}

/// 下载插件 zip 到数据目录；返回下载后的路径。
/// 哈希不匹配时自动刷新在线索引重试一次——缓存索引可能落后于
/// 服务器上重传过的产物（重打包后 SHA-256 变化）。刷新后的条目
/// 哈希与旧条目相同（服务器产物与索引一致，属于真实损坏）或刷新
/// 失败（网络仍不可达）时，按原始失配错误上报。
pub fn download(
    entry: &PluginEntry,
    data_dir: &Path,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    download_with_refresh(entry, data_dir, &mut progress, &refresh_entry)
}

fn download_with_refresh(
    entry: &PluginEntry,
    data_dir: &Path,
    progress: &mut impl FnMut(DownloadProgress),
    refresh: &dyn Fn(&str) -> Result<PluginEntry>,
) -> Result<PathBuf> {
    match download_once(entry, data_dir, progress) {
        Err(error) if error.is::<HashMismatch>() => match refresh(&entry.id) {
            Ok(fresh) if !fresh.sha256.eq_ignore_ascii_case(&entry.sha256) => {
                download_once(&fresh, data_dir, progress)
            }
            Ok(_) => Err(error),
            Err(refresh_error) => Err(error.context(format!(
                "缓存索引与服务器产物不一致，刷新索引失败：{refresh_error:#}"
            ))),
        },
        other => other,
    }
}

fn download_once(
    entry: &PluginEntry,
    data_dir: &Path,
    progress: &mut impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let dest = download_dir(data_dir).join(format!("{}-{}.zip", entry.id, entry.version));
    download_to(&entry.zip_url, &dest, &entry.sha256, entry.size, |p| {
        progress(p)
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::test_util::serve_once;
    use sha2::Digest;

    #[test]
    fn download_path_uses_id_and_version() {
        let dir = Path::new(r"C:\data");
        assert_eq!(
            download_dir(dir).join("trae-0.1.6.zip"),
            Path::new(r"C:\data\updates\plugins\trae-0.1.6.zip")
        );
    }

    /// 唯一临时目录；测试结束手动清理。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "asm-plugin-{name}-{}-{}",
            std::process::id(),
            crate::ir::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str, version: &str, url: String, sha256: String, size: u64) -> PluginEntry {
        PluginEntry {
            id: id.into(),
            label: "Trae CN".into(),
            version: version.into(),
            zip_url: url,
            sha256,
            size,
            min_host: String::new(),
        }
    }

    #[test]
    fn download_retries_with_refreshed_entry_after_hash_mismatch() {
        // 服务器已被发布端重传：内容变了，缓存条目的哈希指向旧字节。
        let content = b"republished-payload".to_vec();
        let fresh_sha = format!("{:x}", sha2::Sha256::digest(&content));
        let (stale_url, stale_server) = serve_once(content.clone());
        let (fresh_url, fresh_server) = serve_once(content.clone());
        let dir = temp_dir("retry");
        let stale = entry(
            "trae",
            "0.1.0",
            stale_url,
            format!("{:x}", sha2::Sha256::digest(b"old-payload")),
            content.len() as u64,
        );
        let fresh = entry("trae", "0.1.0", fresh_url, fresh_sha, content.len() as u64);
        let refresh = move |id: &str| {
            if id == "trae" {
                Ok(fresh.clone())
            } else {
                anyhow::bail!("插件索引中没有 {id}")
            }
        };
        let result = download_with_refresh(&stale, &dir, &mut |_| {}, &refresh);
        stale_server.join().unwrap();
        fresh_server.join().unwrap();
        let dest = result.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), content);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn download_keeps_mismatch_error_when_refresh_returns_same_hash() {
        let content = b"republished-payload".to_vec();
        let (url, server) = serve_once(content);
        let dir = temp_dir("same-hash");
        // 服务器内容变了但索引没跟上：刷新后的哈希与缓存一致，重试无意义。
        let stale = entry(
            "trae",
            "0.1.0",
            url,
            format!("{:x}", sha2::Sha256::digest(b"old-payload")),
            0,
        );
        let same = stale.clone();
        let refresh = move |id: &str| {
            if id == "trae" {
                Ok(same.clone())
            } else {
                unreachable!()
            }
        };
        let error = download_with_refresh(&stale, &dir, &mut |_| {}, &refresh).unwrap_err();
        server.join().unwrap();
        assert!(error.is::<HashMismatch>());
        assert!(error.to_string().contains("SHA-256"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn download_wraps_mismatch_when_refresh_fails() {
        let content = b"republished-payload".to_vec();
        let (url, server) = serve_once(content);
        let dir = temp_dir("refresh-fail");
        let stale = entry(
            "trae",
            "0.1.0",
            url,
            format!("{:x}", sha2::Sha256::digest(b"old-payload")),
            0,
        );
        let error = download_with_refresh(&stale, &dir, &mut |_| {}, &|_id| {
            anyhow::bail!("网络请求失败")
        })
        .unwrap_err();
        server.join().unwrap();
        let chain = format!("{error:#}");
        assert!(chain.contains("刷新索引失败"), "chain: {chain}");
        assert!(chain.contains("更新文件 SHA-256 不匹配"), "chain: {chain}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_keeps_relative_zip_urls_until_resolved() {
        let dir = std::env::temp_dir().join(format!(
            "asm-index-cache-{}-{}",
            std::process::id(),
            crate::ir::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let index = PluginIndex {
            schema: 1,
            generated_at: String::new(),
            plugins: vec![PluginEntry {
                id: "trae".into(),
                label: "Trae CN".into(),
                version: "0.1.6".into(),
                zip_url: "trae-0.1.6.zip".into(),
                sha256: "a".repeat(64),
                size: 1,
                min_host: String::new(),
            }],
        };
        save_index_cache(&dir, &index).unwrap();
        let mut loaded = load_index_cache(&dir).unwrap();
        assert_eq!(loaded.plugins[0].zip_url, "trae-0.1.6.zip");
        crate::update::resolve_plugin_index(
            "http://127.0.0.1:8765/releases/download/plugins/plugins-index.json",
            &mut loaded,
        )
        .unwrap();
        assert_eq!(
            loaded.plugins[0].zip_url,
            "http://127.0.0.1:8765/releases/download/plugins/trae-0.1.6.zip"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
