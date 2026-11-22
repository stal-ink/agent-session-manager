//! App 自更新的下载与安装计划。
//! 替换运行中的 exe 由 `install` 生成的 cmd helper 在进程退出后完成。

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{download_to, file_name_from_url, AppManifest, DownloadProgress};

pub fn download_dir(data_dir: &Path, version: &str) -> PathBuf {
    data_dir.join("updates").join("app").join(version)
}

/// 下载新 exe 到数据目录；返回下载后的路径。
pub fn download(
    manifest: &AppManifest,
    data_dir: &Path,
    progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let file_name = file_name_from_url(&manifest.exe_url);
    let dest = download_dir(data_dir, &manifest.version).join(file_name);
    download_to(
        &manifest.exe_url,
        &dest,
        &manifest.sha256,
        manifest.size,
        progress,
    )?;
    Ok(dest)
}
