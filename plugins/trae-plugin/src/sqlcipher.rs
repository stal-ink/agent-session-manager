//! Trae 数据库的 SQLCipher 4 页级解密。
//!
//! 参数来自 Trae CN 实际存储：page_size 4096，每页 reserve 80 字节，
//! 其中 16 字节 IV 位于 4016..4032，64 字节 HMAC-SHA512 位于 4032..4096。
//! 第 1 页前 16 字节是 salt，密文从偏移 16 开始；其余页从 0 开始。
//!
//! 页格式与社区解密项目一致，感谢 TRAE 官方社区 topic/18248、topic/18351
//! 与 bigmanBass666/trae-db-decrypt；本实现为独立重写。

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use aes::cipher::{block_padding::NoPadding, BlockModeDecrypt, KeyIvInit};
use aes::Aes256;
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use sha2::Sha512;

const PAGE_SIZE: usize = 4096;
const SALT_SIZE: usize = 16;
const IV_SIZE: usize = 16;
const HMAC_SIZE: usize = 64;
const RESERVE_SIZE: usize = 80;
const CIPHERTEXT_END: usize = PAGE_SIZE - RESERVE_SIZE;
const IV_OFFSET: usize = CIPHERTEXT_END;
const HMAC_OFFSET: usize = PAGE_SIZE - HMAC_SIZE;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const WAL_HEADER_SIZE: usize = 32;
const WAL_FRAME_HEADER_SIZE: usize = 24;
const WAL_MAGIC_LE: u32 = 0x377f_0682;
const WAL_MAGIC_BE: u32 = 0x377f_0683;
const WAL_VERSION: u32 = 3_007_000;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// 解密后的临时数据库；离开作用域时删除明文文件。
pub struct DecryptedDb {
    path: PathBuf,
}

impl DecryptedDb {
    /// 用已校验的密钥解密 `db`，随后应用 `wal` 中已提交的页。
    pub fn open(db: &Path, wal: &Path, key: &[u8; 32]) -> Result<Self> {
        let mut source = File::open(db).with_context(|| format!("open {}", db.display()))?;
        let mut page1 = vec![0u8; PAGE_SIZE];
        source
            .read_exact(&mut page1)
            .with_context(|| format!("read {}", db.display()))?;

        let path = temp_path();
        {
            let mut out =
                File::create(&path).with_context(|| format!("create {}", path.display()))?;
            write_plain_page(&mut out, &page1, key, 1)?;
            let mut pgno = 2u64;
            let mut page = vec![0u8; PAGE_SIZE];
            loop {
                match source.read(&mut page) {
                    Ok(0) => break,
                    Ok(n) if n == PAGE_SIZE => {
                        write_plain_page(&mut out, &page, key, pgno)?;
                        pgno += 1;
                    }
                    Ok(n) => {
                        // 尾部不完整页按 SQLite 语义补零。
                        page[n..].fill(0);
                        write_plain_page(&mut out, &page, key, pgno)?;
                        break;
                    }
                    Err(error) => return Err(error).context("读取 Trae 数据库"),
                }
            }
            out.flush()?;
        }

        if wal.is_file() {
            apply_wal(&path, wal, key)?;
        }
        Ok(Self { path })
    }

    /// 解密后的 SQLite 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DecryptedDb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn temp_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("asm-trae-{}-{stamp}.db", std::process::id()))
}

/// 用数据库首页校验密钥是否正确。
pub(crate) fn verify_key(key: &[u8; 32], page1: &[u8]) -> bool {
    if page1.len() < PAGE_SIZE {
        return false;
    }
    let salt = &page1[..SALT_SIZE];
    let mac_salt: Vec<u8> = salt.iter().map(|byte| byte ^ 0x3a).collect();
    let mac_key = pbkdf2_sha512(key, &mac_salt, 2, 32);
    let mut mac = match <Hmac<Sha512>>::new_from_slice(&mac_key) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(&page1[SALT_SIZE..HMAC_OFFSET]);
    mac.update(&1u32.to_le_bytes());
    mac.finalize().into_bytes()[..] == page1[HMAC_OFFSET..]
}

