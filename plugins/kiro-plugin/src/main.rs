use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_kiro as kiro;
use anyhow::Result;

struct KiroPlugin;

impl PluginAgent for KiroPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        kiro::list(&kiro::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        kiro::load(&kiro::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = kiro::Paths::default();
        match overwrite {
            Some(id) => kiro::overwrite(&paths, session, cwd, id),
            None => kiro::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        kiro::delete(&kiro::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(kiro::active(&kiro::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = kiro::Paths::default();
        Ok([paths.sessions, paths.session_index, paths.spec_sessions]
            .into_iter()
            .filter(|path| path.exists())
            .collect())
    }

    fn open_session_ids(&self) -> Option<Vec<String>> {
        kiro::open_session_ids(&kiro::Paths::default()).map(|ids| ids.into_iter().collect())
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        kiro::ID,
        "Kiro",
        "Kiro.exe",
        "kiro",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&KiroPlugin, &descriptor)
}
