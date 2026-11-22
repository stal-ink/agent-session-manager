use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_cursor as cursor;
use anyhow::Result;

struct CursorPlugin;

impl PluginAgent for CursorPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        cursor::list(&cursor::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        cursor::load(&cursor::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = cursor::Paths::default();
        match overwrite {
            Some(id) => cursor::overwrite(&paths, session, cwd, id),
            None => cursor::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        cursor::delete(&cursor::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(cursor::active(&cursor::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = cursor::Paths::default();
        Ok(vec![paths.state_db, paths.workspace_storage])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        cursor::ID,
        "Cursor",
        "Cursor.exe",
        "cursor",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&CursorPlugin, &descriptor)
}
