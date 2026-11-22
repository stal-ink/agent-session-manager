use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
#[cfg(not(windows))]
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::jsonl;

use agent_core::protocol::{PluginRegistration, IR_VERSION, PROTOCOL_VERSION};

#[cfg(not(windows))]
static REGISTRY_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(windows)]
struct RegistryWriteGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl RegistryWriteGuard {
    fn acquire() -> Result<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject, INFINITE};
        unsafe {
            let handle = CreateMutexW(
                std::ptr::null(),
                0,
                windows_sys::core::w!("Local\\AgentSessionManagerPluginRegistry"),
            );
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("创建插件注册表跨进程锁失败");
            }
            let wait = WaitForSingleObject(handle, INFINITE);
            // 接受 WAIT_ABANDONED：前任持有进程异常死亡时接管互斥量继续工作。
            if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error).context("等待插件注册表跨进程锁失败");
            }
            Ok(Self { handle })
        }
    }
}

#[cfg(windows)]
impl Drop for RegistryWriteGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
struct RegistryWriteGuard(std::sync::MutexGuard<'static, ()>);

#[cfg(not(windows))]
impl RegistryWriteGuard {
    fn acquire() -> Result<Self> {
        Ok(Self(
            REGISTRY_WRITE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        ))
    }
}

fn acquire_registry_write_lock() -> Result<RegistryWriteGuard> {
    RegistryWriteGuard::acquire()
}

#[derive(Clone, Debug)]
pub struct RegistryWriter {
    path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegistryFile {
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub plugins: BTreeMap<String, RegisteredPlugin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisteredPlugin {
    pub registration: PluginRegistration,
    pub root: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl RegistryWriter {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("plugin-registry.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<RegistryFile> {
        load_registry(&self.path)
    }

    /// 处理插件运行时的注册申请。完全相同的申请幂等；同 id 的不同版本或哈希
    /// 不允许由运行中的插件静默覆盖，只能经导入流程 activate。
    pub fn register(&self, registration: &PluginRegistration, root: &Path) -> Result<u64> {
        self.write(registration, root, false)
    }

    /// 导入流程已完成包校验，允许显式激活新版本。
    pub fn activate(&self, registration: &PluginRegistration, root: &Path) -> Result<u64> {
        self.write(registration, root, true)
    }

    /// 启用或停用已登记插件；插件包保留。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<u64> {
        let _guard = acquire_registry_write_lock()?;
        let mut registry = self.load()?;
        let entry = registry
            .plugins
            .get_mut(id)
            .with_context(|| format!("插件未安装：{id}"))?;
        if entry.enabled == enabled {
            return Ok(registry.generation);
        }
        entry.enabled = enabled;
        registry.generation = registry.generation.saturating_add(1);
        let text = serde_json::to_string_pretty(&registry)?;
        jsonl::write_atomic(&self.path, &text).context("原子写入插件注册表失败")?;
        Ok(registry.generation)
    }

    /// 删除注册项；调用方负责删除插件包目录。
    pub fn remove(&self, id: &str) -> Result<Option<RegisteredPlugin>> {
        let _guard = acquire_registry_write_lock()?;
        let mut registry = self.load()?;
        let Some(entry) = registry.plugins.remove(id) else {
            return Ok(None);
        };
        registry.generation = registry.generation.saturating_add(1);
        let text = serde_json::to_string_pretty(&registry)?;
        jsonl::write_atomic(&self.path, &text).context("原子写入插件注册表失败")?;
        Ok(Some(entry))
    }

    fn write(&self, registration: &PluginRegistration, root: &Path, replace: bool) -> Result<u64> {
        validate_registration(registration)?;
        let root = relative_root(root)?;
        let _guard = acquire_registry_write_lock()?;
        let mut registry = self.load()?;
        let entry = RegisteredPlugin {
            registration: PluginRegistration {
                sha256: registration.sha256.to_ascii_lowercase(),
                ..registration.clone()
            },
            root,
            enabled: true,
        };
        if let Some(current) = registry.plugins.get(&registration.id) {
            let same = current.registration == entry.registration && current.root == entry.root;
            if same {
                if current.enabled {
                    return Ok(registry.generation);
                }
            } else if !replace {
                anyhow::bail!(
                    "插件注册冲突：{} 已登记版本 {} / SHA-256 {}，申请版本 {} / SHA-256 {}",
                    registration.id,
                    current.registration.version,
                    current.registration.sha256,
                    registration.version,
                    registration.sha256
                );
            }
        }
        registry.plugins.insert(registration.id.clone(), entry);
        registry.generation = registry.generation.saturating_add(1);
        let text = serde_json::to_string_pretty(&registry)?;
        jsonl::write_atomic(&self.path, &text).context("原子写入插件注册表失败")?;
        Ok(registry.generation)
    }
}

pub fn load_registry(path: &Path) -> Result<RegistryFile> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("解析插件注册表失败：{}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RegistryFile::default()),
        Err(error) => Err(error).with_context(|| format!("读取插件注册表失败：{}", path.display())),
    }
}

fn validate_registration(registration: &PluginRegistration) -> Result<()> {
    if registration.schema != 2
        || registration.protocol != PROTOCOL_VERSION
        || registration.ir_version != IR_VERSION
        || registration.id.is_empty()
        || registration.version.is_empty()
        || registration.sha256.len() != 64
        || !registration
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        anyhow::bail!("插件注册申请字段或协议版本无效");
    }
    Ok(())
}

fn relative_root(root: &Path) -> Result<String> {
    if root.as_os_str().is_empty()
        || root.is_absolute()
        || root.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("插件注册根路径必须是相对路径：{}", root.display());
    }
    Ok(root.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("asm-registry-{}-{name}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn registration(id: &str) -> PluginRegistration {
        PluginRegistration {
            schema: 2,
            id: id.into(),
            version: "0.1.0".into(),
            protocol: PROTOCOL_VERSION,
            ir_version: IR_VERSION,
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn concurrent_registrations_are_serialized_and_idempotent() {
        let root = temp_dir("concurrent");
        let writer = std::sync::Arc::new(RegistryWriter::new(&root));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    let id = format!("agent-{index}");
                    let first = writer.register(&registration(&id), Path::new(&id)).unwrap();
                    assert!(
                        (1..=8).contains(&first),
                        "首次注册 generation 异常：{first}"
                    );
                    let second = writer.register(&registration(&id), Path::new(&id)).unwrap();
                    assert!(
                        second >= first,
                        "幂等查询不得回退 generation：{first} -> {second}"
                    );
                    first
                })
            })
            .collect();
        let mut first_generations: Vec<u64> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        first_generations.sort_unstable();
        assert_eq!(first_generations, (1..=8).collect::<Vec<_>>());
        let registry = writer.load().unwrap();
        assert_eq!(registry.generation, 8);
        assert_eq!(registry.plugins.len(), 8);
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_registration_cannot_replace_existing_hash() {
        let root = temp_dir("conflict");
        let writer = RegistryWriter::new(&root);
        let mut request = registration("probe");
        writer.register(&request, Path::new("probe/0.1.0")).unwrap();
        request.sha256 = "b".repeat(64);
        assert!(writer.register(&request, Path::new("probe/0.1.0")).is_err());
        assert_eq!(writer.load().unwrap().generation, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enabled_toggle_and_remove_are_persisted() {
        let root = temp_dir("toggle");
        let writer = RegistryWriter::new(&root);
        writer
            .register(&registration("probe"), Path::new("probe/0.1.0"))
            .unwrap();
        assert_eq!(writer.set_enabled("probe", false).unwrap(), 2);
        assert!(!writer.load().unwrap().plugins["probe"].enabled);
        assert_eq!(writer.set_enabled("probe", false).unwrap(), 2);
        assert_eq!(writer.set_enabled("probe", true).unwrap(), 3);
        assert!(writer.load().unwrap().plugins["probe"].enabled);
        let removed = writer.remove("probe").unwrap().unwrap();
        assert_eq!(removed.registration.id, "probe");
        assert!(writer.load().unwrap().plugins.is_empty());
        assert_eq!(writer.load().unwrap().generation, 4);
        fs::remove_dir_all(root).unwrap();
    }
}
