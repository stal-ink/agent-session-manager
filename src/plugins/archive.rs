//! 插件压缩包导入：只接受归档文件，逐条安全解压到 staging。
//! 插件运行形态始终是普通目录，压缩包只作为导入来源。

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PATH_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    SevenZ,
}

impl Format {
    pub fn detect(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".zip") {
            Ok(Self::Zip)
        } else if name.ends_with(".tar") {
            Ok(Self::Tar)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Ok(Self::TarGz)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Ok(Self::TarXz)
        } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
            Ok(Self::TarZst)
        } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
            Ok(Self::TarBz2)
        } else if name.ends_with(".7z") {
            Ok(Self::SevenZ)
        } else {
            anyhow::bail!(
                "不支持的插件包格式：{}（支持 zip、tar、tar.gz、tar.xz、tar.zst、tar.bz2、7z）",
                path.display()
            )
        }
    }
}

pub fn extract(archive: &Path, target: &Path) -> Result<()> {
    if archive.is_dir() {
        anyhow::bail!("插件只从压缩包导入，不接受目录：{}", archive.display());
    }
    let size = archive
        .metadata()
        .with_context(|| format!("读取插件包信息失败：{}", archive.display()))?
        .len();
    if size > MAX_ARCHIVE_BYTES {
        anyhow::bail!("插件包超过 {MAX_ARCHIVE_BYTES} 字节：{}", archive.display());
    }
    fs::create_dir_all(target)
        .with_context(|| format!("创建插件暂存目录失败：{}", target.display()))?;
    let format = Format::detect(archive)?;
    let mut sink = Sink::new(target)?;
    match format {
        Format::Zip => extract_zip(archive, &mut sink)?,
        Format::Tar => extract_tar(fs::File::open(archive)?, &mut sink)?,
        Format::TarGz => extract_tar(
            flate2::read::GzDecoder::new(fs::File::open(archive)?),
            &mut sink,
        )?,
        Format::TarXz => extract_tar_xz(archive, &mut sink)?,
        Format::TarZst => extract_tar(
            zstd::stream::read::Decoder::new(fs::File::open(archive)?)?,
            &mut sink,
        )?,
        Format::TarBz2 => extract_tar(
            bzip2::read::BzDecoder::new(fs::File::open(archive)?),
            &mut sink,
        )?,
        Format::SevenZ => extract_7z(archive, &mut sink)?,
    }
    if sink.entries == 0 {
        anyhow::bail!("插件包为空：{}", archive.display());
    }
    if !target.join("plugin.toml").is_file() && single_directory(target).is_none() {
        anyhow::bail!("插件包根目录缺少 plugin.toml");
    }
    Ok(())
}

fn extract_zip(archive: &Path, sink: &mut Sink) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).context("读取 zip 插件包失败")?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).context("读取 zip 条目失败")?;
        let name = entry.name().context("读取 zip 条目路径失败")?.to_string();
        if entry.is_dir() {
            sink.directory(&name)?;
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            // 0o170000 是 S_IFMT（文件类型掩码），0o120000 是 S_IFLNK（符号链接）。
            anyhow::bail!("插件包不能包含符号链接：{name}");
        }
        sink.file(&name, &mut entry)?;
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, sink: &mut Sink) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().context("读取 tar 插件包失败")? {
        let mut entry = entry.context("读取 tar 条目失败")?;
        let kind = entry.header().entry_type();
        let path = entry.path().context("读取 tar 条目路径失败")?;
        let name = path.to_string_lossy().into_owned();
        if kind.is_dir() {
            sink.directory(&name)?;
        } else if kind.is_file() {
            sink.file(&name, &mut entry)?;
        } else if kind.is_symlink() || kind.is_hard_link() {
            anyhow::bail!("插件包不能包含链接：{name}");
        } else {
            anyhow::bail!("插件包包含不支持的 tar 条目：{name}");
        }
    }
    Ok(())
}

