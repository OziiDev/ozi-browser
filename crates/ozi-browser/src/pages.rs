//! İç sayfalar: `ozi://newtab`, `extensions`, `history`, `bookmarks`, `downloads`, `settings`.
//! Sayfalar her istekte güncel veriyle üretilir; değişiklikler IPC ile Rust'a gider.

use crate::ext::{ext_url, html_escape, respond, ExtMap};
use ozi_core::settings::Settings;
use ozi_core::userdata::UserData;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::{Arc, Mutex, RwLock};
use wry::http::{Request, Response, StatusCode};

pub type SettingsRef = Arc<RwLock<Settings>>;
pub type UserRef = Arc<Mutex<UserData>>;

#[derive(Clone)]
pub struct PageCtx {
    pub exts: ExtMap,
    pub settings: SettingsRef,
    pub user: UserRef,
}

const STYLE: &str = r#"
:root{--bg:#16171c;--card:#23242c;--fg:#e8e9ee;--muted:#9b9dab;--accent:#ff7a2f;--border:#34353f}
@media (prefers-color-scheme:light){:root{--bg:#f4f5f9;--card:#fff;--fg:#1d1e24;--muted:#6a6c7a;--border:#d8dbe4}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,"Segoe UI",sans-serif}
main{max-width:820px;margin:0 auto;padding:40px 20px}h1{font-size:28px;margin:0 0 6px}
.muted{color:var(--muted)}.card{background:var(--card);border:1px solid var(--border);border-radius:14px;padding:14px 16px;margin:10px 0}
button,input,select{font:inherit}button{border:1px solid var(--border);background:var(--card);color:var(--fg);padding:7px 13px;border-radius:9px;cursor:pointer}
button.p{background:var(--accent);border-color:var(--accent);color:#fff}button:hover{filter:brightness(1.1)}
input[type=text],input[type=search]{width:100%;padding:9px 12px;border-radius:9px;border:1px solid var(--border);background:var(--card);color:var(--fg);outline:none}
input:focus{border-color:var(--accent)}.row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}.grow{flex:1;min-width:0}
.dis{opacity:.55}img.ic{width:36px;height:36px;border-radius:8px;object-fit:contain}
.t{overflow:hidden;white-space:nowrap;text-overflow:ellipsis}a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
"#;

/// JSON'u `<script>` içine güvenle gömmek için kaçış.
fn embed(v: &Value) -> String {
    v.to_string().replace("</", "<\\/").replace("<!--", "<\\!--")
}

fn page(title: &str, body: &str, script: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>{title}</title>\
         <style>{STYLE}</style><main>{body}</main><script>function send(o){{window.ipc.postMessage(JSON.stringify(o));}}\n{script}</script>"
    )
}

pub fn serve(ctx: &PageCtx, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let html = match req.uri().path().trim_matches('/') {
        "newtab" | "" => newtab(&ctx.settings),
        "extensions" => extensions(&ctx.exts),
        "history" => history(&ctx.user),
        "bookmarks" => bookmarks(&ctx.user),
        "downloads" => downloads(&ctx.user),
        "settings" => settings(&ctx.settings),
        _ => return respond(StatusCode::NOT_FOUND, "text/plain", b"404".to_vec()),
    };
    respond(StatusCode::OK, "text/html; charset=utf-8", html.into_bytes())
}

fn newtab(settings: &SettingsRef) -> String {
    let (first, second) = settings
        .read()
        .map(|s| {
            let default = s.engines.iter().find(|e| e.id == s.default_engine).or(s.engines.first());
            let other = s.engines.iter().find(|e| Some(e.id.as_str()) != default.map(|d| d.id.as_str()));
            (
                default.map(|e| e.name.clone()).unwrap_or_default(),
                other
                    .map(|e| format!("{} (<code>{}</code>)", html_escape(&e.name), html_escape(&e.keyword)))
                    .unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let body = format!(
        r#"<div style="text-align:center;padding-top:14vh"><h1 style="font-size:54px;letter-spacing:-1px">Ozi<b style="color:var(--accent)">.</b></h1>
<form id=f style="margin:28px auto 8px;max-width:560px"><input type=text id=q autofocus autocomplete=off style="padding:15px 22px;border-radius:30px;font-size:17px"
placeholder="{first_e} ile ara ya da adres yaz"></form>
<p class=muted>Varsayılan: {first_e} · <code>anahtar sorgu</code> ile başka motor: {second}</p>
<p class=muted><a href="ozi://history">Geçmiş</a> · <a href="ozi://bookmarks">Yer imleri</a> · <a href="ozi://downloads">İndirmeler</a> · <a href="ozi://settings">Ayarlar</a></p></div>"#,
        first_e = html_escape(&first),
    );
    page(
        "Yeni sekme",
        &body,
        "document.getElementById('f').onsubmit=function(e){e.preventDefault();send({cmd:'go',input:document.getElementById('q').value});};",
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
                        "id": e.id, "name": e.display_name(), "version": e.manifest.version,
                        "description": e.display_description(), "enabled": e.enabled,
                        "icon": e.manifest.best_icon(48).map(|p| ext_url(&e.id, p)),
                        "options": e.manifest.options_page_path().is_some(),
                    })
                })
                .collect();
            v.sort_by_key(|x| x["name"].as_str().unwrap_or("").to_lowercase());
            v
        })
        .unwrap_or_default();
    let body = r#"<h1>Uzantılar</h1>
<p class=muted>Chrome (MV2/MV3) ve Firefox WebExtension paketleri: <code>.crx</code>, <code>.xpi</code>, <code>.zip</code> ya da açılmış klasör.</p>
<div class=row><button class=p onclick="send({cmd:'install_file'})">Paket seç…</button>
<button onclick="send({cmd:'install_dir'})">Klasör seç…</button></div><div id=list></div>
<p class=muted>İçerik betikleri sayfa dünyasında çalışır. <code>declarativeNetRequest</code> statik kuralları (Linux) desteklenir; <code>webRequest</code> engelleme desteklenmez.</p>"#;
    let script = format!(
        r#"var L={data};var box=document.getElementById('list');
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
box.appendChild(d);}});"#,
        data = embed(&Value::Array(list))
    );
    page("Uzantılar", body, &script)
}

