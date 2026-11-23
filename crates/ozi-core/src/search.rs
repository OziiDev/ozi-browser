//! Arama motorları ve adres çubuğu (omnibox) çözümleme.
//!
//! Sıralama bilinçli: DuckDuckGo varsayılan, Google ikinci.

use serde::{Deserialize, Serialize};
use url::form_urlencoded;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchEngine {
    pub id: String,
    pub name: String,
    /// Adres çubuğunda `anahtar sorgu` şeklinde kullanılan kısa ad.
    pub keyword: String,
    /// `%s` yer tutucusu sorgu ile değiştirilir.
    pub url: String,
}

impl SearchEngine {
    pub fn search_url(&self, query: &str) -> String {
        let q: String = form_urlencoded::byte_serialize(query.as_bytes()).collect();
        self.url.replace("%s", &q)
    }
}

pub const DEFAULT_ENGINE_ID: &str = "duckduckgo";

pub fn builtin_engines() -> Vec<SearchEngine> {
    let e = |id: &str, name: &str, kw: &str, url: &str| SearchEngine {
        id: id.into(),
        name: name.into(),
        keyword: kw.into(),
        url: url.into(),
    };
    vec![
        e("duckduckgo", "DuckDuckGo", "d", "https://duckduckgo.com/?q=%s"),
        e("google", "Google", "g", "https://www.google.com/search?q=%s"),
        e("brave", "Brave Search", "b", "https://search.brave.com/search?q=%s"),
        e("bing", "Bing", "bing", "https://www.bing.com/search?q=%s"),
    ]
}

/// Adres çubuğuna yazılan metni gidilecek URL'ye çevirir.
pub fn resolve(input: &str, engines: &[SearchEngine], default_id: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return "ozi://newtab".into();
    }
    if has_scheme(s) || s.starts_with("about:") {
        return s.to_string();
    }
    // "g rust lang" -> Google'da ara
    if let Some((kw, rest)) = s.split_once(char::is_whitespace) {
        let rest = rest.trim();
        if !rest.is_empty() {
            if let Some(e) = engines.iter().find(|e| e.keyword.eq_ignore_ascii_case(kw)) {
                return e.search_url(rest);
            }
        }
    }
    if let Some(url) = as_host_url(s) {
        return url;
    }
    let engine = engines
        .iter()
        .find(|e| e.id == default_id)
        .or_else(|| engines.first());
    match engine {
        Some(e) => e.search_url(s),
        None => s.to_string(),
    }
}

fn has_scheme(s: &str) -> bool {
    match s.find("://") {
        Some(i) if i > 0 => {
            let scheme = &s[..i];
            scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        _ => false,
    }
}

fn as_host_url(s: &str) -> Option<String> {
    if s.chars().any(char::is_whitespace) {
        return None;
    }
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    let authority = &s[..end];
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6: [::1]:8080
        let close = rest.find(']')?;
        return Some(format!("http://{s}")).filter(|_| rest[..close].contains(':'));
    } else {
        match authority.rsplit_once(':') {
            Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
            Some(_) => return None,
            None => authority,
        }
    };
    if host.is_empty() {
        return None;
    }
    if host.eq_ignore_ascii_case("localhost") || is_ipv4(host) {
        return Some(format!("http://{s}"));
    }
    if is_domain(host) {
        return Some(format!("https://{s}"));
    }
    None
}

fn is_ipv4(h: &str) -> bool {
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

fn is_domain(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let ok_label = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    let tld = labels[labels.len() - 1];
    // Gerçek TLD olmayan yaygın dosya uzantıları: "notlar.txt" arama olsun, site değil.
    const FILE_EXT: &[&str] = &[
        "txt", "json", "png", "jpg", "jpeg", "gif", "webp", "svg", "pdf", "zip", "exe", "log", "csv",
        "toml", "yaml", "yml", "css", "html", "htm", "js", "mp3", "mp4", "doc", "docx", "xlsx",
    ];
    if FILE_EXT.iter().any(|e| tld.eq_ignore_ascii_case(e)) {
        return false;
    }
    labels.iter().all(|l| ok_label(l)) && tld.len() >= 2 && tld.chars().all(char::is_alphabetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        resolve(s, &builtin_engines(), DEFAULT_ENGINE_ID)
    }

    #[test]
    fn duckduckgo_is_default_google_second() {
        let e = builtin_engines();
        assert_eq!(e[0].id, "duckduckgo");
        assert_eq!(e[1].id, "google");
        assert_eq!(r("rust dili"), "https://duckduckgo.com/?q=rust+dili");
    }

    #[test]
    fn google_keyword() {
        assert_eq!(r("g rust lang"), "https://www.google.com/search?q=rust+lang");
    }

    #[test]
    fn hosts_and_urls() {
        assert_eq!(r("github.com/OziiDev"), "https://github.com/OziiDev");
        assert_eq!(r("localhost:3000"), "http://localhost:3000");
        assert_eq!(r("192.168.1.1"), "http://192.168.1.1");
        assert_eq!(r("https://a.b/c d"), "https://a.b/c d");
        assert_eq!(r("[::1]:8080"), "http://[::1]:8080");
        assert_eq!(r(""), "ozi://newtab");
        assert_eq!(r("ozi://extensions"), "ozi://extensions");
    }

    #[test]
    fn plain_words_search() {
        assert_eq!(r("merhaba"), "https://duckduckgo.com/?q=merhaba");
        assert_eq!(r("version 1.2"), "https://duckduckgo.com/?q=version+1.2");
        assert_eq!(r("file.txt"), "https://duckduckgo.com/?q=file.txt");
    }
}
