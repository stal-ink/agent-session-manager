//! 插件在线更新的下载逻辑；导入复用宿主现有严格导入入口。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{download_to, DownloadProgress, PluginEntry, PluginIndex};

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

/// 下载插件 zip 到数据目录；返回下载后的路径。
pub fn download(
    entry: &PluginEntry,
    data_dir: &Path,
    progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let dest = download_dir(data_dir).join(format!("{}-{}.zip", entry.id, entry.version));
    download_to(&entry.zip_url, &dest, &entry.sha256, entry.size, progress)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_path_uses_id_and_version() {
        let dir = Path::new(r"C:\data");
        assert_eq!(
            download_dir(dir).join("trae-0.1.6.zip"),
            Path::new(r"C:\data\updates\plugins\trae-0.1.6.zip")
        );
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
