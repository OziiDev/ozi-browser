//! Yüklü uzantıların kurulumu: klasör, `.zip`, Firefox `.xpi` ve Chrome `.crx`.

use crate::extension::{ExtError, Extension};
use crate::manifest::Manifest;
use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const MAX_UNPACKED: u64 = 1 << 30; // 1 GiB (zip bombasına karşı)
const MAX_ENTRIES: usize = 50_000;

pub struct ExtensionStore {
    root: PathBuf,
}

impl ExtensionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    fn disabled(&self) -> BTreeSet<String> {
        std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<BTreeSet<String>>(&b).ok())
            .unwrap_or_default()
    }

    fn save_disabled(&self, set: &BTreeSet<String>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.state_path(), serde_json::to_vec(set)?)
    }

    pub fn load_all(&self) -> Vec<Extension> {
        let disabled = self.disabled();
        let Ok(rd) = std::fs::read_dir(&self.root) else { return vec![] };
        let mut out: Vec<Extension> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let id = e.file_name().to_string_lossy().to_string();
                Extension::load(&e.path(), &id, !disabled.contains(&id)).ok()
            })
            .collect();
        out.sort_by(|a, b| a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()));
        out
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> std::io::Result<()> {
        let mut d = self.disabled();
        if enabled {
            d.remove(id);
        } else {
            d.insert(id.to_string());
        }
        self.save_disabled(&d)
    }

    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        if !is_valid_id(id) {
            return Ok(());
        }
        let p = self.root.join(id);
        if p.is_dir() {
            std::fs::remove_dir_all(p)?;
        }
        self.set_enabled(id, true) // devre dışı kaydını temizle
    }

    /// Klasör ya da paket (`.zip`/`.xpi`/`.crx`) kurar; aynı uzantı varsa üzerine yazar.
    pub fn install(&self, src: &Path) -> Result<Extension, ExtError> {
        std::fs::create_dir_all(&self.root)?;
        let staging = self.root.join(format!(".staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        let result = (|| {
            if src.is_dir() {
                copy_dir(src, &staging, 0)?;
            } else {
                let bytes = std::fs::read(src)?;
                extract_package(&bytes, &staging)?;
            }
            let mbytes = std::fs::read(staging.join("manifest.json"))
                .map_err(|_| ExtError::Invalid("pakette manifest.json yok".into()))?;
            let manifest = Manifest::parse(&mbytes)?;
            let id = derive_id(&manifest);
            let dest = self.root.join(&id);
            if dest.exists() {
                std::fs::remove_dir_all(&dest)?;
            }
            std::fs::rename(&staging, &dest)?;
            Extension::load(&dest, &id, true)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        let ext = result?;
        self.set_enabled(&ext.id, true)?;
        Ok(ext)
    }
}

pub fn is_valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Kararlı, URL-güvenli kimlik: gecko kimliği varsa ondan, yoksa uzantı adından (FNV-1a 128).
pub fn derive_id(m: &Manifest) -> String {
    let seed = m.gecko_id().map(str::to_string).unwrap_or_else(|| format!("name:{}", m.name));
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for b in seed.bytes() {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000000001000000000000000000013B);
    }
    (0..32)
        .map(|i| (b'a' + ((h >> (4 * i)) & 0xf) as u8) as char)
        .collect()
}

fn copy_dir(from: &Path, to: &Path, depth: u32) -> std::io::Result<()> {
    if depth > 32 {
        return Err(std::io::Error::other("klasör çok derin"));
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &target, depth + 1)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), target)?;
        } // sembolik bağlar bilerek atlanır
    }
    Ok(())
}

fn extract_package(bytes: &[u8], dest: &Path) -> Result<(), ExtError> {
    let zip_bytes = strip_crx_header(bytes)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes))?;
    if zip.len() > MAX_ENTRIES {
        return Err(ExtError::Invalid("arşivde çok fazla dosya var".into()));
    }
    std::fs::create_dir_all(dest)?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue }; // zip-slip: güvenli olmayan yolları atla
        if f.is_symlink() {
            continue;
        }
        let out = dest.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        total = total.saturating_add(f.size());
        if total > MAX_UNPACKED {
            return Err(ExtError::Invalid("arşiv çok büyük".into()));
        }
        // Başlıktaki boyuta güvenme; gerçek okunan miktarı sınırla.
        let mut limited = (&mut f).take(MAX_UNPACKED);
        let mut file = std::fs::File::create(out)?;
        std::io::copy(&mut limited, &mut file)?;
    }
    Ok(())
}

/// Chrome `.crx` (v2/v3) başlığını atar; düz zip/xpi'yi olduğu gibi döndürür.
fn strip_crx_header(b: &[u8]) -> Result<&[u8], ExtError> {
    if !b.starts_with(b"Cr24") {
        return Ok(b);
    }
    let u32le = |o: usize| -> Option<usize> {
        b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
    };
    let bad = || ExtError::Invalid("bozuk .crx başlığı".into());
    let start = match u32le(4).ok_or_else(bad)? {
        2 => 16usize
            .checked_add(u32le(8).ok_or_else(bad)?)
            .and_then(|x| x.checked_add(u32le(12)?))
            .ok_or_else(bad)?,
        3 => 12usize.checked_add(u32le(8).ok_or_else(bad)?).ok_or_else(bad)?,
        _ => return Err(ExtError::Invalid("desteklenmeyen .crx sürümü".into())),
    };
    b.get(start..).filter(|r| r.starts_with(b"PK")).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ozi-store-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        for (n, d) in entries {
            w.start_file(*n, opts).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
        buf.into_inner()
    }

    const MANIFEST: &[u8] = br#"{"manifest_version":2,"name":"T","version":"1","applications":{"gecko":{"id":"t@x"}}}"#;

    #[test]
    fn installs_xpi_and_crx_and_blocks_zip_slip() {
        let root = tmp("root");
        let store = ExtensionStore::new(root.join("exts"));
        let z = make_zip(&[("manifest.json", MANIFEST), ("../evil.txt", b"x"), ("sub/a.js", b"1")]);
        let xpi = root.join("t.xpi");
        std::fs::write(&xpi, &z).unwrap();
        let ext = store.install(&xpi).unwrap();
        assert!(is_valid_id(&ext.id));
        assert!(ext.read("sub/a.js").is_some());
        assert!(!root.join("evil.txt").exists());
        assert!(!root.join("exts/evil.txt").exists());

        // crx v3: magic, ver=3, header_len=4, 4 bayt başlık, sonra zip
        let mut crx = b"Cr24".to_vec();
        crx.extend_from_slice(&3u32.to_le_bytes());
        crx.extend_from_slice(&4u32.to_le_bytes());
        crx.extend_from_slice(&[0, 0, 0, 0]);
        crx.extend_from_slice(&z);
        let p = root.join("t.crx");
        std::fs::write(&p, crx).unwrap();
        let again = store.install(&p).unwrap();
        assert_eq!(again.id, ext.id);

        store.set_enabled(&ext.id, false).unwrap();
        assert!(!store.load_all()[0].enabled);
        store.remove(&ext.id).unwrap();
        assert!(store.load_all().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installs_directory_and_rejects_missing_manifest() {
        let root = tmp("dir");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let store = ExtensionStore::new(root.join("exts"));
        assert!(store.install(&src).is_err());
        std::fs::write(src.join("manifest.json"), MANIFEST).unwrap();
        assert!(store.install(&src).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}
