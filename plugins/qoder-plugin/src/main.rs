use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_qoder as qoder;
use anyhow::Result;

struct QoderPlugin;

impl PluginAgent for QoderPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        qoder::list(&qoder::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        qoder::load(&qoder::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = qoder::Paths::default();
        match overwrite {
            Some(id) => qoder::overwrite(&paths, session, cwd, id),
            None => qoder::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        qoder::delete(&qoder::Paths::default(), id)
    }

    fn destroy(&self, id: &str) -> Result<()> {
        qoder::destroy(&qoder::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(qoder::active(&qoder::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = qoder::Paths::default();
        Ok(vec![paths.main_db, paths.projects])
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        qoder::ID,
        "Qoder",
        "Qoder CN.exe",
        "qoder-cn",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&QoderPlugin, &descriptor)
}
