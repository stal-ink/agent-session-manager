use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_dsh as dsh;
use anyhow::Result;

struct DshPlugin;

impl PluginAgent for DshPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        dsh::list(&dsh::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        dsh::load(&dsh::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = dsh::Paths::default();
        match overwrite {
            Some(id) => dsh::overwrite(&paths, session, cwd, id),
            None => dsh::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        dsh::delete(&dsh::Paths::default(), id)
    }

    fn destroy(&self, id: &str) -> Result<()> {
        dsh::destroy(&dsh::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(dsh::active(&dsh::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = dsh::Paths::default();
        Ok(vec![paths.sessions, paths.storages])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        dsh::ID,
        "DeepSeek Harness",
        "DeepSeek Harness.exe",
        "deepseek-harness",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&DshPlugin, &descriptor)
}
