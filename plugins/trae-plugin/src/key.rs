//! Trae 数据库密钥获取：缓存、内置候选与可选的进程内存扫描。
//!
//! 缓存在插件自有 `data/` 目录，宿主不读取；每次使用前都用数据库首页
//! HMAC 校验。获取在 `plugin.register` 之后、`list`/`load` 内部惰性执行，
//! 失败通过 JSON-RPC error 返回，不写 stderr。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_core::protocol::PluginWarning;
use anyhow::{bail, Result};
use serde_json::Value;

use crate::sqlcipher::verify_key;

/// 已知可解开 Trae CN 数据库的密钥。
///
/// 密钥是构建期固定值，同一构建跨机器、跨重启通用。升级 Trae 后如果
/// 首页 HMAC 校验失败，需要追加新版本的候选密钥，或由用户在
/// `data/settings.toml` 打开进程内存获取。
pub const BUILTIN_KEYS: &[(&str, &str)] = &[(
    "Trae CN 2.3.105403 (3.4.22)",
    "3605f6691095a993f03d5009c918352ef5be31ae31e8f000212b81ff058da773",
)];

const CACHE_FILE: &str = "key-cache.json";
const SETTINGS_FILE: &str = "settings.toml";

/// `data/settings.toml` 中的密钥获取配置。
#[derive(Clone, Debug)]
pub struct Settings {
    /// 是否缓存已验证的密钥。
    pub cache: bool,
    /// 是否允许从运行中的 Trae 进程内存获取密钥。
    pub memory_scan: bool,
    /// 获取尝试次数，至少 1。
    pub retry_attempts: usize,
    /// 单次进程内存扫描的时间上限。
    pub attempt_timeout: Duration,
    /// 密钥获取总时间上限。
    pub total_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache: true,
            memory_scan: false,
            retry_attempts: 3,
            attempt_timeout: Duration::from_millis(8_000),
            total_timeout: Duration::from_millis(45_000),
        }
    }
}

impl Settings {
    /// 读取 `<data_dir>/settings.toml`；缺失或格式错误时使用默认值。
    pub fn load(data_dir: &Path) -> Self {
        let Ok(text) = fs::read_to_string(data_dir.join(SETTINGS_FILE)) else {
            return Self::default();
        };
        let Ok(value) = toml::from_str::<toml::Value>(&text) else {
            return Self::default();
        };
        let section = value.get("key_acquisition");
        let bool_of = |name: &str, fallback: bool| {
            section
                .and_then(|table| table.get(name))
                .and_then(toml::Value::as_bool)
                .unwrap_or(fallback)
        };
        let int_of = |name: &str, fallback: i64| {
            section
                .and_then(|table| table.get(name))
                .and_then(toml::Value::as_integer)
                .unwrap_or(fallback)
        };
        Self {
            cache: bool_of("cache", true),
            memory_scan: bool_of("memory_scan", false),
            retry_attempts: int_of("retry_attempts", 3).clamp(1, 10) as usize,
            attempt_timeout: Duration::from_millis(
                int_of("attempt_timeout_ms", 8_000).clamp(100, 60_000) as u64,
            ),
            total_timeout: Duration::from_millis(
                int_of("total_timeout_ms", 45_000).clamp(500, 120_000) as u64,
            ),
        }
    }
}

/// 插件数据目录。优先使用环境变量覆盖，便于测试。
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TRAE_PLUGIN_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let mut root = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if root
        .file_name()
        .map(|name| name.eq_ignore_ascii_case("bin"))
        .unwrap_or(false)
    {
        if let Some(parent) = root.parent() {
            root = parent.to_path_buf();
        }
    }
    root.join("data")
}

/// 密钥来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// `data/key-cache.json`。
    Cache,
    /// 内置候选表。
    Builtin,
    /// 运行中的 Trae 进程内存。
    MemoryScan,
}

