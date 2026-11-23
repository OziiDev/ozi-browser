//! Geçmiş, yer imleri ve indirme kayıtları (JSON dosyalarında saklanır).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_HISTORY: usize = 5000;
const MAX_DOWNLOADS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub added: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visits: u32,
    pub last: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub url: String,
    pub path: String,
    /// "downloading" | "done" | "failed"
    pub state: String,
    pub time: u64,
}

#[derive(Default)]
pub struct UserData {
    dir: PathBuf,
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<HistoryEntry>,
    pub downloads: Vec<Download>,
    dirty_history: bool,
}

/// Geçmişe/yer imine yazılmaması gereken iç ve geçici adresler.
pub fn is_recordable(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("file://")
}

impl UserData {
    pub fn load(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let read = |name: &str| std::fs::read(dir.join(name)).ok();
        Self {
            bookmarks: read("bookmarks.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            history: read("history.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            downloads: read("downloads.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            dir,
            dirty_history: false,
        }
    }

    fn write<T: Serialize>(&self, name: &str, v: &T) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let tmp = self.dir.join(format!("{name}.tmp"));
        if let Ok(bytes) = serde_json::to_vec(v) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, self.dir.join(name));
            }
        }
    }

    // ---- yer imleri ----
    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    /// Ekler ya da kaldırır; yeni durumu döndürür (`true` = yer imi var).
    pub fn toggle_bookmark(&mut self, url: &str, title: &str, now: u64) -> bool {
        if !is_recordable(url) {
            return false;
        }
        let now_marked = if let Some(i) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(i);
            false
        } else {
            let title = if title.is_empty() { url } else { title };
            self.bookmarks.insert(0, Bookmark { url: url.into(), title: title.into(), added: now });
            true
        };
        self.write("bookmarks.json", &self.bookmarks);
        now_marked
    }

    pub fn remove_bookmark(&mut self, url: &str) {
        self.bookmarks.retain(|b| b.url != url);
        self.write("bookmarks.json", &self.bookmarks);
    }

    // ---- geçmiş ----
    pub fn add_visit(&mut self, url: &str, title: &str, now: u64) {
        if !is_recordable(url) {
            return;
        }
        if let Some(i) = self.history.iter().position(|h| h.url == url) {
            let mut e = self.history.remove(i);
            e.visits = e.visits.saturating_add(1);
            e.last = now;
            if !title.is_empty() {
                e.title = title.into();
            }
            self.history.insert(0, e);
        } else {
            self.history.insert(
                0,
                HistoryEntry { url: url.into(), title: title.into(), visits: 1, last: now },
            );
            self.history.truncate(MAX_HISTORY);
        }
        self.dirty_history = true;
    }

    /// Sayfa başlığı sonradan gelirse son kaydı güncelle.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(h) = self.history.iter_mut().find(|h| h.url == url) {
            if h.title != title {
                h.title = title.into();
                self.dirty_history = true;
            }
        }
    }

    pub fn remove_history(&mut self, url: &str) {
        self.history.retain(|h| h.url != url);
        self.dirty_history = true;
        self.flush();
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.dirty_history = true;
        self.flush();
    }

    /// Adres çubuğu önerileri: yer imleri önce, sonra geçmiş (ziyaret sayısına göre).
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<(String, String)> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let hit = |url: &str, title: &str| url.to_lowercase().contains(&q) || title.to_lowercase().contains(&q);
        let mut out: Vec<(String, String)> = self
            .bookmarks
            .iter()
            .filter(|b| hit(&b.url, &b.title))
            .map(|b| (b.url.clone(), b.title.clone()))
            .collect();
        let mut h: Vec<&HistoryEntry> = self.history.iter().filter(|h| hit(&h.url, &h.title)).collect();
        h.sort_by(|a, b| b.visits.cmp(&a.visits).then(b.last.cmp(&a.last)));
        for e in h {
            if !out.iter().any(|(u, _)| u == &e.url) {
                out.push((e.url.clone(), e.title.clone()));
            }
        }
        out.truncate(limit);
        out
    }

    /// Bekleyen geçmiş değişikliklerini diske yazar.
    pub fn flush(&mut self) {
        if self.dirty_history {
            self.write("history.json", &self.history);
            self.dirty_history = false;
        }
    }

    // ---- indirmeler ----
    pub fn download_started(&mut self, url: &str, path: &str, now: u64) {
        self.downloads.insert(
            0,
            Download { url: url.into(), path: path.into(), state: "downloading".into(), time: now },
        );
        self.downloads.truncate(MAX_DOWNLOADS);
        self.write("downloads.json", &self.downloads);
    }

    pub fn download_finished(&mut self, url: &str, path: Option<&str>, ok: bool) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.url == url && d.state == "downloading") {
            d.state = if ok { "done" } else { "failed" }.into();
            if let Some(p) = path {
                d.path = p.into();
            }
        }
        self.write("downloads.json", &self.downloads);
    }

    pub fn clear_downloads(&mut self) {
        self.downloads.clear();
        self.write("downloads.json", &self.downloads);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ozi-userdata-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn bookmarks_toggle_and_persist() {
        let d = tmp();
        let mut u = UserData::load(&d);
        assert!(u.toggle_bookmark("https://a.com/", "A", 1));
        assert!(u.is_bookmarked("https://a.com/"));
        assert!(!u.toggle_bookmark("ozi://newtab", "x", 2), "iç sayfa yer imi olmaz");
        assert_eq!(UserData::load(&d).bookmarks.len(), 1);
        assert!(!u.toggle_bookmark("https://a.com/", "A", 3));
        assert!(UserData::load(&d).bookmarks.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn history_dedupes_counts_and_flushes() {
        let d = tmp();
        let mut u = UserData::load(&d);
        u.add_visit("https://a.com/", "A", 1);
        u.add_visit("https://b.com/", "B", 2);
        u.add_visit("https://a.com/", "A2", 3);
        u.add_visit("ozi://newtab", "", 4);
        u.add_visit("about:blank", "", 5);
        assert_eq!(u.history.len(), 2);
        assert_eq!(u.history[0].url, "https://a.com/");
        assert_eq!(u.history[0].visits, 2);
        assert_eq!(u.history[0].title, "A2");
        assert!(UserData::load(&d).history.is_empty(), "flush'tan önce diske yazılmaz");
        u.flush();
        assert_eq!(UserData::load(&d).history.len(), 2);
        u.remove_history("https://b.com/");
        assert_eq!(UserData::load(&d).history.len(), 1);
        u.clear_history();
        assert!(UserData::load(&d).history.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn suggestions_rank_bookmarks_first() {
        let mut u = UserData::default();
        u.add_visit("https://rust-lang.org/", "Rust", 1);
        u.add_visit("https://rust-lang.org/", "Rust", 2);
        u.add_visit("https://crates.io/", "Crates for Rust", 3);
        u.bookmarks.push(Bookmark { url: "https://doc.rust-lang.org/".into(), title: "Docs".into(), added: 0 });
        let s = u.suggest("rust", 5);
        assert_eq!(s[0].0, "https://doc.rust-lang.org/");
        assert_eq!(s[1].0, "https://rust-lang.org/");
        assert_eq!(s.len(), 3);
        assert!(u.suggest("  ", 5).is_empty());
    }

    #[test]
    fn downloads_lifecycle() {
        let d = tmp();
        let mut u = UserData::load(&d);
        u.download_started("https://x/f.zip", "/tmp/f.zip", 1);
        u.download_finished("https://x/f.zip", Some("/tmp/f.zip"), true);
        assert_eq!(UserData::load(&d).downloads[0].state, "done");
        u.download_started("https://x/g.zip", "/tmp/g.zip", 2);
        u.download_finished("https://x/g.zip", None, false);
        assert_eq!(u.downloads[0].state, "failed");
        let _ = std::fs::remove_dir_all(&d);
    }
}
