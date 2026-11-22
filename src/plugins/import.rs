use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::archive;
use super::discovery::{self, DiscoveredPlugin};
use super::manifest::PluginManifest;
use super::process::PluginManager;

/// 从压缩包导入插件：解压到 staging，严格校验后原子移动到
/// `<root>/<id>/<version>`，完成一次注册握手后才提交 registry。
/// 插件运行形态始终是普通目录。
pub fn import_archive(source: &Path, manager: &PluginManager) -> Result<DiscoveredPlugin> {
    let source = source.canonicalize()?;
    if !source.is_file() {
        anyhow::bail!("插件只从压缩包导入：{}", source.display());
    }
    let staging = manager
        .root()
        .join(".staging")
        .join(uuid::Uuid::new_v4().simple().to_string());
    fs::create_dir_all(&staging)?;

    if let Err(error) = archive::extract(&source, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.context("插件包解压失败"));
    }
    let package_root = match archive::package_root(&staging) {
        Ok(root) => root,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let manifest = match PluginManifest::load_strict(&package_root) {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.context("插件包校验失败"));
        }
    };
    let relative_root = Path::new(&manifest.id).join(&manifest.version);
    let target = manager.root().join(&relative_root);
    if target.exists() {
        let _ = fs::remove_dir_all(&staging);
        anyhow::bail!("插件版本目录已存在：{}", target.display());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(error) = fs::rename(&package_root, &target) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error).context("移动插件到安装目录失败");
    }
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }

    let relative = relative_root.to_string_lossy().replace('\\', "/");
    let registered = manifest.registration();
    let result = (|| -> Result<DiscoveredPlugin> {
        let described = manager.probe(&target, &manifest, "agent.describe")?;
        if !described_matches(&described, &manifest) {
            anyhow::bail!("插件 runtime 描述与 manifest 不一致");
        }
        let plugin = discovery::load_registered(
            manager,
            &super::registry::RegisteredPlugin {
                registration: registered.clone(),
                root: relative.clone(),
                enabled: true,
            },
        )?;
        manager.registry().activate(&registered, &relative_root)?;
        Ok(plugin)
    })();
    match result {
        Ok(plugin) => Ok(plugin),
        Err(error) => {
            let cleanup = fs::remove_dir_all(&target).err();
            if let Some(cleanup) = cleanup {
                return Err(error.context(format!("插件导入失败，且清理落盘目录失败：{cleanup}")));
            }
            Err(error.context("插件导入落盘后注册失败"))
        }
    }
}

fn described_matches(described: &serde_json::Value, manifest: &PluginManifest) -> bool {
    [
        ("id", &manifest.id),
        ("label", &manifest.label),
        ("process", &manifest.process),
        ("launch", &manifest.launch),
        ("version", &manifest.version),
    ]
    .into_iter()
    .all(|(field, expected)| {
        described.get(field).and_then(serde_json::Value::as_str) == Some(expected.as_str())
    })
}
