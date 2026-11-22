//! 内置主题：调色板定义在 assets/themes/*.json，编译期内嵌；统一应用 45 个令牌，
//! 并负责壁纸的选择、方向校正、缩放裁切与落地。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use slint::{Color, ComponentHandle};

use agent_session_manager::config::{
    Config, THEME_BEIGE, THEME_DARK, THEME_LIGHT, THEME_MINT, THEME_SYSTEM, THEME_WALLPAPER,
};
use agent_session_manager::transfer::data_dir;

use super::{MainWindow, Theme, ThemeCard};

pub const WALLPAPER_DIR: &str = "wallpapers";
pub const WALLPAPER_FILE: &str = "current.jpg";
const WALLPAPER_MAX_W: u32 = 2560;
const WALLPAPER_MAX_H: u32 = 1600;
const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_SOURCE_PIXELS: u64 = 80_000_000;

const BUILTIN: &[&str] = &[
    include_str!("../../assets/themes/dark.json"),
    include_str!("../../assets/themes/light.json"),
    include_str!("../../assets/themes/beige.json"),
    include_str!("../../assets/themes/mint.json"),
    include_str!("../../assets/themes/wallpaper.json"),
];

#[derive(Clone, Debug, Deserialize)]
pub struct ThemeDef {
    pub schema: u32,
    pub id: String,
    pub label: String,
    pub description: String,
    pub dark: bool,
    #[serde(default)]
    pub wallpaper: bool,
    pub colors: Palette,
}

macro_rules! palette_fields {
    ($($field:ident => $setter:ident),+ $(,)?) => {
        #[derive(Clone, Debug, Deserialize)]
        pub struct Palette {
            $(pub $field: String,)+
        }

        impl Palette {
            fn apply(&self, theme: &Theme) -> Result<()> {
                $(theme.$setter(parse_color(&self.$field)
                    .with_context(|| concat!("主题颜色 ", stringify!($field)))?);)+
                Ok(())
            }

            #[cfg(test)]
            fn validate(&self) -> Result<()> {
                $(parse_color(&self.$field)
                    .with_context(|| concat!("主题颜色 ", stringify!($field)))?;)+
                Ok(())
            }
        }
    };
}

// 字段名与 assets/themes/*.json 的 snake_case 键一致。
palette_fields! {
    bg => set_bg,
    wallpaper_base => set_wallpaper_base,
    surface => set_surface,
    surface2 => set_surface2,
    card => set_card,
    card_hover => set_card_hover,
    border => set_border,
    border_strong => set_border_strong,
    border_accent => set_border_accent,
    text => set_text,
    text2 => set_text2,
    text3 => set_text3,
    text_faint => set_text_faint,
    accent => set_accent,
    accent_hover => set_accent_hover,
    accent_soft => set_accent_soft,
    accent_text => set_accent_text,
    accent_text2 => set_accent_text2,
    on_accent => set_on_accent,
    warn => set_warn,
    danger => set_danger,
    tag_archived_bg => set_tag_archived_bg,
    tag_archived_border => set_tag_archived_border,
    tag_archived_text => set_tag_archived_text,
    tag_active_bg => set_tag_active_bg,
    tag_active_border => set_tag_active_border,
    tag_active_text => set_tag_active_text,
    tag_deleted_bg => set_tag_deleted_bg,
    tag_deleted_border => set_tag_deleted_border,
    tag_deleted_text => set_tag_deleted_text,
    tag_workspace_bg => set_tag_workspace_bg,
    tag_workspace_border => set_tag_workspace_border,
    tag_workspace_text => set_tag_workspace_text,
    icon_tile_bg => set_icon_tile_bg,
    icon_tile_border => set_icon_tile_border,
    input_bg => set_input_bg,
    field_bg => set_field_bg,
    option_bg => set_option_bg,
    btn_text => set_btn_text,
    btn_glow => set_btn_glow,
    btn_shadow => set_btn_shadow,
    overlay => set_overlay,
    dialog_bg => set_dialog_bg,
    dialog_border => set_dialog_border,
    dialog_shadow => set_dialog_shadow,
    wallpaper_scrim => set_wallpaper_scrim,
}

