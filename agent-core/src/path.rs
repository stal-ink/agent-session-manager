//! Windows 路径规范化与工作区比较键。

use std::path::Path;

/// 规范化 Windows 路径：正斜杠转反斜杠，去掉末尾分隔符，盘符根保留分隔符。
pub fn normalize(path: &str) -> String {
    let mut normalized = path.replace('/', "\\");
    while normalized.ends_with('\\') && normalized.len() > 3 {
        normalized.pop();
    }
    normalized
}

/// 工作区比较键：规范化后转 ASCII 小写。
pub fn key(path: &str) -> String {
    normalize(path).to_ascii_lowercase()
}

/// 两个路径是否指向同一工作区。
///
/// ```
/// assert!(agent_core::path::same("D:/Work/App", r"d:\work\app"));
/// ```
pub fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// 只把盘符转为小写，保留其余大小写与分隔符。
pub fn lower_drive(path: &str) -> String {
    if path.as_bytes().get(1) == Some(&b':') {
        format!("{}{}", path[..1].to_ascii_lowercase(), &path[1..])
    } else {
        path.to_string()
    }
}

/// 规范化后取最后一个路径分量。
pub fn name(path: &str) -> String {
    let normalized = normalize(path);
    Path::new(&normalized)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_case_and_trailing_separators_match() {
        assert!(same("D:/Work/Ink", r"D:\Work\Ink"));
        assert!(same(r"c:\work\proj\\", r"C:\Work\Proj"));
        assert!(!same(r"C:\Work\Proj", r"C:\Work\Other"));
    }

    #[test]
    fn drive_root_keeps_its_separator() {
        assert_eq!(normalize("C:/"), r"C:\");
        assert_eq!(normalize("/"), "\\");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn name_uses_last_component() {
        assert_eq!(name("D:/RedInk.pack/Ink/"), "Ink");
    }

    #[test]
    fn lower_drive_only_touches_drive_letter() {
        assert_eq!(lower_drive("D:/Work/Ink"), "d:/Work/Ink");
        assert_eq!(lower_drive(r"E:\Work"), r"e:\Work");
        assert_eq!(lower_drive("/home/User"), "/home/User");
    }
}
