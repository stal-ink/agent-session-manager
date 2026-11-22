use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_codex as codex;
use anyhow::Result;

struct CodexPlugin;

impl PluginAgent for CodexPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        codex::list(&codex::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        codex::load(&codex::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        codex::write(&codex::Paths::default(), session, cwd, overwrite)
    }

    fn delete(&self, id: &str) -> Result<()> {
        codex::delete(&codex::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(codex::active(&codex::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(codex::backup_paths(&codex::Paths::default()))
    }

    fn prepare_open(&self, id: Option<&str>) -> Result<()> {
        if let Some(id) = id {
            let _ = codex::ensure_legacy(id);
        }
        Ok(())
    }

    fn needs_rewrite(&self, id: &str) -> bool {
        codex::needs_rewrite(&codex::Paths::default(), id)
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        codex::ID,
        "Codex",
        "codex.exe",
        "codex",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&CodexPlugin, &descriptor)
}