fn history(user: &UserRef) -> String {
    let list: Vec<Value> = user
        .lock()
        .map(|u| {
            u.history
                .iter()
                .take(1000)
                .map(|h| json!({"url": h.url, "title": h.title, "last": h.last, "visits": h.visits}))
                .collect()
        })
        .unwrap_or_default();
    let body = r#"<h1>Geçmiş</h1><div class=row><input type=search id=q placeholder="Geçmişte ara…" class=grow>
<button onclick="if(confirm('Tüm geçmiş silinsin mi?'))send({cmd:'hist_clear'})">Geçmişi temizle</button></div><div id=list></div>"#;
    let script = format!(
        r#"var L={data};var box=document.getElementById('list');
function draw(){{var q=document.getElementById('q').value.toLowerCase();box.textContent='';var n=0;
L.forEach(function(h){{if(q&&(h.url+' '+h.title).toLowerCase().indexOf(q)<0)return;if(++n>300)return;
var d=document.createElement('div');d.className='card row';
var w=document.createElement('div');w.className='grow';var a=document.createElement('a');a.href=h.url;a.className='t';a.style.display='block';a.textContent=h.title||h.url;
var u=document.createElement('div');u.className='muted t';u.textContent=h.url+'  ·  '+new Date(h.last*1000).toLocaleString()+'  ·  '+h.visits+'×';
w.appendChild(a);w.appendChild(u);d.appendChild(w);var b=document.createElement('button');b.textContent='Sil';
b.onclick=function(){{send({{cmd:'hist_remove',url:h.url}});}};d.appendChild(b);box.appendChild(d);}});
if(!n)box.innerHTML='<div class="card muted">Kayıt yok.</div>';}}
document.getElementById('q').oninput=draw;draw();"#,
        data = embed(&Value::Array(list))
    );
    page("Geçmiş", body, &script)
}

