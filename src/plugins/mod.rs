pub mod adapter;
pub mod archive;
pub mod discovery;
pub mod import;
pub mod manifest;
pub mod process;
pub mod registry;

use std::fs;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};

static GLOBAL_MANAGER: OnceLock<RwLock<Option<process::PluginManager>>> = OnceLock::new();

fn configured_root() -> PathBuf {
    let config = crate::config::load();
    if config.plugins.root.trim().is_empty() {
        crate::transfer::data_dir().join("plugins")
    } else {
        PathBuf::from(config.plugins.root.trim())
    }
}

/// 创建数据目录插件管理器并发现 registry 中已启用的插件。
/// 返回的警告只影响对应插件，不能阻断应用启动。
pub fn initialize() -> Result<Vec<String>> {
    let global = GLOBAL_MANAGER.get_or_init(|| RwLock::new(None));
    let mut slot = global
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(manager) = slot.as_ref() {
        return Ok(discovery::discover(manager));
    }
    let manager = process::PluginManager::new(configured_root())?;
    let warnings = discovery::discover(&manager);
    *slot = Some(manager);
    Ok(warnings)
}

pub fn manager() -> Result<process::PluginManager> {
    let global = GLOBAL_MANAGER.get_or_init(|| RwLock::new(None));
    global
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .context("插件管理器尚未初始化")
}

#[derive(Clone)]
pub struct InstalledPlugin {
    pub id: String,
    pub label: String,
    pub version: String,
    pub root: String,
    pub enabled: bool,
    pub can_write: bool,
    pub variants: usize,
    pub icon: Vec<u8>,
}

/// 插件页数据：registry 是权威列表，已成功发现的 agent 提供显示名、图标与变体数。
pub fn installed() -> Result<Vec<InstalledPlugin>> {
    let manager = manager()?;
    let registry = manager.registry().load()?;
    let mut list: Vec<InstalledPlugin> = registry
        .plugins
        .into_iter()
        .map(|(id, registered)| {
            let info = crate::agents::get(&id);
            let package_root = manager.root().join(&registered.root);
            let parsed = if info.is_none() {
                manifest::PluginManifest::load(&package_root).ok()
            } else {
                None
            };
            let icon = info
                .as_ref()
                .map(|info| info.icon.to_vec())
                .or_else(|| {
                    parsed
                        .as_ref()
                        .and_then(|manifest| manifest.icon_bytes(&package_root).ok())
                })
                .unwrap_or_default();
            InstalledPlugin {
                label: info
                    .as_ref()
                    .map(|info| info.label.to_string())
                    .or_else(|| parsed.as_ref().map(|manifest| manifest.label.clone()))
                    .unwrap_or_else(|| id.clone()),
                can_write: info
                    .as_ref()
                    .map(|info| info.can_write)
                    .or_else(|| parsed.as_ref().map(|manifest| manifest.can_write))
                    .unwrap_or(false),
                variants: info
                    .as_ref()
                    .and_then(|info| info.manifest.as_ref())
                    .map(|manifest| manifest.variants.len())
                    .or_else(|| parsed.as_ref().map(|manifest| manifest.variants.len()))
                    .unwrap_or_default(),
                icon,
                id,
                version: registered.registration.version,
                root: registered.root,
                enabled: registered.enabled,
            }
        })
        .collect();
    list.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(list)
}

/// 从压缩包导入；调用方负责提示用户插件将按当前用户权限运行。
pub fn import_archive(source: &std::path::Path) -> Result<discovery::DiscoveredPlugin> {
    initialize()?;
    let manager = manager()?;
    import::import_archive(source, &manager)
}

/// 启用或停用插件。停用只移出运行时注册表，插件包与 registry 记录保留。
pub fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    let manager = manager()?;
    manager.registry().set_enabled(id, enabled)?;
    if enabled {
        let loaded = (|| -> Result<()> {
            let registry = manager.registry().load()?;
            let entry = registry
                .plugins
                .get(id)
                .with_context(|| format!("插件未安装：{id}"))?
                .clone();
            let plugin = discovery::load_registered(&manager, &entry)?;
            crate::agents::upsert_external(plugin.info)?;
            Ok(())
        })();
        if let Err(error) = loaded {
            let _ = manager.registry().set_enabled(id, false);
            return Err(error);
        }
    } else {
        crate::agents::remove(id);
    }
    Ok(())
}

/// 卸载插件：删除插件包目录、registry 记录与运行时条目。
pub fn uninstall(id: &str) -> Result<()> {
    if !valid_plugin_id(id) {
        anyhow::bail!("插件 id 无效：{id}");
    }
    let manager = manager()?;
    let registry = manager.registry().load()?;
    if !registry.plugins.contains_key(id) {
        anyhow::bail!("插件未安装：{id}");
    }
    let package = manager.root().join(id);
    if package.exists() {
        fs::remove_dir_all(&package)
            .with_context(|| format!("删除插件目录失败：{}", package.display()))?;
    }
    manager.registry().remove(id)?;
    crate::agents::remove(id);
    Ok(())
}

fn valid_plugin_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// 应用退出前调用：拒绝新请求，等待或终止所有由本进程启动的插件，再释放 Job Object。
pub fn shutdown() -> Result<()> {
    let global = GLOBAL_MANAGER.get_or_init(|| RwLock::new(None));
    let manager = {
        let mut slot = global
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.take()
    };
    if let Some(manager) = manager {
        manager.shutdown()?;
    }
    Ok(())
}
