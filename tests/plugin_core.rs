mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_session_manager::agents::{AgentAdapter, AgentInfo};
use agent_session_manager::ir::{Session, SessionSummary};
use agent_session_manager::plugins::discovery;
use agent_session_manager::plugins::manifest::{sha256_file, PluginManifest};
use agent_session_manager::plugins::process::PluginManager;
use agent_session_manager::plugins::registry::RegisteredPlugin;
use common::TempDir;

struct ProbeAdapter;

impl AgentAdapter for ProbeAdapter {
    fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
        Ok(Vec::new())
    }

    fn load(&self, _id: &str) -> anyhow::Result<Session> {
        anyhow::bail!("probe adapter does not read storage")
    }

    fn write(
        &self,
        _session: &Session,
        _cwd: &str,
        _overwrite: Option<&str>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("probe adapter does not write storage")
    }

    fn delete(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn active(&self, _id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn backup_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

fn write_probe_icon(path: &Path) {
    let image = image::RgbaImage::from_pixel(512, 512, image::Rgba([17, 24, 39, 255]));
    image.save(path).unwrap();
}

fn probe_manifest_text(sha256: &str) -> String {
    format!(
        r#"schema = 2
id = "probe"
label = "Probe Agent"
version = "0.17.0"
protocol = 1
ir_version = 1
min_host = "0.17.0"
process = "probe.exe"
launch = "probe"
icon = "icon.png"
can_write = false
sha256 = "{sha256}"

[execution]
mode = "ephemeral"
command = "bin/probe.exe"
timeout_ms = 1000

[capabilities]
list = true
load = false
write = false
delete = false
destroy = false
active = true
backup_paths = false

[safety]
requires_exclusive = true
restore_on_error = true

[[variants]]
id = "probe-desktop"
label = "Probe Agent"
kind = "desktop"
default_open = true

[variants.open]
strategy = "command"
command = "probe"
args = []
"#
    )
}

fn probe_package(temp: &TempDir) -> (PathBuf, PluginManifest) {
    let package = temp.0.join("packages/probe");
    fs::create_dir_all(package.join("bin")).unwrap();
    let executable = std::env::current_exe().unwrap();
    fs::copy(&executable, package.join("bin/probe.exe")).unwrap();
    write_probe_icon(&package.join("icon.png"));
    let sha256 = sha256_file(&package.join("bin/probe.exe")).unwrap();
    fs::write(package.join("plugin.toml"), probe_manifest_text(&sha256)).unwrap();
    let manifest = PluginManifest::load(&package).unwrap();
    (package, manifest)
}

fn copy_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

#[test]
fn generic_manifest_registry_and_discovery_do_not_depend_on_builtin_agents() {
    let temp = TempDir::new();
    let (package, manifest) = probe_package(&temp);
    let manager = PluginManager::new(temp.0.join("plugin-root")).unwrap();
    copy_dir(&package, &manager.root().join("probe/0.17.0")).unwrap();
    let generation = manager
        .registry()
        .activate(&manifest.registration(), Path::new("probe/0.17.0"))
        .unwrap();

    let loaded = discovery::load_registered(
        &manager,
        &RegisteredPlugin {
            registration: manifest.registration(),
            root: "probe/0.17.0".into(),
            enabled: true,
        },
    )
    .unwrap();
    assert_eq!(loaded.info.id.as_ref(), "probe");
    assert_eq!(loaded.info.label.as_ref(), "Probe Agent");
    assert!(!loaded.info.can_write);
    assert_eq!(generation, 1);
    assert_eq!(manager.registry().load().unwrap().generation, 1);
    assert!(manager.root().join("probe/0.17.0").is_dir());
    assert!(package.join("plugin.toml").is_file());
    manager.shutdown().unwrap();
}

#[test]
fn external_registry_rejects_duplicate_ids_without_touching_builtin_ids() {
    let info = AgentInfo::external(
        "probe",
        "Probe Agent",
        "probe.exe",
        "probe",
        Arc::<[u8]>::from(Vec::new()),
        false,
        Arc::new(ProbeAdapter),
    );
    agent_session_manager::agents::register_external(info.clone()).unwrap();
    let duplicate = agent_session_manager::agents::register_external(info);
    assert!(duplicate.is_err());
    let registered = agent_session_manager::agents::get("probe").unwrap();
    assert_eq!(registered.label.as_ref(), "Probe Agent");
}

#[test]
fn manifest_rejects_escaping_command_and_wrong_icon_size() {
    let temp = TempDir::new();
    let (package, _manifest) = probe_package(&temp);
    let executable = package.join("bin/probe.exe");
    let sha256 = sha256_file(&executable).unwrap();
    let mut text = probe_manifest_text(&sha256);
    text = text.replace("command = \"bin/probe.exe\"", "command = \"../probe.exe\"");
    fs::write(package.join("plugin.toml"), text).unwrap();
    assert!(PluginManifest::load(&package).is_err());

    let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([17, 24, 39, 255]));
    image.save(package.join("icon.png")).unwrap();
    fs::write(package.join("plugin.toml"), probe_manifest_text(&sha256)).unwrap();
    assert!(PluginManifest::load(&package).is_ok());
    assert!(PluginManifest::load_strict(&package).is_err());
}

#[test]
fn startup_validation_only_requires_files_but_import_rechecks_hash() {
    let temp = TempDir::new();
    let (package, _manifest) = probe_package(&temp);
    let sha256 = sha256_file(&package.join("bin/probe.exe")).unwrap();
    fs::write(
        package.join("plugin.toml"),
        probe_manifest_text(&sha256).replace(&sha256, &"0".repeat(64)),
    )
    .unwrap();
    assert!(PluginManifest::load(&package).is_ok());
    assert!(PluginManifest::load_strict(&package).is_err());
}
