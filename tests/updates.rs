//! 版本检测解析层集成测试：七个版本源的真实响应裁剪样本与 compare 边界。
//! 全部离线：解析函数只吃 &str，不联网、不碰文件。

mod common;

use std::cmp::Ordering;
use std::fs;

use agent_session_manager::updates;
use agent_session_manager::updates::compare;

/// Qoder latest.yml 真实裁剪样本（2026-11-13 抓取，version 0.5.11）。
/// 特意保留 releaseId、buildNumber 等相似键，防止解析误匹配。
const QODER_YML: &str = "\
version: 0.5.11
files:
  - url: https://static.qoder.com.cn/qoder-app/releases/0.5.11/Qoder-CN-win-x64-user.exe
    sha512: yGgEtZkRxsb6lm/vS59aneOwbkq8VH7qNi5vrXMfeGyV2RqEAwVeIS72VL70PUGEBAFKurTVpVntaPedahQo3A==
    sha256: 88ffcd652573d97c4bfde30eb6dc7a30ae975a58b6d167f8cdfdfb98e5dcc2e4
    size: 268829712
path: Qoder-CN-win-x64-user.exe
sha512: yGgEtZkRxsb6lm/vS59aneOwbkq8VH7qNi5vrXMfeGyV2RqEAwVeIS72VL70PUGEBAFKurTVpVntaPedahQo3A==
releaseDate: 2026-11-13T12:14:10.081Z
releaseId: 0.5.11
commit: 643f372fe5e69c997ce9564ee882d5a7350296bd
buildNumber: \"89557914\"
buildKind: official
releaseNotes: {}
";

/// ZCode 安装文档徽标真实片段（2026-11-13 抓取，v3.18.2）。
/// React 把文本拆开：`v` 与数字之间有 `<!-- -->`，`·` 包在 span 里。
const ZCODE_HTML: &str = "<span class=\"relative inline-flex h-2 w-2 rounded-full bg-emerald-400\" aria-hidden=\"true\"></span><span class=\"text-sm font-medium text-white\">v<!-- -->3.18.2<span class=\"text-white/30\">·</span>最新版本</span></div>";

/// Cursor 下载接口真实响应（2026-11-14 抓取，version 3.30.14）。
const CURSOR_JSON: &str = r#"{"downloadUrl":"https://downloads.cursor.com/production/5f54c8173e5552412beb40702adac16275677fa0/win32/x64/user-setup/CursorUserSetup-x64-3.30.14.exe","rehUrl":"https://downloads.cursor.com/remote-releases/5f54c8173e5552412beb40702adac16275677fa0/vscode-reh-win32-x64-user.tar.gz","version":"3.30.14","commitSha":"5f54c8173e5552412beb40702adac16275677fa0"}"#;

/// Kiro 下载页真实裁剪片段（2026-11-14 抓取，稳定版 1.4.154）。
/// Kiro Crew 的 latest 链接形如 stable 但不带版本号，不应干扰提取。
const KIRO_HTML: &str = "<a href=\"https://download.crew.kiro.dev/desktop/stable/latest/KiroCrew-x64.dmg\">Kiro Crew</a><a href=\"https://prod.download.desktop.kiro.dev/releases/stable/darwin-arm64/signed/1.4.154/kiro-ide-1.4.154-stable-darwin-arm64.dmg\">macOS（Apple Silicon）</a><a href=\"https://prod.download.desktop.kiro.dev/releases/stable/darwin-x64/signed/1.4.154/kiro-ide-1.4.154-stable-darwin-x64.pkg\">macOS（Intel）</a>";

/// WorkBuddy 更新接口真实响应（2026-11-15 匿名抓取，最新包 5.9.3.42398134）。
/// version 末段是构建号；产品版本是前三段（注册表 DisplayVersion = 5.9.3）。
const WORKBUDDY_UPDATE_JSON: &str = r#"{"version":"5.9.3.42398134","url":"https://download.codebuddy.cn/workbuddy/saas/win32-x64-user/WorkBuddy-win32-x64-user-5.9.3.42398134-d4692421.exe","productVersion":"5.9.3.42398134","sha256hash":"b5edb52bd6c5b6c7296cf2fff697b019c86d11992a8a814c3010d611896852f9","timestamp":1793121334,"hash":"","name":"","supportsFastUpdate":false}"#;