fn parse_color(value: &str) -> Result<Color> {
    let hex = value
        .strip_prefix('#')
        .with_context(|| format!("颜色缺少 #：{value}"))?;
    if !hex.is_ascii() {
        bail!("颜色格式无效：{value}");
    }
    let byte = |index: usize| -> Result<u8> {
        u8::from_str_radix(&hex[index..index + 2], 16)
            .with_context(|| format!("颜色格式无效：{value}"))
    };
    match hex.len() {
        // 支持 #RRGGBB 与 #RRGGBBAA（透明度在末尾）。
        6 => Ok(Color::from_rgb_u8(byte(0)?, byte(2)?, byte(4)?)),
        8 => Ok(Color::from_argb_u8(byte(6)?, byte(0)?, byte(2)?, byte(4)?)),
        _ => bail!("颜色格式无效：{value}"),
    }
}

fn builtin() -> &'static [ThemeDef] {
    static THEMES: OnceLock<Vec<ThemeDef>> = OnceLock::new();
    THEMES.get_or_init(|| {
        BUILTIN
            .iter()
            .map(|text| {
                let def: ThemeDef =
                    serde_json::from_str(text).expect("内置主题 JSON 解析失败，请运行主题测试");
                assert_eq!(def.schema, 1, "内置主题 {} schema 必须为 1", def.id);
                def
            })
            .collect()
    })
}

pub fn catalog() -> &'static [ThemeDef] {
    builtin()
}

pub fn find(id: &str) -> Option<&'static ThemeDef> {
    builtin().iter().find(|theme| theme.id == id)
}

pub fn resolve(choice: &str) -> &'static ThemeDef {
    resolve_with(choice, super::shell::system_prefers_dark())
}

fn resolve_with(choice: &str, system_dark: bool) -> &'static ThemeDef {
    match choice {
        THEME_DARK | THEME_LIGHT | THEME_BEIGE | THEME_MINT | THEME_WALLPAPER => {
            find(choice).expect("内置主题缺失")
        }
        // system 与未知 id 都按系统深浅回退。
        _ => find(if system_dark { THEME_DARK } else { THEME_LIGHT }).expect("内置主题缺失"),
    }
}

/// 应用主题令牌与壁纸；返回标题栏使用的明暗标记。
pub fn apply(window: &MainWindow, config: &Config) -> bool {
    let def = resolve(&config.theme);
    let theme = window.global::<Theme>();
    if let Err(error) = def.colors.apply(&theme) {
        // 内置主题由测试保证可解析；真出现异常时保留当前令牌继续运行。
        eprintln!("应用主题失败：{error}");
    }
    theme.set_dark(def.dark);
    match def.wallpaper.then(|| load_wallpaper(config)).flatten() {
        Some(image) => {
            window.set_wallpaper_image(image);
            window.set_wallpaper_visible(true);
        }
        None => window.set_wallpaper_visible(false),
    }
    def.dark
}

fn wallpaper_path(config: &Config) -> Option<PathBuf> {
    let name = config.wallpaper.trim();
    if name.is_empty() {
        return None;
    }
    let path = data_dir().join(WALLPAPER_DIR).join(name);
    path.is_file().then_some(path)
}

