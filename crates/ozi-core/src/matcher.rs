//! Chrome/Firefox "match pattern" (`*://*.example.com/*`, `<all_urls>`) ve glob eşleştirme.

use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostPat {
    Any,
    /// `*.example.com` -> example.com ve tüm alt alan adları
    Suffix(String),
    Exact(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchPattern {
    all_urls: bool,
    /// `None` = `*` (yalnızca http/https/ws/wss)
    scheme: Option<String>,
    host: HostPat,
    path: String,
}

impl MatchPattern {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s == "<all_urls>" {
            return Some(Self {
                all_urls: true,
                scheme: None,
                host: HostPat::Any,
                path: "*".into(),
            });
        }
        let (scheme, rest) = s.split_once("://")?;
        let scheme = if scheme == "*" {
            None
        } else if scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            Some(scheme.to_ascii_lowercase())
        } else {
            return None;
        };
        let slash = rest.find('/')?;
        let (host, path) = rest.split_at(slash);
        let host = host.to_ascii_lowercase();
        let host = if scheme.as_deref() == Some("file") && host.is_empty() {
            HostPat::Any
        } else if host == "*" {
            HostPat::Any
        } else if let Some(sfx) = host.strip_prefix("*.") {
            if sfx.is_empty() || sfx.contains('*') {
                return None;
            }
            HostPat::Suffix(sfx.to_string())
        } else if host.is_empty() || host.contains('*') {
            return None;
        } else {
            // Port, desenlerde yok sayılır.
            HostPat::Exact(strip_port(&host).to_string())
        };
        Some(Self {
            all_urls: false,
            scheme,
            host,
            path: path.to_string(),
        })
    }

    pub fn matches(&self, url: &Url) -> bool {
        let scheme = url.scheme();
        if self.all_urls {
            return matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file");
        }
        match &self.scheme {
            None if !matches!(scheme, "http" | "https" | "ws" | "wss") => return false,
            Some(s) if s != scheme => return false,
            _ => {}
        }
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let ok = match &self.host {
            HostPat::Any => true,
            HostPat::Exact(h) => &host == h,
            HostPat::Suffix(sfx) => host == *sfx || host.ends_with(&format!(".{sfx}")),
        };
        if !ok {
            return false;
        }
        let mut target = url.path().to_string();
        if let Some(q) = url.query() {
            target.push('?');
            target.push_str(q);
        }
        glob_match(&self.path, &target)
    }
}

fn strip_port(h: &str) -> &str {
    match h.rsplit_once(':') {
        Some((a, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => a,
        _ => h,
    }
}

/// `*` (her şey) ve `?` (tek karakter) destekleyen glob eşleştirme.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Bir içerik betiği girdisinin (matches / exclude_matches / include_globs / exclude_globs)
/// verilen URL'ye uygulanıp uygulanmadığı.
pub fn script_applies(
    matches: &[String],
    exclude_matches: &[String],
    include_globs: &[String],
    exclude_globs: &[String],
    url: &Url,
) -> bool {
    let any = |list: &[String]| {
        list.iter()
            .filter_map(|p| MatchPattern::parse(p))
            .any(|p| p.matches(url))
    };
    if !any(matches) || any(exclude_matches) {
        return false;
    }
    let s = url.as_str();
    if !include_globs.is_empty() && !include_globs.iter().any(|g| glob_match(g, s)) {
        return false;
    }
    !exclude_globs.iter().any(|g| glob_match(g, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, u: &str) -> bool {
        MatchPattern::parse(p).unwrap().matches(&Url::parse(u).unwrap())
    }

    #[test]
    fn basic_patterns() {
        assert!(m("<all_urls>", "https://example.com/a"));
        assert!(!m("<all_urls>", "about:blank"));
        assert!(m("*://*.example.com/*", "https://a.b.example.com/x?y=1"));
        assert!(m("*://*.example.com/*", "http://example.com/"));
        assert!(!m("*://*.example.com/*", "https://notexample.com/"));
        assert!(!m("https://example.com/*", "http://example.com/"));
        assert!(m("https://example.com/foo*", "https://example.com/foobar?q=1"));
        assert!(!m("https://example.com/foo", "https://example.com/foobar"));
        assert!(m("*://localhost/*", "http://localhost:8080/x"));
        assert!(m("file:///*", "file:///home/a.txt"));
        assert!(!m("*://*/*", "file:///home/a.txt"));
    }

    #[test]
    fn invalid_patterns() {
        assert!(MatchPattern::parse("example.com").is_none());
        assert!(MatchPattern::parse("https://ex*mple.com/*").is_none());
        assert!(MatchPattern::parse("https://example.com").is_none());
    }

    #[test]
    fn globs() {
        assert!(glob_match("a*c", "abbbc"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn script_rules() {
        let u = Url::parse("https://example.com/private/x").unwrap();
        let ms = vec!["https://example.com/*".to_string()];
        assert!(script_applies(&ms, &[], &[], &[], &u));
        assert!(!script_applies(&ms, &["https://example.com/private/*".to_string()], &[], &[], &u));
        assert!(!script_applies(&ms, &[], &[], &["*private*".to_string()], &u));
    }
}