/// WorkBuddy 平台串不对时的真实错误响应：没有 version 字段，应返回 None。
const WORKBUDDY_UPDATE_BAD_PLATFORM: &str = r#"{"code":10001,"msg":"10001:failed to get package version, invalid platform: workbuddy-win32-x64","requestId":"47f1787d-0d27-4f57-9f44-f86246833b50"}"#;

/// Claude Code 的 npm registry latest 真实响应（2026-11-17 抓取，2.1.326）。
const CLAUDE_NPM_JSON: &str = r#"{"name":"@anthropic-ai/claude-code","version":"2.1.326","description":"Claude Code CLI","bin":{"claude":"cli.js"},"dist":{"tarball":"https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-2.1.326.tgz"}}"#;

/// DSH 桌面端 electron-updater nightly.yml 真实裁剪（2026-11-17 抓取，0.2.2-rc.3）。
const DSH_YML: &str = "\
version: 0.2.2-rc.3
files:
  - url: >-
      https://download.deepseek.com/dsh-desk/bin/win-x64/deepseek-harness-0.2.2-rc.3-win-x64.exe
    size: 299595752
path: >-
  https://download.deepseek.com/dsh-desk/bin/win-x64/deepseek-harness-0.2.2-rc.3-win-x64.exe
releaseDate: '2026-11-13T07:31:42.937Z'
";

fn json_rule(paths: &[&str]) -> updates::Source {
    updates::Source {
        parser: "json".into(),
        paths: paths.iter().map(|path| path.to_string()).collect(),
        ..Default::default()
    }
}

fn key_rule(key: &str) -> updates::Source {
    updates::Source {
        parser: "key".into(),
        key: key.into(),
        ..Default::default()
    }
}

fn regex_rule(patterns: &[&str]) -> updates::Source {
    updates::Source {
        parser: "regex".into(),
        patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
        ..Default::default()
    }
}

