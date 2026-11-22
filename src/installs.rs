//! 安装检测：产品与形态由已安装插件的 manifest 声明，检测器按声明只读探测
//! （卸载注册表、开始菜单快捷方式、安装目录、VS Code 扩展、npm 包、原生二进制）。
//! 扫描结果缓存 installs.json（含未安装形态）；旧版顶层数组缓存按单桌面形态兼容。
//! 测试使用临时目录与显式传入的 [`DetectEnv`]，不读真实注册表、不碰真实 agent 存储。

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, CatalogProduct, CatalogVariant, DetectorSpec};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 全局安装形态分类：变体自带用户可见名称，如 `Cli("Claude Code CLI")`。
/// `DESKTOP / CLI / VSIX` 只用于内部检测、分组与默认打开，不作为 UI 文案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    Desktop(String),
    Cli(String),
    Vsix(String),
}

impl InstallKind {
    /// 用户可见名称，例如 `Cli("Claude Code CLI")`。
    pub fn label(&self) -> &str {
        match self {
            Self::Desktop(label) | Self::Cli(label) | Self::Vsix(label) => label,
        }
    }

    /// 摘要/统计用的短名，例如设置里“已安装 2 项：VS Code 扩展 / CLI”。
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Desktop(_) => "桌面端",
            Self::Cli(_) => "CLI",
            Self::Vsix(_) => "VS Code 扩展",
        }
    }

    /// 仅用于内部日志/判断，不作为 UI 文案。
    pub fn key(&self) -> &'static str {
        match self {
            Self::Desktop(_) => "DESKTOP",
            Self::Cli(_) => "CLI",
            Self::Vsix(_) => "VSIX",
        }
    }

    /// catalog 的 `kind`（desktop/cli/vsix）+ 用户可见 `label` 组合成枚举。
    pub fn from_catalog(kind: &str, label: &str) -> Option<Self> {
        match kind {
            "desktop" => Some(Self::Desktop(label.to_string())),
            "cli" => Some(Self::Cli(label.to_string())),
            "vsix" => Some(Self::Vsix(label.to_string())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstallVariant {
    pub id: String,
    pub product: String,
    pub kind: InstallKind,
    /// 已安装为本机版本；未安装为空。
    pub version: String,
    /// 本机安装路径；仅内部检测与排障，UI 不显示。
    pub path: String,
    pub installed: bool,
    /// 渠道 id；空表示未配置检查源。
    pub channel: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentInstall {
    pub id: String,
    pub label: String,
    /// 任一形态已安装。
    pub installed: bool,
    /// 默认打开形态的版本；不可用时取第一个已安装形态。
    pub version: String,
    /// catalog 建议的默认打开形态（variant id），用户设置优先。
    pub default_open_variant: Option<String>,
    pub variants: Vec<InstallVariant>,
}

/// 卸载注册表条目：DisplayName / DisplayVersion / InstallLocation。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegEntry {
    pub name: String,
    pub version: String,
    pub location: String,
}

/// 检测环境：显式传入路径，测试可整体重定向到临时目录。
#[derive(Clone, Debug)]
pub struct DetectEnv {
    pub home: PathBuf,
    pub appdata: PathBuf,
    pub local: PathBuf,
    pub program_files: PathBuf,
}

impl DetectEnv {
    pub fn from_system() -> Self {
        Self {
            home: PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()),
            appdata: PathBuf::from(std::env::var("APPDATA").unwrap_or_default()),
            local: PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()),
            program_files: PathBuf::from(std::env::var("ProgramFiles").unwrap_or_default()),
        }
    }
}

fn cache_path() -> PathBuf {
    crate::transfer::data_dir().join("installs.json")
}

pub fn load_cache() -> Vec<AgentInstall> {
    load_from(&cache_path(), &crate::catalog::from_plugins())
}

pub fn save_cache(list: &[AgentInstall]) {
    let _ = save_to(&cache_path(), list);
}

pub fn scan() -> Vec<AgentInstall> {
    scan_with(
        &crate::catalog::from_plugins(),
        &registry_entries(),
        &DetectEnv::from_system(),
    )
}

/// 按目录声明逐产品、逐形态检测：检测器按顺序收集命中，多命中取最高版本。
/// 未安装形态也保留在 variants 里（version/path 空），供未安装区展示。
pub fn scan_with(catalog: &Catalog, registry: &[RegEntry], env: &DetectEnv) -> Vec<AgentInstall> {
    catalog
        .products
        .iter()
        .map(|product| {
            let variants = product
                .variants
                .iter()
                .filter_map(|variant| {
                    let kind = InstallKind::from_catalog(&variant.kind, &variant.label)?;
                    let found = detect_variant(variant, registry, env);
                    Some(InstallVariant {
                        id: variant.id.clone(),
                        product: product.id.clone(),
                        kind,
                        version: found.as_ref().map(|(v, _)| v.clone()).unwrap_or_default(),
                        path: found.as_ref().map(|(_, p)| p.clone()).unwrap_or_default(),
                        installed: found.is_some(),
                        channel: variant.channel.clone(),
                    })
                })
                .collect();
            finish_install(product, variants)
        })
        .collect()
}

/// 以 catalog 结构为骨架构造全量产品/形态（全部未安装），供缓存合并使用。
fn skeleton(catalog: &Catalog) -> Vec<AgentInstall> {
    catalog
        .products
        .iter()
        .map(|product| {
            let variants = product
                .variants
                .iter()
                .filter_map(|variant| {
                    let kind = InstallKind::from_catalog(&variant.kind, &variant.label)?;
                    Some(InstallVariant {
                        id: variant.id.clone(),
                        product: product.id.clone(),
                        kind,
                        version: String::new(),
                        path: String::new(),
                        installed: false,
                        channel: variant.channel.clone(),
                    })
                })
                .collect();
            finish_install(product, variants)
        })
        .collect()
}

fn finish_install(product: &CatalogProduct, variants: Vec<InstallVariant>) -> AgentInstall {
    let installed = variants.iter().any(|v| v.installed);
    let version = product_version(product.default_open_variant.as_deref(), &variants);
    AgentInstall {
        id: product.id.clone(),
        label: product.label.clone(),
        installed,
        version,
        default_open_variant: product.default_open_variant.clone(),
        variants,
    }
}

/// 产品版本：默认打开形态已安装时取其版本，否则取第一个已安装形态，都没有则空。
fn product_version(default_open: Option<&str>, variants: &[InstallVariant]) -> String {
    if let Some(id) = default_open {
        if let Some(variant) = variants.iter().find(|v| v.id == id && v.installed) {
            return variant.version.clone();
        }
    }
    variants
        .iter()
        .find(|v| v.installed)
        .map(|v| v.version.clone())
        .unwrap_or_default()
}

fn detect_variant(
    variant: &CatalogVariant,
    registry: &[RegEntry],
    env: &DetectEnv,
) -> Option<(String, String)> {
    let mut hits: Vec<(String, String)> = Vec::new();
    for detector in &variant.detectors {
        hits.extend(run_detector(detector, registry, env));
    }
    hits.into_iter().max_by(|a, b| compare_versions(&a.0, &b.0))
}

fn run_detector(
    spec: &DetectorSpec,
    registry: &[RegEntry],
    env: &DetectEnv,
) -> Vec<(String, String)> {
    match spec.kind.as_str() {
        "desktop_app" => detect_desktop_app(spec, registry, env)
            .into_iter()
            .collect(),
        "vscode_extension" => detect_vscode_extension(spec, env),
        "npm_package" => detect_npm_package(spec, env).into_iter().collect(),
        "native_binary" => detect_native_binary(spec, env).into_iter().collect(),
        "command_version" => detect_command_version(spec).into_iter().collect(),
        // 未知类型在 catalog 加载时已跳过并写警告；这里防御性忽略。
        _ => Vec::new(),
    }
}

/// 桌面端：卸载注册表优先（多命中取最高版本）；无注册表命中时依次尝试
/// 开始菜单快捷方式、常见安装目录（版本读 exe ProductVersion，读不到留空），
/// 最后以 state_db 存在兜底（Cursor 这类会话库即安装证据的形态）。
fn detect_desktop_app(
    spec: &DetectorSpec,
    registry: &[RegEntry],
    env: &DetectEnv,
) -> Option<(String, String)> {
    let keys: Vec<String> = spec
        .registry_keys
        .iter()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    let excludes: Vec<String> = spec
        .exclude_prefixes
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();
    let best = registry
        .iter()
        .filter(|entry| {
            let name = entry.name.to_ascii_lowercase();
            keys.iter().any(|key| name.starts_with(key))
                && !excludes.iter().any(|prefix| name.starts_with(prefix))
        })
        .max_by(|a, b| compare_versions(&a.version, &b.version));
    if let Some(entry) = best {
        return Some((entry.version.clone(), entry.location.clone()));
    }
    for name in &spec.shortcut_names {
        let shortcut = env
            .appdata
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join(format!("{name}.lnk"));
        if shortcut.is_file() {
            return Some((String::new(), shortcut.to_string_lossy().to_string()));
        }
    }
    if !spec.exe.is_empty() {
        let stem = spec.exe.strip_suffix(".exe").unwrap_or(&spec.exe);
        for root in [
            env.local.join("Programs").join(stem),
            env.local.join(stem),
            env.program_files.join(stem),
        ] {
            if root.is_dir() {
                let exe = root.join(&spec.exe);
                let version = if exe.is_file() {
                    exe_product_version(&exe).unwrap_or_default()
                } else {
                    String::new()
                };
                return Some((version, root.to_string_lossy().to_string()));
            }
        }
    }
    if !spec.state_db.is_empty() {
        let db = PathBuf::from(expand_env(&spec.state_db, env));
        if db.is_file() {
            return Some((String::new(), db.to_string_lossy().to_string()));
        }
    }
    None
}

/// VS Code 扩展：roots 下以 prefix 开头的扩展目录，读 package.json 的 version。
fn detect_vscode_extension(spec: &DetectorSpec, env: &DetectEnv) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    for root in &spec.roots {
        let dir = env.home.join(root).join("extensions");
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&spec.prefix) {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(version) = read_package_version(&path.join("package.json")) else {
                continue;
            };
            hits.push((version, path.to_string_lossy().to_string()));
        }
    }
    hits
}

