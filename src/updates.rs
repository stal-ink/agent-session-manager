//! 渠道版本检测：渠道默认值来自已安装插件 manifest，数据目录 update-sources.json
//! 可按渠道覆盖 URL 与解析器；单线程顺序检查已安装形态与未安装条目；
//! 检查结果按渠道缓存到 updates.json，检查日志只在内存实时更新。
//! 网络请求只在后台检查线程使用；测试不联网，解析与结果应用都是只吃字符串的纯函数。

use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::transfer::data_dir;

/// 单次 HTTP 超时。
pub const TIMEOUT: Duration = Duration::from_secs(8);
/// 更新请求 User-Agent。
pub const USER_AGENT: &str = concat!("AgentSessionManager/", env!("CARGO_PKG_VERSION"));
/// 检查日志最多保留条数。
pub const LOG_KEEP: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    /// 最新版本大于本机版本。
    Updatable,
    /// 本机已是最新。
    Latest,
    /// 版本源没给出可比较的版本号。
    Unknown,
}

/// 单个渠道的一次检查结果。失败保留上一次成功的 latest 作为“上次已知”，
/// 但 ok=false，UI 显示检查失败而不是可点击的更新/安装。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelUpdate {
    pub id: String,
    pub latest: String,
    pub checked_at: i64,
    pub ok: bool,
    pub error: String,
    pub download_url: String,
}

/// 一次检查的目标形态：channel 为空表示未配置检查源，跳过不发请求。
/// 调用方保证已安装形态在前、未安装条目在后。
#[derive(Clone, Debug)]
pub struct CheckTarget {
    pub channel: String,
    pub label: String,
    pub installed: bool,
    pub version: String,
}

/// 检查日志条目：只在内存实时更新，不落盘。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub time_ms: i64,
    pub text: String,
    pub ok: bool,
    /// 可更新条目用绿色提示。
    pub updatable: bool,
}

/// 检查结果缓存（updates.json）：只含各渠道的最新版本状态与检查时间，不含日志。
/// 旧版按 agent 的 `entries` 字段被 serde 忽略，首次检查后自然替换。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateCache {
    pub channels: Vec<ChannelUpdate>,
    pub checked_at: i64,
}

fn cache_path() -> PathBuf {
    data_dir().join("updates.json")
}

pub fn load_cache() -> UpdateCache {
    fs::read_to_string(cache_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_cache(cache: &UpdateCache) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(cache).map_err(std::io::Error::other)?;
    crate::jsonl::write_atomic(&cache_path(), &text).map_err(std::io::Error::other)
}

/// 追加日志并只保留最近 [`LOG_KEEP`] 条，最新在前。
pub fn push_log(log: &mut Vec<LogEntry>, text: impl Into<String>, ok: bool) {
    push_log_entry(log, text, ok, false);
}

/// 追加日志并保留最近 `LOG_KEEP` 条；`updatable` 用于绿色「可更新」提示。
pub fn push_log_entry(log: &mut Vec<LogEntry>, text: impl Into<String>, ok: bool, updatable: bool) {
    log.insert(
        0,
        LogEntry {
            time_ms: crate::ir::now_ms(),
            text: text.into(),
            ok,
            updatable,
        },
    );
    log.truncate(LOG_KEEP);
}

/// 版本按数字段比较，忽略前缀 `v`；非数字后缀不参与大小判断。
pub fn compare(a: &str, b: &str) -> Ordering {
    version_key(a).cmp(&version_key(b))
}

fn version_key(version: &str) -> Vec<u32> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    version
        .split(|c: char| !c.is_ascii_digit())
        // 非数字后缀（如 -rc.1）不参与大小判断：遇到首个非数字段即停。
        .map_while(|part| part.parse().ok())
        .collect()
}

/// 单个渠道的版本源：默认由插件 manifest 声明，数据目录 `update-sources.json`
/// 可按渠道 id 覆盖地址与解析规则。手工编辑的条目允许缺字段：缺的按空串处理，
/// 解析不到版本时记 unknown。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub id: String,
    pub product: String,
    pub label: String,
    /// Agent 产品版本检查页；不是插件包下载地址。
    #[serde(alias = "url")]
    pub version_url: String,
    /// json、key、regex、html_regex 或 text。
    pub parser: String,
    pub download_url: String,
    pub paths: Vec<String>,
    pub key: String,
    pub patterns: Vec<String>,
    pub group: usize,
    pub segments: usize,
    pub min_parts: usize,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            id: String::new(),
            product: String::new(),
            label: String::new(),
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    pub sources: Vec<Source>,
}