fn load_wallpaper(config: &Config) -> Option<slint::Image> {
    let path = wallpaper_path(config)?;
    // 自行解码，避免为 Slint 打开全格式图片支持带来的依赖与体积。
    let decoded = image::ImageReader::open(&path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(decoded.as_raw());
    Some(slint::Image::from_rgba8(buffer))
}

pub fn wallpaper_ready(config: &Config) -> bool {
    wallpaper_path(config).is_some()
}

/// 设置页的主题卡片：跟随系统置首，其余按内置顺序。
/// 系统卡片的色块始终取 Windows 当前深浅主题，不随应用主题变化。
pub fn cards() -> Vec<ThemeCard> {
    let system = resolve(THEME_SYSTEM);
    let mut rows = vec![ThemeCard {
        id: THEME_SYSTEM.into(),
        label: "跟随系统".into(),
        description: "读取 Windows 应用主题".into(),
        wallpaper: false,
        swatch_bg: swatch(&system.colors.bg),
        swatch_surface: swatch(&system.colors.surface),
        swatch_accent: swatch(&system.colors.accent),
        swatch_text: swatch(&system.colors.text3),
    }];
    rows.extend(catalog().iter().map(|def| ThemeCard {
        id: def.id.clone().into(),
        label: def.label.clone().into(),
        description: def.description.clone().into(),
        wallpaper: def.wallpaper,
        swatch_bg: swatch(&def.colors.bg),
        swatch_surface: swatch(&def.colors.surface),
        swatch_accent: swatch(&def.colors.accent),
        swatch_text: swatch(&def.colors.text3),
    }));
    rows
}

/// 预览块使用不透明色，避免叠在卡片/壁纸上时失真。
fn swatch(value: &str) -> Color {
    parse_color(value)
        .map(|color| Color::from_argb_u8(255, color.red(), color.green(), color.blue()))
        .unwrap_or_else(|_| transparent())
}

fn transparent() -> Color {
    Color::from_argb_u8(0, 0, 0, 0)
}

/// 打开系统文件选择框；取消返回 None。对话框放在专用 STA 线程上弹出：
/// winit 的 UI 线程不跑 common dialog 需要的 COM 消息循环；路径以 UTF-16
/// 留在进程内，不经过任何文本编码转换。
#[cfg(windows)]
pub fn pick_wallpaper() -> Result<Option<PathBuf>> {
    pick_file(
        "选择壁纸",
        "图片 (*.png;*.jpg;*.jpeg;*.bmp;*.webp)",
        "*.png;*.jpg;*.jpeg;*.bmp;*.webp",
    )
}

#[cfg(not(windows))]
pub fn pick_wallpaper() -> Result<Option<PathBuf>> {
    Ok(None)
}

/// 插件包导入框：支持宿主 archive 模块声明的全部归档格式。
pub fn pick_plugin_archive() -> Result<Option<PathBuf>> {
    #[cfg(windows)]
    {
        pick_file(
            "导入插件",
            "插件包 (*.zip;*.7z;*.tar;*.tar.gz;*.tgz;*.tar.xz;*.txz;*.tar.zst;*.tzst;*.tar.bz2;*.tbz2;*.tbz)",
            "*.zip;*.7z;*.tar;*.tar.gz;*.tgz;*.tar.xz;*.txz;*.tar.zst;*.tzst;*.tar.bz2;*.tbz2;*.tbz",
        )
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(windows)]
fn pick_file(title: &str, description: &str, pattern: &str) -> Result<Option<PathBuf>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let title = title.to_string();
    let description = description.to_string();
    let pattern = pattern.to_string();
    std::thread::spawn(move || {
        let _ = sender.send(show_file_dialog(&title, &description, &pattern));
    });
    receiver.recv().context("文件选择框异常终止")?
}

#[cfg(windows)]
fn show_file_dialog(title: &str, description: &str, pattern: &str) -> Result<Option<PathBuf>> {
    use std::ffi::c_void;

    const OFN_HIDEREADONLY: u32 = 0x0000_0004;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_EXPLORER: u32 = 0x0008_0000;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    #[repr(C)]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: *mut c_void,
        h_instance: *mut c_void,
        lpstr_filter: *const u16,
        lpstr_custom_filter: *mut u16,
        n_max_cust_filter: u32,
        n_filter_index: u32,
        lpstr_file: *mut u16,
        n_max_file: u32,
        lpstr_file_title: *mut u16,
        n_max_file_title: u32,
        lpstr_initial_dir: *const u16,
        lpstr_title: *const u16,
        flags: u32,
        n_file_offset: u16,
        n_file_extension: u16,
        lpstr_def_ext: *const u16,
        l_cust_data: isize,
        lpfn_hook: *mut c_void,
        lp_template_name: *const u16,
        pv_reserved: *mut c_void,
        dw_reserved: u32,
        flags_ex: u32,
    }

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> *mut c_void;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
        fn CoUninitialize();
    }

    // 专用线程从零开始，STA 初始化保证 modern common dialog 可用。
    let initialized =
        unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) } >= 0;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let mut filter = Vec::new();
    for part in [description, pattern] {
        filter.extend(part.encode_utf16());
        filter.push(0);
    }
    filter.push(0);
    let title = wide(title);
    let mut file = vec![0u16; 1024];
    let owner = unsafe { FindWindowW(std::ptr::null(), wide("Agent Session Manager").as_ptr()) };
    let mut dialog = OpenFileNameW {
        l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
        hwnd_owner: owner,
        h_instance: std::ptr::null_mut(),
        lpstr_filter: filter.as_ptr(),
        lpstr_custom_filter: std::ptr::null_mut(),
        n_max_cust_filter: 0,
        n_filter_index: 1,
        lpstr_file: file.as_mut_ptr(),
        n_max_file: file.len() as u32,
        lpstr_file_title: std::ptr::null_mut(),
        n_max_file_title: 0,
        lpstr_initial_dir: std::ptr::null(),
        lpstr_title: title.as_ptr(),
        flags: OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | OFN_FILEMUSTEXIST
            | OFN_EXPLORER,
        n_file_offset: 0,
        n_file_extension: 0,
        lpstr_def_ext: std::ptr::null(),
        l_cust_data: 0,
        lpfn_hook: std::ptr::null_mut(),
        lp_template_name: std::ptr::null(),
        pv_reserved: std::ptr::null_mut(),
        dw_reserved: 0,
        flags_ex: 0,
    };
    let picked = unsafe { GetOpenFileNameW(&mut dialog) };
    if initialized {
        unsafe { CoUninitialize() };
    }
    if picked == 0 {
        return Ok(None);
    }
    let end = file.iter().position(|&ch| ch == 0).unwrap_or(0);
    Ok((end > 0).then(|| PathBuf::from(String::from_utf16_lossy(&file[..end]))))
}

