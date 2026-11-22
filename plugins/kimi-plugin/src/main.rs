use std::path::PathBuf;

use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use agent_core::{Session, SessionSummary};
use agent_plugin_kimi as kimi;
use anyhow::Result;

struct KimiPlugin;

impl PluginAgent for KimiPlugin {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        kimi::list(&kimi::Paths::default())
    }

    fn load(&self, id: &str) -> Result<Session> {
        kimi::load(&kimi::Paths::default(), id)
    }

    fn write(&self, session: &Session, cwd: &str, overwrite: Option<&str>) -> Result<String> {
        let paths = kimi::Paths::default();
        match overwrite {
            Some(id) => kimi::overwrite(&paths, session, cwd, id),
            None => kimi::inject(&paths, session, cwd),
        }
    }

    fn delete(&self, id: &str) -> Result<()> {
        kimi::delete(&kimi::Paths::default(), id)
    }

    fn active(&self, id: &str) -> Result<bool> {
        Ok(kimi::active(&kimi::Paths::default(), id))
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = kimi::Paths::default();
        Ok(vec![paths.sessions, paths.index, paths.projects])
    }

    /// npm 形态跑在 node.exe 里，不能按映像名判活；有会话锁即视为运行中。
    fn ensure_closable(&self) -> Result<()> {
        let paths = kimi::Paths::default();
        let busy = std::fs::read_dir(&paths.locks)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if busy {
            anyhow::bail!("Kimi 正在运行，请先在 Kimi 中退出后重试");
        }
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let descriptor = PluginDescriptor::new(
        kimi::ID,
        "Kimi Code",
        "kimi.exe",
        "kimi",
        env!("CARGO_PKG_VERSION"),
    );
    serve(&KimiPlugin, &descriptor)
}
