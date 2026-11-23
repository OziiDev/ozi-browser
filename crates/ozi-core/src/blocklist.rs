//! Reklam/izleyici engelleme kuralları.
//!
//! WebKit "content rule list" biçimini üretir: yerleşik alan adı listesi + uzantıların
//! `declarativeNetRequest` statik kural dosyalarının çevirisi. (Windows/WebView2'de bu
//! biçim kullanılmaz; orada engelleme henüz yoktur.)

use serde_json::{json, Value};

/// Üçüncü taraf olarak yüklendiğinde engellenen yaygın reklam/izleme alan adları.
pub const BUILTIN_DOMAINS: &[&str] = &[
    "doubleclick.net", "googlesyndication.com", "googleadservices.com", "adservice.google.com",
    "google-analytics.com", "2mdn.net", "adnxs.com", "adsrvr.org", "advertising.com",
    "amazon-adsystem.com", "criteo.com", "criteo.net", "taboola.com", "outbrain.com",
    "scorecardresearch.com", "quantserve.com", "quantcount.com", "hotjar.com", "hotjar.io",
    "rubiconproject.com", "pubmatic.com", "openx.net", "moatads.com", "yieldmo.com",
    "bidswitch.net", "casalemedia.com", "smartadserver.com", "adform.net", "adition.com",
    "contextweb.com", "lijit.com", "sharethrough.com", "media.net", "revcontent.com",
    "mgid.com", "zedo.com", "serving-sys.com", "turn.com", "mathtag.com", "bluekai.com",
    "krxd.net", "demdex.net", "everesttech.net", "agkn.com", "rlcdn.com", "tapad.com",
    "chartbeat.net", "newrelic-bam.example.invalid", "mxpnl.com", "segment.io", "fullstory.com",
    "mouseflow.com", "crazyegg.com", "inspectlet.com", "popads.net", "popcash.net",
    "propellerads.com", "exoclick.com", "juicyads.com", "trafficjunky.net", "adcolony.com",
    "applovin.com", "unityads.unity3d.com", "ads.twitter.com", "ads.linkedin.com",
    "analytics.tiktok.com", "ads.yahoo.com", "adsymptotic.com", "yandexadexchange.net",
    "an.yandex.ru", "mc.yandex.ru", "ad.doubleclick.net", "pagead2.googlesyndication.com",
];

const ALL_TYPES: &[&str] = &[
    "image", "style-sheet", "script", "font", "media", "svg-document", "fetch", "websocket",
    "ping", "other", "popup",
];

fn escape_re(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if ".+?()[]{}\\^$|*".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Yerleşik liste: her alan adı için "üçüncü taraf istek -> engelle" kuralı.
pub fn builtin_rules() -> Vec<Value> {
    BUILTIN_DOMAINS
        .iter()
        .filter(|d| !d.ends_with(".invalid"))
        .map(|d| {
            json!({
                "trigger": {
                    "url-filter": format!("^[^:]+://+([^:/]+\\.)?{}[:/]", escape_re(d)),
                    "resource-type": ALL_TYPES,
                    "load-type": ["third-party"],
                },
                "action": { "type": "block" },
            })
        })
        .collect()
}

/// DNR `urlFilter` söz dizimini (`||`, `|`, `^`, `*`) WebKit regex'ine çevirir.
pub fn url_filter_to_regex(f: &str) -> String {
    let mut rest = f;
    let mut re = String::new();
    if let Some(r) = rest.strip_prefix("||") {
        re.push_str("^[^:]+://+([^:/]+\\.)?");
        rest = r;
    } else if let Some(r) = rest.strip_prefix('|') {
        re.push('^');
        rest = r;
    }
    let (body, anchored_end) = match rest.strip_suffix('|') {
        Some(b) => (b, true),
        None => (rest, false),
    };
    for c in body.chars() {
        match c {
            '*' => re.push_str(".*"),
            '^' => re.push_str("[^a-z0-9_.%-]"),
            c => {
                if ".+?()[]{}\\$|".contains(c) {
                    re.push('\\');
                }
                re.push(c);
            }
        }
    }
    if anchored_end {
        re.push('$');
    }
    if re.is_empty() {
        ".*".into()
    } else {
        re
    }
}

fn map_types(list: &[Value]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for t in list.iter().filter_map(Value::as_str) {
        let m: &[&'static str] = match t {
            "main_frame" | "sub_frame" => &["document"],
            "stylesheet" => &["style-sheet"],
            "script" => &["script"],
            "image" => &["image"],
            "font" => &["font"],
            "xmlhttprequest" => &["fetch"],
            "ping" => &["ping"],
            "media" => &["media"],
            "websocket" => &["websocket"],
            _ => &["other"],
        };
        for x in m {
            if !out.contains(x) {
                out.push(x);
            }
        }
    }
    out
}

fn domains(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(|d| format!("*{}", d.to_ascii_lowercase())).collect())
        .unwrap_or_default()
}

