//! Kullanıcı ayarları ve disk konumları.

use crate::search::{builtin_engines, SearchEngine, DEFAULT_ENGINE_ID};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const THEMES: &[&str] = &["system", "light", "dark"];
pub const RENDER_MODES: &[&str] = &["auto", "compat", "fast"];
/// (kimlik, renk)
pub const ACCENTS: &[(&str, &str)] = &[
    ("blue", "#3b8eea"),
    ("orange", "#ff7a2f"),
    ("violet", "#8b6cff"),
    ("green", "#22b58f"),
    ("pink", "#ff4d8d"),
];

pub fn accent_hex(id: &str) -> &'static str {
    ACCENTS.iter().find(|(i, _)| *i == id).map(|(_, c)| *c).unwrap_or(ACCENTS[0].1)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_engine: String,
    pub engines: Vec<SearchEngine>,
    pub homepage: String,
    /// Yerleşik reklam/izleyici engelleme (Linux/WebKitGTK)
    pub block_ads: bool,
    /// Açılışta önceki oturumun sekmelerini geri yükle (varsayılan: kapalı)
    pub restore_session: bool,
    /// "system" | "light" | "dark"
    pub theme: String,
    /// "blue" | "orange" | "violet" | "green" | "pink"
    pub accent: String,
    /// WebKit çizim yolu: "auto" (DMABUF kapalı, önerilen) | "compat" (DMABUF + hızlandırılmış birleştirme kapalı) | "fast" (varsayılanlar)
    pub rendering: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_engine: DEFAULT_ENGINE_ID.into(),
            engines: builtin_engines(),
            homepage: "ozi://newtab".into(),
            block_ads: true,
            restore_session: false,
            theme: "system".into(),
            accent: "blue".into(),
            rendering: "auto".into(),
        }
    }
}

impl Settings {
    pub fn load(path: &std::path::Path) -> Self {
        let mut s: Settings = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if s.engines.is_empty() {
            s.engines = builtin_engines();
        }
        if !s.engines.iter().any(|e| e.id == s.default_engine) {
            s.default_engine = s.engines[0].id.clone();
        }
        if !THEMES.contains(&s.theme.as_str()) {
            s.theme = "system".into();
        }
        if !ACCENTS.iter().any(|(id, _)| *id == s.accent) {
            s.accent = "blue".into();
        }
        if !RENDER_MODES.contains(&s.rendering.as_str()) {
            s.rendering = "auto".into();
        }
        s
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    /// Linux: `~/.config/ozi-browser` ve `~/.local/share/ozi-browser`;
    /// Windows: `%APPDATA%\ozi-browser` ve `%LOCALAPPDATA%\ozi-browser`.
    pub fn detect() -> Self {
        let cfg = dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("ozi-browser");
        let data = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("ozi-browser");
        Self { config: cfg, data }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }
    pub fn window_file(&self) -> PathBuf {
        self.config.join("window.json")
    }
    pub fn session_file(&self) -> PathBuf {
        self.data.join("userdata").join("session.json")
    }
    pub fn extensions_dir(&self) -> PathBuf {
        self.data.join("extensions")
    }
    pub fn storage_dir(&self) -> PathBuf {
        self.data.join("ext-storage")
    }
    /// Geçmiş, yer imleri, indirme kayıtları
    pub fn userdata_dir(&self) -> PathBuf {
        self.data.join("userdata")
    }
    /// WebKit/WebView2 çerez, önbellek ve oturum verisi
    pub fn profile_dir(&self) -> PathBuf {
        self.data.join("profile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_repair() {
        let s = Settings::default();
        assert_eq!(s.default_engine, "duckduckgo");
        assert_eq!(s.engines[1].id, "google");
        let p = std::env::temp_dir().join(format!("ozi-settings-test-{}.json", std::process::id()));
        std::fs::write(&p, br#"{"default_engine":"yok","engines":[]}"#).unwrap();
        let l = Settings::load(&p);
        assert_eq!(l.default_engine, "duckduckgo");
        assert_eq!((l.theme.as_str(), l.accent.as_str()), ("system", "blue"));
        std::fs::write(&p, br#"{"theme":"neon","accent":"nope"}"#).unwrap();
        let l = Settings::load(&p);
        assert_eq!((l.theme.as_str(), l.accent.as_str()), ("system", "blue"), "geçersiz değerler onarılır");
        assert_eq!(accent_hex("orange"), "#ff7a2f");
        assert_eq!(l.rendering, "auto");
        let _ = std::fs::remove_file(p);
    }
}