/// npm 全局包：root/<package>/package.json 的 version。
fn detect_npm_package(spec: &DetectorSpec, env: &DetectEnv) -> Option<(String, String)> {
    let dir = PathBuf::from(expand_env(&spec.root, env)).join(&spec.package);
    let version = read_package_version(&dir.join("package.json"))?;
    Some((version, dir.to_string_lossy().to_string()))
}

/// 原生二进制：只查文件存在，不执行 --version，版本留空。
fn detect_native_binary(spec: &DetectorSpec, env: &DetectEnv) -> Option<(String, String)> {
    let path = PathBuf::from(expand_env(&spec.path, env));
    path.is_file()
        .then(|| (String::new(), path.to_string_lossy().to_string()))
}

/// 走 PATH 的命令安装（自定义 npm 前缀、原生安装器等无法从固定目录判断的情况）：
/// 执行 `<command> <args...>` 读取版本号，限时 5 秒、不带任何联网参数，超时结束进程。
/// 不写死安装路径；命令不存在就当作未安装。
#[cfg(windows)]
fn detect_command_version(spec: &DetectorSpec) -> Option<(String, String)> {
    let command = spec.command.trim();
    if command.is_empty() {
        return None;
    }
    let output = run_with_timeout(command, &spec.args, std::time::Duration::from_secs(5))?;
    parse_version_output(&output).map(|version| (version, String::new()))
}

