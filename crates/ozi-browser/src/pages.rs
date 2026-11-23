//! `ozi://newtab` ve `ozi://extensions` iç sayfaları.

use crate::ext::{ext_url, html_escape, respond, ExtMap};
use ozi_core::settings::Settings;
use serde_json::json;
use std::borrow::Cow;
use std::sync::{Arc, RwLock};
use wry::http::{Request, Response, StatusCode};

const STYLE: &str = r#"
:root{--bg:#16171c;--card:#23242c;--fg:#e8e9ee;--muted:#9b9dab;--accent:#ff7a2f;--border:#34353f}
@media (prefers-color-scheme:light){:root{--bg:#f4f5f9;--card:#fff;--fg:#1d1e24;--muted:#6a6c7a;--border:#d8dbe4}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,"Segoe UI",sans-serif}
main{max-width:760px;margin:0 auto;padding:48px 20px}h1{font-size:28px;margin:0 0 6px}
.muted{color:var(--muted)}.card{background:var(--card);border:1px solid var(--border);border-radius:14px;padding:16px;margin:12px 0}
button{font:inherit;border:1px solid var(--border);background:var(--card);color:var(--fg);padding:8px 14px;border-radius:9px;cursor:pointer}
button.p{background:var(--accent);border-color:var(--accent);color:#fff}button:hover{filter:brightness(1.1)}
.row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}.grow{flex:1;min-width:0}
.dis{opacity:.55}img.ic{width:36px;height:36px;border-radius:8px;object-fit:contain}
"#;

pub type SettingsRef = Arc<RwLock<Settings>>;

pub fn serve(map: &ExtMap, settings: &SettingsRef, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let html = match req.uri().path().trim_matches('/') {
        "newtab" | "" => newtab(settings),
        "extensions" => extensions(map),
        _ => return respond(StatusCode::NOT_FOUND, "text/plain", b"404".to_vec()),
    };
    respond(StatusCode::OK, "text/html; charset=utf-8", html.into_bytes())
}

fn newtab(settings: &SettingsRef) -> String {
    let (first, second) = settings
        .read()
        .map(|s| {
            let name = |i: usize| s.engines.get(i).map(|e| e.name.clone()).unwrap_or_default();
            (name(0), name(1))
        })
        .unwrap_or_default();
    format!(
        r#"<!doctype html><meta charset=utf-8><title>Yeni sekme</title><style>{STYLE}
main{{text-align:center;padding-top:18vh}}h1{{font-size:54px;letter-spacing:-1px}}h1 b{{color:var(--accent)}}
form{{margin:28px auto 8px;max-width:560px}}input{{width:100%;padding:15px 22px;border-radius:30px;border:1px solid var(--border);
background:var(--card);color:var(--fg);font-size:17px;outline:none}}input:focus{{border-color:var(--accent)}}</style>
<main><h1>Ozi<b>.</b></h1><form id=f><input id=q autofocus autocomplete=off placeholder="{first} ile ara ya da adres yaz"></form>
<p class=muted>Varsayılan: {first} · <code>g sorgu</code> yazarak {second} ile ara</p></main>
<script>document.getElementById('f').onsubmit=function(e){{e.preventDefault();
window.ipc.postMessage(JSON.stringify({{cmd:'go',input:document.getElementById('q').value}}));}};</script>"#,
        first = html_escape(&first),
        second = html_escape(&second),
    )
}

fn extensions(map: &ExtMap) -> String {
    let list: Vec<_> = map
        .read()
        .map(|m| {
            let mut v: Vec<_> = m
                .values()
                .map(|e| {
                    json!({
                        "id": e.id,
                        "name": e.display_name(),
                        "version": e.manifest.version,
                        "description": e.display_description(),
                        "enabled": e.enabled,
                        "icon": e.manifest.best_icon(48).map(|p| ext_url(&e.id, p)),
                        "options": e.manifest.options_page_path().is_some(),
                    })
                })
                .collect();
            v.sort_by_key(|x| x["name"].as_str().unwrap_or("").to_lowercase());
            v
        })
        .unwrap_or_default();
    // `</` kaçışı: JSON'u <script> içine güvenle gömmek için
    let data = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()).replace("</", "<\\/");
    format!(
        r#"<!doctype html><meta charset=utf-8><title>Uzantılar</title><style>{STYLE}</style>
<main><h1>Uzantılar</h1>
<p class=muted>Chrome (MV2/MV3) ve Firefox WebExtension paketleri: <code>.crx</code>, <code>.xpi</code>, <code>.zip</code> ya da açılmış klasör.</p>
<div class=row><button class=p onclick="send({{cmd:'install_file'}})">Paket seç…</button>
<button onclick="send({{cmd:'install_dir'}})">Klasör seç…</button></div><div id=list></div>
<p class=muted>Not: İçerik betikleri sayfa dünyasında çalışır; <code>webRequest</code>/<code>declarativeNetRequest</code> engelleme henüz yok.</p></main>
<script>
var L={data};function send(o){{window.ipc.postMessage(JSON.stringify(o));}}
var box=document.getElementById('list');
if(!L.length){{box.innerHTML='<div class="card muted">Henüz uzantı yüklenmedi.</div>';}}
L.forEach(function(e){{var d=document.createElement('div');d.className='card row'+(e.enabled?'':' dis');
var im=e.icon?'<img class=ic src="'+e.icon.replace(/"/g,'&quot;')+'">':'<div class=ic></div>';
d.innerHTML=im+'<div class=grow><b></b> <span class=muted></span><div class=muted></div></div>';
d.querySelector('b').textContent=e.name;d.querySelector('span').textContent='v'+e.version;
d.querySelector('.grow .muted:last-child').textContent=e.description;
function btn(t,f){{var b=document.createElement('button');b.textContent=t;b.onclick=f;d.appendChild(b);}}
if(e.options)btn('Ayarlar',function(){{send({{cmd:'ext_options',id:e.id}});}});
btn(e.enabled?'Kapat':'Aç',function(){{send({{cmd:'ext_toggle',id:e.id}});}});
btn('Kaldır',function(){{if(confirm(e.name+' kaldırılsın mı?'))send({{cmd:'ext_remove',id:e.id}});}});
box.appendChild(d);}});
</script>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ozi_core::extension::Extension;
    use std::collections::HashMap;

    #[test]
    fn pages_render() {
        let dir = std::env::temp_dir().join(format!("ozi-pages-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            br#"{"manifest_version":3,"name":"Test <b>Ext</b>","version":"1.2","description":"</script><script>alert(1)</script>"}"#,
        )
        .unwrap();
        let ext = Extension::load(&dir, "abcdefghijklmnopabcdefghijklmnop", true).unwrap();
        let map: ExtMap = Arc::new(RwLock::new(HashMap::from([(ext.id.clone(), ext)])));
        let settings: SettingsRef = Arc::new(RwLock::new(Settings::default()));

        let nt = newtab(&settings);
        assert!(nt.contains("DuckDuckGo ile ara"), "varsayılan motor DuckDuckGo olmalı");
        assert!(nt.contains("Google ile ara"), "ikinci motor Google olmalı");

        let ex = extensions(&map);
        // Kötü amaçlı açıklama <script> bloğunu kapatamamalı
        assert!(!ex.contains("</script><script>alert"));
        assert!(ex.contains("Test <b>Ext<\\/b>"), "ad, kaçışlı JSON olarak gömülmeli");

        if let Ok(out) = std::env::var("OZI_DUMP") {
            std::fs::write(format!("{out}/newtab.html"), &nt).unwrap();
            std::fs::write(format!("{out}/extensions.html"), &ex).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
