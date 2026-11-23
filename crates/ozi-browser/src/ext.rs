//! Uzantı çalışma zamanı: enjeksiyon betikleri, arka plan sayfası ve `ozi-ext://` sunucusu.

use ozi_core::extension::Extension;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use wry::http::{header, Request, Response, StatusCode};

/// Windows'ta WebView2 özel şemaları `http://<ad>.localhost/` olarak sunar.
#[cfg(windows)]
pub const OZI_BASE: &str = "http://ozi.localhost/";
#[cfg(not(windows))]
pub const OZI_BASE: &str = "ozi://localhost/";
#[cfg(windows)]
pub const EXT_BASE: &str = "http://ozi-ext.localhost/";
#[cfg(not(windows))]
pub const EXT_BASE: &str = "ozi-ext://localhost/";

pub type ExtMap = Arc<RwLock<HashMap<String, Extension>>>;

const SHIM: &str = include_str!("ext_shim.js");
pub const BACKGROUND_PAGE: &str = "_ozi_background.html";

pub fn ext_url(id: &str, path: &str) -> String {
    format!("{EXT_BASE}{id}/{}", path.trim_start_matches('/'))
}

/// `ozi://sayfa` -> gerçek (platforma özgü) URL ve tersi.
pub fn to_real_url(s: &str) -> String {
    match s.strip_prefix("ozi://") {
        Some(rest) => format!("{OZI_BASE}{rest}"),
        None => s.to_string(),
    }
}

pub fn to_display_url(s: &str) -> String {
    match s.strip_prefix(OZI_BASE) {
        Some(rest) => format!("ozi://{rest}"),
        None => s.to_string(),
    }
}

pub fn is_internal(url: &str) -> bool {
    url.starts_with(OZI_BASE)
}

/// URL bu uzantının kendi sayfası mı? (`Some(uzantı kimliği)`)
pub fn ext_page_owner(url: &str) -> Option<&str> {
    let rest = url.strip_prefix(EXT_BASE)?;
    rest.split('/').next().filter(|id| !id.is_empty())
}

fn ext_info(e: &Extension) -> Value {
    json!({
        "pub": e.public_id(),
        "base": ext_url(&e.id, ""),
        "manifest": e.manifest,
        "messages": e.messages,
        "os": if cfg!(windows) { "win" } else { "linux" },
    })
}

/// Webview başına gizli kimlik: sayfa betikleri bunları bilemez.
#[derive(Clone, Debug)]
pub struct WebCtx {
    pub nonce: String,
    pub rx: String,
    pub handoff: String,
}