/// 解析后的密钥与非致命诊断。
#[derive(Debug)]
pub struct ResolvedKey {
    /// 32 字节加密密钥。
    pub key: [u8; 32],
    /// 来源；目前用于测试与诊断。
    #[allow(dead_code)]
    pub source: KeySource,
    /// 缓存写入失败等诊断。
    pub warnings: Vec<PluginWarning>,
}

/// 密钥获取所需的上下文。
pub struct KeyRequest<'a> {
    /// 数据库路径，用于错误信息。
    pub db_path: &'a Path,
    /// 插件数据目录。
    pub data_dir: &'a Path,
    /// 数据库首页 4096 字节。
    pub page1: &'a [u8],
    /// Trae 构建号，仅用于缓存诊断。
    pub build: &'a str,
    /// 用户配置。
    pub settings: &'a Settings,
    /// 内置候选。
    pub builtin: &'a [(&'a str, &'a str)],
    /// 可选的进程内存扫描函数。
    pub scan: Option<&'a dyn Fn(&[u8], Duration) -> Result<Option<[u8; 32]>>>,
}

/// 依次尝试缓存、内置候选与进程内存，最多 `retry_attempts` 轮。
pub fn resolve(request: &KeyRequest) -> Result<ResolvedKey> {
    let salt = hex::encode(&request.page1[..16]);
    let mut warnings = Vec::new();

    if request.settings.cache {
        if let Some(key) = read_cache(request.data_dir, &salt) {
            if verify_key(&key, request.page1) {
                return Ok(ResolvedKey {
                    key,
                    source: KeySource::Cache,
                    warnings,
                });
            }
        }
    }

    let attempts = request.settings.retry_attempts.max(1);
    let started = Instant::now();
    let mut last = String::from("未执行");
    for _ in 0..attempts {
        if started.elapsed() >= request.settings.total_timeout {
            bail!(
                "无法获取 Trae 数据库密钥：总超时 {}ms。数据库：{}",
                request.settings.total_timeout.as_millis(),
                request.db_path.display()
            );
        }
        for (_, hex_key) in request.builtin {
            if let Some(key) = decode_key(hex_key) {
                if verify_key(&key, request.page1) {
                    cache_key(request, &salt, &key, &mut warnings);
                    return Ok(ResolvedKey {
                        key,
                        source: KeySource::Builtin,
                        warnings,
                    });
                }
            }
        }
        if request.settings.memory_scan {
            if let Some(scan) = request.scan {
                let remaining = request
                    .settings
                    .total_timeout
                    .saturating_sub(started.elapsed());
                let budget = request.settings.attempt_timeout.min(remaining);
                let attempt_started = Instant::now();
                match scan(request.page1, budget) {
                    Ok(Some(key)) => {
                        if request.page1.len() >= 16
                            && crate::sqlcipher::verify_key(&key, request.page1)
                        {
                            cache_key(request, &salt, &key, &mut warnings);
                            return Ok(ResolvedKey {
                                key,
                                source: KeySource::MemoryScan,
                                warnings,
                            });
                        }
                        last = "进程内存候选未通过首页 HMAC 校验".into();
                    }
                    Ok(None) => {
                        last = if attempt_started.elapsed() > budget {
                            format!("进程内存获取超时（{}ms）", budget.as_millis())
                        } else {
                            "进程内存中未找到密钥".into()
                        };
                    }
                    Err(error) => {
                        last = if attempt_started.elapsed() > budget {
                            format!("进程内存获取超时（{}ms）", budget.as_millis())
                        } else {
                            format!("进程内存获取失败：{error:#}")
                        };
                    }
                }
            } else {
                last = "当前平台不支持进程内存获取".into();
            }
        } else {
            last = format!(
                "进程内存获取未启用（可在 data/{SETTINGS_FILE} 将 key_acquisition.memory_scan 设为 true）"
            );
        }
    }

    bail!(
        "无法获取 Trae 数据库密钥：已尝试 {attempts} 次，{last}。数据库：{}",
        request.db_path.display()
    )
}