/// Tek bir DNR kuralını çevirir; faithful çevrilemiyorsa `None` (yanlış engellemektense atla).
fn convert_rule(r: &Value) -> Option<(i64, bool, Value)> {
    let action = r["action"]["type"].as_str()?;
    let (kind, is_allow) = match action {
        "block" => ("block", false),
        "allow" | "allowAllRequests" => ("ignore-previous-rules", true),
        "upgradeScheme" => ("make-https", false),
        _ => return None, // redirect, modifyHeaders: WebKit eşdeğeri yok
    };
    let c = &r["condition"];
    if c.get("requestDomains").is_some() || c.get("excludedRequestDomains").is_some() || c.get("tabIds").is_some() {
        return None;
    }
    let filter = if let Some(f) = c["urlFilter"].as_str() {
        url_filter_to_regex(f)
    } else if let Some(re) = c["regexFilter"].as_str() {
        // Yalnızca WebKit'in desteklediği basit alt küme (alternasyon/küme sayacı yok)
        if re.contains(['|', '{', '}']) || re.contains("(?") {
            return None;
        }
        re.to_string()
    } else {
        ".*".into()
    };
    let mut trigger = json!({ "url-filter": filter });
    if c["isUrlFilterCaseSensitive"].as_bool() == Some(true) {
        trigger["url-filter-is-case-sensitive"] = json!(true);
    }
    let types: Vec<&'static str> = match c["resourceTypes"].as_array() {
        Some(a) => map_types(a),
        None => ALL_TYPES.iter().copied().filter(|t| *t != "popup").collect(),
    };
    let excluded: Vec<&'static str> = c["excludedResourceTypes"].as_array().map(|a| map_types(a)).unwrap_or_default();
    let types: Vec<&str> = types.into_iter().filter(|t| !excluded.contains(t)).collect();
    if types.is_empty() {
        return None;
    }
    trigger["resource-type"] = json!(types);
    match c["domainType"].as_str() {
        Some("thirdParty") => trigger["load-type"] = json!(["third-party"]),
        Some("firstParty") => trigger["load-type"] = json!(["first-party"]),
        _ => {}
    }
    let include = domains(c.get("initiatorDomains").or_else(|| c.get("domains")).unwrap_or(&Value::Null));
    let exclude = domains(c.get("excludedInitiatorDomains").or_else(|| c.get("excludedDomains")).unwrap_or(&Value::Null));
    match (include.is_empty(), exclude.is_empty()) {
        (false, false) => return None,
        (false, true) => trigger["if-domain"] = json!(include),
        (true, false) => trigger["unless-domain"] = json!(exclude),
        _ => {}
    }
    let priority = r["priority"].as_i64().unwrap_or(1);
    Some((priority, is_allow, json!({ "trigger": trigger, "action": { "type": kind } })))
}