fn bookmarks(user: &UserRef) -> String {
    let list: Vec<Value> = user
        .lock()
        .map(|u| u.bookmarks.iter().map(|b| json!({"url": b.url, "title": b.title})).collect())
        .unwrap_or_default();
    let body = "<h1>Yer imleri</h1><div id=list></div>";
    let script = format!(
        r#"var L={data};var box=document.getElementById('list');
if(!L.length)box.innerHTML='<div class="card muted">Yer imi yok. Adres çubuğundaki ☆ düğmesi ya da Ctrl+D ile ekleyin.</div>';
L.forEach(function(h){{var d=document.createElement('div');d.className='card row';
var w=document.createElement('div');w.className='grow';var a=document.createElement('a');a.href=h.url;a.className='t';a.style.display='block';a.textContent=h.title||h.url;
var u=document.createElement('div');u.className='muted t';u.textContent=h.url;w.appendChild(a);w.appendChild(u);d.appendChild(w);
var b=document.createElement('button');b.textContent='Kaldır';b.onclick=function(){{send({{cmd:'bm_remove',url:h.url}});}};d.appendChild(b);box.appendChild(d);}});"#,
        data = embed(&Value::Array(list))
    );
    page("Yer imleri", body, &script)
}

fn downloads(user: &UserRef) -> String {
    let list: Vec<Value> = user
        .lock()
        .map(|u| {
            u.downloads
                .iter()
                .map(|d| json!({"url": d.url, "path": d.path, "state": d.state, "time": d.time}))
                .collect()
        })
        .unwrap_or_default();
    let body = "<h1>İndirmeler</h1><div class=row><button onclick=\"send({cmd:'dl_clear'})\">Listeyi temizle</button></div><div id=list></div>";
    let script = format!(
        r#"var L={data};var box=document.getElementById('list');
if(!L.length)box.innerHTML='<div class="card muted">İndirme yok.</div>';
var S={{downloading:'İndiriliyor…',done:'Tamamlandı',failed:'Başarısız'}};
L.forEach(function(h){{var d=document.createElement('div');d.className='card row';
var w=document.createElement('div');w.className='grow';var a=document.createElement('div');a.className='t';a.textContent=h.path.split(/[\\/]/).pop();
var u=document.createElement('div');u.className='muted t';u.textContent=(S[h.state]||h.state)+'  ·  '+h.url+'  ·  '+new Date(h.time*1000).toLocaleString();
w.appendChild(a);w.appendChild(u);d.appendChild(w);
if(h.state==='done'){{var b=document.createElement('button');b.textContent='Klasörde göster';b.onclick=function(){{send({{cmd:'dl_show',path:h.path}});}};d.appendChild(b);}}
box.appendChild(d);}});"#,
        data = embed(&Value::Array(list))
    );
    page("İndirmeler", body, &script)
}