fn decode_key(hex_key: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_key.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(key)
}

fn read_cache(data_dir: &Path, salt: &str) -> Option<[u8; 32]> {
    let text = fs::read_to_string(data_dir.join(CACHE_FILE)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    if value.get("salt").and_then(Value::as_str)? != salt {
        return None;
    }
    let key_hex = value.get("key").and_then(Value::as_str)?;
    decode_key(key_hex)
}

fn cache_key(request: &KeyRequest, salt: &str, key: &[u8; 32], warnings: &mut Vec<PluginWarning>) {
    if !request.settings.cache {
        return;
    }
    if let Err(error) = write_cache(request.data_dir, salt, request.build, key) {
        warnings.push(PluginWarning {
            kind: "key_cache_write_failed".into(),
            message: format!("密钥缓存写入失败：{error:#}"),
            session_id: None,
        });
    }
}

fn write_cache(data_dir: &Path, salt: &str, build: &str, key: &[u8; 32]) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let value = serde_json::json!({
        "schema": 1,
        "salt": salt,
        "build": build,
        "key": hex::encode(key),
        "acquired_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    });
    let destination = data_dir.join(CACHE_FILE);
    let temporary = data_dir.join(format!("{CACHE_FILE}.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    if destination.exists() {
        let _ = fs::remove_file(&destination);
    }
    fs::rename(&temporary, &destination)?;
    Ok(())
}

/// 从运行中的 Trae 进程内存获取密钥。
#[cfg(windows)]
pub fn scan_processes(page1: &[u8], budget: Duration) -> Result<Option<[u8; 32]>> {
    windows_scan::scan(page1, budget)
}

/// 非 Windows 平台不提供进程内存扫描。
#[cfg(not(windows))]
pub fn scan_processes(_page1: &[u8], _budget: Duration) -> Result<Option<[u8; 32]>> {
    Ok(None)
}

#[cfg(windows)]
mod windows_scan {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Result};

    use super::verify_key;

    // 在 Trae 进程的用户态内存里寻找 SQLCipher 密钥候选：64 位十六进制串或
    // `x'...'` 字面量形式。候选必须先通过 verify_key 的首页 HMAC 校验才会返回，
    // 误报候选不会污染结果。扫描按 1 MB 分块进行，受单轮超时约束，失败不产生副作用。

    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const PROCESS_VM_READ: u32 = 0x10;
    const PROCESS_QUERY_INFORMATION: u32 = 0x400;
    const MEM_COMMIT: u32 = 0x1000;
    const PAGE_GUARD: u32 = 0x100;
    const PAGE_NOACCESS: u32 = 0x01;
    // 用户态地址上限：x64 Windows 用户空间最高到 0x7fff_ffffffff，跳过内核空间。
    const MAX_ADDRESS: usize = 0x7fff_ffff_ffff;
    const CHUNK: usize = 1024 * 1024;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        priority_class: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[repr(C)]
    struct MemoryBasicInformation {
        base_address: *mut c_void,
        allocation_base: *mut c_void,
        allocation_protect: u32,
        partition_id: u16,
        region_size: usize,
        state: u32,
        protect: u32,
        kind: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn VirtualQueryEx(
            process: *mut c_void,
            address: *const c_void,
            buffer: *mut MemoryBasicInformation,
            length: usize,
        ) -> usize;
        fn ReadProcessMemory(
            process: *mut c_void,
            address: *const c_void,
            buffer: *mut c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
    }

    pub fn scan(page1: &[u8], budget: Duration) -> Result<Option<[u8; 32]>> {
        let deadline = Instant::now() + budget;
        let pids = trae_processes()?;
        if pids.is_empty() {
            bail!("未找到运行中的 Trae CN 进程");
        }
        for pid in pids {
            if Instant::now() >= deadline {
                bail!("进程内存扫描超时");
            }
            let handle =
                unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
            if handle.is_null() {
                continue;
            }
            let result = scan_handle(handle, page1, deadline);
            unsafe {
                CloseHandle(handle);
            }
            if let Some(key) = result? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    fn trae_processes() -> Result<Vec<u32>> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot.is_null() || snapshot as usize == usize::MAX {
            bail!("无法枚举进程");
        }
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut pids = Vec::new();
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while ok {
            let length = entry
                .exe_file
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.exe_file.len());
            let name = String::from_utf16_lossy(&entry.exe_file[..length]);
            if name.eq_ignore_ascii_case("Trae CN.exe") || name.eq_ignore_ascii_case("Trae.exe") {
                pids.push(entry.process_id);
            }
            ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe {
            CloseHandle(snapshot);
        }
        Ok(pids)
    }

    fn scan_handle(
        handle: *mut c_void,
        page1: &[u8],
        deadline: Instant,
    ) -> Result<Option<[u8; 32]>> {
        let mut address = 0usize;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            let mut info: MemoryBasicInformation = unsafe { std::mem::zeroed() };
            let queried = unsafe {
                VirtualQueryEx(
                    handle,
                    address as *const c_void,
                    &mut info,
                    std::mem::size_of::<MemoryBasicInformation>(),
                )
            };
            if queried == 0 {
                break;
            }
            let next = address.saturating_add(info.region_size);
            if info.state == MEM_COMMIT
                && info.protect & PAGE_GUARD == 0
                // protect 低 8 位是基本保护属性（高位是修饰标志），排除不可读区域。
                && info.protect & 0xff != PAGE_NOACCESS
                && !info.base_address.is_null()
            {
                let mut position = address;
                while position < next {
                    if Instant::now() >= deadline {
                        bail!("进程内存扫描超时");
                    }
                    let size = CHUNK.min(next - position);
                    let mut read = 0usize;
                    let ok = unsafe {
                        ReadProcessMemory(
                            handle,
                            position as *const c_void,
                            buffer.as_mut_ptr() as *mut c_void,
                            size,
                            &mut read,
                        )
                    };
                    if ok != 0 && read > 0 {
                        if let Some(key) = scan_chunk(&buffer[..read], page1) {
                            return Ok(Some(key));
                        }
                    }
                    position += size;
                }
            }
            if next <= address || next > MAX_ADDRESS {
                break;
            }
            address = next;
        }
        Ok(None)
    }

    fn scan_chunk(chunk: &[u8], page1: &[u8]) -> Option<[u8; 32]> {
        let mut index = 0usize;
        while index + 2 < chunk.len() {
            if (chunk[index] == b'x' || chunk[index] == b'X') && chunk[index + 1] == b'\'' {
                let start = index + 2;
                let mut end = start;
                while end < chunk.len() && chunk[end].is_ascii_hexdigit() {
                    end += 1;
                }
                if end < chunk.len() && chunk[end] == b'\'' && end - start >= 64 {
                    if let Some(key) = try_hex(&chunk[start..start + 64], page1) {
                        return Some(key);
                    }
                }
                index = end.saturating_add(1);
                continue;
            }
            if chunk[index].is_ascii_hexdigit()
                && (index == 0 || !chunk[index - 1].is_ascii_hexdigit())
            {
                let mut end = index;
                while end < chunk.len() && chunk[end].is_ascii_hexdigit() {
                    end += 1;
                }
                if end - index >= 64 {
                    if let Some(key) = try_hex(&chunk[index..index + 64], page1) {
                        return Some(key);
                    }
                }
                index = end;
                continue;
            }
            index += 1;
        }
        None
    }

    fn try_hex(bytes: &[u8], page1: &[u8]) -> Option<[u8; 32]> {
        let text = std::str::from_utf8(bytes).ok()?;
        let decoded = hex::decode(text).ok()?;
        if decoded.len() != 32 {
            return None;
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&decoded);
        if verify_key(&key, page1) {
            Some(key)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("asm-trae-key-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const KEY: [u8; 32] = [7u8; 32];

    fn sealed_page() -> Vec<u8> {
        let mut plain = [0u8; 4096];
        plain[..16].copy_from_slice(b"SQLite format 3\0");
        plain[16..24].copy_from_slice(b"\x10\x00\x01\x01\x00\x40\x20\x20");
        crate::sqlcipher::seal_page_for_test(&plain, &KEY, 1)
    }

    fn settings(scan: bool, attempts: usize, timeout_ms: u64) -> Settings {
        Settings {
            cache: true,
            memory_scan: scan,
            retry_attempts: attempts,
            attempt_timeout: Duration::from_millis(timeout_ms),
            total_timeout: Duration::from_secs(5),
        }
    }

    fn request<'a>(
        dir: &'a Path,
        page: &'a [u8],
        settings: &'a Settings,
        builtin: &'a [(&'a str, &'a str)],
        scan: Option<&'a dyn Fn(&[u8], Duration) -> Result<Option<[u8; 32]>>>,
    ) -> KeyRequest<'a> {
        KeyRequest {
            db_path: Path::new("C:\\test\\database.db"),
            data_dir: dir,
            page1: page,
            build: "test-build",
            settings,
            builtin,
            scan,
        }
    }

    #[test]
    fn cache_hit_is_used_first() {
        let temp = TempDir::new("cache-hit");
        let page = sealed_page();
        let salt = hex::encode(&page[..16]);
        write_cache(&temp.0, &salt, "test-build", &KEY).unwrap();
        let config = settings(false, 3, 100);
        let resolved = resolve(&request(&temp.0, &page, &config, &[], None)).unwrap();
        assert_eq!(resolved.source, KeySource::Cache);
        assert_eq!(resolved.key, KEY);
    }

    #[test]
    fn corrupt_cache_falls_back_to_builtin() {
        let temp = TempDir::new("cache-corrupt");
        fs::write(temp.0.join(CACHE_FILE), b"not json").unwrap();
        let page = sealed_page();
        let key_hex = hex::encode(KEY);
        let builtin = [("test", key_hex.as_str())];
        let config = settings(false, 3, 100);
        let resolved = resolve(&request(&temp.0, &page, &config, &builtin, None)).unwrap();
        assert_eq!(resolved.source, KeySource::Builtin);
    }

    #[test]
    fn cache_with_wrong_key_falls_back_to_builtin() {
        let temp = TempDir::new("cache-wrong");
        let page = sealed_page();
        let salt = hex::encode(&page[..16]);
        write_cache(&temp.0, &salt, "test-build", &[9u8; 32]).unwrap();
        let key_hex = hex::encode(KEY);
        let builtin = [("test", key_hex.as_str())];
        let config = settings(false, 3, 100);
        let resolved = resolve(&request(&temp.0, &page, &config, &builtin, None)).unwrap();
        assert_eq!(resolved.source, KeySource::Builtin);
    }

    #[test]
    fn all_sources_fail_reports_attempts_and_path() {
        let temp = TempDir::new("all-fail");
        let page = sealed_page();
        let config = settings(false, 3, 100);
        let error = resolve(&request(&temp.0, &page, &config, &[], None)).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("已尝试 3 次"), "{message}");
        assert!(message.contains("database.db"), "{message}");
        assert!(message.contains("密钥"), "{message}");
    }

    #[test]
    fn slow_scan_hits_timeout() {
        let temp = TempDir::new("slow-scan");
        let page = sealed_page();
        let config = settings(true, 1, 5);
        let scan = |_page: &[u8], _budget: Duration| -> Result<Option<[u8; 32]>> {
            std::thread::sleep(Duration::from_millis(20));
            Ok(None)
        };
        let error = resolve(&request(&temp.0, &page, &config, &[], Some(&scan))).unwrap_err();
        assert!(format!("{error:#}").contains("超时"), "{error:#}");
    }
}
