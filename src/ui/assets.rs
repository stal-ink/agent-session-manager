use std::collections::HashMap;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// 图标在 30px 方块内按 26px 绘制；预缩到 2 倍尺寸，
/// 避免 512px 源图在 Skia 的双线性缩小时跳过大量像素。
const ICON_EDGE: u32 = 52;

pub struct Icons {
    pub by_agent: HashMap<String, Image>,
}

impl Icons {
    pub fn new() -> Self {
        Self {
            by_agent: agent_session_manager::agents::all()
                .into_iter()
                .filter_map(|agent| icon(&agent.icon).map(|image| (agent.id.to_string(), image)))
                .collect(),
        }
    }

    /// 运行时导入的插件不在启动时的缓存里，按插件包图标补一次。
    pub fn for_product(
        &self,
        product: &str,
        plugins: &[agent_session_manager::plugins::InstalledPlugin],
    ) -> Image {
        self.by_agent
            .get(product)
            .cloned()
            .or_else(|| {
                plugins
                    .iter()
                    .find(|plugin| plugin.id == product)
                    .and_then(|plugin| icon(&plugin.icon))
            })
            .unwrap_or_default()
    }
}

pub(super) fn icon(bytes: &[u8]) -> Option<Image> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let img = image::imageops::resize(
        &img,
        ICON_EDGE,
        ICON_EDGE,
        image::imageops::FilterType::Lanczos3,
    );
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(ICON_EDGE, ICON_EDGE);
    buf.make_mut_bytes().copy_from_slice(img.as_raw());
    Some(Image::from_rgba8(buf))
}

#[cfg(test)]
mod tests {
    use super::{icon, ICON_EDGE};

    #[test]
    fn agent_icons_are_downscaled_for_the_icon_tile() {
        for agent in agent_session_manager::agents::all() {
            let size = icon(&agent.icon).expect("agent icon decode").size();
            assert_eq!(
                (size.width, size.height),
                (ICON_EDGE, ICON_EDGE),
                "{} icon",
                agent.id
            );
        }
    }
}
