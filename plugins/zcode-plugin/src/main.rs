use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_zcode as zcode;
use anyhow::Result;

struct ZCodePlugin;

impl PluginAgent for ZCodePlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        zcode::list(&zcode::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        zcode::load(&zcode::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = zcode::Paths::default();
        match overwrite {
            Some(id) => zcode::overwrite(&paths, session, cwd, id),
            None => zcode::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        zcode::delete(&zcode::Paths::default(), id)
    }

    fn destroy(&self, id: &str) -> Result<()> {
        zcode::destroy(&zcode::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(zcode::active(&zcode::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = zcode::Paths::default();
        Ok(vec![paths.db, paths.tasks_db])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        zcode::ID,
        "ZCode",
        "ZCode.exe",
        "zcode",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&ZCodePlugin, &descriptor)
}
