//! Chrome (MV2/MV3) ve Firefox WebExtension `manifest.json` modeli.
//! Her iki tarayıcının ortak alt kümesini okur; bilinmeyen alanlar yok sayılır.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub manifest_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default_locale: Option<String>,
    #[serde(default)]
    pub permissions: Vec<Value>,
    #[serde(default)]
    pub host_permissions: Vec<String>,
    #[serde(default)]
    pub content_scripts: Vec<ContentScript>,
    #[serde(default)]
    pub background: Option<Background>,
    #[serde(default)]
    pub action: Option<Action>,
    #[serde(default)]
    pub browser_action: Option<Action>,
    #[serde(default)]
    pub page_action: Option<Action>,
    #[serde(default)]
    pub options_ui: Option<OptionsUi>,
    #[serde(default)]
    pub options_page: Option<String>,
    #[serde(default)]
    pub icons: HashMap<String, String>,
    #[serde(default)]
    pub browser_specific_settings: Option<BrowserSpecific>,
    #[serde(default)]
    pub applications: Option<BrowserSpecific>,
    #[serde(default)]
    pub declarative_net_request: Option<DeclarativeNetRequest>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeclarativeNetRequest {
    #[serde(default)]
    pub rule_resources: Vec<RuleResource>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleResource {
    #[serde(default)]
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub path: String,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContentScript {
    #[serde(default)]
    pub matches: Vec<String>,
    #[serde(default)]
    pub exclude_matches: Vec<String>,
    #[serde(default)]
    pub include_globs: Vec<String>,
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    #[serde(default)]
    pub js: Vec<String>,
    #[serde(default)]
    pub css: Vec<String>,
    /// document_start | document_end | document_idle (varsayılan)
    #[serde(default)]
    pub run_at: Option<String>,
    #[serde(default)]
    pub all_frames: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Background {
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub service_worker: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Action {
    #[serde(default)]
    pub default_popup: Option<String>,
    #[serde(default)]
    pub default_title: Option<String>,
    /// Metin ya da `{"16": "a.png", "32": "b.png"}`
    #[serde(default)]
    pub default_icon: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OptionsUi {
    #[serde(default)]
    pub page: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BrowserSpecific {
    #[serde(default)]
    pub gecko: Option<Gecko>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Gecko {
    #[serde(default)]
    pub id: Option<String>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        // Bazı uzantılar manifest başında UTF-8 BOM taşır.
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        serde_json::from_slice(bytes)
    }

    pub fn gecko_id(&self) -> Option<&str> {
        self.browser_specific_settings
            .as_ref()
            .or(self.applications.as_ref())
            .and_then(|b| b.gecko.as_ref())
            .and_then(|g| g.id.as_deref())
    }

    /// MV3 `action`, MV2 `browser_action`/`page_action`.
    pub fn toolbar_action(&self) -> Option<&Action> {
        self.action
            .as_ref()
            .or(self.browser_action.as_ref())
            .or(self.page_action.as_ref())
    }

    pub fn options_page_path(&self) -> Option<&str> {
        self.options_ui
            .as_ref()
            .and_then(|o| o.page.as_deref())
            .or(self.options_page.as_deref())
    }

    /// Verilen simgeler arasından istenen boyuta en yakınını seçer.
    pub fn best_icon(&self, want: u32) -> Option<&str> {
        let from_action = self
            .toolbar_action()
            .and_then(|a| a.default_icon.as_ref())
            .and_then(|v| match v {
                Value::String(s) => Some(s.as_str()),
                Value::Object(m) => pick_icon(m.iter().map(|(k, v)| (k.as_str(), v.as_str().unwrap_or(""))), want),
                _ => None,
            });
        from_action.or_else(|| pick_icon(self.icons.iter().map(|(k, v)| (k.as_str(), v.as_str())), want))
    }

    /// Arka plan komut dosyaları (MV2 `scripts`, MV3 `service_worker`).
    pub fn background_scripts(&self) -> Vec<String> {
        match &self.background {
            Some(b) if !b.scripts.is_empty() => b.scripts.clone(),
            Some(b) => b.service_worker.iter().cloned().collect(),
            None => vec![],
        }
    }
}

fn pick_icon<'a>(it: impl Iterator<Item = (&'a str, &'a str)>, want: u32) -> Option<&'a str> {
    it.filter(|(_, p)| !p.is_empty())
        .min_by_key(|(k, _)| k.parse::<u32>().map(|n| n.abs_diff(want)).unwrap_or(u32::MAX))
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chrome_mv3() {
        let m = Manifest::parse(
            br#"{"manifest_version":3,"name":"X","version":"1.0",
            "content_scripts":[{"matches":["*://*/*"],"js":["a.js"],"run_at":"document_start"}],
            "background":{"service_worker":"sw.js"},
            "action":{"default_popup":"p.html","default_icon":{"16":"i16.png","48":"i48.png"}},
            "host_permissions":["<all_urls>"],"unknown_field":1}"#,
        )
        .unwrap();
        assert_eq!(m.manifest_version, 3);
        assert_eq!(m.content_scripts[0].js, vec!["a.js"]);
        assert_eq!(m.background_scripts(), vec!["sw.js"]);
        assert_eq!(m.best_icon(40), Some("i48.png"));
        assert_eq!(m.toolbar_action().unwrap().default_popup.as_deref(), Some("p.html"));
    }

    #[test]
    fn parses_firefox_mv2_with_bom() {
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(
            br#"{"manifest_version":2,"name":"F","version":"2",
            "applications":{"gecko":{"id":"f@example.org"}},
            "browser_action":{"default_icon":"i.png"},
            "background":{"scripts":["b1.js","b2.js"]},"permissions":["tabs","storage","https://*/*"]}"#,
        );
        let m = Manifest::parse(&b).unwrap();
        assert_eq!(m.gecko_id(), Some("f@example.org"));
        assert_eq!(m.background_scripts(), vec!["b1.js", "b2.js"]);
        assert_eq!(m.best_icon(16), Some("i.png"));
    }
}