fn html_regex_rule(patterns: &[&str]) -> updates::Source {
    updates::Source {
        parser: "html_regex".into(),
        patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn qoder_yml_sample_parses_version() {
    assert_eq!(
        updates::parse_latest(&key_rule("version"), QODER_YML).as_deref(),
        Some("0.5.11")
    );
}

#[test]
fn claude_npm_sample_parses_version() {
    assert_eq!(
        updates::parse_latest(&json_rule(&["version"]), CLAUDE_NPM_JSON).as_deref(),
        Some("2.1.326")
    );
}

/// VS Code Marketplace 扩展页真实裁剪（2026-11-17 抓取，扩展 2.1.326）。
/// 页面内嵌 JSON 的 Versions 数组首项即最新版本。
const VSIX_HTML: &str = r#"<script>self.__next_f.push([1,"{"VersionHistory":{"Versions":[{"version":"2.1.326","lastUpdated":"2026-11-15T00:00:00Z"},{"version":"2.1.325"}]}}"])</script>"#;

#[test]
fn vscode_marketplace_sample_parses_latest_version() {
    let source = regex_rule(&[
        r#""Versions":\s*\[\s*\{[^}]*?"version"\s*:\s*"([^"]+)""#,
        r#""Version"\s*:\s*"([^"]+)""#,
    ]);
    assert_eq!(
        updates::parse_latest(&source, VSIX_HTML).as_deref(),
        Some("2.1.326")
    );
    assert_eq!(
        updates::parse_latest(&source, "<html>没有版本</html>"),
        None
    );
}

#[test]
fn dsh_nightly_sample_parses_version() {
    assert_eq!(
        updates::parse_latest(&key_rule("version"), DSH_YML).as_deref(),
        Some("0.2.2-rc.3")
    );
}

#[test]
fn qoder_yml_without_version_returns_none() {
    let fragment = "path: Qoder-CN-win-x64-user.exe\nbuildNumber: \"89557914\"\nreleaseNotes: {}\n";
    assert_eq!(updates::parse_latest(&key_rule("version"), fragment), None);
}

fn zcode_rule() -> updates::Source {
    html_regex_rule(&[r"([0-9]+(?:\.[0-9]+)+)\s*·?\s*最新版本"])
}

#[test]
fn zcode_badge_html_parses_version() {
    assert_eq!(
        updates::parse_latest(&zcode_rule(), ZCODE_HTML).as_deref(),
        Some("3.18.2")
    );
}

#[test]
fn zcode_plain_badge_variant_parses_version() {
    // 注释与 span 都不存在的朴素写法也要能解析。
    let html = "<h2>安装 <span>v3.4.0 · 最新版本</span></h2>";
    assert_eq!(
        updates::parse_latest(&zcode_rule(), html).as_deref(),
        Some("3.4.0")
    );
}

#[test]
fn zcode_page_without_badge_returns_none() {
    let html = "<p>下载 ZCode 并安装到本机。</p>";
    assert_eq!(updates::parse_latest(&zcode_rule(), html), None);
}

#[test]
fn cursor_json_parses_version() {
    assert_eq!(
        updates::parse_latest(&json_rule(&["version"]), CURSOR_JSON).as_deref(),
        Some("3.30.14")
    );
}

#[test]
fn cursor_non_json_body_returns_none() {
    // 源站未跟随重定向时返回纯文本，不是 JSON。
    assert_eq!(
        updates::parse_latest(&json_rule(&["version"]), "Redirecting..."),
        None
    );
}

fn kiro_rule() -> updates::Source {
    regex_rule(&[
        r#"/releases/stable/[^"\s<>]*?/([0-9]+(?:\.[0-9]+)+)"#,
        r"kiro-ide-([0-9]+(?:\.[0-9]+)+)-stable",
    ])
}

#[test]
fn kiro_stable_link_parses_version() {
    assert_eq!(
        updates::parse_latest(&kiro_rule(), KIRO_HTML).as_deref(),
        Some("1.4.154")
    );
}

#[test]
fn kiro_page_without_stable_version_returns_none() {
    let html = "<a href=\"https://download.crew.kiro.dev/desktop/stable/latest/KiroCrew.dmg\">Kiro Crew</a>";
    assert_eq!(updates::parse_latest(&kiro_rule(), html), None);
}

fn workbuddy_rule() -> updates::Source {
    updates::Source {
        segments: 3,
        min_parts: 3,
        ..json_rule(&["version"])
    }
}

#[test]
fn workbuddy_update_json_truncates_build_segment() {
    assert_eq!(
        updates::parse_latest(&workbuddy_rule(), WORKBUDDY_UPDATE_JSON).as_deref(),
        Some("5.9.3")
    );
    // 截取后与注册表里的三段版本相等 → 已是最新。
    assert_eq!(compare("5.9.3", "5.9.3"), Ordering::Equal);
}

#[test]
fn workbuddy_error_json_returns_none() {
    assert_eq!(
        updates::parse_latest(&workbuddy_rule(), WORKBUDDY_UPDATE_BAD_PLATFORM),
        None
    );
    // 版本段不足三段同样拒绝：宁可未知，不猜测。
    assert_eq!(
        updates::parse_latest(&workbuddy_rule(), r#"{"version":"5.9"}"#),
        None
    );
}

#[test]
fn parse_latest_strips_v_prefix_and_rejects_unknown_parser() {
    assert_eq!(
        updates::parse_latest(&json_rule(&["version"]), r#"{"version":"v9.9.9"}"#).as_deref(),
        Some("9.9.9")
    );
    assert_eq!(
        updates::parse_latest(&key_rule("version"), "version: V1.2.3").as_deref(),
        Some("1.2.3")
    );
    let unknown = updates::Source {
        parser: "unknown".into(),
        ..Default::default()
    };
    assert_eq!(updates::parse_latest(&unknown, "version: 1.2.3"), None);
}

#[test]
fn compare_prefixed_and_prerelease_boundaries() {
    // 前缀 v/V 只影响展示，不影响比较。
    assert_eq!(compare("v1.2.3", "1.2.3"), Ordering::Equal);
    assert_eq!(compare("V2.0.0", "2.0.0"), Ordering::Equal);
    // 预发布后缀不参与比较，仅按数字段判定。
    assert_eq!(compare("1.2.3-rc.1", "1.2.3"), Ordering::Equal);
    assert_eq!(compare("1.2.3-rc.1", "1.2.4"), Ordering::Less);
    // 段数不同：公共前缀相等时更短的一方更旧。
    assert_eq!(compare("1.2", "1.2.3"), Ordering::Less);
    assert_eq!(compare("1.2.3.1", "1.2.3"), Ordering::Greater);
}

fn temp_sources_dir(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("asm-update-sources-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn channel_update(latest: &str, ok: bool) -> updates::ChannelUpdate {
    updates::ChannelUpdate {
        id: "claude-cli".into(),
        latest: latest.into(),
        checked_at: 1,
        ok,
        error: if ok {
            String::new()
        } else {
            "网络请求失败：超时".into()
        },
        download_url: "https://www.npmjs.com/package/@anthropic-ai/claude-code".into(),
    }
}

#[test]
fn row_action_covers_full_state_table() {
    use updates::RowAction;
    let ok_newer = channel_update("2.1.326", true);
    let ok_same = channel_update("2.1.325", true);
    let failed = channel_update("2.1.325", false);

    // 未配置渠道：无论安装状态与检查结果，一律“未配置”。
    for installed in [true, false] {
        assert_eq!(
            updates::row_action(installed, false, "", Some(&ok_newer), "2.1.325"),
            RowAction::Unconfigured
        );
        assert_eq!(
            updates::row_action(installed, true, "", None, ""),
            RowAction::Unconfigured
        );
    }

    // 单形态已安装：沿用“下载”按钮，不受检查状态影响。
    for update in [None, Some(&failed), Some(&ok_same), Some(&ok_newer)] {
        assert_eq!(
            updates::row_action(true, true, "qoder-desktop", update, "0.5.7"),
            RowAction::Download
        );
    }

    // 尚无检查结果：灰色禁用。
    assert_eq!(
        updates::row_action(true, false, "claude-cli", None, "2.1.325"),
        RowAction::UpdateDisabled
    );
    assert_eq!(
        updates::row_action(false, false, "claude-cli", None, ""),
        RowAction::InstallDisabled
    );
    // 单形态未安装不走下载按钮。
    assert_eq!(
        updates::row_action(false, true, "claude-cli", None, ""),
        RowAction::InstallDisabled
    );

    // 检查失败：无按钮，错误标记。
    assert_eq!(
        updates::row_action(true, false, "claude-cli", Some(&failed), "2.1.325"),
        RowAction::Failed
    );
    assert_eq!(
        updates::row_action(false, false, "claude-cli", Some(&failed), ""),
        RowAction::Failed
    );

    // 检查成功 + 已安装：最新 > 本机 → 更新；最新 <= 本机 → 灰更新；本机版本未知 → 灰更新。
    assert_eq!(
        updates::row_action(true, false, "claude-cli", Some(&ok_newer), "2.1.325"),
        RowAction::Update
    );
    assert_eq!(
        updates::row_action(true, false, "claude-cli", Some(&ok_same), "2.1.325"),
        RowAction::UpdateDisabled
    );
    assert_eq!(
        updates::row_action(true, false, "claude-cli", Some(&ok_newer), ""),
        RowAction::UpdateDisabled
    );

    // 检查成功 + 未安装：安装。
    assert_eq!(
        updates::row_action(false, false, "claude-cli", Some(&ok_newer), ""),
        RowAction::Install
    );
    assert_eq!(
        updates::row_action(false, true, "claude-cli", Some(&ok_newer), ""),
        RowAction::Install
    );
}

fn target(installed: bool, version: &str) -> updates::CheckTarget {
    updates::CheckTarget {
        channel: "claude-cli".into(),
        label: "Claude Code CLI".into(),
        installed,
        version: version.into(),
    }
}

fn source() -> updates::Source {
    updates::Source {
        id: "claude-cli".into(),
        product: "claude".into(),
        label: "Claude Code CLI".into(),
        version_url: "https://registry.npmjs.org/@anthropic-ai/claude-code/latest".into(),
        parser: "json".into(),
        paths: vec!["version".into()],
        download_url: "https://www.npmjs.com/package/@anthropic-ai/claude-code".into(),
        ..Default::default()
    }
}

#[test]
fn apply_result_success_updatable_logs_green() {
    let prior = channel_update("2.1.325", true);
    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(true, "2.1.325"),
        Some(&source()),
        Ok("2.1.326".into()),
        42,
        &mut log,
    );
    assert!(update.ok);
    assert_eq!(update.latest, "2.1.326");
    assert_eq!(update.checked_at, 42);
    assert_eq!(update.error, "");
    assert_eq!(update.download_url, source().download_url);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].text, "Claude Code CLI 可更新 2.1.326");
    assert!(log[0].ok);
    assert!(log[0].updatable);
}

#[test]
fn apply_result_success_latest_and_unknown_local_version() {
    let prior = updates::ChannelUpdate::default();
    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(true, "2.1.326"),
        Some(&source()),
        Ok("2.1.326".into()),
        1,
        &mut log,
    );
    assert!(update.ok);
    assert_eq!(log[0].text, "Claude Code CLI 已是最新 2.1.326");
    assert!(!log[0].updatable);

    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(true, ""),
        Some(&source()),
        Ok("2.1.326".into()),
        1,
        &mut log,
    );
    assert!(update.ok);
    assert_eq!(log[0].text, "Claude Code CLI 最新 2.1.326（本机版本未知）");
    assert!(!log[0].updatable);
}

