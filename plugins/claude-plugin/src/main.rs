use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_claude as claude;
use anyhow::Result;

struct ClaudePlugin;

impl PluginAgent for ClaudePlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        claude::list(&claude::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        claude::load(&claude::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = claude::Paths::default();
        match overwrite {
            Some(id) => claude::overwrite(&paths, session, cwd, id),
            None => claude::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        claude::delete(&claude::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(claude::active(&claude::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(vec![claude::Paths::default().projects])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        claude::ID,
        "Claude Code",
        "claude.exe",
        "claude",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&ClaudePlugin, &descriptor)
}