#[cfg(not(windows))]
fn detect_command_version(_spec: &DetectorSpec) -> Option<(String, String)> {
    None
}

#[cfg(windows)]
fn run_with_timeout(
    command: &str,
    args: &[String],
    timeout: std::time::Duration,
) -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let mut cmd = Command::new("cmd");
    cmd.args(["/c", command]).args(args);
    cmd.creation_flags(0x0800_0000);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 从命令输出里取第一个点分纯数字版本号，例如 `2.1.326 (Claude Code)`。
fn parse_version_output(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let parts: Vec<&str> = token.split('.').collect();
        (parts.len() >= 2
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
        .then(|| token.to_string())
    })
}

fn read_package_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = value.get("version")?.as_str()?;
    (!version.trim().is_empty()).then(|| version.to_string())
}

/// 环境变量展开只认四个白名单变量（用 DetectEnv 的值，保证测试可重定向）；
/// 未知 %X% 原样保留。前导 `~/` 或 `~\` 展开为 home。
fn expand_env(text: &str, env: &DetectEnv) -> String {
    let text = match text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        Some(rest) => env.home.join(rest).to_string_lossy().to_string(),
        None => text.to_string(),
    };
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push('%');
            out.push_str(after);
            return out;
        };
        let name = &after[..end];
        let value = match name {
            "USERPROFILE" => Some(env.home.as_os_str()),
            "APPDATA" => Some(env.appdata.as_os_str()),
            "LOCALAPPDATA" => Some(env.local.as_os_str()),
            "ProgramFiles" => Some(env.program_files.as_os_str()),
            _ => None,
        };
        match value {
            Some(value) => out.push_str(&value.to_string_lossy()),
            None => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 读 exe 的 ProductVersion：PowerShell 限时 5 秒，超时 kill；读不到返回 None。
#[cfg(windows)]
fn exe_product_version(path: &Path) -> Option<String> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    // PowerShell 单引号字符串内的单引号用 doubling 转义。
    let escaped = path.to_string_lossy().replace('\'', "''");
    let script = format!("(Get-Item -LiteralPath '{escaped}').VersionInfo.ProductVersion");
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-Command", &script]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(windows))]
fn exe_product_version(_path: &Path) -> Option<String> {
    None
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    version_key(a).cmp(&version_key(b))
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(windows)]
fn registry_entries() -> Vec<RegEntry> {
    let roots = [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ];
    let mut entries = Vec::new();
    for root in roots {
        let mut cmd = Command::new("reg");
        cmd.args(["query", root, "/s"]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let Ok(output) = cmd.output() else { continue };
        if output.status.success() {
            entries.extend(parse_reg_output(&String::from_utf8_lossy(&output.stdout)));
        }
    }
    entries
}

#[cfg(not(windows))]
fn registry_entries() -> Vec<RegEntry> {
    Vec::new()
}

fn parse_reg_output(text: &str) -> Vec<RegEntry> {
    let mut entries = Vec::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut location: Option<String> = None;
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            if let Some(found) = name.take() {
                entries.push(RegEntry {
                    name: found,
                    version: version.take().unwrap_or_default(),
                    location: location.take().unwrap_or_default(),
                });
            } else {
                version = None;
                location = None;
            }
            continue;
        }
        if let Some(value) = reg_value(line, "DisplayName") {
            name = Some(value);
        } else if let Some(value) = reg_value(line, "DisplayVersion") {
            version = Some(value);
        } else if let Some(value) = reg_value(line, "InstallLocation") {
            location = Some(value);
        }
    }
    if let Some(found) = name {
        entries.push(RegEntry {
            name: found,
            version: version.unwrap_or_default(),
            location: location.unwrap_or_default(),
        });
    }
    entries
}

