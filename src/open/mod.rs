use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::agents;
use crate::installs::{AgentInstall, InstallKind, InstallVariant};
use crate::plugins::manifest::{OpenManifest, VariantManifest};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 用插件声明的启动策略打开应用。cwd 非空时按 variant 的 args 模板传入；
/// 带 session_id 时使用 session_args 或 session_uri。
pub fn open_in(agent: &str, cwd: &str, session_id: Option<&str>) -> Result<()> {
    open_agent_in(agent, cwd, session_id)
}

/// 该形态是否支持打开：以插件 manifest 的 variant.open 声明为准。
/// 尚未注册的产品（例如测试夹具）按可打开处理，不在宿主写死产品规则。
pub fn variant_openable(product: &str, kind: &InstallKind) -> bool {
    let Some(info) = agents::get(product) else {
        return true;
    };
    let Some(manifest) = &info.manifest else {
        return true;
    };
    let kind = kind_key(kind);
    manifest
        .variants
        .iter()
        .any(|variant| variant.kind == kind && !variant.open.strategy.trim().is_empty())
}

fn kind_key(kind: &InstallKind) -> &'static str {
    match kind {
        InstallKind::Desktop(_) => "desktop",
        InstallKind::Cli(_) => "cli",
        InstallKind::Vsix(_) => "vsix",
    }
}

/// 解析产品的默认打开形态：用户设置 → 插件 default_open → 第一个已安装可打开形态。
pub fn resolve_open_variant(
    product: &str,
    installs: &[AgentInstall],
    user_default: Option<&str>,
) -> Option<String> {
    let install = installs.iter().find(|item| item.id == product)?;
    let usable =
        |variant: &InstallVariant| variant.installed && variant_openable(product, &variant.kind);
    if let Some(id) = user_default {
        if let Some(variant) = install.variants.iter().find(|v| v.id == id && usable(v)) {
            return Some(variant.id.clone());
        }
    }
    if let Some(id) = &install.default_open_variant {
        if let Some(variant) = install.variants.iter().find(|v| v.id == *id && usable(v)) {
            return Some(variant.id.clone());
        }
    }
    install
        .variants
        .iter()
        .find(|v| usable(v))
        .map(|v| v.id.clone())
}

pub fn open_agent_in(id: &str, cwd: &str, session_id: Option<&str>) -> Result<()> {
    let info = agents::get(id).with_context(|| format!("未知 agent：{id}"))?;
    let _ = info.adapter.prepare_open(session_id);
    let installs = crate::installs::load_cache();
    let config = crate::config::load();
    if let Some(variant) = resolve_open_variant(
        id,
        &installs,
        config.open.defaults.get(id).map(String::as_str),
    ) {
        return open_variant_in(id, &variant, cwd, session_id);
    }

    // 没有可用安装缓存时，按 manifest 顺序尝试 default_open 在前、其余变体兜底。
    if let Some(manifest) = &info.manifest {
        let mut variants: Vec<&VariantManifest> = manifest.variants.iter().collect();
        variants.sort_by_key(|variant| !variant.default_open);
        let mut last_error = None;
        for variant in variants {
            match launch_variant(&variant.open, cwd, session_id) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            return Err(error.context(format!("launch {}", info.label)));
        }
    }
    launch(&info.launch).with_context(|| format!("launch {}", info.label))
}

pub fn open_variant_in(
    product: &str,
    variant_id: &str,
    cwd: &str,
    session_id: Option<&str>,
) -> Result<()> {
    let info = agents::get(product).with_context(|| format!("未知 agent：{product}"))?;
    let _ = info.adapter.prepare_open(session_id);
    let Some(manifest) = &info.manifest else {
        return launch(&info.launch).with_context(|| format!("launch {}", info.label));
    };
    let variant = manifest
        .variants
        .iter()
        .find(|variant| variant.id == variant_id)
        .with_context(|| format!("{} 没有形态 {variant_id}", info.label))?;
    launch_variant(&variant.open, cwd, session_id).with_context(|| format!("launch {}", info.label))
}

pub fn open_variant_direct(product: &str, variant_id: &str) -> Result<()> {
    let kind = crate::installs::load_cache()
        .into_iter()
        .find(|item| item.id == product)
        .and_then(|item| {
            item.variants
                .into_iter()
                .find(|variant| variant.id == variant_id)
        })
        .map(|variant| variant.kind);
    let cwd = direct_open_cwd(kind.as_ref());
    open_variant_in(product, variant_id, &cwd, None)
}

