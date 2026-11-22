use std::sync::Arc;

use anyhow::Result;

use crate::agents::{self, AgentInfo};

use super::adapter::ProcessPluginAdapter;
use super::manifest::PluginManifest;
use super::process::PluginManager;
use super::registry::RegisteredPlugin;

pub struct DiscoveredPlugin {
    pub info: AgentInfo,
}

pub fn load_registered(
    manager: &PluginManager,
    registered: &RegisteredPlugin,
) -> Result<DiscoveredPlugin> {
    let package_root = manager.root().join(&registered.root);
    let manifest = PluginManifest::load(&package_root)?;
    if manifest.registration() != registered.registration {
        anyhow::bail!("插件包 {} 的 manifest 与注册表记录不一致", manifest.id);
    }
    let icon = manifest.icon_bytes(&package_root)?;
    let manifest = Arc::new(manifest);
    let adapter = Arc::new(ProcessPluginAdapter::new(
        manager.clone(),
        package_root,
        (*manifest).clone(),
        Vec::new(),
    ));
    let icon: Arc<[u8]> = Arc::from(icon);
    Ok(DiscoveredPlugin {
        info: AgentInfo::external_plugin(
            manifest.id.clone(),
            manifest.label.clone(),
            manifest.process.clone(),
            manifest.launch.clone(),
            icon,
            manifest.can_write,
            adapter,
            manifest,
        ),
    })
}

/// 启动时按 registry 只读发现外部插件；坏插件只记录警告，不阻断应用。
pub fn discover(manager: &PluginManager) -> Vec<String> {
    let mut warnings = Vec::new();
    let registry = match manager.registry().load() {
        Ok(registry) => registry,
        Err(error) => {
            warnings.push(format!("插件注册表不可读：{error:#}"));
            return warnings;
        }
    };
    for registered in registry.plugins.values() {
        if !registered.enabled {
            continue;
        }
        match load_registered(manager, registered) {
            Ok(plugin) => {
                if let Err(error) = agents::register_external(plugin.info) {
                    warnings.push(format!("插件注册失败：{error:#}"));
                }
            }
            Err(error) => warnings.push(format!("插件加载失败：{error:#}")),
        }
    }
    warnings
}
