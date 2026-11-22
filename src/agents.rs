//! Agent 运行时注册表。具体存储实现和适配器都在独立插件包内；
//! 这里只做静态内置组合、动态插件注册与通用查询。

use std::sync::{Arc, OnceLock, RwLock};

pub use agent_core::AgentAdapter;

use crate::plugins::manifest::PluginManifest;

#[derive(Clone)]
pub struct AgentInfo {
    pub id: Arc<str>,
    pub label: Arc<str>,
    pub process: Arc<str>,
    pub launch: Arc<str>,
    pub icon: Arc<[u8]>,
    pub can_write: bool,
    pub adapter: Arc<dyn AgentAdapter>,
    pub manifest: Option<Arc<PluginManifest>>,
}

impl AgentInfo {
    pub fn external(
        id: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        process: impl Into<Arc<str>>,
        launch: impl Into<Arc<str>>,
        icon: impl Into<Arc<[u8]>>,
        can_write: bool,
        adapter: Arc<dyn AgentAdapter>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            process: process.into(),
            launch: launch.into(),
            icon: icon.into(),
            can_write,
            adapter,
            manifest: None,
        }
    }

    pub fn external_plugin(
        id: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        process: impl Into<Arc<str>>,
        launch: impl Into<Arc<str>>,
        icon: impl Into<Arc<[u8]>>,
        can_write: bool,
        adapter: Arc<dyn AgentAdapter>,
        manifest: Arc<PluginManifest>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            process: process.into(),
            launch: launch.into(),
            icon: icon.into(),
            can_write,
            adapter,
            manifest: Some(manifest),
        }
    }
}

static REGISTRY: OnceLock<RwLock<Vec<Arc<AgentInfo>>>> = OnceLock::new();

fn registry() -> &'static RwLock<Vec<Arc<AgentInfo>>> {
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn all() -> Vec<Arc<AgentInfo>> {
    registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn get(id: &str) -> Option<Arc<AgentInfo>> {
    let id = normalize(id);
    registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|agent| agent.id.as_ref() == id)
        .cloned()
}

/// 导入/升级插件后替换同 id 的运行时条目；其他插件仍走严格注册。
pub fn upsert_external(info: AgentInfo) -> anyhow::Result<()> {
    let id = normalize(&info.id);
    if id != info.id.as_ref() {
        anyhow::bail!("外部插件 id 必须为小写规范形式：{}", info.id);
    }
    let mut registry = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(slot) = registry.iter_mut().find(|agent| agent.id == info.id) {
        *slot = Arc::new(info);
    } else {
        registry.push(Arc::new(info));
    }
    Ok(())
}

/// 从运行时注册表移除 agent；停用或卸载插件时使用。
pub fn remove(id: &str) -> bool {
    let id = normalize(id);
    let mut registry = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = registry.len();
    registry.retain(|agent| agent.id.as_ref() != id);
    registry.len() != before
}

/// 外部插件注册入口。重复 id 不能静默覆盖；调用方负责记录警告。
pub fn register_external(info: AgentInfo) -> anyhow::Result<()> {
    let id = normalize(&info.id);
    if id != info.id.as_ref() {
        anyhow::bail!("外部插件 id 必须为小写规范形式：{}", info.id);
    }
    let mut registry = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if registry.iter().any(|agent| agent.id == info.id) {
        anyhow::bail!("agent id 已注册：{}", info.id);
    }
    registry.push(Arc::new(info));
    Ok(())
}

pub fn label(id: &str) -> String {
    get(id)
        .map(|agent| agent.label.to_string())
        .unwrap_or_else(|| id.to_string())
}

pub fn running(info: &AgentInfo) -> bool {
    info.adapter.tracks_process() && crate::transfer::process_running(&info.process)
}

pub fn normalize(id: &str) -> String {
    id.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let mut ids = Vec::new();
        for agent in all() {
            assert!(!agent.id.is_empty());
            assert!(!agent.label.is_empty());
            assert!(!agent.process.is_empty());
            assert!(!agent.launch.is_empty());
            assert!(!agent.icon.is_empty(), "{} icon must be embedded", agent.id);
            assert!(
                ids.iter().all(|existing| existing != &agent.id),
                "duplicate agent id {}",
                agent.id
            );
            ids.push(agent.id.clone());
            if agent.can_write {
                assert!(
                    !agent
                        .adapter
                        .backup_paths()
                        .expect("builtin backup paths")
                        .is_empty(),
                    "{} must declare backup paths",
                    agent.id
                );
            }
        }
        for id in ids {
            assert_eq!(
                normalize(id.as_ref()),
                id.as_ref(),
                "id must already be lowercase"
            );
        }
    }
}