/// 解码、按 EXIF 方向校正、缩放到 2560×1600 以内并居中裁成 16:10，
/// 输出 `current.jpg` 后返回文件名。
pub fn import_wallpaper(src: &Path) -> Result<String> {
    import_wallpaper_to(src, &data_dir().join(WALLPAPER_DIR))
}

fn import_wallpaper_to(src: &Path, dir: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(src).with_context(|| format!("读取壁纸失败：{}", src.display()))?;
    if meta.len() > MAX_SOURCE_BYTES {
        bail!("图片超过 50 MB，请换一张");
    }
    let (width, height) = image::ImageReader::open(src)?
        .with_guessed_format()?
        .into_dimensions()
        .context("读取图片尺寸失败")?;
    if (width as u64) * (height as u64) > MAX_SOURCE_PIXELS {
        bail!("图片尺寸过大（{width}×{height}），请换一张");
    }

    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(src)?
        .with_guessed_format()?
        .into_decoder()
        .context("解码图片失败")?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder).context("解码图片失败")?;
    image.apply_orientation(orientation);
    let cropped = cover_crop(image, WALLPAPER_MAX_W, WALLPAPER_MAX_H);

    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("current.jpg.tmp");
    {
        use std::io::Write;
        let file = std::fs::File::create(&tmp)?;
        let mut writer = std::io::BufWriter::new(file);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 90)
            .encode_image(&cropped)?;
        writer.flush()?;
    }
    let dest = dir.join(WALLPAPER_FILE);
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&tmp, &dest)?;

    // 清理旧扩展名残留（current.png 等）。
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("current.") && name != WALLPAPER_FILE && !name.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(WALLPAPER_FILE.to_string())
}

