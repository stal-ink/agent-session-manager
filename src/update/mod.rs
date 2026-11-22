//! App 自更新与插件在线更新的公共清单、签名、下载与选择逻辑。
//! 网络请求只在后台检查线程执行；解析、版本选择与签名验证都是纯函数。

use std::cmp::Ordering;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod app;
pub mod install;
pub mod plugins;

/// 官方仓库根地址；更新清单与下载地址都由它派生。
pub const REPO_ROOT: &str = "https://github.com/stal-ink/agent-session-manager";
/// 覆盖仓库根地址的环境变量，用于本地 fake hub 测试。
pub const REPO_ROOT_ENV: &str = "ASM_REPO_ROOT";
/// 插件发布使用的固定预发布标签。
pub const PLUGIN_TAG: &str = "plugins";
/// 构建时注入的更新签名公钥；未设置时更新检查会给出明确错误。
pub const PUBLIC_KEY_HEX: &str = match option_env!("ASM_UPDATE_PUBKEY") {
    Some(value) => value,
    None => "",
};
/// 单个更新文件大小上限。
pub const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppManifest {
    pub schema: u32,
    pub version: String,
    /// 软件本体 exe 的下载地址，不是插件或 Agent 产品的地址。
    pub exe_url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub notes_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIndex {
    pub schema: u32,
    /// 索引生成时间；由发布工具写入，仅用于诊断。
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    /// 插件显示名；旧索引没有该字段时回退到 id。
    #[serde(default)]
    pub label: String,
    pub version: String,
    /// 插件包 zip 的下载地址，不是 Agent 产品下载页。
    #[serde(alias = "url")]
    pub zip_url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    /// 要求的最低宿主版本；为空表示不限制，宿主低于该版本时不显示更新。
    #[serde(default)]
    pub min_host: String,
}

/// 下载进度；total 为 0 表示清单未给出大小。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

/// 当前生效的仓库根地址；`ASM_REPO_ROOT` 优先，末尾斜杠会被去掉。
pub fn repo_root() -> String {
    let value = std::env::var(REPO_ROOT_ENV).ok();
    normalize_repo_root(value.as_deref())
}

/// app 发布资产目录（跟随 latest 正式 release）。
pub fn app_release_base() -> String {
    format!("{}/releases/latest/download", repo_root())
}

/// 插件发布资产目录（固定 `plugins` 预发布标签）。
pub fn plugin_release_base() -> String {
    format!("{}/releases/download/{PLUGIN_TAG}", repo_root())
}

/// 官方 app 更新清单地址。
pub fn app_manifest_url() -> String {
    format!("{}/app.json", app_release_base())
}

/// 插件索引地址。
pub fn plugin_index_url() -> String {
    format!("{}/plugins-index.json", plugin_release_base())
}

fn normalize_repo_root(value: Option<&str>) -> String {
    value
        .map(|value| value.trim().trim_end_matches('/'))
        .filter(|value| !value.is_empty())
        .unwrap_or(REPO_ROOT)
        .to_string()
}

/// 校验 app 清单签名并解析；只返回比当前版本更新的结果。
pub fn check_app(url: &str) -> Result<Option<AppManifest>> {
    let (body, signature) = fetch_manifest(url)?;
    verify_with_builtin_key(body.as_bytes(), &signature)?;
    let manifest = parse_app_manifest(&body)?;
    Ok(is_newer(&manifest.version).then_some(manifest))
}

/// 校验插件索引签名并解析；返回原始索引，相对下载地址保持原样。
/// 展示或安装前调用 [`resolve_plugin_index`] 按当前索引地址展开。
pub fn check_plugins(url: &str) -> Result<PluginIndex> {
    let (body, signature) = fetch_manifest(url)?;
    verify_with_builtin_key(body.as_bytes(), &signature)?;
    parse_plugin_index(&body)
}

/// 把索引里的相对 `zip_url` 按索引所在目录展开，并校验最终地址。
pub fn resolve_plugin_index(index_url: &str, index: &mut PluginIndex) -> Result<()> {
    resolve_plugin_urls(index_url, index);
    for entry in &index.plugins {
        validate_transfer_url(&format!("插件 {} 的下载地址", entry.id), &entry.zip_url)?;
    }
    Ok(())
}

pub fn parse_app_manifest(body: &str) -> Result<AppManifest> {
    let manifest: AppManifest = serde_json::from_str(body).context("app 更新清单不是有效 JSON")?;
    if manifest.schema != 1 {
        bail!("不支持的 app 更新清单 schema：{}", manifest.schema);
    }
    if manifest.version.trim().is_empty() {
        bail!("app 更新清单缺少版本号");
    }
    validate_transfer_url("app 更新清单", &manifest.exe_url)?;
    validate_sha256("app", &manifest.sha256)?;
    Ok(manifest)
}

pub fn parse_plugin_index(body: &str) -> Result<PluginIndex> {
    let index: PluginIndex = serde_json::from_str(body).context("插件索引不是有效 JSON")?;
    if index.schema != 1 {
        bail!("不支持的插件索引 schema：{}", index.schema);
    }
    for entry in &index.plugins {
        if entry.id.trim().is_empty() || entry.version.trim().is_empty() {
            bail!("插件索引存在缺少 id 或 version 的条目");
        }
        validate_plugin_url(&entry.id, &entry.zip_url)?;
        validate_sha256(&entry.id, &entry.sha256)?;
    }
    Ok(index)
}

/// 索引里的下载地址可以是绝对地址，也可以是相对索引目录的文件名。
fn validate_plugin_url(id: &str, url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        bail!("插件 {id} 的下载地址为空");
    }
    if is_relative_url(url) {
        if url.starts_with('/') || url.split('/').any(|segment| segment == "..") {
            bail!("插件 {id} 的下载地址必须是相对文件名");
        }
        return Ok(());
    }
    validate_transfer_url(&format!("插件 {id} 的下载地址"), url)
}