fn settings(settings: &SettingsRef) -> String {
    let s = settings.read().map(|s| s.clone()).unwrap_or_default();
    let data = serde_json::to_value(&s).unwrap_or(Value::Null);
    let body = r#"<h1>Ayarlar</h1>
<div class=card><b>Arama motorları</b><p class=muted>Seçili olan varsayılandır. Adres çubuğunda <code>anahtar sorgu</code> yazarak diğerlerini kullanın. URL'de <code>%s</code> sorgunun yerine geçer.</p>
<div id=engines></div>
<div class=row style="margin-top:10px"><input type=text id=nn placeholder="Ad" style="width:130px"><input type=text id=nk placeholder="Anahtar" style="width:90px">
<input type=text id=nu placeholder="https://site.com/ara?q=%s" class=grow><button id=add>Ekle</button></div></div>
<div class=card><b>Ana sayfa</b><div style="margin-top:8px"><input type=text id=home></div></div>
<div class=card><label><input type=checkbox id=ads> Reklam ve izleyicileri engelle <span class=muted>(Linux; yeniden başlatma gerekmez)</span></label></div>
<div class=row><button class=p id=save>Kaydet</button><span id=msg class=muted></span></div>"#;
    let script = format!(
        r#"var S={data};var box=document.getElementById('engines');
function draw(){{box.textContent='';S.engines.forEach(function(e,i){{var r=document.createElement('div');r.className='row';r.style.margin='4px 0';
var rd=document.createElement('input');rd.type='radio';rd.name='def';rd.checked=e.id===S.default_engine;rd.onchange=function(){{S.default_engine=e.id;}};
var n=document.createElement('b');n.style.width='120px';n.textContent=e.name;var k=document.createElement('code');k.style.width='60px';k.textContent=e.keyword;
var u=document.createElement('span');u.className='muted t grow';u.textContent=e.url;r.append(rd,n,k,u);
var up=document.createElement('button');up.textContent='↑';up.onclick=function(){{if(i>0){{var t=S.engines[i-1];S.engines[i-1]=e;S.engines[i]=t;draw();}}}};
var dn=document.createElement('button');dn.textContent='↓';dn.onclick=function(){{if(i<S.engines.length-1){{var t=S.engines[i+1];S.engines[i+1]=e;S.engines[i]=t;draw();}}}};
var rm=document.createElement('button');rm.textContent='Sil';rm.onclick=function(){{if(S.engines.length>1){{S.engines.splice(i,1);if(S.default_engine===e.id)S.default_engine=S.engines[0].id;draw();}}}};
r.append(up,dn,rm);box.appendChild(r);}});}}
document.getElementById('add').onclick=function(){{var nm=nn.value.trim(),kw=nk.value.trim(),ur=nu.value.trim();
if(!nm||!kw||ur.indexOf('%s')<0||!/^https?:\/\//.test(ur)){{msg.textContent='Ad, anahtar ve %s içeren http(s) URL gerekli.';return;}}
S.engines.push({{id:'',name:nm,keyword:kw,url:ur}});nn.value=nk.value=nu.value='';msg.textContent='';draw();}};
home.value=S.homepage;ads.checked=S.block_ads;
document.getElementById('save').onclick=function(){{S.homepage=home.value.trim()||'ozi://newtab';S.block_ads=ads.checked;send({{cmd:'settings_save',settings:S}});}};
draw();"#,
        data = embed(&data)
    );
    page("Ayarlar", body, &script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ozi_core::extension::Extension;
    use std::collections::HashMap;

    fn ctx_with_evil_ext() -> (PageCtx, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ozi-pages-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            br#"{"manifest_version":3,"name":"Test <b>Ext</b>","version":"1.2","description":"</script><script>alert(1)</script>"}"#,
        )
        .unwrap();
        let ext = Extension::load(&dir, "abcdefghijklmnopabcdefghijklmnop", true).unwrap();
        let ctx = PageCtx {
            exts: Arc::new(RwLock::new(HashMap::from([(ext.id.clone(), ext)]))),
            settings: Arc::new(RwLock::new(Settings::default())),
            user: Arc::new(Mutex::new(UserData::default())),
        };
        (ctx, dir)
    }

    #[test]
    fn pages_render_and_escape() {
        let (ctx, dir) = ctx_with_evil_ext();
        let nt = newtab(&ctx.settings);
        assert!(nt.contains("DuckDuckGo ile ara"), "varsayılan motor DuckDuckGo olmalı");
        assert!(nt.contains("Google"), "ikinci motor Google olmalı");

        let ex = extensions(&ctx.exts);
        assert!(!ex.contains("</script><script>alert"), "açıklama <script> bloğunu kapatamamalı");
        assert!(ex.contains("Test <b>Ext<\\/b>"));

        {
            let mut u = ctx.user.lock().unwrap();
            u.add_visit("https://a.com/</script><script>x", "T</script>", 1);
            u.toggle_bookmark("https://b.com/", "B", 2);
            u.download_started("https://c/f.zip", "/tmp/f.zip", 3);
        }
        for html in [history(&ctx.user), bookmarks(&ctx.user), downloads(&ctx.user)] {
            assert!(!html.contains("</script><script>x"), "kullanıcı verisi script'i kapatamamalı");
        }
        assert!(bookmarks(&ctx.user).contains("https://b.com/"));
        assert!(settings(&ctx.settings).contains("duckduckgo"));

        if let Ok(out) = std::env::var("OZI_DUMP") {
            for (n, h) in [
                ("newtab", nt),
                ("extensions", ex),
                ("history", history(&ctx.user)),
                ("settings", settings(&ctx.settings)),
            ] {
                std::fs::write(format!("{out}/{n}.html"), h).unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