fn extract_tar_xz(archive: &Path, sink: &mut Sink) -> Result<()> {
    let mut input = std::io::BufReader::new(fs::File::open(archive)?);
    let mut tar = LimitedVec::new(MAX_TOTAL_BYTES);
    lzma_rs::xz_decompress(&mut input, &mut tar).context("解压 tar.xz 失败")?;
    extract_tar(std::io::Cursor::new(tar.data), sink)
}

struct LimitedVec {
    data: Vec<u8>,
    limit: u64,
}

impl LimitedVec {
    fn new(limit: u64) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }
}

impl Write for LimitedVec {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.data.len() as u64 + buffer.len() as u64 > self.limit {
            return Err(std::io::Error::other("解压总量超过限制"));
        }
        self.data.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn extract_7z(archive: &Path, sink: &mut Sink) -> Result<()> {
    let mut reader = sevenz_rust::SevenZReader::open(archive, sevenz_rust::Password::empty())
        .context("读取 7z 插件包失败")?;
    let mut failure = None;
    reader
        .for_each_entries(|entry, input| {
            let name = entry.name().to_string();
            let result = (|| -> Result<()> {
                if entry.is_anti_item() {
                    anyhow::bail!("插件包包含 7z 删除标记：{name}");
                } else if entry.is_directory() {
                    sink.directory(&name)?;
                } else if entry.windows_attributes() & 0x400 != 0 {
                    // 0x400 即 FILE_ATTRIBUTE_REPARSE_POINT，重解析点与符号链接一并拒绝。
                    anyhow::bail!("插件包不能包含重解析点/符号链接：{name}");
                } else {
                    sink.file(&name, input)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
                return Ok(false);
            }
            Ok(true)
        })
        .context("解压 7z 插件包失败")?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct Sink {
    root: PathBuf,
    seen: HashSet<String>,
    entries: usize,
    total: u64,
}

impl Sink {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            seen: HashSet::new(),
            entries: 0,
            total: 0,
        })
    }

    fn directory(&mut self, raw: &str) -> Result<()> {
        let relative = self.claim(raw)?;
        fs::create_dir_all(self.root.join(&relative))
            .with_context(|| format!("创建解压目录失败：{}", relative.display()))?;
        Ok(())
    }

    fn file(&mut self, raw: &str, reader: &mut dyn Read) -> Result<()> {
        let relative = self.claim(raw)?;
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建解压目录失败：{}", parent.display()))?;
        }
        let mut file =
            File::create(&path).with_context(|| format!("创建解压文件失败：{}", path.display()))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            let read = reader.read(&mut buffer).context("读取压缩包条目失败")?;
            if read == 0 {
                break;
            }
            written += read as u64;
            self.total += read as u64;
            if written > MAX_FILE_BYTES {
                anyhow::bail!("解压文件超过 {MAX_FILE_BYTES} 字节：{raw}");
            }
            if self.total > MAX_TOTAL_BYTES {
                anyhow::bail!("解压总量超过 {MAX_TOTAL_BYTES} 字节");
            }
            file.write_all(&buffer[..read])
                .with_context(|| format!("写入解压文件失败：{}", path.display()))?;
        }
        file.flush()?;
        Ok(())
    }

    fn claim(&mut self, raw: &str) -> Result<PathBuf> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            anyhow::bail!("插件包条目超过 {MAX_ENTRIES} 个");
        }
        let relative = safe_relative(raw)?;
        let key = relative.to_string_lossy().to_ascii_lowercase();
        if !self.seen.insert(key) {
            anyhow::bail!("插件包包含重复路径：{raw}");
        }
        Ok(relative)
    }
}

fn safe_relative(raw: &str) -> Result<PathBuf> {
    if raw.chars().count() > MAX_PATH_CHARS {
        anyhow::bail!("插件包路径过长：{raw}");
    }
    let normalized = raw.replace('\\', "/");
    let mut out = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_string_lossy();
                if value.is_empty() || value == "." {
                    continue;
                }
                validate_component(&value)?;
                out.push(value.as_ref());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("插件包路径必须是相对路径：{raw}");
            }
        }
    }
    if out.as_os_str().is_empty() {
        anyhow::bail!("插件包包含空路径");
    }
    Ok(out)
}