fn sources_path() -> PathBuf {
    data_dir().join("update-sources.json")
}

/// 插件 manifest 声明的默认版本渠道。
pub fn plugin_sources() -> Vec<Source> {
    let mut sources = Vec::new();
    for info in crate::agents::all() {
        let Some(manifest) = &info.manifest else {
            continue;
        };
        for variant in &manifest.variants {
            let Some(update) = &variant.update else {
                continue;
            };
            sources.push(Source {
                id: update.channel.clone(),
                product: info.id.to_string(),
                label: variant.label.clone(),
                version_url: update.version_url.clone(),
                parser: update.parser.clone(),
                download_url: update.download_url.clone(),
                paths: update.paths.clone(),
                key: update.key.clone(),
                patterns: update.patterns.clone(),
                group: update.group,
                segments: update.segments,
                min_parts: update.min_parts,
            });
        }
    }
    sources
}

/// 读取版本源配置：默认值来自已安装插件，数据目录文件只覆盖同 id 的地址。
/// 每次检查时读一次，编辑文件后下一次检查即生效。
pub fn load_sources() -> Vec<Source> {
    let mut sources = plugin_sources();
    if let Ok(text) = fs::read_to_string(sources_path()) {
        if let Ok(config) = serde_json::from_str::<SourceConfig>(&text) {
            apply_source_overrides(&mut sources, config);
        }
    }
    sources
}

/// 当前支持的通用解析格式。旧版 `update-sources.json` 里的产品专用 parser
/// 名已不再实现，遇到时保留插件 manifest 的解析规则，只应用地址覆盖。
fn is_generic_parser(parser: &str) -> bool {
    matches!(parser, "json" | "key" | "regex" | "html_regex" | "text")
}

fn apply_source_overrides(sources: &mut [Source], config: SourceConfig) {
    for user in config.sources {
        if let Some(source) = sources.iter_mut().find(|source| source.id == user.id) {
            if !user.version_url.trim().is_empty() {
                source.version_url = user.version_url;
            }
            if is_generic_parser(&user.parser) {
                source.parser = user.parser;
                source.paths = user.paths;
                source.key = user.key;
                source.patterns = user.patterns;
                source.group = user.group;
                source.segments = user.segments;
                source.min_parts = user.min_parts;
            }
            if !user.download_url.trim().is_empty() {
                source.download_url = user.download_url;
            }
        }
    }
}

fn find_source<'a>(sources: &'a [Source], id: &str) -> Option<&'a Source> {
    sources.iter().find(|s| s.id == id)
}

/// 单线程顺序检查目标形态（调用方保证已安装在前、未安装在后）：
/// 结果缓存原地更新（含落盘），日志原地追加（仅内存），返回本次涉及的渠道条目。
/// channel 为空或渠道源没有检查 URL 时跳过：不发请求、不写条目（界面显示「—」与下载）。
/// 找不到渠道源记中文错误。只发 GET 请求，不携带账号、会话内容或设备标识；探测失败保留上次最新。
pub fn check(
    cache: &mut UpdateCache,
    log: &mut Vec<LogEntry>,
    targets: &[CheckTarget],
) -> Vec<ChannelUpdate> {
    check_with(cache, log, targets, |_, _| {})
}

/// 与 [`check`] 相同，但每完成一个渠道调用一次 `on_progress`，
/// 供界面逐条刷新日志；最终缓存仍在整轮检查结束后落盘。
pub fn check_with(
    cache: &mut UpdateCache,
    log: &mut Vec<LogEntry>,
    targets: &[CheckTarget],
    mut on_progress: impl FnMut(&UpdateCache, &[LogEntry]),
) -> Vec<ChannelUpdate> {
    let now = crate::ir::now_ms();
    let sources = load_sources();
    let mut updates = Vec::new();
    for target in targets {
        if target.channel.is_empty() {
            continue;
        }
        let source = find_source(&sources, &target.channel);
        if source.is_some_and(|s| s.version_url.trim().is_empty()) {
            continue;
        }
        let prior = cache
            .channels
            .iter()
            .find(|c| c.id == target.channel)
            .cloned()
            .unwrap_or_default();
        let result = match source {
            Some(source) => fetch_latest(source).map_err(|e| e.to_string()),
            None => Err(format!("渠道源缺失：{}", target.channel)),
        };
        let update = apply_check_result(&prior, target, source, result, now, log);
        cache.channels.retain(|c| c.id != target.channel);
        cache.channels.push(update.clone());
        updates.push(update);
        on_progress(&*cache, log.as_slice());
    }
    cache.checked_at = now;
    // 结果缓存落盘；写失败只影响下次启动的预热，不影响本次界面。
    let _ = save_cache(cache);
    updates
}