fn reg_value(line: &str, key: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix(key)?;
    if !rest.chars().next().is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("REG_SZ")
        .or_else(|| rest.strip_prefix("REG_EXPAND_SZ"))?;
    Some(rest.trim().to_string())
}

/// installs.json 新格式：按产品/形态缓存扫描结果。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CacheFile {
    products: Vec<CachedProduct>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CachedProduct {
    id: String,
    installed: bool,
    version: String,
    variants: Vec<CachedVariant>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CachedVariant {
    id: String,
    product: String,
    kind: String,
    label: String,
    version: String,
    path: String,
    installed: bool,
    channel: String,
}

/// 旧格式：顶层数组，只有产品级 installed/version。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyInstall {
    id: String,
    installed: bool,
    version: String,
}

fn load_from(path: &Path, catalog: &Catalog) -> Vec<AgentInstall> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut list = skeleton(catalog);
    if value.is_array() {
        // 旧缓存：每个产品视为一个桌面形态，映射到 catalog 第一个形态，下次扫描补全。
        let Ok(legacy) = serde_json::from_value::<Vec<LegacyInstall>>(value) else {
            return Vec::new();
        };
        for item in &mut list {
            let Some(cached) = legacy.iter().find(|c| c.id == item.id) else {
                continue;
            };
            if let Some(first) = item.variants.first_mut() {
                first.installed = cached.installed;
                first.version = cached.version.clone();
            }
            refresh_product(item);
        }
        return list;
    }
    let Ok(cache) = serde_json::from_value::<CacheFile>(value) else {
        return Vec::new();
    };
    for item in &mut list {
        let Some(cached) = cache.products.iter().find(|p| p.id == item.id) else {
            continue;
        };
        for variant in &mut item.variants {
            let Some(hit) = cached.variants.iter().find(|v| v.id == variant.id) else {
                continue;
            };
            variant.installed = hit.installed;
            variant.version = hit.version.clone();
            variant.path = hit.path.clone();
        }
        refresh_product(item);
    }
    list
}