pub fn rand_hex() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        h.write_u64(t ^ i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

impl WebCtx {
    pub fn new() -> Self {
        Self {
            nonce: rand_hex(),
            rx: format!("_{}", &rand_hex()[..16]),
            handoff: format!("_{}", &rand_hex()[..16]),
        }
    }

    /// Rust -> sayfa çağrısı: `kind` = rx | dispatch | storage | fire
    pub fn call(&self, kind: &str, args: &[Value]) -> String {
        let mut a = String::new();
        for v in args {
            a.push(',');
            a.push_str(&v.to_string());
        }
        format!(
            "window[{rx}]&&window[{rx}]({nonce},{kind}{a});",
            rx = json!(self.rx),
            nonce = json!(self.nonce),
            kind = json!(kind),
        )
    }
}

/// Her webview'a eklenen başlangıç betikleri (sıra önemli: önce şim, en son temizlik).
pub fn init_scripts(exts: &[&Extension], ctx: &WebCtx) -> Vec<String> {
    let map: serde_json::Map<String, Value> = exts.iter().map(|e| (e.id.clone(), ext_info(e))).collect();
    let shim = SHIM
        .replace("__EXTS_JSON__", &Value::Object(map).to_string())
        .replace("__NONCE__", &json!(ctx.nonce).to_string())
        .replace("__RXNAME__", &json!(ctx.rx).to_string())
        .replace("__HANDOFF__", &json!(ctx.handoff).to_string());
    let mut out = vec![shim];
    let h = json!(ctx.handoff);
    for e in exts {
        // Uzantının kendi sayfalarında chrome/browser nesnesi
        out.push(format!(
            "(function(){{var rt=window[{h}];if(!rt)return;var id={id};var E=rt.exts[id];\
             if(!E||location.href.indexOf(E.base)!==0)return;var api=rt.mk(id);window.chrome=api;window.browser=api;}})();",
            id = json!(e.id)
        ));
        for cs in &e.manifest.content_scripts {
            if let Some(s) = content_script_entry(e, cs, &ctx.handoff) {
                out.push(s);
            }
        }
    }
    out.push(format!("try{{delete window[{h}];}}catch(e){{}}"));
    out
}

fn content_script_entry(e: &Extension, cs: &ozi_core::manifest::ContentScript, handoff: &str) -> Option<String> {
    let mut code = String::new();
    for f in &cs.js {
        if let Some(src) = e.read_string(f) {
            code.push_str(&src);
            code.push_str("\n;\n");
        }
    }
    let mut css = String::new();
    for f in &cs.css {
        if let Some(src) = e.read_string(f) {
            css.push_str(&src);
            css.push('\n');
        }
    }
    if code.is_empty() && css.is_empty() {
        return None;
    }
    let run_at = cs.run_at.as_deref().unwrap_or("document_idle");
    let frames = if cs.all_frames { "" } else { "if(window.top!==window)return;" };
    let css_call = if css.is_empty() {
        String::new()
    } else {
        format!("rt.schedule('document_start',function(){{rt.css({});}});", json!(css))
    };
    Some(format!(
        "(function(){{var rt=window[{h}];if(!rt)return;{frames}\
         if(!rt.match(location.href,{m},{x},{ig},{eg}))return;\
         var api=rt.mk({id});if(!api)return;{css_call}\
         rt.schedule({run_at},function(){{(function(chrome,browser){{\n{code}\n}}).call(window,api,api);}});}})();",
        h = json!(handoff),
        m = json!(cs.matches),
        x = json!(cs.exclude_matches),
        ig = json!(cs.include_globs),
        eg = json!(cs.exclude_globs),
        id = json!(e.id),
        run_at = json!(run_at),
    ))
}

/// Arka plan betikleri için üretilen gizli sayfa (MV2 `scripts`, MV3 `service_worker`).
pub fn background_html(e: &Extension) -> Option<String> {
    let bg = e.manifest.background.as_ref()?;
    let module = bg.kind.as_deref() == Some("module");
    let scripts = e.manifest.background_scripts();
    if scripts.is_empty() {
        return None;
    }
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>background</title><script>\
         self.importScripts=function(){for(var i=0;i<arguments.length;i++){var x=new XMLHttpRequest();\
         x.open('GET',new URL(arguments[i],location.href).href,false);x.send();(0,eval)(x.responseText);}};\
         </script>",
    );
    for s in scripts {
        let ty = if module { " type=\"module\"" } else { "" };
        html.push_str(&format!("<script{ty} src=\"{}\"></script>", html_escape(&s)));
    }
    Some(html)
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "ogg" => "audio/ogg",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

pub fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
}

/// `ozi-ext://localhost/<id>/<yol>` isteklerini uzantı klasöründen karşılar.
pub fn serve_ext(map: &ExtMap, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let not_found = || respond(StatusCode::NOT_FOUND, "text/plain", b"404".to_vec());
    let path = req.uri().path().trim_start_matches('/');
    let Some((id, rest)) = path.split_once('/') else { return not_found() };
    let rest = percent_encoding::percent_decode_str(rest).decode_utf8_lossy().to_string();
    let Ok(guard) = map.read() else { return not_found() };
    let Some(ext) = guard.get(id).filter(|e| e.enabled) else { return not_found() };
    if rest == BACKGROUND_PAGE {
        return match background_html(ext) {
            Some(h) => respond(StatusCode::OK, "text/html; charset=utf-8", h.into_bytes()),
            None => not_found(),
        };
    }
    match ext.read(&rest) {
        Some(bytes) => respond(StatusCode::OK, mime(&rest), bytes),
        None => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_mapping_roundtrip() {
        let real = to_real_url("ozi://newtab");
        assert!(is_internal(&real));
        assert_eq!(to_display_url(&real), "ozi://newtab");
        assert_eq!(to_real_url("https://a.b"), "https://a.b");
    }

    #[test]
    fn page_owner() {
        let u = ext_url("abcdefghijklmnopabcdefghijklmnop", "popup.html");
        assert_eq!(ext_page_owner(&u), Some("abcdefghijklmnopabcdefghijklmnop"));
        assert_eq!(ext_page_owner("https://evil.example/"), None);
    }
}