/// 把一次检查的结果应用到渠道条目并写日志；纯函数（不联网、不落盘），便于离线测试。
/// 成功：latest/ok=true/error 清空，download_url 取渠道源；按安装状态与本机版本写日志。
/// 失败：保留上次 latest，ok=false，error 为错误文本。
pub fn apply_check_result(
    prior: &ChannelUpdate,
    target: &CheckTarget,
    source: Option<&Source>,
    result: Result<String, String>,
    now: i64,
    log: &mut Vec<LogEntry>,
) -> ChannelUpdate {
    let mut update = ChannelUpdate {
        id: target.channel.clone(),
        latest: prior.latest.clone(),
        checked_at: now,
        ok: false,
        error: String::new(),
        download_url: source
            .map(|s| s.download_url.clone())
            .unwrap_or_else(|| prior.download_url.clone()),
    };
    let label = &target.label;
    match result {
        Ok(latest) => {
            update.latest = latest.clone();
            update.ok = true;
            if !target.installed {
                push_log(log, format!("{label} 最新 {latest}（未安装）"), true);
            } else if target.version.trim().is_empty() {
                // 安装存在但拿不到本机版本（如未写卸载表的 DSH）：只报告最新版，不判定可更新。
                push_log(log, format!("{label} 最新 {latest}（本机版本未知）"), true);
            } else if compare(&latest, &target.version) == Ordering::Greater {
                push_log(log, format!("{label} 可更新 {latest}"), true);
                if let Some(entry) = log.first_mut() {
                    entry.updatable = true;
                }
            } else {
                push_log(log, format!("{label} 已是最新 {latest}"), true);
            }
        }
        Err(error) => {
            // 保留上一次成功的 latest；当前状态显示检查失败。
            update.error = error.clone();
            push_log(log, format!("{label} {error}"), false);
        }
    }
    update
}

/// 操作列按钮状态：由安装状态、渠道检查结果与版本比较推导。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// 可更新：点击打开该形态下载页。
    Update,
    /// 已是最新/本机版本未知/尚无检查结果：灰色禁用。
    UpdateDisabled,
    /// 单形态已安装产品沿用现有“下载”按钮，不受检查状态影响。
    Download,
    /// 未安装且检查成功：点击打开该形态下载页。
    Install,
    /// 未安装且尚无检查结果：灰色禁用。
    InstallDisabled,
    /// 检查失败：无按钮，显示错误标记。
    Failed,
    /// 未配置渠道：无按钮，显示“未配置”灰标记。
    Unconfigured,
}

/// 按钮状态表（见 docs/v0.12-install-variants-plan.md）：
/// 未配置渠道优先；单形态已安装沿用下载按钮；其余按检查结果与版本比较推导。
pub fn row_action(
    installed: bool,
    single_form: bool,
    channel: &str,
    update: Option<&ChannelUpdate>,
    version: &str,
) -> RowAction {
    if channel.is_empty() {
        return RowAction::Unconfigured;
    }
    if single_form && installed {
        return RowAction::Download;
    }
    let Some(update) = update else {
        return if installed {
            RowAction::UpdateDisabled
        } else {
            RowAction::InstallDisabled
        };
    };
    if !update.ok {
        return RowAction::Failed;
    }
    if installed {
        if version.trim().is_empty() {
            RowAction::UpdateDisabled
        } else if compare(&update.latest, version) == Ordering::Greater {
            RowAction::Update
        } else {
            RowAction::UpdateDisabled
        }
    } else {
        RowAction::Install
    }
}

