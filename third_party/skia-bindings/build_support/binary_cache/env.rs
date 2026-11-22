use std::path::PathBuf;

use crate::build_support::cargo;

/// A directory containing an unpacked official prebuilt binaries package.
///
/// Honors `SKIA_BINARIES_DIR` first. When unset, falls back to the sibling
/// `skia` directory next to this crate (the fixed in-repo prebuilt location),
/// but only when it carries the `tag.txt`/`key.txt` markers; otherwise returns
/// `None` so the build follows the upstream download path.
pub fn skia_binaries_dir() -> Option<PathBuf> {
    if let Some(dir) = cargo::env_var("SKIA_BINARIES_DIR") {
        return Some(PathBuf::from(dir));
    }
    let sibling = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .and_then(|manifest| {
            PathBuf::from(manifest)
                .parent()
                .map(|parent| parent.join("skia"))
        });
    match sibling {
        Some(dir) if dir.join("tag.txt").is_file() && dir.join("key.txt").is_file() => Some(dir),
        _ => None,
    }
}

/// Returns `true` if the download of prebuilt binaries should be forced.
///
/// This can be used to test and download prebuilt binaries from within a repository build.
/// If this environment variable is not set, binaries are downloaded from crate builds only.
pub fn force_skia_binaries_download() -> bool {
    cargo::env_var("FORCE_SKIA_BINARIES_DOWNLOAD").is_some()
}

/// The URL template to download the Skia binaries from.
///
/// `{tag}` will be replaced by the Tag (usually the released skia-binding's crate's version).
/// `{key}` will be replaced by the Key (a combination of the repository hash, target, and features).
///
/// `file://` URLs are supported for local testing.
pub fn skia_binaries_url() -> Option<String> {
    cargo::env_var("SKIA_BINARIES_URL")
}

/// The default URL template to download the binaries from.
pub fn skia_binaries_url_default() -> String {
    "https://github.com/rust-skia/skia-binaries/releases/download/{tag}/skia-binaries-{key}.tar.gz"
        .into()
}

/// Force to build Skia, even if there is a binary available.
pub fn force_skia_build() -> bool {
    cargo::env_var("FORCE_SKIA_BUILD").is_some()
}
