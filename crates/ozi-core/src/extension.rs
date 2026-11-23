//! Diske açılmış bir WebExtension: manifest, güvenli dosya okuma ve `_locales` desteği.

use crate::manifest::Manifest;
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error("G/Ç hatası: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest.json okunamadı: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("arşiv hatası: {0}")]
    Zip(#[from] zip::result::ZipError),
}

#[derive(Clone, Debug)]
pub struct Extension {
    /// URL'de kullanılan güvenli kimlik (`[a-p]{32}`).
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub enabled: bool,
    /// `chrome.i18n.getMessage` için küçük harfli anahtar -> `{message, placeholders}`
    pub messages: Map<String, Value>,
}

impl Extension {
    pub fn load(dir: &Path, id: &str, enabled: bool) -> Result<Self, ExtError> {
        let bytes = std::fs::read(dir.join("manifest.json"))?;
        let manifest = Manifest::parse(&bytes)?;
        if !(2..=3).contains(&manifest.manifest_version) {
            return Err(ExtError::Invalid(format!(
                "desteklenmeyen manifest_version: {}",
                manifest.manifest_version
            )));
        }
        let messages = load_messages(dir, manifest.default_locale.as_deref());
        Ok(Self {
            id: id.to_string(),
            dir: dir.to_path_buf(),
            manifest,
            enabled,
            messages,
        })
    }

    /// `runtime.id`: Firefox uzantılarında gecko kimliği, aksi halde iç kimlik.
    pub fn public_id(&self) -> String {
        self.manifest.gecko_id().map(str::to_string).unwrap_or_else(|| self.id.clone())
    }

    pub fn display_name(&self) -> String {
        self.resolve_msg(&self.manifest.name)
    }

    pub fn display_description(&self) -> String {
        self.resolve_msg(&self.manifest.description)
    }

    /// `__MSG_anahtar__` -> yerelleştirilmiş metin.
    pub fn resolve_msg(&self, s: &str) -> String {
        if let Some(key) = s.strip_prefix("__MSG_").and_then(|k| k.strip_suffix("__")) {
            if let Some(m) = self
                .messages
                .get(&key.to_ascii_lowercase())
                .and_then(|v| v.get("message"))
                .and_then(Value::as_str)
            {
                return m.to_string();
            }
        }
        s.to_string()
    }

    /// Uzantı klasörü içinden güvenli dosya okuma (`..`, mutlak yol ve sembolik bağ kaçışı reddedilir).
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        let path = self.safe_path(rel)?;
        std::fs::read(path).ok()
    }

    pub fn read_string(&self, rel: &str) -> Option<String> {
        self.read(rel).and_then(|b| String::from_utf8(b).ok())
    }

    pub fn safe_path(&self, rel: &str) -> Option<PathBuf> {
        let rel = rel.trim_start_matches('/');
        let rel_path = Path::new(rel);
        if rel.is_empty()
            || rel_path
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        let full = self.dir.join(rel_path).canonicalize().ok()?;
        let root = self.dir.canonicalize().ok()?;
        full.starts_with(&root).then_some(full)
    }
}

fn locale_candidates(default_locale: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let env = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let base = env.split('.').next().unwrap_or("");
    if !base.is_empty() && base != "C" && base != "POSIX" {
        out.push(base.to_string());
        if let Some((lang, _)) = base.split_once('_') {
            out.push(lang.to_string());
        }
    }
    out.push("en".into());
    out.push("en_US".into());
    if let Some(d) = default_locale {
        out.push(d.to_string());
    }
    out
}

fn load_messages(dir: &Path, default_locale: Option<&str>) -> Map<String, Value> {
    let mut merged = Map::new();
    // Öncelik: kullanıcı dili ... en sonda default_locale. Önce gelen kazanır.
    for loc in locale_candidates(default_locale) {
        if loc.contains(['/', '\\']) || loc.contains("..") {
            continue;
        }
        let p = dir.join("_locales").join(&loc).join("messages.json");
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(bytes) else { continue };
        for (k, v) in m {
            merged.entry(k.to_ascii_lowercase()).or_insert(v);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ozi-core-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn safe_path_blocks_escape() {
        let d = tmp("safe");
        std::fs::write(d.join("manifest.json"), br#"{"manifest_version":3,"name":"__MSG_n__","version":"1","default_locale":"en"}"#).unwrap();
        std::fs::create_dir_all(d.join("_locales/en")).unwrap();
        std::fs::write(d.join("_locales/en/messages.json"), br#"{"N":{"message":"Merhaba"}}"#).unwrap();
        std::fs::write(d.join("a.js"), b"1").unwrap();
        std::fs::write(d.parent().unwrap().join("secret.txt"), b"s").unwrap();
        let e = Extension::load(&d, "x", true).unwrap();
        assert_eq!(e.display_name(), "Merhaba");
        assert!(e.read("a.js").is_some());
        assert!(e.read("/a.js").is_some());
        assert!(e.read("../secret.txt").is_none());
        assert!(e.read("a/../a.js").is_none());
        assert!(e.read("").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