fn is_relative_url(url: &str) -> bool {
    !url.contains("://")
}

/// 相对下载地址按索引所在目录展开；线上与本地 fake hub 共用同一份索引。
fn resolve_plugin_urls(index_url: &str, index: &mut PluginIndex) {
    let Some((base, _)) = index_url.rsplit_once('/') else {
        return;
    };
    let base = base.trim_end_matches('/');
    for entry in &mut index.plugins {
        let url = entry.zip_url.trim();
        if !is_relative_url(url) {
            continue;
        }
        entry.zip_url = format!("{base}/{}", url.trim_start_matches("./"));
    }
}

/// 从已安装版本中挑出可更新的插件；未安装的条目不参与。
pub fn select_plugin_updates(
    installed: &std::collections::HashMap<String, String>,
    index: &PluginIndex,
) -> Vec<PluginEntry> {
    let mut updates: Vec<PluginEntry> = index
        .plugins
        .iter()
        .filter(|entry| {
            let Some(current) = installed.get(&entry.id) else {
                return false;
            };
            if crate::updates::compare(&entry.version, current) != Ordering::Greater {
                return false;
            }
            entry.min_host.is_empty()
                || crate::updates::compare(env!("CARGO_PKG_VERSION"), &entry.min_host)
                    != Ordering::Less
        })
        .cloned()
        .collect();
    updates.sort_by(|left, right| left.id.cmp(&right.id));
    updates
}

pub fn is_newer(version: &str) -> bool {
    crate::updates::compare(version, env!("CARGO_PKG_VERSION")) == Ordering::Greater
}

pub fn verify_with_builtin_key(bytes: &[u8], signature_hex: &str) -> Result<()> {
    if PUBLIC_KEY_HEX.trim().is_empty() {
        bail!("未配置更新签名公钥；构建时设置 ASM_UPDATE_PUBKEY");
    }
    verify_signature(bytes, signature_hex, PUBLIC_KEY_HEX)
}

/// Ed25519 验签；签名与公钥都是十六进制文本。
///
/// Ed25519（RFC 8032）签名固定 64 字节、公钥固定 32 字节，长度不符直接拒绝。
pub fn verify_signature(bytes: &[u8], signature_hex: &str, public_hex: &str) -> Result<()> {
    let signature = hex::decode(signature_hex.trim()).context("更新签名不是有效的十六进制")?;
    let public = hex::decode(public_hex.trim()).context("更新公钥不是有效的十六进制")?;
    if signature.len() != 64 {
        bail!("更新签名长度必须为 64 字节，实际为 {}", signature.len());
    }
    if public.len() != 32 {
        bail!("更新公钥长度必须为 32 字节，实际为 {}", public.len());
    }
    UnparsedPublicKey::new(&ED25519, &public)
        .verify(bytes, &signature)
        .map_err(|_| anyhow!("更新签名验证失败"))
}

/// 拉取清单正文与签名；签名以 `<清单 url>.sig` sidecar 发布，与发布工具约定一致。
pub fn fetch_manifest(url: &str) -> Result<(String, String)> {
    let body = fetch_text(url)?;
    let signature = fetch_text(&format!("{url}.sig"))?;
    Ok((body, signature))
}

pub fn fetch_text(url: &str) -> Result<String> {
    let mut response = http_agent()
        .get(url)
        .call()
        .map_err(|error| anyhow!("网络请求失败：{error}"))?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|error| anyhow!("读取更新清单失败：{error}"))
}