/// PBKDF2-HMAC-SHA512；Trae 只用到 2 轮，用于从加密密钥派生 HMAC 密钥。
fn pbkdf2_sha512(password: &[u8], salt: &[u8], iterations: u32, dklen: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(dklen);
    let mut block = 1u32;
    while out.len() < dklen {
        let mut mac = <Hmac<Sha512>>::new_from_slice(password).expect("HMAC 接受任意密钥长度");
        mac.update(salt);
        mac.update(&block.to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut t = u;
        for _ in 1..iterations {
            let mut mac = <Hmac<Sha512>>::new_from_slice(password).expect("HMAC 接受任意密钥长度");
            mac.update(&u);
            u = mac.finalize().into_bytes();
            for (left, right) in t.iter_mut().zip(u.iter()) {
                *left ^= right;
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(dklen);
    out
}

fn decrypt_page(page: &[u8], key: &[u8; 32], pgno: u64) -> Result<Vec<u8>> {
    if page.len() < PAGE_SIZE {
        bail!("Trae 数据库页长度不足");
    }
    let iv = &page[IV_OFFSET..IV_OFFSET + IV_SIZE];
    let mut out = if pgno == 1 {
        let mut out = Vec::with_capacity(PAGE_SIZE);
        out.extend_from_slice(SQLITE_MAGIC);
        out
    } else {
        Vec::with_capacity(PAGE_SIZE)
    };
    let start = if pgno == 1 { SALT_SIZE } else { 0 };
    let mut buf = page[start..CIPHERTEXT_END].to_vec();
    let cipher = Aes256CbcDec::new_from_slices(key, iv)
        .map_err(|error| anyhow::anyhow!("创建 AES 解密器失败：{error}"))?;
    let plain = cipher
        .decrypt_padded::<NoPadding>(&mut buf)
        .map_err(|_| anyhow::anyhow!("SQLCipher 页解密失败"))?;
    out.extend_from_slice(plain);
    out.resize(PAGE_SIZE, 0);
    Ok(out)
}

fn write_plain_page(out: &mut File, page: &[u8], key: &[u8; 32], pgno: u64) -> Result<()> {
    let plain = decrypt_page(page, key, pgno)?;
    out.write_all(&plain)?;
    Ok(())
}

/// 把 WAL 中最后一次提交之前的页写入解密库。
///
/// 这里不校验 WAL 校验和：读取场景下以“最后一次提交”为边界，
/// 崩溃导致的半写帧会被自然截断。
fn apply_wal(db_path: &Path, wal: &Path, key: &[u8; 32]) -> Result<()> {
    let data = match fs::read(wal) {
        Ok(data) => data,
        Err(_) => return Ok(()),
    };
    let frame_size = WAL_FRAME_HEADER_SIZE + PAGE_SIZE;
    if data.len() < WAL_HEADER_SIZE + frame_size {
        return Ok(());
    }
    let magic = u32::from_be_bytes(data[0..4].try_into().expect("4 bytes"));
    if magic != WAL_MAGIC_LE && magic != WAL_MAGIC_BE {
        return Ok(());
    }
    let version = u32::from_be_bytes(data[4..8].try_into().expect("4 bytes"));
    if version != WAL_VERSION {
        return Ok(());
    }
    let page_size = u32::from_be_bytes(data[8..12].try_into().expect("4 bytes"));
    if page_size as usize != PAGE_SIZE {
        return Ok(());
    }
    let salt1 = &data[16..20];
    let salt2 = &data[20..24];

    let mut frames = Vec::new();
    let mut last_commit = None;
    let mut offset = WAL_HEADER_SIZE;
    while offset + frame_size <= data.len() {
        let header = &data[offset..offset + WAL_FRAME_HEADER_SIZE];
        if &header[8..12] != salt1 || &header[12..16] != salt2 {
            break;
        }
        let pgno = u32::from_be_bytes(header[0..4].try_into().expect("4 bytes"));
        let db_size = u32::from_be_bytes(header[4..8].try_into().expect("4 bytes"));
        if pgno == 0 {
            break;
        }
        frames.push((
            pgno,
            &data[offset + WAL_FRAME_HEADER_SIZE..offset + frame_size],
        ));
        if db_size != 0 {
            last_commit = Some((frames.len(), db_size));
        }
        offset += frame_size;
    }
    let Some((count, db_size)) = last_commit else {
        return Ok(());
    };

    let mut db = File::options()
        .write(true)
        .open(db_path)
        .with_context(|| format!("open {}", db_path.display()))?;
    for (pgno, page) in frames.into_iter().take(count) {
        let plain = decrypt_page(page, key, pgno as u64)?;
        db.seek(SeekFrom::Start((pgno as u64 - 1) * PAGE_SIZE as u64))?;
        db.write_all(&plain)?;
    }
    let pages = db_size as u64;
    if pages > 0 {
        let len = pages * PAGE_SIZE as u64;
        if db.metadata()?.len() != len {
            db.set_len(len)?;
        }
    }
    Ok(())
}

/// 测试用：按 SQLCipher 页格式封装一个明文页。
#[cfg(test)]
pub(crate) fn seal_page_for_test(page: &[u8; PAGE_SIZE], key: &[u8; 32], pgno: u64) -> Vec<u8> {
    use aes::cipher::block_padding::NoPadding as EncNoPadding;
    use aes::cipher::{BlockModeEncrypt, KeyIvInit};

    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    let iv = [0x42u8; IV_SIZE];
    let start = if pgno == 1 { SALT_SIZE } else { 0 };
    let mut body = page[start..CIPHERTEXT_END].to_vec();
    let len = body.len();
    let cipher = Aes256CbcEnc::new_from_slices(key, &iv).unwrap();
    let encrypted = cipher
        .encrypt_padded::<EncNoPadding>(&mut body, len)
        .unwrap()
        .to_vec();
    let mut out = vec![0u8; PAGE_SIZE];
    if pgno == 1 {
        out[..SALT_SIZE].copy_from_slice(&[0x11u8; SALT_SIZE]);
        out[SALT_SIZE..CIPHERTEXT_END].copy_from_slice(&encrypted);
    } else {
        out[..CIPHERTEXT_END].copy_from_slice(&encrypted);
    }
    out[IV_OFFSET..IV_OFFSET + IV_SIZE].copy_from_slice(&iv);
    let mac_salt: Vec<u8> = (0..SALT_SIZE).map(|_| 0x11u8 ^ 0x3a).collect();
    let mac_key = pbkdf2_sha512(key, &mac_salt, 2, 32);
    let mut mac = <Hmac<Sha512>>::new_from_slice(&mac_key).unwrap();
    mac.update(&out[SALT_SIZE..HMAC_OFFSET]);
    mac.update(&(pgno as u32).to_le_bytes());
    out[HMAC_OFFSET..].copy_from_slice(&mac.finalize().into_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_and_decrypts_page_one() {
        let key = [7u8; 32];
        let mut plain = [0u8; PAGE_SIZE];
        plain[..16].copy_from_slice(SQLITE_MAGIC);
        plain[16..24].copy_from_slice(b"\x10\x00\x01\x01\x00\x40\x20\x20");
        let sealed = seal_page_for_test(&plain, &key, 1);
        assert!(verify_key(&key, &sealed));

        let decrypted = decrypt_page(&sealed, &key, 1).unwrap();
        assert_eq!(&decrypted[..16], SQLITE_MAGIC);
        assert_eq!(&decrypted[16..24], b"\x10\x00\x01\x01\x00\x40\x20\x20");
    }

    #[test]
    fn rejects_wrong_key() {
        let key = [7u8; 32];
        let mut plain = [0u8; PAGE_SIZE];
        plain[..16].copy_from_slice(SQLITE_MAGIC);
        let sealed = seal_page_for_test(&plain, &key, 1);
        assert!(!verify_key(&[8u8; 32], &sealed));
    }

    #[test]
    fn applies_committed_wal_frames() {
        let key = [7u8; 32];
        let temp = std::env::temp_dir().join(format!("asm-trae-wal-{}", std::process::id()));
        let db_path = temp.with_extension("db");
        let wal_path = temp.with_extension("db-wal");
        let mut db = [0u8; PAGE_SIZE * 2];
        db[..16].copy_from_slice(SQLITE_MAGIC);
        let page1 = {
            let mut page = [0u8; PAGE_SIZE];
            page.copy_from_slice(&db[..PAGE_SIZE]);
            seal_page_for_test(&page, &key, 1)
        };
        let page2 = {
            let mut page = [0u8; PAGE_SIZE];
            page[0] = 9;
            let sealed = seal_page_for_test(&page, &key, 2);
            assert_eq!(decrypt_page(&sealed, &key, 2).unwrap()[0], 9);
            sealed
        };
        fs::write(&db_path, [&page1[..], &[0u8; PAGE_SIZE][..]].concat()).unwrap();

        let mut wal = vec![0u8; WAL_HEADER_SIZE];
        wal[0..4].copy_from_slice(&WAL_MAGIC_LE.to_be_bytes());
        wal[4..8].copy_from_slice(&WAL_VERSION.to_be_bytes());
        wal[8..12].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
        wal[16..20].copy_from_slice(&[1u8; 4]);
        wal[20..24].copy_from_slice(&[2u8; 4]);
        for (pgno, page, db_size) in [(2u32, &page2, 2u32)] {
            let mut header = [0u8; WAL_FRAME_HEADER_SIZE];
            header[0..4].copy_from_slice(&pgno.to_be_bytes());
            header[4..8].copy_from_slice(&db_size.to_be_bytes());
            header[8..12].copy_from_slice(&[1u8; 4]);
            header[12..16].copy_from_slice(&[2u8; 4]);
            wal.extend_from_slice(&header);
            wal.extend_from_slice(page);
        }
        fs::write(&wal_path, &wal).unwrap();

        apply_wal(&db_path, &wal_path, &key).unwrap();
        let result = fs::read(&db_path).unwrap();
        assert_eq!(result.len(), PAGE_SIZE * 2);
        // 解密库中写入的是已解密的页。
        assert_eq!(result[PAGE_SIZE], 9);

        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(&wal_path);
    }
}