fn refresh_product(item: &mut AgentInstall) {
    item.installed = item.variants.iter().any(|v| v.installed);
    item.version = product_version(item.default_open_variant.as_deref(), &item.variants);
}

fn save_to(path: &Path, list: &[AgentInstall]) -> std::io::Result<()> {
    let cache = CacheFile {
        products: list
            .iter()
            .map(|item| CachedProduct {
                id: item.id.to_string(),
                installed: item.installed,
                version: item.version.clone(),
                variants: item
                    .variants
                    .iter()
                    .map(|variant| CachedVariant {
                        id: variant.id.clone(),
                        product: variant.product.clone(),
                        kind: match variant.kind {
                            InstallKind::Desktop(_) => "desktop",
                            InstallKind::Cli(_) => "cli",
                            InstallKind::Vsix(_) => "vsix",
                        }
                        .to_string(),
                        label: variant.kind.label().to_string(),
                        version: variant.version.clone(),
                        path: variant.path.clone(),
                        installed: variant.installed,
                        channel: variant.channel.clone(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&cache).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "asm-installs-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_env(root: &Path) -> DetectEnv {
        DetectEnv {
            home: root.join("home"),
            appdata: root.join("appdata"),
            local: root.join("local"),
            program_files: root.join("program-files"),
        }
    }

    fn desktop_detector(keys: &[&str], exe: &str) -> DetectorSpec {
        DetectorSpec {
            kind: "desktop_app".into(),
            registry_keys: keys.iter().map(|k| k.to_string()).collect(),
            exe: exe.into(),
            ..DetectorSpec::default()
        }
    }

    fn variant(id: &str, kind: &str, label: &str, detectors: Vec<DetectorSpec>) -> CatalogVariant {
        CatalogVariant {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            channel: String::new(),
            detectors,
        }
    }

    fn product(id: &str, variants: Vec<CatalogVariant>) -> CatalogProduct {
        CatalogProduct {
            id: id.into(),
            label: id.into(),
            group_color: String::new(),
            default_open_variant: None,
            variants,
        }
    }

    /// 假 claude 目录：vsix 扩展 + cli（npm/原生）两个形态，默认打开扩展。
    fn claude_catalog() -> Catalog {
        let mut claude = product(
            "claude",
            vec![
                variant(
                    "claude-vscode",
                    "vsix",
                    "Claude Code for VS Code",
                    vec![DetectorSpec {
                        kind: "vscode_extension".into(),
                        roots: vec![".vscode".into(), ".vscode-cn".into()],
                        prefix: "anthropic.claude-code-".into(),
                        ..DetectorSpec::default()
                    }],
                ),
                variant(
                    "claude-cli",
                    "cli",
                    "Claude Code CLI",
                    vec![
                        DetectorSpec {
                            kind: "npm_package".into(),
                            root: "%APPDATA%/npm/node_modules".into(),
                            package: "@anthropic-ai/claude-code".into(),
                            ..DetectorSpec::default()
                        },
                        DetectorSpec {
                            kind: "native_binary".into(),
                            path: "~/.local/bin/claude.exe".into(),
                            ..DetectorSpec::default()
                        },
                    ],
                ),
            ],
        );
        claude.default_open_variant = Some("claude-vscode".into());
        Catalog {
            schema: 1,
            products: vec![claude],
            warnings: Vec::new(),
        }
    }

    fn seed_extension(env: &DetectEnv, root: &str, version: &str) -> PathBuf {
        let dir = env
            .home
            .join(root)
            .join("extensions")
            .join(format!("anthropic.claude-code-{version}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"claude-code","version":"{version}"}}"#),
        )
        .unwrap();
        dir
    }

    fn seed_npm(env: &DetectEnv, version: &str) -> PathBuf {
        let dir = env
            .appdata
            .join("npm")
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"@anthropic-ai/claude-code","version":"{version}"}}"#),
        )
        .unwrap();
        dir
    }

    fn find<'a>(list: &'a [AgentInstall], id: &str) -> &'a AgentInstall {
        list.iter().find(|item| item.id == id).unwrap()
    }

    #[test]
    fn install_kind_labels_and_keys() {
        let desktop = InstallKind::Desktop("Kiro".into());
        let cli = InstallKind::Cli("Claude Code CLI".into());
        let vsix = InstallKind::Vsix("Claude Code for VS Code".into());
        assert_eq!(desktop.label(), "Kiro");
        assert_eq!(cli.label(), "Claude Code CLI");
        assert_eq!(vsix.label(), "Claude Code for VS Code");
        assert_eq!(desktop.short_label(), "桌面端");
        assert_eq!(cli.short_label(), "CLI");
        assert_eq!(vsix.short_label(), "VS Code 扩展");
        assert_eq!(desktop.key(), "DESKTOP");
        assert_eq!(cli.key(), "CLI");
        assert_eq!(vsix.key(), "VSIX");
    }

    #[test]
    fn from_catalog_maps_known_kinds_only() {
        assert_eq!(
            InstallKind::from_catalog("desktop", "Qoder"),
            Some(InstallKind::Desktop("Qoder".into()))
        );
        assert_eq!(
            InstallKind::from_catalog("cli", "Claude Code CLI"),
            Some(InstallKind::Cli("Claude Code CLI".into()))
        );
        assert_eq!(
            InstallKind::from_catalog("vsix", "Claude Code for VS Code"),
            Some(InstallKind::Vsix("Claude Code for VS Code".into()))
        );
        assert_eq!(InstallKind::from_catalog("mobile", "X"), None);
    }

    const SAMPLE: &str = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\{A}
    DisplayName    REG_SZ    Qoder CN 0.5.7
    DisplayVersion    REG_SZ    0.5.7
    InstallLocation    REG_SZ    C:\Users\test\AppData\Local\Programs\Qoder CN
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B}
    DisplayName    REG_SZ    ZCode 3.17.2
    DisplayVersion    REG_SZ    3.17.2
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\{C}
    DisplayName    REG_SZ    Cursor (User)
    DisplayVersion    REG_SZ    3.29.15
    InstallLocation    REG_EXPAND_SZ    %LOCALAPPDATA%\Programs\cursor
