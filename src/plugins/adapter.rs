use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::agents::AgentAdapter;
use crate::ir::{Session, SessionSummary};
use agent_core::adapter::AgentListing;
use agent_core::protocol::PluginWarning;
use agent_core::CloseBehavior;

use super::manifest::PluginManifest;
use super::process::PluginManager;

#[derive(Clone)]
pub struct ProcessPluginAdapter {
    manager: PluginManager,
    root: Arc<PathBuf>,
    manifest: Arc<PluginManifest>,
    envs: Arc<Vec<(String, String)>>,
}

impl ProcessPluginAdapter {
    pub fn new(
        manager: PluginManager,
        root: PathBuf,
        manifest: PluginManifest,
        envs: Vec<(String, String)>,
    ) -> Self {
        Self {
            manager,
            root: Arc::new(root),
            manifest: Arc::new(manifest),
            envs: Arc::new(envs),
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.manager
            .request(&self.root, &self.manifest, &self.envs, method, params)
            .with_context(|| format!("插件 {} 调用 {method} 失败", self.manifest.id))
    }

    fn require(&self, capability: bool, method: &str) -> Result<()> {
        if capability {
            Ok(())
        } else {
            anyhow::bail!("插件 {} 未启用 {method} 能力", self.manifest.id)
        }
    }
}

impl AgentAdapter for ProcessPluginAdapter {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        Ok(self.list_with_warnings()?.sessions)
    }

    fn list_with_warnings(&self) -> Result<AgentListing> {
        self.require(self.manifest.capabilities.list, "agent.list")?;
        let result = self.request("agent.list", serde_json::json!({}))?;
        let sessions =
            serde_json::from_value(result.get("sessions").cloned().unwrap_or(Value::Null))?;
        let warnings: Vec<PluginWarning> = match result.get("warnings") {
            Some(value) if !value.is_null() => serde_json::from_value(value.clone())?,
            _ => Vec::new(),
        };
        Ok(AgentListing { sessions, warnings })
    }

    fn load(&self, id: &str) -> Result<Session> {
        self.require(self.manifest.capabilities.load, "agent.load")?;
        let result = self.request("agent.load", serde_json::json!({ "id": id }))?;
        Ok(serde_json::from_value(
            result.get("session").cloned().unwrap_or(Value::Null),
        )?)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        self.require(
            self.manifest.can_write && self.manifest.capabilities.write,
            "agent.write",
        )?;
        let result = self.request(
            "agent.write",
            serde_json::json!({
                "session": session,
                "cwd": cwd,
                "overwrite": overwrite,
            }),
        )?;
        result
            .get("target_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("插件 agent.write 响应缺少 target_id")
    }

    fn delete(&self, id: &str) -> Result<()> {
        self.require(self.manifest.capabilities.delete, "agent.delete")?;
        self.request("agent.delete", serde_json::json!({ "id": id }))?;
        Ok(())
    }

    fn destroy(&self, id: &str) -> Result<()> {
        self.require(self.manifest.capabilities.destroy, "agent.destroy")?;
        self.request("agent.destroy", serde_json::json!({ "id": id }))?;
        Ok(())
    }

    fn active(&self, id: &str) -> Result<bool> {
        self.require(self.manifest.capabilities.active, "agent.active")?;
        let result = self.request("agent.active", serde_json::json!({ "id": id }))?;
        result
            .get("active")
            .and_then(Value::as_bool)
            .context("插件 agent.active 响应缺少 active")
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        self.require(
            self.manifest.capabilities.backup_paths,
            "agent.backup_paths",
        )?;
        let result = self.request("agent.backup_paths", serde_json::json!({}))?;
        let values: Vec<String> =
            serde_json::from_value(result.get("paths").cloned().unwrap_or(Value::Null))?;
        Ok(values.into_iter().map(PathBuf::from).collect())
    }

    fn requires_exclusive(&self) -> bool {
        self.manifest.safety.requires_exclusive
    }

    fn restore_on_error(&self) -> bool {
        self.manifest.safety.restore_on_error
    }

    fn tracks_process(&self) -> bool {
        self.manifest.integration.tracks_process
    }

    fn prepare_open(&self, session_id: Option<&str>) -> Result<()> {
        if session_id.is_none() {
            return Ok(());
        }
        self.request(
            "agent.prepare_open",
            serde_json::json!({ "id": session_id }),
        )?;
        Ok(())
    }

    fn ensure_closable(&self) -> Result<()> {
        if self.manifest.integration.close_mode != "check_only" {
            return Ok(());
        }
        self.request("agent.ensure_closable", serde_json::json!({}))?;
        Ok(())
    }

    fn close_behavior(&self) -> CloseBehavior {
        if self.manifest.integration.close_mode == "check_only" {
            return CloseBehavior::CheckOnly;
        }
        match &self.manifest.integration.close_process {
            Some(image) if !image.trim().is_empty() => CloseBehavior::Image(image.clone()),
            _ => CloseBehavior::Process,
        }
    }

    fn open_session_ids(&self) -> Option<std::collections::HashSet<String>> {
        if !self.manifest.integration.open_session_ids {
            return None;
        }
        let result = self
            .request("agent.open_session_ids", serde_json::json!({}))
            .ok()?;
        let ids: Option<Vec<String>> = serde_json::from_value(result.get("ids")?.clone()).ok()?;
        ids.map(|ids| ids.into_iter().collect())
    }

    fn needs_rewrite(&self, id: &str) -> bool {
        self.request("agent.needs_rewrite", serde_json::json!({ "id": id }))
            .ok()
            .and_then(|result| result.get("needs_rewrite").and_then(Value::as_bool))
            .unwrap_or(false)
    }
}