/// Bir DNR kural dosyasını (JSON dizi) WebKit kurallarına çevirir.
/// Sıra: öncelik artan; aynı öncelikte `allow` sonda (DNR'de allow kazanır).
pub fn dnr_to_webkit(rules: &Value) -> Vec<Value> {
    let mut v: Vec<(i64, bool, Value)> = rules.as_array().map(|a| a.iter().filter_map(convert_rule).collect()).unwrap_or_default();
    v.sort_by_key(|(p, allow, _)| (*p, *allow));
    v.into_iter().map(|(_, _, r)| r).collect()
}

/// Birleşik filtre JSON'u. Boşsa `None` (filtre kurmaya gerek yok).
pub fn build_filter(builtin: bool, rulesets: &[Value]) -> Option<String> {
    const MAX_RULES: usize = 100_000;
    let mut all: Vec<Value> = if builtin { builtin_rules() } else { vec![] };
    for rs in rulesets {
        all.extend(dnr_to_webkit(rs));
    }
    all.truncate(MAX_RULES);
    if all.is_empty() {
        None
    } else {
        serde_json::to_string(&all).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_filter_syntax() {
        assert_eq!(url_filter_to_regex("||ads.example.com^"), "^[^:]+://+([^:/]+\\.)?ads\\.example\\.com[^a-z0-9_.%-]");
        assert_eq!(url_filter_to_regex("|https://x.com/a*b|"), "^https://x\\.com/a.*b$");
        assert_eq!(url_filter_to_regex("/banner?id="), "/banner\\?id=");
        assert_eq!(url_filter_to_regex(""), ".*");
    }

    #[test]
    fn builtin_has_rules_with_third_party() {
        let r = builtin_rules();
        assert!(r.len() > 50);
        assert_eq!(r[0]["trigger"]["load-type"][0], "third-party");
        assert!(r.iter().all(|x| !x["trigger"]["url-filter"].as_str().unwrap().contains(".invalid")));
    }

    #[test]
    fn dnr_conversion_and_ordering() {
        let rules = json!([
            {"id":1,"priority":2,"action":{"type":"allow"},"condition":{"urlFilter":"||ok.com^"}},
            {"id":2,"priority":1,"action":{"type":"block"},"condition":{"urlFilter":"||ads.com^","resourceTypes":["script","xmlhttprequest","main_frame"],"domainType":"thirdParty","excludedInitiatorDomains":["Good.com"]}},
            {"id":3,"priority":1,"action":{"type":"redirect","redirect":{"url":"x"}},"condition":{"urlFilter":"a"}},
            {"id":4,"priority":1,"action":{"type":"block"},"condition":{"regexFilter":"a|b"}},
            {"id":5,"priority":1,"action":{"type":"block"},"condition":{"urlFilter":"x","initiatorDomains":["a.com"],"excludedInitiatorDomains":["b.com"]}},
            {"id":6,"priority":1,"action":{"type":"block"},"condition":{"urlFilter":"x","requestDomains":["a.com"]}},
        ]);
        let out = dnr_to_webkit(&rules);
        assert_eq!(out.len(), 2, "redirect, alternasyonlu regex, çelişkili alan ve requestDomains atlanır");
        assert_eq!(out[0]["action"]["type"], "block");
        assert_eq!(out[0]["trigger"]["resource-type"], json!(["script", "fetch", "document"]));
        assert_eq!(out[0]["trigger"]["load-type"], json!(["third-party"]));
        assert_eq!(out[0]["trigger"]["unless-domain"], json!(["*good.com"]));
        assert_eq!(out[1]["action"]["type"], "ignore-previous-rules", "allow, yüksek öncelikle sonda");
    }

    #[test]
    fn build_filter_empty_and_nonempty() {
        assert!(build_filter(false, &[]).is_none());
        let s = build_filter(true, &[]).unwrap();
        assert!(serde_json::from_str::<Vec<Value>>(&s).unwrap().len() > 50);
    }
}