fn validate_component(value: &str) -> Result<()> {
    if value.ends_with('.') || value.ends_with(' ') {
        anyhow::bail!("插件包路径包含 Windows 不允许的结尾：{value}");
    }
    if value.contains(':') {
        anyhow::bail!("插件包路径不能包含备用数据流：{value}");
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        anyhow::bail!("插件包路径包含非法字符：{value}");
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        anyhow::bail!("插件包路径使用 Windows 保留名：{value}");
    }
    Ok(())
}

/// 压缩包允许把整个包放在单一顶层目录里；返回该目录。
pub fn package_root(staging: &Path) -> Result<PathBuf> {
    if staging.join("plugin.toml").is_file() {
        return Ok(staging.to_path_buf());
    }
    single_directory(staging)
        .filter(|path| path.join("plugin.toml").is_file())
        .with_context(|| "插件包根目录缺少 plugin.toml")
}

fn single_directory(path: &Path) -> Option<PathBuf> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(path).ok()?.flatten() {
        let file_type = entry.file_type().ok()?;
        if file_type.is_dir() {
            directories.push(entry.path());
        } else if entry.file_name() != ".DS_Store" {
            return None;
        }
    }
    (directories.len() == 1).then(|| directories.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_staging() {
        assert!(safe_relative("../plugin.toml").is_err());
        assert!(safe_relative("a/../../b").is_err());
        assert!(safe_relative(r"C:\plugin\a.exe").is_err());
        assert!(safe_relative(r"\\server\share\a.exe").is_err());
        assert!(safe_relative("a/file:stream").is_err());
        assert!(safe_relative("CON/plugin.toml").is_err());
        assert!(safe_relative("a ").is_err());
        assert!(safe_relative("dir/./file").is_ok());
    }

    #[test]
    fn formats_are_detected_by_extension() {
        assert_eq!(Format::detect(Path::new("q.zip")).unwrap(), Format::Zip);
        assert_eq!(Format::detect(Path::new("q.tar")).unwrap(), Format::Tar);
        assert_eq!(Format::detect(Path::new("q.tgz")).unwrap(), Format::TarGz);
        assert_eq!(Format::detect(Path::new("q.txz")).unwrap(), Format::TarXz);
        assert_eq!(Format::detect(Path::new("q.tzst")).unwrap(), Format::TarZst);
        assert_eq!(Format::detect(Path::new("q.tbz2")).unwrap(), Format::TarBz2);
        assert_eq!(Format::detect(Path::new("q.7z")).unwrap(), Format::SevenZ);
        assert!(Format::detect(Path::new("q.rar")).is_err());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("asm-archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn zip_import_extracts_and_rejects_escape() {
        let dir = temp_dir("zip");
        let package = dir.join("package");
        write_zip(
            &dir.join("good.zip"),
            &[
                ("plugin.toml", b"schema = 1"),
                ("icon.png", b"icon"),
                ("bin/plugin.exe", b"exe"),
            ],
        );
        extract(&dir.join("good.zip"), &package).unwrap();
        assert_eq!(fs::read(package.join("bin/plugin.exe")).unwrap(), b"exe");
        assert_eq!(package_root(&package).unwrap(), package);

        write_zip(
            &dir.join("bad.zip"),
            &[("../escape.txt", b"escape"), ("plugin.toml", b"x")],
        );
        let escaped = dir.join("escape.txt");
        assert!(extract(&dir.join("bad.zip"), &dir.join("bad")).is_err());
        assert!(!escaped.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tar_gz_import_and_single_wrapper_directory_are_supported() {
        let dir = temp_dir("targz");
        let archive = dir.join("package.tar.gz");
        let file = File::create(&archive).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        for (name, bytes) in [
            ("qoder/plugin.toml", b"schema = 1".as_slice()),
            ("qoder/icon.png", b"icon".as_slice()),
            ("qoder/bin/plugin.exe", b"exe".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();

        let staging = dir.join("staging");
        extract(&archive, &staging).unwrap();
        let root = package_root(&staging).unwrap();
        assert_eq!(root, staging.join("qoder"));
        let _ = fs::remove_dir_all(&dir);
    }
}