/// 等比缩放到不超过目标尺寸（不放大），再居中裁剪为目标宽高比。
fn cover_crop(image: image::DynamicImage, max_w: u32, max_h: u32) -> image::RgbImage {
    let (width, height) = (image.width().max(1), image.height().max(1));
    let scale = f64::max(max_w as f64 / width as f64, max_h as f64 / height as f64).min(1.0);
    let (scaled_w, scaled_h) = if scale < 1.0 {
        (
            ((width as f64 * scale).round() as u32).max(1),
            ((height as f64 * scale).round() as u32).max(1),
        )
    } else {
        (width, height)
    };
    let resized = if (scaled_w, scaled_h) != (width, height) {
        image.resize_exact(scaled_w, scaled_h, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let aspect = max_w as f64 / max_h as f64;
    let (crop_w, crop_h) = if scaled_w as f64 / scaled_h as f64 > aspect {
        (
            (((scaled_h as f64 * aspect).round() as u32).max(1)).min(scaled_w),
            scaled_h,
        )
    } else {
        (
            scaled_w,
            (((scaled_w as f64 / aspect).round() as u32).max(1)).min(scaled_h),
        )
    };
    resized
        .crop_imm(
            (scaled_w - crop_w) / 2,
            (scaled_h - crop_h) / 2,
            crop_w,
            crop_h,
        )
        .to_rgb8()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_are_complete_and_unique() {
        let themes = catalog();
        assert_eq!(themes.len(), BUILTIN.len());
        for def in themes {
            assert_eq!(def.schema, 1);
            assert!(!def.id.is_empty() && !def.label.is_empty());
            def.colors.validate().unwrap_or_else(|e| {
                panic!("主题 {} 调色板无效：{e}", def.id);
            });
        }
        let mut ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), themes.len(), "主题 id 必须唯一");
        for id in [
            THEME_DARK,
            THEME_LIGHT,
            THEME_BEIGE,
            THEME_MINT,
            THEME_WALLPAPER,
        ] {
            assert!(find(id).is_some(), "缺少内置主题 {id}");
        }
        assert!(find(THEME_WALLPAPER).unwrap().wallpaper);
        assert!(!find(THEME_BEIGE).unwrap().dark);
        assert!(!find(THEME_MINT).unwrap().dark);
        assert!(!find(THEME_DARK).unwrap().wallpaper);
    }

    #[test]
    fn color_parser_accepts_rgb_and_rgba() {
        let rgb = parse_color("#123456").unwrap();
        assert_eq!(
            (rgb.red(), rgb.green(), rgb.blue(), rgb.alpha()),
            (0x12, 0x34, 0x56, 255)
        );
        // #RRGGBBAA：透明度在末尾。
        let rgba = parse_color("#12345678").unwrap();
        assert_eq!(
            (rgba.red(), rgba.green(), rgba.blue(), rgba.alpha()),
            (0x12, 0x34, 0x56, 0x78)
        );
        assert!(parse_color("123456").is_err());
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("#zzzzzz").is_err());
    }

    #[test]
    fn system_and_unknown_choices_fall_back_to_os_theme() {
        assert_eq!(resolve_with(THEME_SYSTEM, true).id, THEME_DARK);
        assert_eq!(resolve_with(THEME_SYSTEM, false).id, THEME_LIGHT);
        assert_eq!(resolve_with("no-such-theme", true).id, THEME_DARK);
        assert_eq!(resolve_with("no-such-theme", false).id, THEME_LIGHT);
        assert_eq!(resolve_with(THEME_BEIGE, true).id, THEME_BEIGE);
        assert_eq!(resolve_with(THEME_MINT, true).id, THEME_MINT);
        assert_eq!(resolve_with(THEME_WALLPAPER, false).id, THEME_WALLPAPER);
    }

    #[test]
    fn cover_crop_scales_down_and_never_upscales() {
        let big = image::DynamicImage::ImageRgb8(image::RgbImage::new(4000, 3000));
        let out = cover_crop(big, 2560, 1600);
        assert_eq!((out.width(), out.height()), (2560, 1600));

        // 小图不放大，只裁成 16:10。
        let small = image::DynamicImage::ImageRgb8(image::RgbImage::new(800, 600));
        let out = cover_crop(small, 2560, 1600);
        assert_eq!((out.width(), out.height()), (800, 500));

        // 竖图居中裁成 16:10。
        let tall = image::DynamicImage::ImageRgb8(image::RgbImage::new(1000, 2000));
        let out = cover_crop(tall, 2560, 1600);
        assert_eq!((out.width(), out.height()), (1000, 625));

        // 已匹配目标尺寸的图片原样保留。
        let exact = image::DynamicImage::ImageRgb8(image::RgbImage::new(2560, 1600));
        let out = cover_crop(exact, 2560, 1600);
        assert_eq!((out.width(), out.height()), (2560, 1600));
    }

    #[test]
    fn import_writes_cropped_wallpaper_and_replaces_old_file() {
        let dir = std::env::temp_dir().join(format!("asm-wallpaper-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.png");
        image::RgbImage::from_pixel(400, 300, image::Rgb([20, 40, 60]))
            .save(&src)
            .unwrap();
        // 旧扩展名残留应在替换后清理。
        std::fs::write(dir.join("current.png"), b"old").unwrap();

        let name = import_wallpaper_to(&src, &dir).unwrap();
        assert_eq!(name, WALLPAPER_FILE);
        let saved = image::ImageReader::open(dir.join(WALLPAPER_FILE))
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(saved, (400, 250));
        assert!(!dir.join("current.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_handles_unicode_paths() {
        let root = std::env::temp_dir().join(format!("asm-壁纸-{}", uuid::Uuid::new_v4()));
        let src_dir = root.join("素材目录");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("蓝色风景.jpg");
        image::RgbImage::from_pixel(320, 240, image::Rgb([10, 20, 30]))
            .save(&src)
            .unwrap();
        let out_dir = root.join("输出");
        let name = import_wallpaper_to(&src, &out_dir).unwrap();
        assert_eq!(name, WALLPAPER_FILE);
        assert!(out_dir.join(WALLPAPER_FILE).is_file());
        let dims = image::ImageReader::open(out_dir.join(WALLPAPER_FILE))
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(dims, (320, 200));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
