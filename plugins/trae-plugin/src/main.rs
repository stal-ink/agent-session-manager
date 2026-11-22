use std::path::PathBuf;

use agent_core::adapter::AgentListing;
use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_trae as trae;
use anyhow::Result;

struct TraePlugin;

impl PluginAgent for TraePlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        trae::list(&trae::Paths::default())
    }

    fn list_with_warnings(&self) -> Result<AgentListing> {
        trae::list_with_warnings(&trae::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        trae::load(&trae::Paths::default(), id)
    }

    fn write(&self, _session: &Session, _cwd: &str, _overwrite: Option<&str>) -> Result<String> {
        anyhow::bail!("Trae 插件为只读，不支持转接写入")
    }

    fn delete(&self, id: &str) -> Result<()> {
        trae::delete(&trae::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        trae::active(&trae::Paths::default(), id)
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        trae::backup_paths(&trae::Paths::default())
    }
}

fn main() -> anyhow::Result<()> {
    let mut descriptor = PluginDescriptor::new(
        trae::ID,
        "Trae CN",
        "Trae CN.exe",
        "Trae CN",
        env!("CARGO_PKG_VERSION"),
    );
    descriptor.can_write = false;
    descriptor.requires_exclusive = false;
    descriptor.restore_on_error = false;
    serve(&TraePlugin, &descriptor)
}