/// 请求版本源并解析出最新版本号。网络与解析都走这里，测试只测解析。
fn fetch_latest(source: &Source) -> anyhow::Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .new_agent();
    let body = agent
        .get(&source.version_url)
        .call()
        .map_err(|e| anyhow::anyhow!("网络请求失败：{e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("网络请求失败：{e}"))?;
    parse_latest(source, &body).ok_or_else(|| anyhow::anyhow!("渠道校验失败：缺少版本号"))
}

/// 按 manifest 声明的通用规则解析最新版本号；纯函数，只吃字符串。
/// 返回值统一去掉前导 v/V；解析不到返回 None，由上层记 unknown。
pub fn parse_latest(source: &Source, body: &str) -> Option<String> {
    let candidate = match source.parser.as_str() {
        "json" => parse_json_paths(body, &source.paths),
        "key" => parse_key(body, &source.key),
        "regex" => parse_regexes(body, &source.patterns, source.group),
        "html_regex" => parse_regexes(&strip_html(body), &source.patterns, source.group),
        "text" => parse_text(body),
        _ => None,
    }?;
    finalize_version(&candidate, source.segments, source.min_parts)
}

fn parse_json_paths(body: &str, paths: &[String]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    paths.iter().find_map(|path| {
        let mut current = &value;
        for segment in path.split('.') {
            current = current.get(segment)?;
        }
        current.as_str().map(str::to_string)
    })
}

fn parse_key(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    body.lines().find_map(|line| {
        let value = line.trim().strip_prefix(&prefix)?.trim();
        Some(value.trim_matches('"').trim_matches('\'').to_string())
    })
}

fn parse_regexes(body: &str, patterns: &[String], group: usize) -> Option<String> {
    patterns.iter().find_map(|pattern| {
        let regex = Regex::new(pattern).ok()?;
        let captures = regex.captures(body)?;
        captures
            .get(group)
            .map(|capture| capture.as_str().to_string())
    })
}

fn parse_text(body: &str) -> Option<String> {
    Regex::new(r"[vV]?[0-9]+(?:\.[0-9]+)+")
        .ok()?
        .find(body)
        .map(|found| found.as_str().to_string())
}

/// 取版本号主体，按声明截取段数；预发布后缀在展示时保留。
fn finalize_version(value: &str, segments: usize, min_parts: usize) -> Option<String> {
    let normalized = normalize_version(value.trim().trim_matches('"').trim_matches('\''));
    let core_end = normalized
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(normalized.len());
    let core = &normalized[..core_end];
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() < min_parts
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    if segments == 0 {
        return Some(normalized);
    }
    if parts.len() < segments {
        return None;
    }
    Some(parts[..segments].join("."))
}

/// 去掉 HTML 注释与标签，只保留文本节点；
/// 页面把 `v` 和数字用 `<!-- -->` 拆开、把 `·` 包进 span 时靠它还原。
fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        // 注释按 --> 收尾，普通标签按 > 收尾。
        let end = if after.starts_with("<!--") {
            after.find("-->").map(|pos| pos + 3)
        } else {
            after.find('>').map(|pos| pos + 1)
        };
        match end {
            Some(len) => rest = &after[len..],
            // 截断的标签没有收尾，剩余内容全部视为标签内部。
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// 统一去掉前导 v/V；compare 本身也忽略前缀，这里保证展示一致。
fn normalize_version(version: &str) -> String {
    let trimmed = version.trim();
    match trimmed.strip_prefix(['v', 'V']) {
        // 只在 v 后面跟数字时剥掉，避免误剥占位文本。
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest.to_string(),
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_rule(paths: &[&str]) -> Source {
        Source {
            parser: "json".into(),
            paths: paths.iter().map(|path| path.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_parser_override_keeps_manifest_rule() {
        let mut sources = vec![Source {
            id: "qoder-desktop".into(),
            version_url: "https://manifest.example/latest.yml".into(),
            parser: "key".into(),
            key: "version".into(),
            download_url: "https://manifest.example/".into(),
            ..Default::default()
        }];
        let config = SourceConfig {
            sources: vec![Source {
                id: "qoder-desktop".into(),
                version_url: "https://user.example/latest.yml".into(),
                parser: "qoder".into(),
                download_url: "https://user.example/".into(),
                ..Default::default()
            }],
        };
        apply_source_overrides(&mut sources, config);
        // 地址覆盖生效，旧版产品 parser 被忽略，manifest 的通用规则保留。
        assert_eq!(sources[0].version_url, "https://user.example/latest.yml");
        assert_eq!(sources[0].download_url, "https://user.example/");
        assert_eq!(sources[0].parser, "key");
        assert_eq!(sources[0].key, "version");
    }

    #[test]
    fn generic_parser_override_replaces_manifest_rule() {
        let mut sources = vec![Source {
            id: "qoder-desktop".into(),
            parser: "key".into(),
            key: "version".into(),
            ..Default::default()
        }];
        let config = SourceConfig {
            sources: vec![Source {
                id: "qoder-desktop".into(),
                parser: "json".into(),
                paths: vec!["version".into()],
                ..Default::default()
            }],
        };
        apply_source_overrides(&mut sources, config);
        assert_eq!(sources[0].parser, "json");
        assert_eq!(sources[0].paths, vec!["version"]);
    }

    #[test]
    fn json_path_reads_nested_version_and_ignores_sibling_build_number() {
        let body = r#"{"data":{"needUpdate":true,"appVersion":"3.4.20","manifest":{"win32":{"version":"2.3.103559"}}}}"#;
        assert_eq!(
            parse_latest(&json_rule(&["data.appVersion"]), body),
            Some("3.4.20".into())
        );
        assert_eq!(
            parse_latest(
                &json_rule(&["data.appVersion"]),
                r#"{"data":{"needUpdate":false}}"#
            ),
            None
        );
    }

    #[test]
    fn json_paths_try_in_declared_order() {
        assert_eq!(
            parse_latest(&json_rule(&["version"]), r#"{"version":"0.170.0"}"#),
            Some("0.170.0".into())
        );
        assert_eq!(
            parse_latest(
                &json_rule(&["packageVersion", "buildVersion"]),
                r#"{"packageVersion":"26.1111.2228.0","buildVersion":"26.1111.9265.0"}"#
            ),
            Some("26.1111.2228.0".into())
        );
        assert_eq!(
            parse_latest(
                &json_rule(&["packageVersion", "buildVersion"]),
                r#"{"buildVersion":"26.1111.9265.0"}"#
            ),
            Some("26.1111.9265.0".into())
        );
    }

    #[test]
    fn key_rule_reads_top_level_version_line() {
        let source = Source {
            parser: "key".into(),
            key: "version".into(),
            ..Default::default()
        };
        assert_eq!(
            parse_latest(&source, "version: 0.2.2-rc.3\nfiles: []"),
            Some("0.2.2-rc.3".into())
        );
    }

    #[test]
    fn compare_ignores_v_prefix_and_compares_numeric_segments() {
        assert_eq!(compare("v1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(compare("0.5.11", "0.5.7"), Ordering::Greater);
        assert_eq!(compare("3.18.2", "v3.18.2"), Ordering::Equal);
        assert_eq!(compare("1.10", "1.9"), Ordering::Greater);
    }

    #[test]
    fn log_keeps_latest_twenty() {
        let mut log = Vec::new();
        for i in 0..25 {
            push_log(&mut log, format!("条目 {i}"), true);
        }
        assert_eq!(log.len(), LOG_KEEP);
        assert_eq!(log[0].text, "条目 24");
        assert_eq!(log[LOG_KEEP - 1].text, "条目 5");
    }

    #[test]
    fn cache_roundtrip_keeps_channels_without_log() {
        let dir = std::env::temp_dir().join(format!("asm-updates-{}-rt", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("updates.json");
        let mut cache = UpdateCache::default();
        cache.channels.push(ChannelUpdate {
            id: "qoder-desktop".into(),
            latest: "0.5.11".into(),
            checked_at: 1,
            ok: true,
            error: String::new(),
            download_url: "https://y".into(),
        });
        cache.checked_at = 2;
        let text = serde_json::to_string_pretty(&cache).unwrap();
        fs::write(&path, text).unwrap();
        assert_eq!(
            fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<UpdateCache>(&t).ok()),
            Some(cache)
        );
        // 旧版按 agent 的 entries 与日志字段都被忽略，缺字段按默认值兼容。
        let legacy = r#"{"entries":[{"id":"qoder","installed_version":"0.5.7","latest_version":"0.5.11","checked_at":1,"status":"updatable","source":"https://x","download_url":"https://y"}],"checked_at":3,"log":[{"time_ms":1,"text":"x","ok":true}]}"#;
        fs::write(&path, legacy).unwrap();
        let read: UpdateCache = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            read,
            UpdateCache {
                channels: vec![],
                checked_at: 3,
            }
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