fn direct_open_cwd(kind: Option<&InstallKind>) -> String {
    match kind {
        Some(InstallKind::Cli(_)) => std::env::var("USERPROFILE").unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn restart(id: &str, cwd: &str, session_id: Option<&str>) -> Result<()> {
    close(id)?;
    open_agent_in(id, cwd, session_id)
}

pub fn close(id: &str) -> Result<()> {
    let info = agents::get(id).with_context(|| format!("未知 agent：{id}"))?;
    info.adapter.ensure_closable()?;
    match info.adapter.close_behavior() {
        agent_core::CloseBehavior::CheckOnly => Ok(()),
        agent_core::CloseBehavior::Image(image) => close_image(&image, &info.label),
        agent_core::CloseBehavior::Process => close_image(&info.process, &info.label),
    }
}

fn close_image(image: &str, label: &str) -> Result<()> {
    if !crate::transfer::process_running(image) {
        return Ok(());
    }
    let _ = run_taskkill(&["/IM", image]);
    if wait_gone(image, std::time::Duration::from_secs(5)) {
        return Ok(());
    }
    let _ = run_taskkill(&["/IM", image, "/T", "/F"]);
    if wait_gone(image, std::time::Duration::from_secs(10)) {
        Ok(())
    } else {
        anyhow::bail!("{label} 未能关闭，请手动关闭后重试")
    }
}

fn run_taskkill(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new("taskkill");
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output()
}

fn wait_gone(image: &str, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while crate::transfer::process_running(image) {
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    true
}

fn launch(protocol: &str) -> Result<()> {
    let exe = registry_exe(protocol)?;
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

fn launch_variant(spec: &OpenManifest, cwd: &str, session_id: Option<&str>) -> Result<()> {
    let args = render_args(spec, cwd, session_id);
    match spec.strategy.as_str() {
        "registry" => {
            let exe = registry_exe(&spec.command)?;
            start_detached_in(&exe, cwd, &args)
        }
        "command" => {
            let command = expand_path(&spec.command);
            start_detached_in(&command, cwd, &args)
        }
        "candidates" => {
            for candidate in &spec.candidates {
                let path = expand_path(candidate);
                if path.is_file() {
                    return start_detached_in(&path, cwd, &args);
                }
            }
            anyhow::bail!("未找到可启动的候选路径")
        }
        "vscode_workspace" => {
            launch_vscode_workspace(cwd)?;
            if let Some(uri) = &spec.session_uri {
                if let Some(uri) = render_template(uri, cwd, session_id) {
                    start_uri(&uri)?;
                }
            }
            Ok(())
        }
        "uri" => {
            let template = match (session_id, &spec.session_uri) {
                (Some(_), Some(uri)) => uri,
                _ => &spec.command,
            };
            let uri = render_template(template, cwd, session_id)
                .context("启动链接缺少 cwd 或 session_id")?;
            start_uri(&uri)
        }
        other => anyhow::bail!("未知启动策略：{other}"),
    }
}

fn render_args(spec: &OpenManifest, cwd: &str, session_id: Option<&str>) -> Vec<String> {
    let mut templates = spec.args.clone();
    if session_id.is_some_and(|id| !id.trim().is_empty()) {
        templates.extend(spec.session_args.iter().cloned());
    }
    templates
        .into_iter()
        .filter_map(|arg| render_template(&arg, cwd, session_id))
        .collect()
}

fn render_template(template: &str, cwd: &str, session_id: Option<&str>) -> Option<String> {
    let mut value = template.to_string();
    if value.contains("{cwd}") {
        let dir = usable_cwd(cwd)?;
        value = value.replace("{cwd}", &dir.to_string_lossy());
    }
    if value.contains("{session_id}") {
        let id = session_id.map(str::trim).filter(|id| !id.is_empty())?;
        value = value.replace("{session_id}", id);
    }
    Some(value)
}

fn expand_path(template: &str) -> PathBuf {
    let mut text = template.to_string();
    for name in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramFiles"] {
        let value = std::env::var(name).unwrap_or_default();
        text = text.replace(&format!("%{name}%"), &value);
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        text = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
            .join(rest)
            .to_string_lossy()
            .to_string();
    }
    PathBuf::from(text)
}

fn launch_vscode_workspace(cwd: &str) -> Result<()> {
    let exe = find_vscode_exe()
        .context("未找到 VS Code 启动路径，请确认 VS Code 已安装并已登记协议、App Paths 或 PATH")?;
    let args: Vec<String> = usable_cwd(cwd)
        .map(|dir| vec![dir.to_string_lossy().to_string()])
        .unwrap_or_default();
    let is_script = exe
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("cmd"))
        .unwrap_or(false);
    if is_script {
        return start_detached_in(&exe, "", &args);
    }
    let mut cmd = Command::new(&exe);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

fn find_vscode_exe() -> Option<PathBuf> {
    if let Ok(exe) = registry_exe("vscode") {
        if exe.is_file() {
            return Some(exe);
        }
    }
    for key in [
        "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Code.exe",
        "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Code.exe",
    ] {
        let Some(value) = reg_default_string(key) else {
            continue;
        };
        let exe = PathBuf::from(value);
        if exe.is_file() {
            return Some(exe);
        }
    }
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let program_files = std::env::var("PROGRAMFILES").unwrap_or_default();
    for root in [
        PathBuf::from(&local)
            .join("Programs")
            .join("Microsoft VS Code"),
        PathBuf::from(&program_files).join("Microsoft VS Code"),
    ] {
        let exe = root.join("Code.exe");
        if exe.is_file() {
            return Some(exe);
        }
    }
    let path = std::env::var_os("PATH")?;
    find_vscode_in_path(&path)
}

fn find_vscode_in_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        let exe = dir.join("Code.exe");
        if exe.is_file() {
            return Some(exe);
        }
    }
    for dir in std::env::split_paths(path) {
        let script = dir.join("code.cmd");
        if !script.is_file() {
            continue;
        }
        if let Some(root) = dir.parent() {
            let exe = root.join("Code.exe");
            if exe.is_file() {
                return Some(exe);
            }
        }
        return Some(script);
    }
    None
}

fn reg_default_string(key: &str) -> Option<String> {
    let mut cmd = Command::new("reg");
    cmd.args(["query", key, "/ve"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_default(&String::from_utf8_lossy(&out.stdout))
}

fn parse_reg_default(stdout: &str) -> Option<String> {
    let (_, tail) = stdout.split_once("REG_SZ")?;
    let value = tail.lines().next()?.trim();
    let value = match value.strip_prefix('"') {
        Some(rest) => rest
            .split_once('"')
            .map(|(quoted, _)| quoted)
            .unwrap_or(rest),
        None => value,
    };
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn start_uri(uri: &str) -> Result<()> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "start", "", uri]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

fn start_detached_in(target: &Path, cwd: &str, args: &[String]) -> Result<()> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "start", "", &target.to_string_lossy().to_string()]);
    cmd.args(args);
    if let Some(dir) = usable_cwd(cwd) {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

fn usable_cwd(cwd: &str) -> Option<&Path> {
    let path = Path::new(cwd);
    (!cwd.trim().is_empty() && path.is_dir()).then_some(path)
}

fn registry_exe(protocol: &str) -> Result<PathBuf> {
    let mut cmd = Command::new("reg");
    cmd.args([
        "query",
        &format!("HKCU\\Software\\Classes\\{protocol}\\shell\\open\\command"),
        "/ve",
    ]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().context("query protocol registry")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text
        .find('"')
        .context("protocol command has no quoted exe")?;
    let rest = &text[start + 1..];
    let end = rest.find('"').context("unterminated exe quote")?;
    Ok(PathBuf::from(&rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_cwd_requires_an_existing_directory() {
        let dir = std::env::temp_dir();
        assert_eq!(usable_cwd(dir.to_str().unwrap()), Some(dir.as_path()));
        assert!(usable_cwd("").is_none());
        assert!(usable_cwd("   ").is_none());
        let missing = dir.join(format!("asm-missing-{}", uuid::Uuid::new_v4()));
        assert!(usable_cwd(missing.to_str().unwrap()).is_none());
    }

    #[test]
    fn direct_open_uses_home_only_for_cli() {
        assert!(direct_open_cwd(None).is_empty());
        assert!(direct_open_cwd(Some(&InstallKind::Desktop("Qoder".into()))).is_empty());
        assert!(direct_open_cwd(Some(&InstallKind::Vsix("Ext".into()))).is_empty());
        if let Ok(home) = std::env::var("USERPROFILE") {
            assert_eq!(direct_open_cwd(Some(&InstallKind::Cli("CLI".into()))), home);
        }
    }

    #[test]
    fn reg_default_value_parsing() {
        let quoted = "HKEY_CURRENT_USER\\Software\\Classes\\vscode\\shell\\open\\command\r\n    (默认)    REG_SZ    \"D:\\Apps\\Microsoft VS Code\\Code.exe\" --open-url -- \"%1\"\r\n\r\n";
        assert_eq!(
            parse_reg_default(quoted).as_deref(),
            Some("D:\\Apps\\Microsoft VS Code\\Code.exe")
        );
        let plain = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Code.exe\r\n    (Default)    REG_SZ    D:\\Apps\\Microsoft VS Code\\Code.exe\r\n\r\n";
        assert_eq!(
            parse_reg_default(plain).as_deref(),
            Some("D:\\Apps\\Microsoft VS Code\\Code.exe")
        );
        assert_eq!(
            parse_reg_default("错误: 系统找不到指定的注册表项或值。"),
            None
        );
    }

    #[test]
    fn vscode_launcher_from_path_prefers_exe_then_bin_script() {
        let root = std::env::temp_dir().join(format!("asm-vscode-{}", uuid::Uuid::new_v4()));
        let bin = root.join("Microsoft VS Code").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("code.cmd"), "@echo off\r\n").unwrap();
        let path = std::ffi::OsString::from(bin.to_string_lossy().to_string());
        assert_eq!(find_vscode_in_path(&path), Some(bin.join("code.cmd")));
        let exe = root.join("Microsoft VS Code").join("Code.exe");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(find_vscode_in_path(&path), Some(exe));
        let empty = std::env::temp_dir().join(format!("asm-vscode-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            find_vscode_in_path(std::ffi::OsStr::new(empty.to_str().unwrap())),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn render_args_drop_missing_session_and_cwd() {
        let spec = OpenManifest {
            strategy: "command".into(),
            command: "claude".into(),
            args: vec!["{cwd}".into(), "--flag".into()],
            session_args: vec!["--resume".into(), "{session_id}".into()],
            ..OpenManifest::default()
        };
        let dir = std::env::temp_dir();
        assert_eq!(
            render_args(&spec, dir.to_str().unwrap(), Some(" id ")),
            vec![
                dir.to_string_lossy().to_string(),
                "--flag".to_string(),
                "--resume".to_string(),
                "id".to_string()
            ]
        );
        assert_eq!(render_args(&spec, "", None), vec!["--flag".to_string()]);
    }

    fn fake_variant(id: &str, product: &str, kind: InstallKind, installed: bool) -> InstallVariant {
        InstallVariant {
            id: id.into(),
            product: product.into(),
            kind,
            version: if installed {
                "1.0.0".into()
            } else {
                String::new()
            },
            path: String::new(),
            installed,
            channel: String::new(),
        }
    }

    fn claude_install(vscode: bool, cli: bool) -> AgentInstall {
        AgentInstall {
            id: "claude".to_string(),
            label: "Claude Code".to_string(),
            installed: vscode || cli,
            version: String::new(),
            default_open_variant: Some("claude-vscode".into()),
            variants: vec![
                fake_variant(
                    "claude-vscode",
                    "claude",
                    InstallKind::Vsix("Claude Code for VS Code".into()),
                    vscode,
                ),
                fake_variant(
                    "claude-cli",
                    "claude",
                    InstallKind::Cli("Claude Code CLI".into()),
                    cli,
                ),
            ],
        }
    }

    #[test]
    fn resolve_prefers_user_default_when_usable() {
        let installs = vec![claude_install(true, true)];
        assert_eq!(
            resolve_open_variant("claude", &installs, Some("claude-cli")).as_deref(),
            Some("claude-cli")
        );
    }

    #[test]
    fn resolve_falls_back_to_catalog_default() {
        let installs = vec![claude_install(true, true)];
        assert_eq!(
            resolve_open_variant("claude", &installs, None).as_deref(),
            Some("claude-vscode")
        );
        assert_eq!(
            resolve_open_variant(
                "claude",
                &vec![claude_install(true, false)],
                Some("claude-cli")
            )
            .as_deref(),
            Some("claude-vscode")
        );
    }

    #[test]
    fn resolve_falls_back_to_first_remaining_openable() {
        let installs = vec![claude_install(false, true)];
        assert_eq!(
            resolve_open_variant("claude", &installs, None).as_deref(),
            Some("claude-cli")
        );
        assert_eq!(
            resolve_open_variant("claude", &installs, Some("claude-vscode")).as_deref(),
            Some("claude-cli")
        );
    }

    #[test]
    fn resolve_none_when_nothing_openable() {
        let empty = AgentInstall {
            id: "ghost".into(),
            label: "Ghost".into(),
            installed: false,
            version: String::new(),
            default_open_variant: None,
            variants: Vec::new(),
        };
        assert_eq!(resolve_open_variant("ghost", &[empty], None), None);
    }
}