#[test]
fn apply_result_success_not_installed() {
    let prior = updates::ChannelUpdate::default();
    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(false, ""),
        Some(&source()),
        Ok("2.1.326".into()),
        1,
        &mut log,
    );
    assert!(update.ok);
    assert_eq!(log[0].text, "Claude Code CLI 最新 2.1.326（未安装）");
    assert!(!log[0].updatable);
}

#[test]
fn apply_result_failure_keeps_prior_latest() {
    let prior = channel_update("2.1.325", true);
    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(true, "2.1.325"),
        Some(&source()),
        Err("网络请求失败：超时".into()),
        9,
        &mut log,
    );
    assert!(!update.ok);
    // 保留上一次成功的最新版本作为“上次已知”，但状态是检查失败。
    assert_eq!(update.latest, "2.1.325");
    assert_eq!(update.error, "网络请求失败：超时");
    assert_eq!(log[0].text, "Claude Code CLI 网络请求失败：超时");
    assert!(!log[0].ok);
    assert!(!log[0].updatable);
}

#[test]
fn apply_result_missing_source_reports_chinese_error() {
    let prior = channel_update("2.1.325", true);
    let mut log = Vec::new();
    let update = updates::apply_check_result(
        &prior,
        &target(true, "2.1.325"),
        None,
        Err("渠道源缺失：claude-cli".into()),
        9,
        &mut log,
    );
    assert!(!update.ok);
    assert_eq!(update.error, "渠道源缺失：claude-cli");
    assert_eq!(update.latest, "2.1.325");
    // 没有渠道源时沿用缓存里的下载页。
    assert_eq!(update.download_url, prior.download_url);
    assert_eq!(log[0].text, "Claude Code CLI 渠道源缺失：claude-cli");
}

#[test]
fn channels_cache_roundtrip_and_legacy_entries_ignored() {
    let dir = temp_sources_dir("channels-cache");
    let path = dir.join("updates.json");
    let cache = updates::UpdateCache {
        channels: vec![channel_update("2.1.326", true)],
        checked_at: 1795000000000,
    };
    fs::write(&path, serde_json::to_string_pretty(&cache).unwrap()).unwrap();
    let read: updates::UpdateCache =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(read, cache);

    // 旧版按 agent 的 entries 不再参与按钮判断：读取时被忽略，缺字段按默认。
    let legacy = r#"{"entries":[{"id":"claude","installed_version":"2.1.325","latest_version":"2.1.326","checked_at":1,"status":"updatable","source":"https://x","download_url":"https://y"}],"checked_at":3}"#;
    fs::write(&path, legacy).unwrap();
    let read: updates::UpdateCache =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        read,
        updates::UpdateCache {
            channels: vec![],
            checked_at: 3,
        }
    );
    fs::remove_dir_all(dir).unwrap();
}
