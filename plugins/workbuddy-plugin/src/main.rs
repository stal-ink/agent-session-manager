use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_workbuddy as workbuddy;
use anyhow::Result;

struct WorkbuddyPlugin;

impl PluginAgent for WorkbuddyPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        workbuddy::list(&workbuddy::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        workbuddy::load(&workbuddy::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = workbuddy::Paths::default();
        match overwrite {
            Some(id) => workbuddy::overwrite(&paths, session, cwd, id),
            None => workbuddy::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        workbuddy::delete(&workbuddy::Paths::default(), id)
    }

    fn destroy(&self, id: &str) -> Result<()> {
        workbuddy::destroy(&workbuddy::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(workbuddy::active(&workbuddy::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = workbuddy::Paths::default();
        Ok(vec![paths.db, paths.projects])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        workbuddy::ID,
        "WorkBuddy",
        "WorkBuddy.exe",
        "workbuddy",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&WorkbuddyPlugin, &descriptor)
}
