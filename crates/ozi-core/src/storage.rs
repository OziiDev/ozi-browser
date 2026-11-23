//! `chrome.storage.{local,sync,session}` için disk destekli depo.
//! `sync` yerel olarak davranır (hesap eşitleme yok).

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: usize = 20 << 20;

pub struct ExtStorage {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Map<String, Value>>>,
}

fn area_ok(a: &str) -> bool {
    matches!(a, "local" | "sync" | "session" | "managed")
}

impl ExtStorage {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn with<R>(&self, ext: &str, area: &str, f: impl FnOnce(&mut Map<String, Value>) -> R) -> Result<R, String> {
        if !area_ok(area) || !crate::store::is_valid_id(ext) {
            return Err("geçersiz depolama alanı".into());
        }
        let key = format!("{ext}.{area}");
        let mut cache = self.cache.lock().map_err(|_| "kilit hatası".to_string())?;
        let map = cache.entry(key.clone()).or_insert_with(|| {
            if area == "session" {
                return Map::new();
            }
            std::fs::read(self.dir.join(format!("{key}.json")))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        });
        let before = map.clone();
        let r = f(map);
        if area != "session" && *map != before {
            let data = serde_json::to_vec(&*map).map_err(|e| e.to_string())?;
            if data.len() > MAX_BYTES {
                *map = before;
                return Err("depolama kotası aşıldı".into());
            }
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
            let tmp = self.dir.join(format!("{key}.json.tmp"));
            std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
            std::fs::rename(tmp, self.dir.join(format!("{key}.json"))).map_err(|e| e.to_string())?;
        }
        Ok(r)
    }

    /// `keys`: null (hepsi) | "k" | ["a","b"] | {"a": varsayılan}
    pub fn get(&self, ext: &str, area: &str, keys: &Value) -> Result<Value, String> {
        self.with(ext, area, |m| {
            let mut out = Map::new();
            match keys {
                Value::Null => out = m.clone(),
                Value::String(k) => {
                    if let Some(v) = m.get(k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
                Value::Array(ks) => {
                    for k in ks.iter().filter_map(Value::as_str) {
                        if let Some(v) = m.get(k) {
                            out.insert(k.to_string(), v.clone());
                        }
                    }
                }
                Value::Object(defaults) => {
                    for (k, d) in defaults {
                        out.insert(k.clone(), m.get(k).cloned().unwrap_or_else(|| d.clone()));
                    }
                }
                _ => {}
            }
            Value::Object(out)
        })
    }

    /// Döndürür: `onChanged` için `{anahtar: {oldValue?, newValue}}`
    pub fn set(&self, ext: &str, area: &str, items: &Map<String, Value>) -> Result<Value, String> {
        self.with(ext, area, |m| {
            let mut changes = Map::new();
            for (k, v) in items {
                let mut c = Map::new();
                if let Some(old) = m.insert(k.clone(), v.clone()) {
                    c.insert("oldValue".into(), old);
                }
                c.insert("newValue".into(), v.clone());
                changes.insert(k.clone(), Value::Object(c));
            }
            Value::Object(changes)
        })
    }

    pub fn remove(&self, ext: &str, area: &str, keys: &Value) -> Result<Value, String> {
        let list: Vec<String> = match keys {
            Value::String(k) => vec![k.clone()],
            Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        self.with(ext, area, |m| {
            let mut changes = Map::new();
            for k in list {
                if let Some(old) = m.remove(&k) {
                    changes.insert(k, serde_json::json!({ "oldValue": old }));
                }
            }
            Value::Object(changes)
        })
    }

    pub fn clear(&self, ext: &str, area: &str) -> Result<Value, String> {
        self.with(ext, area, |m| {
            let changes: Map<String, Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!({ "oldValue": v })))
                .collect();
            m.clear();
            Value::Object(changes)
        })
    }

    pub fn forget(&self, ext: &str) {
        for a in ["local", "sync", "managed", "session"] {
            let key = format!("{ext}.{a}");
            if let Ok(mut c) = self.cache.lock() {
                c.remove(&key);
            }
            let _ = std::fs::remove_file(self.dir.join(format!("{key}.json")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn get_set_remove_clear_and_persist() {
        let d = std::env::temp_dir().join(format!("ozi-storage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = ExtStorage::new(&d);
        let items = json!({"a": 1, "b": "x"}).as_object().unwrap().clone();
        let ch = s.set(ID, "local", &items).unwrap();
        assert_eq!(ch["a"]["newValue"], 1);
        assert_eq!(s.get(ID, "local", &json!("a")).unwrap(), json!({"a": 1}));
        assert_eq!(s.get(ID, "local", &json!({"z": 9, "a": 0})).unwrap(), json!({"z": 9, "a": 1}));
        assert_eq!(s.get(ID, "local", &Value::Null).unwrap(), json!({"a": 1, "b": "x"}));
        let ch = s.set(ID, "local", json!({"a": 2}).as_object().unwrap()).unwrap();
        assert_eq!(ch["a"]["oldValue"], 1);

        // Yeni örnek diskten okur
        let s2 = ExtStorage::new(&d);
        assert_eq!(s2.get(ID, "local", &json!(["a"])).unwrap(), json!({"a": 2}));
        s2.remove(ID, "local", &json!("a")).unwrap();
        assert_eq!(s2.get(ID, "local", &Value::Null).unwrap(), json!({"b": "x"}));
        s2.clear(ID, "local").unwrap();
        assert_eq!(s2.get(ID, "local", &Value::Null).unwrap(), json!({}));

        // session diske yazılmaz, geçersiz alan/kimlik reddedilir
        s2.set(ID, "session", json!({"k": 1}).as_object().unwrap()).unwrap();
        assert!(!d.join(format!("{ID}.session.json")).exists());
        assert!(s2.get(ID, "../x", &Value::Null).is_err());
        assert!(s2.get("../evil", "local", &Value::Null).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