/// 流式下载并校验 SHA-256；先写 `.part`，成功后改名。
pub fn download_to(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    expected_size: u64,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<()> {
    if expected_size > MAX_DOWNLOAD_SIZE {
        bail!("更新文件超过大小上限");
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建更新目录失败：{}", parent.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let result = (|| -> Result<()> {
        let mut response = http_agent()
            .get(url)
            .call()
            .map_err(|error| anyhow!("下载失败：{error}"))?;
        let mut reader = response.body_mut().as_reader();
        let mut file = fs::File::create(&part)
            .with_context(|| format!("创建下载文件失败：{}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut downloaded = 0u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            downloaded += read as u64;
            if downloaded > MAX_DOWNLOAD_SIZE {
                bail!("更新文件超过大小上限");
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
            progress(DownloadProgress {
                downloaded,
                total: expected_size,
            });
        }
        file.sync_all()?;
        drop(file);
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            bail!("更新文件 SHA-256 不匹配");
        }
        // 清单未声明 size（0）时跳过严格长度校验，SHA-256 仍是最终判据。
        if expected_size > 0 && downloaded != expected_size {
            bail!("更新文件大小不匹配：期望 {expected_size}，实际 {downloaded}");
        }
        if dest.exists() {
            fs::remove_file(dest)
                .with_context(|| format!("替换旧更新文件失败：{}", dest.display()))?;
        }
        fs::rename(&part, dest).with_context(|| format!("保存更新文件失败：{}", dest.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

pub fn file_name_from_url(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("update.bin").to_string()
}

pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(crate::updates::TIMEOUT))
        .user_agent(crate::updates::USER_AGENT)
        .build()
        .new_agent()
}

fn validate_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} 的 sha256 必须是 64 位十六进制");
    }
    Ok(())
}

/// 更新地址必须是 HTTPS；本地 fake hub 允许回环地址的 HTTP。
fn validate_transfer_url(label: &str, url: &str) -> Result<()> {
    if url.starts_with("https://") || (url.starts_with("http://") && is_loopback(url)) {
        return Ok(());
    }
    bail!("{label}必须是 HTTPS 或回环 HTTP")
}

fn is_loopback(url: &str) -> bool {
    let authority = url["http://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority
            .split_once(':')
            .map_or(authority, |(host, _port)| host),
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC8032_PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const RFC8032_SIGNATURE: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    #[test]
    fn normalizes_repo_root_override() {
        assert_eq!(normalize_repo_root(None), REPO_ROOT);
        assert_eq!(normalize_repo_root(Some("   ")), REPO_ROOT);
        assert_eq!(
            normalize_repo_root(Some(" http://127.0.0.1:8765/ ")),
            "http://127.0.0.1:8765"
        );
    }

    #[test]
    fn allows_loopback_http_for_fake_hub() {
        assert!(validate_transfer_url("app", "https://example.com/app.json").is_ok());
        assert!(validate_transfer_url("app", "http://127.0.0.1:8765/app.json").is_ok());
        assert!(validate_transfer_url("app", "http://localhost:8765/app.json").is_ok());
        assert!(validate_transfer_url("app", "http://[::1]:8765/app.json").is_ok());
        assert!(validate_transfer_url("app", "http://example.com/app.json").is_err());
        assert!(validate_transfer_url("app", "http://127.0.0.1.example.com/app.json").is_err());
        assert!(validate_transfer_url("app", "ftp://127.0.0.1/app.json").is_err());
    }

    #[test]
    fn resolves_relative_plugin_urls_against_index_directory() {
        let sha = "a".repeat(64);
        let body = format!(
            r#"{{"schema":1,"plugins":[
                {{"id":"trae","version":"0.1.6","zip_url":"trae-0.1.6.zip","sha256":"{sha}"}},
                {{"id":"qoder","version":"0.2.0","url":"https://example.com/qoder.zip","sha256":"{sha}"}}
            ]}}"#
        );
        let mut index = parse_plugin_index(&body).unwrap();
        resolve_plugin_index(
            "http://127.0.0.1:8765/releases/download/plugins/plugins-index.json",
            &mut index,
        )
        .unwrap();
        assert_eq!(
            index.plugins[0].zip_url,
            "http://127.0.0.1:8765/releases/download/plugins/trae-0.1.6.zip"
        );
        // 旧索引的 url 字段仍可读取。
        assert_eq!(index.plugins[1].zip_url, "https://example.com/qoder.zip");
    }

    #[test]
    fn rejects_plugin_urls_escaping_index_directory() {
        let sha = "a".repeat(64);
        for url in ["/trae.zip", "../trae.zip", "nested/../../trae.zip"] {
            let body = format!(
                r#"{{"schema":1,"plugins":[{{"id":"trae","version":"0.1.6","zip_url":"{url}","sha256":"{sha}"}}]}}"#
            );
            assert!(parse_plugin_index(&body).is_err(), "{url}");
        }
    }

    #[test]
    fn verifies_rfc8032_empty_message_signature() {
        verify_signature(b"", RFC8032_SIGNATURE, RFC8032_PUBLIC).unwrap();
        assert!(verify_signature(b"x", RFC8032_SIGNATURE, RFC8032_PUBLIC).is_err());
    }

    #[test]
    fn parses_app_manifest_and_rejects_http_url() {
        let body = r#"{
            "schema": 1,
            "version": "0.18.0",
            "exe_url": "https://example.com/app.exe",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "size": 12,
            "notes_url": "https://example.com/notes"
        }"#;
        let manifest = parse_app_manifest(body).unwrap();
        assert_eq!(manifest.version, "0.18.0");
        assert_eq!(manifest.size, 12);
        assert_eq!(manifest.exe_url, "https://example.com/app.exe");

        let insecure = body.replace("https://example.com/app.exe", "http://example.com/app.exe");
        assert!(parse_app_manifest(&insecure).is_err());
    }

    #[test]
    fn selects_only_installed_older_plugins_with_compatible_host() {
        let index = PluginIndex {
            schema: 1,
            generated_at: String::new(),
            plugins: vec![
                PluginEntry {
                    id: "trae".into(),
                    label: "Trae".into(),
                    version: "0.1.6".into(),
                    zip_url: "https://example.com/trae.zip".into(),
                    sha256: "a".repeat(64),
                    size: 1,
                    min_host: "0.17.0".into(),
                },
                PluginEntry {
                    id: "qoder".into(),
                    label: "Qoder".into(),
                    version: "0.2.3".into(),
                    zip_url: "https://example.com/qoder.zip".into(),
                    sha256: "b".repeat(64),
                    size: 1,
                    min_host: "99.0.0".into(),
                },
                PluginEntry {
                    id: "cursor".into(),
                    label: "Cursor".into(),
                    version: "0.1.2".into(),
                    zip_url: "https://example.com/cursor.zip".into(),
                    sha256: "c".repeat(64),
                    size: 1,
                    min_host: String::new(),
                },
            ],
        };
        let installed = std::collections::HashMap::from([
            ("trae".to_string(), "0.1.5".to_string()),
            ("qoder".to_string(), "0.2.2".to_string()),
            ("cursor".to_string(), "0.1.2".to_string()),
        ]);
        let updates = select_plugin_updates(&installed, &index);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, "trae");
    }

    #[test]
    fn file_name_is_taken_from_url() {
        assert_eq!(
            file_name_from_url("https://example.com/releases/app.exe"),
            "app.exe"
        );
        assert_eq!(file_name_from_url("https://example.com"), "example.com");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "asm-download-{name}-{}-{}",
            std::process::id(),
            crate::ir::now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 本地一次性 HTTP 服务，用于验证下载校验；不联网。
    fn serve_once(body: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        (format!("http://127.0.0.1:{port}/update.bin"), handle)
    }

    #[test]
    fn download_rejects_hash_mismatch_and_removes_partial_file() {
        let body = b"payload".to_vec();
        let expected = format!("{:x}", Sha256::digest(&body));
        let (url, server) = serve_once(body.clone());
        let dir = temp_dir("hash");
        let dest = dir.join("app.exe");
        let mut size = 0u64;
        let result = download_to(&url, &dest, &expected, body.len() as u64, |progress| {
            size = progress.downloaded;
        });
        server.join().unwrap();
        result.unwrap();
        assert_eq!(size, body.len() as u64);
        assert_eq!(fs::read(&dest).unwrap(), body);

        let (url, server) = serve_once(body.clone());
        let bad = dir.join("bad.exe");
        assert!(download_to(&url, &bad, &"0".repeat(64), body.len() as u64, |_| {}).is_err());
        server.join().unwrap();
        assert!(!bad.exists());
        assert!(!dir.join("bad.exe.part").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn download_rejects_size_mismatch() {
        let body = b"payload".to_vec();
        let expected = format!("{:x}", Sha256::digest(&body));
        let (url, server) = serve_once(body.clone());
        let dir = temp_dir("size");
        let dest = dir.join("app.exe");
        assert!(download_to(&url, &dest, &expected, body.len() as u64 + 1, |_| {}).is_err());
        server.join().unwrap();
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(dir);
    }
}
