use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use agent_core::protocol::{PluginRegistration, IR_VERSION, PROTOCOL_VERSION};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub schema: u32,
    pub id: String,
    pub label: String,
    pub version: String,
    pub protocol: u32,
    pub ir_version: u32,
    pub min_host: String,
    pub process: String,
    pub launch: String,
    pub icon: String,
    pub can_write: bool,
    pub sha256: String,
    #[serde(default)]
    pub group_color: String,
    pub execution: Execution,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub safety: Safety,
    #[serde(default)]
    pub integration: Integration,
    #[serde(default)]
    pub variants: Vec<VariantManifest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Execution {
    #[serde(default = "ephemeral")]
    pub mode: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn ephemeral() -> String {
    "ephemeral".into()
}

fn default_timeout() -> u64 {
    30_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub list: bool,
    pub load: bool,
    pub write: bool,
    pub delete: bool,
    pub destroy: bool,
    pub active: bool,
    pub backup_paths: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            list: true,
            load: true,
            write: true,
            delete: true,
            destroy: true,
            active: true,
            backup_paths: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Safety {
    pub requires_exclusive: bool,
    pub restore_on_error: bool,
    /// 该 agent 的 delete 本身就是物理删除，销毁与删除效果相同。
    pub delete_is_physical: bool,
}

impl Default for Safety {
    fn default() -> Self {
        Self {
            requires_exclusive: true,
            restore_on_error: true,
            delete_is_physical: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Integration {
    pub tracks_process: bool,
    /// 为空时关闭 `process` 指定的映像。
    pub close_process: Option<String>,
    /// `process` 或 `check_only`。
    pub close_mode: String,
    /// 是否支持读取客户端当前打开的会话 id。
    pub open_session_ids: bool,
    /// 客户端打开会话清单代表活跃状态，未打开的会话按已归档展示。
    pub archive_from_open_ids: bool,
}

impl Default for Integration {
    fn default() -> Self {
        Self {
            tracks_process: true,
            close_process: None,
            close_mode: "process".into(),
            open_session_ids: false,
            archive_from_open_ids: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VariantManifest {
    pub id: String,
    pub label: String,
    /// desktop / cli / vsix
    pub kind: String,
    pub default_open: bool,
    /// 相对插件包的变体图标；为空时使用产品图标。
    pub icon: Option<String>,
    pub open: OpenManifest,
    pub update: Option<UpdateManifest>,
    pub detectors: Vec<DetectorManifest>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenManifest {
    /// registry / command / candidates / vscode_workspace / uri
    pub strategy: String,
    pub command: String,
    pub args: Vec<String>,
    pub session_args: Vec<String>,
    /// 依次尝试的环境变量展开路径，供 candidates 策略使用。
    pub candidates: Vec<String>,
    /// 工作区打开后再启动的深链模板。
    pub session_uri: Option<String>,
}

/// 版本源的声明式解析规则。`parser` 是通用格式名，不再对应具体产品。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateManifest {
    pub channel: String,
    /// Agent 产品版本检查页，抓取版本号用；不是插件包下载地址。
    #[serde(alias = "url")]
    pub version_url: String,
    /// json、key、regex、html_regex 或 text。
    pub parser: String,
    /// Agent 产品下载页，给用户打开；不是插件包下载地址。
    pub download_url: String,
    /// json 格式依次尝试的点分路径。
    pub paths: Vec<String>,
    /// key 格式匹配的顶层键名。
    pub key: String,
    /// regex/html_regex 格式依次尝试的正则。
    pub patterns: Vec<String>,
    /// 正则捕获组编号；0 表示整个匹配。
    pub group: usize,
    /// 截取前 N 段版本；0 表示不截取。
    pub segments: usize,
    /// 至少需要多少段数字。
    pub min_parts: usize,
}

impl Default for UpdateManifest {
    fn default() -> Self {
        Self {
            channel: String::new(),
            version_url: String::new(),
            parser: String::new(),
            download_url: String::new(),
            paths: Vec::new(),
            key: String::new(),
            patterns: Vec::new(),
            group: 1,
            segments: 0,
            min_parts: 2,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorManifest {
    #[serde(rename = "type")]
    pub kind: String,
    pub registry_keys: Vec<String>,
    pub exclude_prefixes: Vec<String>,
    pub exe: String,
    pub shortcut_names: Vec<String>,
    pub state_db: String,
    pub roots: Vec<String>,
    pub prefix: String,
    pub root: String,
    pub package: String,
    pub path: String,
    pub command: String,
    pub args: Vec<String>,
}

impl PluginManifest {
    /// 启动发现：解析清单并确认 exe、icon、toml 存在。不对 exe 做整文件
    /// 哈希，也不解码图标；真实性与尺寸在导入时校验，运行时注册握手再比对
    /// 可执行文件 SHA-256。
    pub fn load(root: &Path) -> Result<Self> {
        Self::parse(root)?.verify_files(root, false)
    }

    /// 导入：在启动校验之上核对 SHA-256 与 512×512 图标。
    pub fn load_strict(root: &Path) -> Result<Self> {
        Self::parse(root)?.verify_files(root, true)
    }

    fn parse(root: &Path) -> Result<Self> {
        let path = root.join("plugin.toml");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("读取插件清单失败：{}", path.display()))?;
        let manifest: Self = toml::from_str(&text)
            .with_context(|| format!("解析插件清单失败：{}", path.display()))?;
        manifest.validate_contract()?;
        Ok(manifest)
    }

    fn validate_contract(&self) -> Result<()> {
        if self.schema != 2 {
            anyhow::bail!("不支持的插件清单 schema：{}", self.schema);
        }
        if self.protocol != PROTOCOL_VERSION || self.ir_version != IR_VERSION {
            anyhow::bail!(
                "插件协议不兼容：protocol={} ir_version={}",
                self.protocol,
                self.ir_version
            );
        }
        if !valid_id(&self.id) {
            anyhow::bail!("插件 id 无效：{}", self.id);
        }
        if self.version.is_empty()
            || self.version.len() > 64
            || !self
                .version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
            || self.version.starts_with('.')
        {
            anyhow::bail!("插件 version 必须是安全文件名：{}", self.version);
        }
        for (name, value) in [
            ("label", &self.label),
            ("version", &self.version),
            ("process", &self.process),
            ("launch", &self.launch),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("插件字段 {name} 不能为空");
            }
        }
        if !host_supports(&self.min_host, env!("CARGO_PKG_VERSION")) {
            anyhow::bail!(
                "插件要求宿主版本 {}，当前为 {}",
                self.min_host,
                env!("CARGO_PKG_VERSION")
            );
        }
        if self.execution.mode != "ephemeral" {
            anyhow::bail!("实验版本仅支持 ephemeral 插件，不支持 daemon");
        }
        if !(100..=120_000).contains(&self.execution.timeout_ms) {
            anyhow::bail!("插件 timeout_ms 必须位于 100..=120000");
        }
        if self.can_write && !self.capabilities.write {
            anyhow::bail!("can_write=true 时 capabilities.write 必须为 true");
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("插件 sha256 必须是 64 位十六进制");
        }
        if !matches!(
            self.integration.close_mode.as_str(),
            "process" | "check_only"
        ) {
            anyhow::bail!("插件 integration.close_mode 只能是 process 或 check_only");
        }
        self.validate_variants()
    }

    fn validate_variants(&self) -> Result<()> {
        if self.variants.is_empty() {
            anyhow::bail!("插件必须至少声明一个 variants 条目");
        }
        let mut ids = Vec::new();
        let mut defaults = 0;
        for variant in &self.variants {
            if !valid_variant_id(&variant.id) {
                anyhow::bail!("插件变体 id 无效：{}", variant.id);
            }
            if ids.contains(&variant.id) {
                anyhow::bail!("插件变体 id 重复：{}", variant.id);
            }
            ids.push(variant.id.clone());
            if variant.label.trim().is_empty() {
                anyhow::bail!("插件变体 {} 的 label 不能为空", variant.id);
            }
            if !matches!(variant.kind.as_str(), "desktop" | "cli" | "vsix") {
                anyhow::bail!("插件变体 {} 的 kind 无效：{}", variant.id, variant.kind);
            }
            if variant.default_open {
                defaults += 1;
            }
            match variant.open.strategy.as_str() {
                "registry" | "command" | "uri" if variant.open.command.trim().is_empty() => {
                    anyhow::bail!("插件变体 {} 的 open.command 不能为空", variant.id);
                }
                "candidates" if variant.open.candidates.is_empty() => {
                    anyhow::bail!("插件变体 {} 的 open.candidates 不能为空", variant.id);
                }
                "registry" | "command" | "uri" | "candidates" | "vscode_workspace" => {}
                other => anyhow::bail!("插件变体 {} 的 open.strategy 无效：{other}", variant.id),
            }
            if let Some(update) = &variant.update {
                for (field, value) in [
                    ("channel", &update.channel),
                    ("version_url", &update.version_url),
                    ("parser", &update.parser),
                ] {
                    if value.trim().is_empty() {
                        anyhow::bail!("插件变体 {} 的 update.{field} 不能为空", variant.id);
                    }
                }
                validate_update_rule(update)
                    .with_context(|| format!("插件变体 {} 的 update 规则无效", variant.id))?;
            }
            for detector in &variant.detectors {
                if detector.kind.trim().is_empty() {
                    anyhow::bail!("插件变体 {} 存在空检测器类型", variant.id);
                }
            }
            if let Some(icon) = &variant.icon {
                if icon.trim().is_empty() {
                    anyhow::bail!("插件变体 {} 的 icon 不能为空", variant.id);
                }
            }
        }
        if defaults > 1 {
            anyhow::bail!("插件最多只能有一个 default_open 变体");
        }
        Ok(())
    }

    fn verify_files(&self, root: &Path, strict: bool) -> Result<Self> {
        let command = contained_path(root, &self.execution.command)?;
        if !command.is_file() {
            anyhow::bail!("插件可执行文件不存在：{}", command.display());
        }
        let icon = contained_path(root, &self.icon)?;
        if !icon.is_file() {
            anyhow::bail!("插件图标不存在：{}", icon.display());
        }
        for variant in &self.variants {
            if let Some(icon) = &variant.icon {
                let path = contained_path(root, icon)?;
                if !path.is_file() {
                    anyhow::bail!("插件变体 {} 的图标不存在：{}", variant.id, path.display());
                }
            }
        }
        if strict {
            let actual = sha256_file(&command)?;
            if !self.sha256.eq_ignore_ascii_case(&actual) {
                anyhow::bail!(
                    "插件可执行文件 SHA-256 不匹配：期望 {}，实际 {actual}",
                    self.sha256
                );
            }
            let bytes =
                fs::read(&icon).with_context(|| format!("读取图标失败：{}", icon.display()))?;
            let image = image::load_from_memory(&bytes)
                .with_context(|| format!("插件图标不是有效图片：{}", icon.display()))?;
            if image.width() != 512 || image.height() != 512 {
                anyhow::bail!(
                    "插件图标必须为 512×512，实际为 {}×{}",
                    image.width(),
                    image.height()
                );
            }
        }
        Ok(self.clone())
    }

    pub fn command_path(&self, root: &Path) -> Result<PathBuf> {
        contained_path(root, &self.execution.command)
    }

    pub fn icon_bytes(&self, root: &Path) -> Result<Vec<u8>> {
        Ok(fs::read(contained_path(root, &self.icon)?)?)
    }

    pub fn registration(&self) -> PluginRegistration {
        PluginRegistration {
            schema: self.schema,
            id: self.id.clone(),
            version: self.version.clone(),
            protocol: self.protocol,
            ir_version: self.ir_version,
            sha256: self.sha256.to_ascii_lowercase(),
        }
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn contained_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("插件路径必须是目录内相对路径：{relative:?}");
    }
    let root = root.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) {
        anyhow::bail!("插件路径逃逸出插件目录：{}", path.display());
    }
    Ok(path)
}

fn validate_update_rule(update: &UpdateManifest) -> Result<()> {
    match update.parser.as_str() {
        "json" if update.paths.is_empty() => {
            anyhow::bail!("update.parser=json 时 update.paths 不能为空")
        }
        "json" => {
            if update.paths.iter().any(|path| path.trim().is_empty()) {
                anyhow::bail!("update.paths 不能包含空路径");
            }
        }
        "key" if update.key.trim().is_empty() => {
            anyhow::bail!("update.parser=key 时 update.key 不能为空")
        }
        "regex" | "html_regex" if update.patterns.is_empty() => {
            anyhow::bail!(
                "update.parser={} 时 update.patterns 不能为空",
                update.parser
            )
        }
        "regex" | "html_regex" => {
            for pattern in &update.patterns {
                regex::Regex::new(pattern)
                    .with_context(|| format!("update.patterns 正则无效：{pattern}"))?;
            }
        }
        "text" => {}
        // 兼容尚未迁移的旧包；未知 parser 在检查时记 unknown，不阻断启动。
        _ => {}
    }
    if update.min_parts == 0 {
        anyhow::bail!("update.min_parts 必须大于 0");
    }
    if update.segments > 0 && update.segments < update.min_parts {
        anyhow::bail!("update.segments 不能小于 min_parts");
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_variant_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn host_supports(required: &str, current: &str) -> bool {
    fn parts(value: &str) -> Option<[u32; 3]> {
        let mut out = [0; 3];
        let mut split = value.split('.');
        for slot in &mut out {
            *slot = split.next()?.parse().ok()?;
        }
        if split.next().is_some() {
            return None;
        }
        Some(out)
    }
    match (parts(required), parts(current)) {
        (Some(required), Some(current)) => current >= required,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_update_rules_are_valid() {
        let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins");
        let mut checked = 0;
        for entry in fs::read_dir(plugins).unwrap() {
            let path = entry.unwrap().path().join("plugin.toml.in");
            if !path.is_file() {
                continue;
            }
            let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let variants = value["variants"].as_array().unwrap();
            for (index, variant) in variants.iter().enumerate() {
                let Some(update) = variant.get("update") else {
                    continue;
                };
                let update: UpdateManifest = update.clone().try_into().unwrap();
                validate_update_rule(&update).unwrap_or_else(|error| {
                    panic!("{} variants[{index}]：{error:#}", path.display())
                });
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    #[test]
    fn ids_and_versions_are_strict() {
        assert!(valid_id("probe"));
        assert!(valid_id("new-agent-2"));
        assert!(!valid_id("Probe"));
        assert!(!valid_id("a"));
        assert!(!valid_id("../probe"));
        assert!(host_supports("0.17.0", "0.17.0"));
        assert!(host_supports("0.16.9", "0.17.0"));
        assert!(!host_supports("0.18.0", "0.17.0"));
        assert!(!host_supports("unknown", "0.17.0"));
    }
}
