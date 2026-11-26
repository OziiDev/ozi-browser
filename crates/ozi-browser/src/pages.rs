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
:root{--ease:cubic-bezier(.2,.8,.2,1)}
*{box-sizing:border-box}html{scroll-behavior:smooth}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,"Segoe UI",sans-serif}
main{max-width:860px;margin:0 auto;padding:28px 20px 60px}
nav{display:flex;gap:6px;flex-wrap:wrap;margin:0 0 26px;animation:fadeUp .45s var(--ease) both}
nav a{padding:7px 14px;border-radius:20px;color:var(--muted);text-decoration:none;transition:background .18s,color .18s,transform .18s var(--ease)}
nav a:hover{background:var(--card);color:var(--fg);transform:translateY(-1px)}
nav a.on{background:linear-gradient(135deg,var(--accent),var(--accent2));color:#fff}
h1{font-size:30px;margin:0 0 6px;letter-spacing:-.5px;animation:fadeUp .45s var(--ease) .04s both}
.muted{color:var(--muted)}.card{background:var(--card);border:1px solid var(--border);border-radius:16px;padding:14px 18px;margin:10px 0;
 animation:fadeUp .45s var(--ease) both;animation-delay:calc(var(--i,0)*35ms + 80ms);transition:transform .2s var(--ease),border-color .2s,box-shadow .2s}
.card:hover{transform:translateY(-2px);border-color:rgba(255,122,47,.45);box-shadow:0 8px 24px rgba(0,0,0,.12)}
.card.flat:hover{transform:none;box-shadow:none;border-color:var(--border)}
button,input,select{font:inherit}button{border:1px solid var(--border);background:var(--card2);color:var(--fg);padding:7px 14px;border-radius:10px;cursor:pointer;
 transition:transform .12s var(--ease),filter .15s,background .15s}
button:hover{filter:brightness(1.12)}button:active{transform:scale(.96)}
button.p{background:linear-gradient(135deg,var(--accent),var(--accent2));border-color:transparent;color:#fff}
input[type=text],input[type=search]{width:100%;padding:10px 14px;border-radius:12px;border:1px solid var(--border);background:var(--card);color:var(--fg);outline:none;
 transition:border-color .18s,box-shadow .2s}
input[type=text]:focus,input[type=search]:focus{border-color:var(--accent);box-shadow:0 0 0 3px rgba(255,122,47,.18)}
.row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}.grow{flex:1;min-width:0}
.dis{opacity:.55}img.ic{width:38px;height:38px;border-radius:10px;object-fit:contain}
.t{overflow:hidden;white-space:nowrap;text-overflow:ellipsis}a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
code{background:var(--card2);padding:1px 6px;border-radius:6px;font-size:.9em}
.switch{position:relative;display:inline-flex;align-items:center;gap:12px;cursor:pointer}
.switch input{display:none}.switch i{width:42px;height:24px;border-radius:12px;background:var(--border);position:relative;transition:background .22s;flex:none}
.switch i::after{content:"";position:absolute;top:3px;left:3px;width:18px;height:18px;border-radius:50%;background:#fff;transition:transform .25s var(--ease)}
.switch input:checked+i{background:linear-gradient(135deg,var(--accent),var(--accent2))}.switch input:checked+i::after{transform:translateX(18px)}
@keyframes fadeUp{from{opacity:0;transform:translateY(12px)}}
.seg{display:inline-flex;background:var(--card2);border-radius:10px;padding:3px;gap:2px}
.seg button{border:0;background:none;padding:7px 16px;border-radius:8px;color:var(--muted)}
.seg button.on{background:var(--card);color:var(--fg);box-shadow:0 1px 3px var(--shadow)}
.sw{width:30px;height:30px;border-radius:50%;border:0;padding:0;cursor:pointer;transition:transform .15s var(--ease),box-shadow .2s}
.sw:hover{transform:scale(1.12)}.sw.on{box-shadow:0 0 0 2px var(--bg),0 0 0 4px var(--fg)}
@media (prefers-reduced-motion:reduce){*,*::before,*::after{animation-duration:.01ms!important;transition-duration:.01ms!important}}
"#;


/// Geçerli tema (sayfalar her istekte buradan okur): `(tema, vurgu)`.
static THEME: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

pub fn set_theme(theme: &str, accent: &str) {
    if let Ok(mut t) = THEME.lock() {
        *t = (theme.to_string(), accent.to_string());
    }
}

const LIGHT: &str = "--bg:#f9f9fb;--card:#ffffff;--card2:#f0f0f4;--fg:#15141a;--muted:#5b5b66;--border:#dcdce2;--shadow:rgba(0,0,0,.14)";
const DARK: &str = "--bg:#1c1b22;--card:#2b2a33;--card2:#42414d;--fg:#fbfbfe;--muted:#bfbfc9;--border:#3a3944;--shadow:rgba(0,0,0,.45)";

/// `(<style> içeriği, <html> öznitelikleri)` — "system" ise sistemin koyu/açık tercihini izler.
fn theme_parts() -> (String, String) {
    let (theme, accent) = THEME.lock().map(|t| t.clone()).unwrap_or_default();
    let hex = ozi_core::settings::accent_hex(&accent);
    let css = format!(
        ":root{{{LIGHT};--accent:{hex};--accent2:var(--accent)}}\
         @media (prefers-color-scheme:dark){{:root:not([data-theme=light]){{{DARK}}}}}\
         :root[data-theme=dark]{{{DARK}}}:root[data-theme=light]{{{LIGHT}}}"
    );
    let attr = match theme.as_str() {
        "light" | "dark" => format!(" data-theme={theme}"),
        _ => String::new(),
    };
    (css, attr)
}

/// JSON'u `<script>` içine güvenle gömmek için kaçış.
fn embed(v: &Value) -> String {
    v.to_string().replace("</", "<\\/").replace("<!--", "<\\!--")
}

const NAV: &[(&str, &str, &str)] = &[
    ("history", "Geçmiş", "ozi://history"),
    ("bookmarks", "Yer imleri", "ozi://bookmarks"),
    ("downloads", "İndirmeler", "ozi://downloads"),
    ("extensions", "Uzantılar", "ozi://extensions"),
    ("settings", "Ayarlar", "ozi://settings"),
];

fn page(active: &str, title: &str, body: &str, script: &str) -> String {
    let nav: String = NAV
        .iter()
        .map(|(id, label, href)| format!("<a href=\"{href}\"{}>{label}</a>", if *id == active { " class=on" } else { "" }))
        .collect();
    let (tok, attr) = theme_parts();
    format!(
        "<!doctype html><html lang=tr{attr}><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>{title}</title>\
         <style>{tok}{STYLE}</style><main><nav>{nav}</nav>{body}</main><script>function send(o){{window.ipc.postMessage(JSON.stringify(o));}}\n\
         document.querySelectorAll('.card').forEach(function(c,i){{c.style.setProperty('--i',Math.min(i,12));}});\n{script}</script>"
    )
}

pub fn serve(ctx: &PageCtx, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let html = match req.uri().path().trim_matches('/') {
        "newtab" | "" => newtab(&ctx.settings, &ctx.user),
        "extensions" => extensions(&ctx.exts),
        "history" => history(&ctx.user),
        "bookmarks" => bookmarks(&ctx.user),
        "downloads" => downloads(&ctx.user),
        "settings" => settings(&ctx.settings),
        _ => return respond(StatusCode::NOT_FOUND, "text/plain", b"404".to_vec()),
    };
    respond(StatusCode::OK, "text/html; charset=utf-8", html.into_bytes())
}

fn newtab(settings: &SettingsRef, user: &UserRef) -> String {
    let (first, second) = settings
        .read()
        .map(|s| {
            let default = s.engines.iter().find(|e| e.id == s.default_engine).or(s.engines.first());
            let other = s.engines.iter().find(|e| Some(e.id.as_str()) != default.map(|d| d.id.as_str()));
            (
                default.map(|e| e.name.clone()).unwrap_or_default(),
                other
                    .map(|e| format!("{} için <code>{}</code>", html_escape(&e.name), html_escape(&e.keyword)))
                    .unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let sites: Vec<Value> = user
        .lock()
        .map(|u| u.top_sites(8).into_iter().map(|(url, title, host)| json!({"url": url, "title": title, "host": host})).collect())
        .unwrap_or_default();
    let css = r#"
body{min-height:100vh;overflow-x:hidden}
.bg{position:fixed;inset:0;z-index:-1;overflow:hidden}
.bg i{position:absolute;width:70vmax;height:70vmax;border-radius:50%;will-change:transform;animation:drift 34s ease-in-out infinite alternate}
.bg i:nth-child(1){background:radial-gradient(circle,color-mix(in srgb,var(--accent) 40%,transparent) 0,transparent 62%);left:-25vmax;top:-32vmax}
.bg i:nth-child(2){background:radial-gradient(circle,rgba(255,77,141,.26) 0,transparent 62%);right:-32vmax;top:-2vmax;animation-duration:42s;animation-delay:-9s}
.bg i:nth-child(3){background:radial-gradient(circle,rgba(106,92,255,.26) 0,transparent 62%);left:12vmax;bottom:-42vmax;animation-duration:48s;animation-delay:-17s}
@keyframes drift{to{transform:translate3d(9vmax,6vmax,0) scale(1.12)}}
.wrap{max-width:720px;margin:0 auto;padding:11vh 20px 40px;text-align:center}
#clock{font-size:76px;font-weight:200;letter-spacing:-3px;line-height:1;animation:fadeUp .7s var(--ease) both}
#greet{margin:8px 0 30px;color:var(--muted);font-size:17px;animation:fadeUp .7s var(--ease) .08s both}
form{position:relative;max-width:580px;margin:0 auto;animation:fadeUp .7s var(--ease) .16s both}
form input{padding:16px 54px 16px 24px;border-radius:32px;font-size:17px;background:var(--card);border:1px solid var(--border);box-shadow:0 1px 4px var(--shadow);transition:border-color .2s,box-shadow .25s,transform .3s var(--ease),background .2s}
form input:focus{transform:scale(1.02)}
form button{position:absolute;right:8px;top:50%;transform:translateY(-50%);width:40px;height:40px;padding:0;border-radius:50%;border:0;font-size:18px;
 background:var(--accent);color:#fff}
form button:active{transform:translateY(-50%) scale(.92)}
.hint{margin:14px 0 36px;color:var(--muted);font-size:13px;animation:fadeUp .7s var(--ease) .24s both}
.tiles{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:14px}
.tile{display:flex;flex-direction:column;align-items:center;gap:9px;padding:16px 8px 12px;border-radius:18px;text-decoration:none!important;color:var(--fg);
 background:var(--card);border:1px solid var(--border);animation:fadeUp .6s var(--ease) both;animation-delay:calc(var(--i)*55ms + 330ms);
 transition:transform .22s var(--ease),background .2s,border-color .2s,box-shadow .25s}
.tile:hover{transform:translateY(-4px);border-color:var(--accent);box-shadow:0 10px 24px var(--shadow)}
.tile:active{transform:translateY(-1px) scale(.97)}
.av{width:46px;height:46px;border-radius:14px;display:grid;place-items:center;font-weight:700;font-size:20px;color:#fff;text-shadow:0 1px 2px rgba(0,0,0,.25)}
.tile span.n{max-width:100%;font-size:13px}
.empty{color:var(--muted);font-size:14px;animation:fadeUp .6s var(--ease) .35s both}
.links{margin-top:42px;font-size:13px;color:var(--muted);animation:fadeUp .6s var(--ease) .6s both}.links a{margin:0 9px;color:var(--muted)}.links a:hover{color:var(--accent)}
@media (max-width:560px){.tiles{grid-template-columns:repeat(2,minmax(0,1fr))}#clock{font-size:56px}}
"#;
    let script = format!(
        r#"var SITES={sites};
function hue(s){{var h=0;for(var i=0;i<s.length;i++)h=(h*31+s.charCodeAt(i))%360;return h;}}
function tick(){{var d=new Date(),h=d.getHours();
document.getElementById('clock').textContent=('0'+h).slice(-2)+':'+('0'+d.getMinutes()).slice(-2);
var g=h<5?'İyi geceler':h<12?'Günaydın':h<18?'İyi günler':'İyi akşamlar';
document.getElementById('greet').textContent=g+' · '+d.toLocaleDateString('tr-TR',{{weekday:'long',day:'numeric',month:'long'}});}}
tick();setInterval(tick,15000);
var box=document.getElementById('tiles');
SITES.forEach(function(s,i){{var a=document.createElement('a');a.className='tile';a.href=s.url;a.style.setProperty('--i',i);
var av=document.createElement('div');av.className='av';av.style.background='linear-gradient(135deg,hsl('+hue(s.host)+',65%,52%),hsl('+((hue(s.host)+40)%360)+',65%,42%))';
av.textContent=s.host[0].toUpperCase();var n=document.createElement('span');n.className='n t';n.textContent=s.title||s.host;n.title=s.url;
a.append(av,n);box.appendChild(a);}});
if(!SITES.length)box.innerHTML='<div class=empty style="grid-column:1/-1">Gezdikçe en çok kullandığınız siteler burada görünür.</div>';
document.getElementById('f').onsubmit=function(e){{e.preventDefault();send({{cmd:'go',input:document.getElementById('q').value}});}};
window.addEventListener('keydown',function(e){{if(!e.ctrlKey&&!e.metaKey&&!e.altKey&&e.key.length===1)document.getElementById('q').focus();}});"#,
        sites = embed(&Value::Array(sites))
    );
    let (tok, attr) = theme_parts();
    format!(
        "<!doctype html><html lang=tr{attr}><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Yeni sekme</title>\
         <style>{tok}{STYLE}{css}</style><div class=bg><i></i><i></i><i></i></div>\
         <div class=wrap><div id=clock></div><div id=greet></div>\
         <form id=f><input type=text id=q autofocus autocomplete=off placeholder=\"{first_e} ile ara ya da adres yaz\"><button type=submit>→</button></form>\
         <div class=hint>{second}</div><div class=tiles id=tiles></div>\
         <div class=links><a href=\"ozi://history\">Geçmiş</a><a href=\"ozi://bookmarks\">Yer imleri</a><a href=\"ozi://downloads\">İndirmeler</a>\
         <a href=\"ozi://extensions\">Uzantılar</a><a href=\"ozi://settings\">Ayarlar</a></div></div>\
         <script>function send(o){{window.ipc.postMessage(JSON.stringify(o));}}\n{script}</script>",
        first_e = html_escape(&first),
        second = if second.is_empty() { String::new() } else { format!("Başka motor: {second}") },
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
    page("extensions", "Uzantılar", body, &script)
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
    page("history", "Geçmiş", body, &script)
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
    page("bookmarks", "Yer imleri", body, &script)
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
    page("downloads", "İndirmeler", body, &script)
}

fn settings(settings: &SettingsRef) -> String {
    let s = settings.read().map(|s| s.clone()).unwrap_or_default();
    let data = serde_json::to_value(&s).unwrap_or(Value::Null);
    let body = r#"<h1>Ayarlar</h1>
<div class="card flat"><b>Görünüm</b><div style="margin-top:10px"><div class=seg id=themeSeg></div></div>
<div class=row id=swatches style="margin-top:14px"></div></div>
<div class="card flat"><b>Arama motorları</b><p class=muted>Seçili olan varsayılandır. Adres çubuğunda <code>anahtar sorgu</code> yazarak diğerlerini kullanın. URL'de <code>%s</code> sorgunun yerine geçer.</p>
<div id=engines></div>
<div class=row style="margin-top:10px"><input type=text id=nn placeholder="Ad" style="width:130px"><input type=text id=nk placeholder="Anahtar" style="width:90px">
<input type=text id=nu placeholder="https://site.com/ara?q=%s" class=grow><button id=add>Ekle</button></div></div>
<div class="card flat"><b>Ana sayfa</b><div style="margin-top:8px"><input type=text id=home></div></div>
<div class="card flat"><label class=switch><input type=checkbox id=ads><i></i><span>Reklam ve izleyicileri engelle <span class=muted>(Linux)</span></span></label></div>
<div class="card flat"><label class=switch><input type=checkbox id=rs><i></i><span>Açılışta önceki sekmeleri geri yükle <span class=muted>(gizli sekmeler hariç)</span></span></label></div>
<div class=row><button class=p id=save>Kaydet</button><span id=msg class=muted></span></div>"#;
    let accents = Value::Array(
        ozi_core::settings::ACCENTS.iter().map(|(id, hex)| json!([id, hex])).collect(),
    );
    let script = format!(
        r#"var ACC={accents};var S={data};var box=document.getElementById('engines');
var TH=[['system','Sistem'],['light','Açık'],['dark','Koyu']];
function drawTheme(){{var seg=document.getElementById('themeSeg');seg.textContent='';
TH.forEach(function(t){{var b=document.createElement('button');b.textContent=t[1];b.className=S.theme===t[0]?'on':'';
b.onclick=function(){{S.theme=t[0];if(t[0]==='system')document.documentElement.removeAttribute('data-theme');else document.documentElement.setAttribute('data-theme',t[0]);drawTheme();}};seg.appendChild(b);}});
var sw=document.getElementById('swatches');sw.textContent='';
ACC.forEach(function(a){{var b=document.createElement('button');b.className='sw'+(S.accent===a[0]?' on':'');b.style.background=a[1];b.title=a[0];
b.onclick=function(){{S.accent=a[0];document.documentElement.style.setProperty('--accent',a[1]);drawTheme();}};sw.appendChild(b);}});}}
drawTheme();
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
home.value=S.homepage;ads.checked=S.block_ads;rs.checked=!!S.restore_session;
document.getElementById('save').onclick=function(){{S.homepage=home.value.trim()||'ozi://newtab';S.block_ads=ads.checked;S.restore_session=rs.checked;send({{cmd:'settings_save',settings:S}});}};
draw();"#,
        data = embed(&data),
        accents = embed(&accents)
    );
    page("settings", "Ayarlar", body, &script)
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
        let nt = newtab(&ctx.settings, &ctx.user);
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
            // Görsel inceleme için gerçekçi örnek veri
            {
                let mut u = ctx.user.lock().unwrap();
                *u = UserData::default();
                for (i, (url, title)) in [
                    ("https://github.com/OziiDev/ozi-browser", "OziiDev/ozi-browser"),
                    ("https://www.youtube.com/", "YouTube"),
                    ("https://duckduckgo.com/?q=rust", "rust at DuckDuckGo"),
                    ("https://doc.rust-lang.org/book/", "The Rust Programming Language"),
                    ("https://news.ycombinator.com/", "Hacker News"),
                    ("https://www.wikipedia.org/", "Wikipedia"),
                    ("https://crates.io/", "crates.io: Rust Package Registry"),
                    ("https://archlinux.org/", "Arch Linux"),
                ]
                .iter()
                .enumerate()
                {
                    for _ in 0..(8 - i) {
                        u.add_visit(url, title, 1_795_000_000 + i as u64 * 3600);
                    }
                }
                u.toggle_bookmark("https://www.reddit.com/", "Reddit", 1);
                u.download_started("https://example.com/ozi.tar.gz", "/home/ozi/İndirilenler/ozi.tar.gz", 1_795_000_100);
                u.download_finished("https://example.com/ozi.tar.gz", Some("/home/ozi/İndirilenler/ozi.tar.gz"), true);
            }
            for (n, h) in [
                ("newtab", newtab(&ctx.settings, &ctx.user)),
                ("extensions", ex),
                ("history", history(&ctx.user)),
                ("bookmarks", bookmarks(&ctx.user)),
                ("downloads", downloads(&ctx.user)),
                ("settings", settings(&ctx.settings)),
            ] {
                std::fs::write(format!("{out}/{n}.html"), h).unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
