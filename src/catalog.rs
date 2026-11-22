//! 产品/安装形态目录：由已安装插件的 manifest 组装，声明支持哪些产品、形态、
//! 检测器、产品色与默认打开建议。宿主不保留静态产品表；没有已注册插件时目录为空。
//! 未知检测器类型只跳过该检测器并记录中文警告，不整体失败。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub schema: u32,
    pub products: Vec<CatalogProduct>,
    /// 加载期警告（如未知检测器类型），不落盘。
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogProduct {
    pub id: String,
    pub label: String,
    pub group_color: String,
    pub default_open_variant: Option<String>,
    pub variants: Vec<CatalogVariant>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogVariant {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub channel: String,
    pub detectors: Vec<DetectorSpec>,
}

impl From<&crate::plugins::manifest::DetectorManifest> for DetectorSpec {
    fn from(detector: &crate::plugins::manifest::DetectorManifest) -> Self {
        Self {
            kind: detector.kind.clone(),
            registry_keys: detector.registry_keys.clone(),
            exclude_prefixes: detector.exclude_prefixes.clone(),
            exe: detector.exe.clone(),
            shortcut_names: detector.shortcut_names.clone(),
            state_db: detector.state_db.clone(),
            roots: detector.roots.clone(),
            prefix: detector.prefix.clone(),
            root: detector.root.clone(),
            package: detector.package.clone(),
            path: detector.path.clone(),
            command: detector.command.clone(),
            args: detector.args.clone(),
        }
    }
}

/// 检测器声明：`type` 选择检测器实现，其余字段按类型取用，缺省为空。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorSpec {
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

/// 目录结构版本；仅供调试识别，目录内容全部来自已安装插件。
pub const CATALOG_SCHEMA: u32 = 6;

/// 运行时目录来自已安装插件的 manifest；没有插件时为空。
pub fn from_plugins() -> Catalog {
    let mut products = Vec::new();
    for info in crate::agents::all() {
        let Some(manifest) = &info.manifest else {
            continue;
        };
        let variants: Vec<CatalogVariant> = manifest
            .variants
            .iter()
            .map(|variant| CatalogVariant {
                id: variant.id.clone(),
                kind: variant.kind.clone(),
                label: variant.label.clone(),
                channel: variant
                    .update
                    .as_ref()
                    .map(|update| update.channel.clone())
                    .unwrap_or_default(),
                detectors: variant.detectors.iter().map(DetectorSpec::from).collect(),
            })
            .collect();
        products.push(CatalogProduct {
            id: info.id.to_string(),
            label: info.label.to_string(),
            group_color: manifest.group_color.clone(),
            default_open_variant: manifest
                .variants
                .iter()
                .find(|variant| variant.default_open)
                .map(|variant| variant.id.clone()),
            variants,
        });
    }
    let mut catalog = Catalog {
        schema: CATALOG_SCHEMA,
        products,
        warnings: Vec::new(),
    };
    filter_detectors(&mut catalog);
    catalog
}

/// 跳过未注册的检测器类型并记录中文警告；其余检测器保持声明顺序。
fn filter_detectors(catalog: &mut Catalog) {
    let mut warnings = Vec::new();
    for product in &mut catalog.products {
        for variant in &mut product.variants {
            variant.detectors.retain(|detector| {
                let known = matches!(
                    detector.kind.as_str(),
                    "desktop_app"
                        | "vscode_extension"
                        | "npm_package"
                        | "native_binary"
                        | "command_version"
                );
                if !known {
                    warnings.push(format!(
                        "产品 {} 形态 {} 的检测器类型 {} 未注册，已跳过",
                        product.id, variant.id, detector.kind
                    ));
                }
                known
            });
        }
    }
    catalog.warnings.extend(warnings);
}
