//! 应用配置 `%LOCALAPPDATA%/agent-session-manager/config.json`。
//! 界面只读写这个文件；不修改 agent 存储。版本检查结果缓存在 updates.json，
//! 检查日志只在内存实时更新，两者都不写进 config.json。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::transfer::data_dir;

/// 主题：跟随系统、深色、浅色、米黄、薄荷、自定义壁纸；调色板定义在 assets/themes。
pub const THEME_SYSTEM: &str = "system";
pub const THEME_DARK: &str = "dark";
pub const THEME_LIGHT: &str = "light";
pub const THEME_BEIGE: &str = "beige";
pub const THEME_MINT: &str = "mint";
pub const THEME_WALLPAPER: &str = "wallpaper";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    /// 壁纸文件名，位于数据目录 wallpapers/；空表示未选择。
    pub wallpaper: String,
    pub export: ExportConfig,
    pub transfer: TransferConfig,
    pub check: CheckConfig,
    pub open: OpenConfig,
    pub plugins: PluginConfig,
}

/// 检查设置：启动后是否自动检查（app 自更新、插件索引与各渠道版本；启动 5 秒后一次，之后每 20 分钟一次）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckConfig {
    /// 默认开启；关闭后不发起任何自动检查，各页手动「检查」仍可用。
    pub auto_on_startup: bool,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            auto_on_startup: true,
        }
    }
}

/// 打开设置：产品级默认打开形态（product -> variant id），覆盖 catalog 建议。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenConfig {
    pub defaults: std::collections::HashMap<String, String>,
}

/// 插件设置：root 为空时落到数据目录 plugins/；用户可把它重映射到其他本地磁盘。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub root: String,
}

/// 原生转接设置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferConfig {
    /// 仅注入最近一次压缩边界之后的消息；默认关闭（全量写入）。
    pub compaction_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportCategory {
    pub tool_results: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: THEME_SYSTEM.into(),
            wallpaper: String::new(),
            export: ExportConfig::default(),
            transfer: TransferConfig::default(),
            check: CheckConfig::default(),
            open: OpenConfig::default(),
            plugins: PluginConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub normal: ExportCategory,
    pub handoff: ExportCategory,
}

impl Default for ExportCategory {
    fn default() -> Self {
        // 导出工具调用结果默认开启。
        Self { tool_results: true }
    }
}

pub fn path() -> PathBuf {
    data_dir().join("config.json")
}

/// 读取配置；缺失或损坏时回退默认值，并从旧版 settings.json 迁移主题。
pub fn load() -> Config {
    load_from(&path(), &legacy_settings_path())
}

pub fn save(config: &Config) -> std::io::Result<()> {
    save_to(&path(), config)
}

fn legacy_settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

fn load_from(path: &std::path::Path, legacy: &std::path::Path) -> Config {
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(config) = serde_json::from_str::<Config>(&text) {
            return config;
        }
        // 损坏时保留现场再重建，避免误删用户仅有的配置。
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let _ = fs::rename(path, path.with_extension(format!("json.corrupt-{stamp}")));
    }
    let mut config = Config::default();
    // 旧版 settings.json 只有 {"theme":"dark"|"light"}；能读到就迁移，读不到保持跟随系统。
    if let Ok(text) = fs::read_to_string(legacy) {
        if let Ok(legacy) = serde_json::from_str::<LegacySettings>(&text) {
            config.theme = legacy.theme;
        }
    }
    config
}

fn save_to(path: &std::path::Path, config: &Config) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    crate::jsonl::write_atomic(path, &text).map_err(std::io::Error::other)
}

#[derive(Deserialize)]
struct LegacySettings {
    theme: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("asm-config-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_are_system_theme_and_enabled_tool_results() {
        let config = Config::default();
        assert_eq!(config.theme, THEME_SYSTEM);
        assert!(config.wallpaper.is_empty());
        assert!(config.export.normal.tool_results);
        assert!(config.export.handoff.tool_results);
        // 原生转接截断默认关闭：全量写入。
        assert!(!config.transfer.compaction_only);
        // 自动检查更新默认开启；默认打开形态未设置。
        assert!(config.check.auto_on_startup);
        assert!(config.open.defaults.is_empty());
        assert!(config.plugins.root.is_empty());
    }

    #[test]
    fn roundtrip_keeps_all_fields() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("config.json");
        let config = Config {
            theme: THEME_LIGHT.into(),
            wallpaper: "current.jpg".into(),
            export: ExportConfig {
                normal: ExportCategory {
                    tool_results: false,
                },
                handoff: ExportCategory::default(),
            },
            transfer: TransferConfig {
                compaction_only: true,
            },
            check: CheckConfig {
                auto_on_startup: false,
            },
            open: OpenConfig {
                defaults: [("claude".to_string(), "claude-cli".to_string())]
                    .into_iter()
                    .collect(),
            },
            plugins: PluginConfig {
                root: "E:/plugins".into(),
            },
        };
        save_to(&path, &config).unwrap();
        assert_eq!(load_from(&path, &dir.join("missing.json")), config);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_config_without_new_fields_uses_defaults() {
        let dir = temp_dir("legacy-fields");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"theme":"dark","export":{"normal":{"tool_results":false}},"transfer":{"compaction_only":true}}"#,
        )
        .unwrap();
        let config = load_from(&path, &dir.join("missing.json"));
        assert_eq!(config.theme, THEME_DARK);
        assert!(config.wallpaper.is_empty());
        assert!(!config.export.normal.tool_results);
        assert!(config.export.handoff.tool_results);
        assert!(config.transfer.compaction_only);
        assert!(config.check.auto_on_startup);
        assert!(config.open.defaults.is_empty());
        assert!(config.plugins.root.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_falls_back_to_legacy_theme() {
        let dir = temp_dir("legacy");
        let path = dir.join("config.json");
        fs::write(dir.join("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let config = load_from(&path, &dir.join("settings.json"));
        assert_eq!(config.theme, THEME_DARK);
        assert!(config.export.normal.tool_results);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_file_is_preserved_and_defaults_apply() {
        let dir = temp_dir("corrupt");
        let path = dir.join("config.json");
        fs::write(&path, "{not json").unwrap();
        let config = load_from(&path, &dir.join("missing.json"));
        assert_eq!(config, Config::default());
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            leftovers
                .iter()
                .any(|n| n.starts_with("config.json.corrupt-")),
            "损坏文件应保留现场：{leftovers:?}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_everything_yields_system_theme() {
        let dir = temp_dir("empty");
        let config = load_from(&dir.join("config.json"), &dir.join("settings.json"));
        assert_eq!(config.theme, THEME_SYSTEM);
        fs::remove_dir_all(dir).unwrap();
    }
}