"#;

    #[test]
    fn registry_output_includes_install_location() {
        let entries = parse_reg_output(SAMPLE);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Qoder CN 0.5.7");
        assert_eq!(entries[0].version, "0.5.7");
        assert_eq!(
            entries[0].location,
            r"C:\Users\test\AppData\Local\Programs\Qoder CN"
        );
        assert_eq!(entries[1].location, "");
        assert_eq!(entries[2].location, r"%LOCALAPPDATA%\Programs\cursor");
    }

    #[test]
    fn scan_extension_only() {
        let dir = temp_dir("ext-only");
        let env = temp_env(&dir);
        let ext = seed_extension(&env, ".vscode", "2.1.325");
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert!(claude.installed);
        assert_eq!(claude.version, "2.1.325");
        assert!(claude.variants[0].installed);
        assert_eq!(claude.variants[0].path, ext.to_string_lossy().to_string());
        assert!(!claude.variants[1].installed);
        assert_eq!(claude.variants[1].version, "");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scan_cli_only_via_npm() {
        let dir = temp_dir("cli-only");
        let env = temp_env(&dir);
        seed_npm(&env, "2.1.326");
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert!(claude.installed);
        // 默认打开形态（扩展）未安装：产品版本取第一个已安装形态（CLI）。
        assert_eq!(claude.version, "2.1.326");
        assert!(!claude.variants[0].installed);
        assert!(claude.variants[1].installed);
        assert_eq!(claude.variants[1].version, "2.1.326");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scan_both_forms_prefers_default_open_version() {
        let dir = temp_dir("both");
        let env = temp_env(&dir);
        seed_extension(&env, ".vscode-cn", "2.1.325");
        seed_npm(&env, "2.1.326");
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert!(claude.variants.iter().all(|v| v.installed));
        assert_eq!(claude.version, "2.1.325");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scan_none_installed_keeps_variants() {
        let dir = temp_dir("none");
        let env = temp_env(&dir);
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert!(!claude.installed);
        assert_eq!(claude.version, "");
        assert_eq!(claude.variants.len(), 2);
        assert!(claude.variants.iter().all(|v| !v.installed));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn extension_multiple_versions_keep_highest() {
        let dir = temp_dir("ext-multi");
        let env = temp_env(&dir);
        seed_extension(&env, ".vscode", "2.1.300");
        let newest = seed_extension(&env, ".vscode-cn", "2.1.325");
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert_eq!(claude.variants[0].version, "2.1.325");
        assert_eq!(
            claude.variants[0].path,
            newest.to_string_lossy().to_string()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn kiro_manager_is_excluded() {
        let catalog = Catalog {
            schema: 1,
            products: vec![product(
                "kiro",
                vec![variant(
                    "kiro-desktop",
                    "desktop",
                    "Kiro",
                    vec![DetectorSpec {
                        exclude_prefixes: vec!["kiro manager".into()],
                        ..desktop_detector(&["Kiro"], "Kiro.exe")
                    }],
                )],
            )],
            warnings: Vec::new(),
        };
        let dir = temp_dir("kiro");
        let env = temp_env(&dir);
        let manager_only = vec![RegEntry {
            name: "Kiro Manager Lite 1.0.44".into(),
            version: "1.0.44".into(),
            location: String::new(),
        }];
        let list = scan_with(&catalog, &manager_only, &env);
        assert!(!find(&list, "kiro").installed);
        let registry = vec![
            manager_only[0].clone(),
            RegEntry {
                name: "Kiro (User)".into(),
                version: "1.4.121".into(),
                location: r"C:\Kiro".into(),
            },
        ];
        let list = scan_with(&catalog, &registry, &env);
        let kiro = find(&list, "kiro");
        assert!(kiro.installed);
        assert_eq!(kiro.version, "1.4.121");
        assert_eq!(kiro.variants[0].path, r"C:\Kiro");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shortcut_fallback_when_unregistered() {
        let catalog = Catalog {
            schema: 1,
            products: vec![product(
                "dsh",
                vec![variant(
                    "dsh-desktop",
                    "desktop",
                    "DeepSeek Harness",
                    vec![DetectorSpec {
                        shortcut_names: vec!["DeepSeek Harness".into()],
                        ..desktop_detector(&["DeepSeek Harness"], "DeepSeek Harness.exe")
                    }],
                )],
            )],
            warnings: Vec::new(),
        };
        let dir = temp_dir("shortcut");
        let env = temp_env(&dir);
        let programs = env
            .appdata
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        fs::create_dir_all(&programs).unwrap();
        let lnk = programs.join("DeepSeek Harness.lnk");
        fs::write(&lnk, "stub").unwrap();
        let list = scan_with(&catalog, &[], &env);
        let dsh = find(&list, "dsh");
        assert!(dsh.installed);
        assert_eq!(dsh.version, "");
        assert_eq!(dsh.variants[0].path, lnk.to_string_lossy().to_string());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn install_dir_fallback_without_exe_version() {
        let catalog = Catalog {
            schema: 1,
            products: vec![product(
                "qoder",
                vec![variant(
                    "qoder-desktop",
                    "desktop",
                    "Qoder",
                    vec![desktop_detector(&["Qoder"], "Qoder CN.exe")],
                )],
            )],
            warnings: Vec::new(),
        };
        let dir = temp_dir("install-dir");
        let env = temp_env(&dir);
        // 只建目录不放 exe：命中安装目录，版本留空（不触发 PowerShell）。
        let root = env.local.join("Programs").join("Qoder CN");
        fs::create_dir_all(&root).unwrap();
        let list = scan_with(&catalog, &[], &env);
        let qoder = find(&list, "qoder");
        assert!(qoder.installed);
        assert_eq!(qoder.version, "");
        assert_eq!(qoder.variants[0].path, root.to_string_lossy().to_string());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_binary_fallback_detects_file_without_version() {
        let dir = temp_dir("native");
        let env = temp_env(&dir);
        let bin = env.home.join(".local").join("bin").join("claude.exe");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "stub").unwrap();
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert!(claude.variants[1].installed);
        assert_eq!(claude.variants[1].version, "");
        assert_eq!(PathBuf::from(&claude.variants[1].path), bin);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn npm_version_wins_over_native_empty_version() {
        let dir = temp_dir("npm-native");
        let env = temp_env(&dir);
        seed_npm(&env, "2.1.326");
        let bin = env.home.join(".local").join("bin").join("claude.exe");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "stub").unwrap();
        let list = scan_with(&claude_catalog(), &[], &env);
        let claude = find(&list, "claude");
        assert_eq!(claude.variants[1].version, "2.1.326");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn state_db_fallback_detects_cursor() {
        let catalog = Catalog {
            schema: 1,
            products: vec![product(
                "cursor",
                vec![variant(
                    "cursor-desktop",
                    "desktop",
                    "Cursor",
                    vec![DetectorSpec {
                        state_db: "%APPDATA%/Cursor/User/globalStorage/state.vscdb".into(),
                        ..desktop_detector(&["Cursor"], "Cursor.exe")
                    }],
                )],
            )],
            warnings: Vec::new(),
        };
        let dir = temp_dir("state-db");
        let env = temp_env(&dir);
        let db = env
            .appdata
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        fs::write(&db, "stub").unwrap();
        let list = scan_with(&catalog, &[], &env);
        let cursor = find(&list, "cursor");
        assert!(cursor.installed);
        assert_eq!(cursor.version, "");
        assert_eq!(PathBuf::from(&cursor.variants[0].path), db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expand_env_whitelist_and_unknown_vars() {
        let dir = temp_dir("expand");
        let env = temp_env(&dir);
        let home = env.home.to_string_lossy().to_string();
        let appdata = env.appdata.to_string_lossy().to_string();
        assert_eq!(
            expand_env("%APPDATA%/npm/node_modules", &env),
            format!("{appdata}/npm/node_modules")
        );
        assert_eq!(
            PathBuf::from(expand_env("~/.local/bin/claude.exe", &env)),
            env.home.join(".local").join("bin").join("claude.exe")
        );
        assert_eq!(expand_env("%USERPROFILE%\\x", &env), format!("{home}\\x"));
        // 未知变量原样保留。
        assert_eq!(expand_env("%FOO%/bar", &env), "%FOO%/bar");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_cache_maps_to_first_variant() {
        let dir = temp_dir("legacy-cache");
        let path = dir.join("installs.json");
        fs::write(
            &path,
            r#"[{"id":"claude","installed":true,"version":"2.1.325"},{"id":"qoder","installed":false,"version":""}]"#,
        )
        .unwrap();
        let mut catalog = claude_catalog();
        catalog.products.push(product(
            "qoder",
            vec![variant(
                "qoder-desktop",
                "desktop",
                "Qoder",
                vec![desktop_detector(&["Qoder"], "Qoder CN.exe")],
            )],
        ));
        let list = load_from(&path, &catalog);
        let claude = find(&list, "claude");
        assert!(claude.installed);
        assert_eq!(claude.version, "2.1.325");
        assert!(claude.variants[0].installed);
        assert_eq!(claude.variants[0].version, "2.1.325");
        assert!(!claude.variants[1].installed);
        let qoder = find(&list, "qoder");
        assert!(!qoder.installed);
        assert_eq!(qoder.variants.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_cache_roundtrip_keeps_variants() {
        let dir = temp_dir("cache-roundtrip");
        let path = dir.join("installs.json");
        let env = temp_env(&dir);
        seed_extension(&env, ".vscode", "2.1.325");
        // 补一个未安装产品，确认未安装形态也完整往返。
        let mut catalog = claude_catalog();
        catalog.products.push(product(
            "qoder",
            vec![variant(
                "qoder-desktop",
                "desktop",
                "Qoder",
                vec![desktop_detector(&["Qoder"], "Qoder CN.exe")],
            )],
        ));
        let list = scan_with(&catalog, &[], &env);
        save_to(&path, &list).unwrap();
        assert_eq!(load_from(&path, &catalog), list);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_falls_back_when_missing_or_corrupt() {
        let dir = temp_dir("cache-corrupt");
        let path = dir.join("installs.json");
        let catalog = claude_catalog();
        assert!(load_from(&path, &catalog).is_empty());
        fs::write(&path, "{not json").unwrap();
        assert!(load_from(&path, &catalog).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_output_parses_first_version_token() {
        assert_eq!(
            parse_version_output("2.1.326 (Claude Code)"),
            Some("2.1.326".into())
        );
        assert_eq!(parse_version_output("v1.2.3"), Some("1.2.3".into()));
        assert_eq!(parse_version_output("command not found"), None);
        assert_eq!(parse_version_output(""), None);
    }
}
