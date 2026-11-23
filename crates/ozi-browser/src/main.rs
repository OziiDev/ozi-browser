//! Ozi Browser — Rust ile yazılmış, Chrome ve Firefox uzantılarını çalıştırabilen hafif tarayıcı.
//!
//! Katmanlar: `tao` (pencere) + `wry` (Linux: WebKitGTK, Windows: WebView2) + `ozi-core`
//! (arama, uzantı yükleme/eşleştirme/depolama).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ext;
mod pages;

use ext::{ext_url, WebCtx, BACKGROUND_PAGE};
use ozi_core::{
    extension::Extension,
    matcher,
    search,
    settings::{Paths, Settings},
    storage::ExtStorage,
    store::ExtensionStore,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tao::{
    dpi::{LogicalSize, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

/// Üstteki arayüz çubuğunun (sekmeler + adres) mantıksal yüksekliği.
const UI_H: f64 = 86.0;
const NO_RECEIVER: &str = "Could not establish connection. Receiving end does not exist.";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Src {
    Ui,
    Tab(u64),
    Bg(String),
}

enum UserEvent {
    Ipc(Src, String),
    Title(u64, String),
    Nav(u64, String),
    Load(u64, bool, String),
    NewTab(String),
    BgLoaded(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Uzantının kendi sayfası / arka planı: tüm API'ler
    Priv,
    /// Sayfaya enjekte edilmiş içerik betiği: yalnızca runtime/storage/i18n
    Content,
}

struct Tab {
    id: u64,
    wv: WebView,
    ctx: WebCtx,
    url: String,
    title: String,
    loading: bool,
}

struct Bg {
    wv: WebView,
    ctx: WebCtx,
}

struct Pending {
    origin: Src,
    rid: u64,
    target: Src,
}

struct Host {
    window: Window,
    #[cfg(target_os = "linux")]
    fixed: gtk::Fixed,
}

fn build<'a>(
    host: &Host,
    ctx: &'a mut WebContext,
    cfg: impl FnOnce(WebViewBuilder<'a>) -> WebViewBuilder<'a>,
) -> wry::Result<WebView> {
    let b = cfg(WebViewBuilder::new_with_web_context(ctx));
    #[cfg(target_os = "linux")]
    {
        use wry::WebViewBuilderExtUnix;
        b.build_gtk(&host.fixed)
    }
    #[cfg(not(target_os = "linux"))]
    {
        b.build_as_child(&host.window)
    }
}

/// Linux'ta özel protokoller ortak `WebContext`'e yalnızca bir kez kaydedilebilir
/// (işleyici tüm webview'lar için geçerli olur); Windows'ta her webview kendi kaydını yapar.
#[cfg(target_os = "linux")]
fn protocols_needed(ctx: &WebContext) -> bool {
    !ctx.is_custom_protocol_registered("ozi")
}
#[cfg(not(target_os = "linux"))]
fn protocols_needed(_: &WebContext) -> bool {
    true
}

fn with_protocols<'a>(
    b: WebViewBuilder<'a>,
    need: bool,
    map: &ext::ExtMap,
    settings: &pages::SettingsRef,
) -> WebViewBuilder<'a> {
    if !need {
        return b;
    }
    let (m1, s1, m2) = (map.clone(), settings.clone(), map.clone());
    b.with_custom_protocol("ozi".into(), move |_, req| pages::serve(&m1, &s1, &req))
        .with_custom_protocol("ozi-ext".into(), move |_, req| ext::serve_ext(&m2, &req))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        position: wry::dpi::LogicalPosition::new(x, y).into(),
        size: wry::dpi::LogicalSize::new(w.max(1.0), h.max(1.0)).into(),
    }
}

fn unique_path(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    (1..1000)
        .map(|n| p.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(p)
}

struct App {
    host: Host,
    web_ctx: WebContext,
    proxy: EventLoopProxy<UserEvent>,
    ui: WebView,
    tabs: Vec<Tab>,
    active: u64,
    next_id: u64,
    size: (f64, f64),
    settings: pages::SettingsRef,
    store: ExtensionStore,
    storage: ExtStorage,
    ext_map: ext::ExtMap,
    bg: HashMap<String, Bg>,
    pending: HashMap<String, Pending>,
    just_installed: HashSet<String>,
    quit: bool,
}

impl App {
    // ---------------------------------------------------------------- yardımcılar
    fn tab(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn enabled_exts(&self) -> Vec<Extension> {
        let Ok(m) = self.ext_map.read() else { return vec![] };
        let mut v: Vec<Extension> = m.values().filter(|e| e.enabled).cloned().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn content_rect(&self) -> Rect {
        rect(0.0, UI_H, self.size.0, self.size.1 - UI_H)
    }

    fn layout(&self) {
        let _ = self.ui.set_bounds(rect(0.0, 0.0, self.size.0, UI_H));
        let r = || self.content_rect();
        for t in &self.tabs {
            let _ = t.wv.set_bounds(r());
        }
    }

    fn resize(&mut self, sz: PhysicalSize<u32>) {
        let s = self.host.window.scale_factor();
        self.size = (sz.width as f64 / s, sz.height as f64 / s);
        self.layout();
    }

    fn target(&self, src: &Src) -> Option<(&WebView, &WebCtx)> {
        match src {
            Src::Tab(id) => self.tab(*id).map(|t| (&t.wv, &t.ctx)),
            Src::Bg(id) => self.bg.get(id).map(|b| (&b.wv, &b.ctx)),
            Src::Ui => None,
        }
    }

    fn eval_in(&self, src: &Src, kind: &str, args: &[Value]) {
        if let Some((wv, ctx)) = self.target(src) {
            let _ = wv.evaluate_script(&ctx.call(kind, args));
        }
    }

    fn tab_json(&self, t: &Tab) -> Value {
        let index = self.tabs.iter().position(|x| x.id == t.id).unwrap_or(0);
        json!({
            "id": t.id, "index": index, "windowId": 1, "active": t.id == self.active,
            "highlighted": t.id == self.active, "incognito": false, "pinned": false,
            "url": t.url, "title": t.title,
            "status": if t.loading { "loading" } else { "complete" },
        })
    }

    fn push_ui(&self) {
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .map(|t| json!({"id": t.id, "title": t.title, "url": ext::to_display_url(&t.url), "loading": t.loading}))
            .collect();
        let exts: Vec<Value> = self
            .enabled_exts()
            .iter()
            .filter(|e| e.manifest.toolbar_action().is_some())
            .map(|e| {
                json!({
                    "id": e.id,
                    "name": e.display_name(),
                    "icon": e.manifest.best_icon(32).map(|p| ext_url(&e.id, p)),
                })
            })
            .collect();
        let state = json!({"tabs": tabs, "active": self.active, "exts": exts});
        let _ = self.ui.evaluate_script(&format!("window.oziUi&&window.oziUi.state({state})"));
        if let Some(t) = self.tab(self.active) {
            let title = if t.title.is_empty() { "Ozi Browser".to_string() } else { format!("{} — Ozi", t.title) };
            self.host.window.set_title(&title);
        }
    }

    fn fire_bg_all(&self, name: &str, args: Value) {
        for (ext_id, b) in &self.bg {
            let _ = b.wv.evaluate_script(&b.ctx.call("fire", &[json!(ext_id), json!(name), args.clone()]));
        }
    }

    // ---------------------------------------------------------------- sekmeler
    fn create_tab(&mut self, url: &str, activate: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let real = ext::to_real_url(url);
        let ctx = WebCtx::new();
        let enabled = self.enabled_exts();
        let refs: Vec<&Extension> = enabled.iter().collect();
        let scripts = ext::init_scripts(&refs, &ctx);
        let bounds = self.content_rect();
        let p = self.proxy.clone();
        let (p_ipc, p_nav, p_title, p_load, p_new) = (p.clone(), p.clone(), p.clone(), p.clone(), p);
        let need = protocols_needed(&self.web_ctx);
        let (m, s) = (self.ext_map.clone(), self.settings.clone());
        let result = build(&self.host, &mut self.web_ctx, |mut b| {
            for sc in scripts {
                b = b.with_initialization_script(sc);
            }
            with_protocols(b, need, &m, &s)
                .with_bounds(bounds)
                .with_url(real.clone())
                .with_devtools(true)
                .with_hotkeys_zoom(true)
                .with_ipc_handler(move |req| {
                    let _ = p_ipc.send_event(UserEvent::Ipc(Src::Tab(id), req.into_body()));
                })
                .with_navigation_handler(move |u| {
                    let _ = p_nav.send_event(UserEvent::Nav(id, u));
                    true
                })
                .with_document_title_changed_handler(move |t| {
                    let _ = p_title.send_event(UserEvent::Title(id, t));
                })
                .with_on_page_load_handler(move |ev, u| {
                    let _ = p_load.send_event(UserEvent::Load(id, matches!(ev, PageLoadEvent::Started), u));
                })
                .with_new_window_req_handler(move |u, _| {
                    let _ = p_new.send_event(UserEvent::NewTab(u));
                    NewWindowResponse::Deny
                })
                .with_download_started_handler(|_url, path| {
                    if let (Some(dir), Some(name)) = (dirs::download_dir(), path.file_name()) {
                        *path = unique_path(dir.join(name));
                    }
                    true
                })
        });
        let wv = match result {
            Ok(w) => w,
            Err(e) => {
                eprintln!("sekme oluşturulamadı: {e}");
                return 0;
            }
        };
        if activate {
            for t in &self.tabs {
                let _ = t.wv.set_visible(false);
            }
        } else {
            let _ = wv.set_visible(false);
        }
        self.tabs.push(Tab { id, wv, ctx, url: real, title: String::new(), loading: true });
        if activate {
            self.active = id;
            if let Some(t) = self.tab(id) {
                let _ = t.wv.focus();
            }
        }
        self.push_ui();
        if let Some(t) = self.tab(id) {
            let tj = self.tab_json(t);
            self.fire_bg_all("tabs.onCreated", json!([tj]));
        }
        id
    }

    fn switch_tab(&mut self, id: u64) {
        if self.tab(id).is_none() {
            return;
        }
        for t in &self.tabs {
            let _ = t.wv.set_visible(t.id == id);
        }
        self.active = id;
        if let Some(t) = self.tab(id) {
            let _ = t.wv.set_bounds(self.content_rect());
            let _ = t.wv.focus();
        }
        self.push_ui();
        self.fire_bg_all("tabs.onActivated", json!([{"tabId": id, "windowId": 1}]));
    }

    fn close_tab(&mut self, id: u64) {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else { return };
        self.tabs.remove(pos);
        self.pending.retain(|_, p| p.origin != Src::Tab(id) && p.target != Src::Tab(id));
        if self.tabs.is_empty() {
            self.quit = true;
            return;
        }
        if self.active == id {
            let next = self.tabs[pos.min(self.tabs.len() - 1)].id;
            self.switch_tab(next);
        } else {
            self.push_ui();
        }
        self.fire_bg_all("tabs.onRemoved", json!([id, {"windowId": 1, "isWindowClosing": false}]));
    }

    fn navigate(&mut self, tab_id: u64, input: &str) {
        let s = self.settings.read().map(|s| s.clone()).unwrap_or_default();
        let url = ext::to_real_url(&search::resolve(input, &s.engines, &s.default_engine));
        if url.trim_start().to_ascii_lowercase().starts_with("javascript:") {
            return;
        }
        if let Some(t) = self.tab_mut(tab_id) {
            t.url = url.clone();
            t.loading = true;
            let _ = t.wv.load_url(&url);
        }
        self.push_ui();
    }

    // ---------------------------------------------------------------- uzantılar
    fn reload_extensions(&mut self) {
        let all = self.store.load_all();
        if let Ok(mut m) = self.ext_map.write() {
            *m = all.into_iter().map(|e| (e.id.clone(), e)).collect();
        }
        self.bg.clear();
        self.pending.clear();
        for e in self.enabled_exts() {
            self.start_background(&e);
        }
        self.push_ui();
    }

    fn start_background(&mut self, e: &Extension) {
        let Some(bgm) = e.manifest.background.as_ref() else { return };
        let page = match (&bgm.page, e.manifest.background_scripts().is_empty()) {
            (Some(p), true) => p.clone(),
            _ => BACKGROUND_PAGE.to_string(),
        };
        if page == BACKGROUND_PAGE && ext::background_html(e).is_none() {
            return;
        }
        let ctx = WebCtx::new();
        let enabled = self.enabled_exts();
        let refs: Vec<&Extension> = enabled.iter().collect();
        let scripts = ext::init_scripts(&refs, &ctx);
        let (id, url) = (e.id.clone(), ext_url(&e.id, &page));
        let (p_ipc, p_load) = (self.proxy.clone(), self.proxy.clone());
        let (id_ipc, id_load) = (id.clone(), id.clone());
        let need = protocols_needed(&self.web_ctx);
        let (m, s) = (self.ext_map.clone(), self.settings.clone());
        let result = build(&self.host, &mut self.web_ctx, |mut b| {
            for sc in scripts {
                b = b.with_initialization_script(sc);
            }
            with_protocols(b, need, &m, &s)
                .with_visible(false)
                .with_bounds(rect(0.0, 0.0, 1.0, 1.0))
                .with_url(url)
                .with_ipc_handler(move |req| {
                    let _ = p_ipc.send_event(UserEvent::Ipc(Src::Bg(id_ipc.clone()), req.into_body()));
                })
                .with_on_page_load_handler(move |ev, _| {
                    if matches!(ev, PageLoadEvent::Finished) {
                        let _ = p_load.send_event(UserEvent::BgLoaded(id_load.clone()));
                    }
                })
        });
        match result {
            Ok(wv) => {
                self.bg.insert(id, Bg { wv, ctx });
            }
            Err(err) => eprintln!("arka plan sayfası başlatılamadı ({}): {err}", e.display_name()),
        }
    }

    fn ext_level(&self, src: &Src, ext_id: &str) -> Option<Level> {
        let map = self.ext_map.read().ok()?;
        let e = map.get(ext_id).filter(|e| e.enabled)?;
        match src {
            Src::Bg(id) => (id == ext_id).then_some(Level::Priv),
            Src::Tab(t) => {
                let tab = self.tab(*t)?;
                let url = tab.wv.url().unwrap_or_else(|_| tab.url.clone());
                if ext::ext_page_owner(&url) == Some(ext_id) {
                    return Some(Level::Priv);
                }
                let u = url::Url::parse(&url).ok()?;
                e.manifest
                    .content_scripts
                    .iter()
                    .any(|cs| matcher::script_applies(&cs.matches, &cs.exclude_matches, &cs.include_globs, &cs.exclude_globs, &u))
                    .then_some(Level::Content)
            }
            Src::Ui => None,
        }
    }

    fn on_ext_req(&mut self, src: Src, v: Value) {
        let ext_id = v["ext"].as_str().unwrap_or("").to_string();
        let op = v["op"].as_str().unwrap_or("").to_string();
        let rid = v["rid"].as_u64();
        let args: Vec<Value> = v["args"].as_array().cloned().unwrap_or_default();
        // Kimlik doğrulama: webview'a özel nonce
        let nonce_ok = self.target(&src).is_some_and(|(_, c)| v["n"].as_str() == Some(c.nonce.as_str()));
        if !nonce_ok {
            return;
        }
        let Some(level) = self.ext_level(&src, &ext_id) else {
            if let Some(rid) = rid {
                self.reply(&src, rid, Err("izin yok".into()));
            }
            return;
        };
        if op == "__resp" {
            self.on_resp(&src, &args);
            return;
        }
        let Some(rid) = rid else { return };
        if let Some(res) = self.run_op(&src, &ext_id, level, &op, args) {
            self.reply(&src, rid, res);
        } else if let Some(p) = self.pending.values_mut().find(|p| p.origin == src && p.rid == 0) {
            p.rid = rid; // ertelenmiş (mesaj iletimi): rid şimdi biliniyor
        }
    }

    fn reply(&self, src: &Src, rid: u64, r: Result<Value, String>) {
        let (ok, val) = match r {
            Ok(v) => (true, v),
            Err(e) => (false, Value::String(e)),
        };
        self.eval_in(src, "rx", &[json!(rid), json!(ok), val]);
    }

    fn on_resp(&mut self, src: &Src, args: &[Value]) {
        let Some(token) = args.first().and_then(Value::as_str) else { return };
        // Yalnızca mesajın asıl hedefi yanıt verebilir.
        if self.pending.get(token).is_none_or(|p| &p.target != src) {
            return;
        }
        let Some(p) = self.pending.remove(token) else { return };
        let ok = args.get(1).and_then(Value::as_bool).unwrap_or(false);
        let val = args.get(2).cloned().unwrap_or(Value::Null);
        let res = if ok { Ok(val) } else { Err(val.as_str().unwrap_or("hata").to_string()) };
        self.reply(&p.origin, p.rid, res);
    }

    /// Mesajı `target` bağlamına iletir; yanıt `__resp` ile gelir. `None` = ertelendi.
    fn forward(
        &mut self,
        origin: &Src,
        target: Src,
        ext_id: &str,
        msg: Value,
        sender: Value,
    ) -> Option<Result<Value, String>> {
        if self.ext_level(&target, ext_id).is_none() || self.target(&target).is_none() {
            return Some(Err(NO_RECEIVER.into()));
        }
        if self.pending.len() > 4096 {
            self.pending.clear();
        }
        let token = ext::rand_hex()[..16].to_string();
        self.pending.insert(token.clone(), Pending { origin: origin.clone(), rid: 0, target: target.clone() });
        self.eval_in(&target, "dispatch", &[json!(ext_id), msg, sender, json!(token)]);
        None
    }

    fn sender_for(&self, src: &Src, ext_id: &str) -> Value {
        let pub_id = self.ext_map.read().ok().and_then(|m| m.get(ext_id).map(|e| e.public_id())).unwrap_or_default();
        match src {
            Src::Tab(id) => match self.tab(*id) {
                Some(t) => json!({"id": pub_id, "url": t.url, "tab": self.tab_json(t), "frameId": 0}),
                None => json!({"id": pub_id}),
            },
            _ => json!({"id": pub_id}),
        }
    }

    fn resolve_ext_url(&self, ext_id: &str, u: &str) -> String {
        if u.contains("://") || u.starts_with("about:") {
            ext::to_real_url(u)
        } else {
            ext_url(ext_id, u)
        }
    }

    fn run_op(&mut self, src: &Src, ext_id: &str, level: Level, op: &str, args: Vec<Value>) -> Option<Result<Value, String>> {
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);

        if let Some(rest) = op.strip_prefix("storage.") {
            let (area, method) = rest.split_once('.')?;
            let res = match method {
                "get" => self.storage.get(ext_id, area, &arg(0)),
                "set" => match arg(0) {
                    Value::Object(o) => self.storage.set(ext_id, area, &o),
                    _ => Err("storage.set: nesne bekleniyor".into()),
                },
                "remove" => self.storage.remove(ext_id, area, &arg(0)),
                "clear" => self.storage.clear(ext_id, area),
                _ => Err(format!("desteklenmeyen işlem: {op}")),
            };
            return Some(match res {
                Ok(changes) => {
                    if matches!(method, "set" | "remove" | "clear") {
                        if changes.as_object().is_some_and(|m| !m.is_empty()) {
                            self.broadcast_storage(ext_id, area, &changes);
                        }
                        Ok(Value::Null)
                    } else {
                        Ok(changes)
                    }
                }
                Err(e) => Err(e),
            });
        }

        match op {
            "runtime.sendMessage" => {
                let sender = self.sender_for(src, ext_id);
                // İçerik/uzantı sayfası -> arka plan
                if matches!(src, Src::Bg(_)) {
                    return Some(Err(NO_RECEIVER.into()));
                }
                self.forward(src, Src::Bg(ext_id.to_string()), ext_id, arg(0), sender)
            }
            _ if level != Level::Priv => Some(Err(format!("'{op}' bu bağlamda kullanılamaz"))),
            "runtime.openOptionsPage" => {
                let path = self.ext_map.read().ok()?.get(ext_id)?.manifest.options_page_path().map(String::from);
                match path {
                    Some(p) => {
                        self.create_tab(&ext_url(ext_id, &p), true);
                        Some(Ok(Value::Null))
                    }
                    None => Some(Err("uzantının ayar sayfası yok".into())),
                }
            }
            "tabs.query" => {
                let f = arg(0);
                let url_pats: Vec<matcher::MatchPattern> = match &f["url"] {
                    Value::String(s) => matcher::MatchPattern::parse(s).into_iter().collect(),
                    Value::Array(a) => a.iter().filter_map(|s| s.as_str().and_then(matcher::MatchPattern::parse)).collect(),
                    _ => vec![],
                };
                let list: Vec<Value> = self
                    .tabs
                    .iter()
                    .filter(|t| f["active"].as_bool().is_none_or(|a| a == (t.id == self.active)))
                    .filter(|t| {
                        url_pats.is_empty()
                            || url::Url::parse(&t.url).is_ok_and(|u| url_pats.iter().any(|p| p.matches(&u)))
                    })
                    .map(|t| self.tab_json(t))
                    .collect();
                Some(Ok(Value::Array(list)))
            }
            "tabs.get" => Some(match arg(0).as_u64().and_then(|id| self.tab(id)) {
                Some(t) => Ok(self.tab_json(t)),
                None => Err("sekme bulunamadı".into()),
            }),
            "tabs.getCurrent" => Some(Ok(match src {
                Src::Tab(id) => self.tab(*id).map(|t| self.tab_json(t)).unwrap_or(Value::Null),
                _ => Value::Null,
            })),
            "tabs.create" => {
                let p = arg(0);
                let url = p["url"].as_str().map(|u| self.resolve_ext_url(ext_id, u)).unwrap_or_else(|| "ozi://newtab".into());
                let active = p["active"].as_bool().unwrap_or(true);
                let id = self.create_tab(&url, active);
                Some(match self.tab(id) {
                    Some(t) => Ok(self.tab_json(t)),
                    None => Err("sekme oluşturulamadı".into()),
                })
            }
            "tabs.update" => {
                let (id, props) = match arg(0) {
                    Value::Number(n) => (n.as_u64().unwrap_or(self.active), arg(1)),
                    other => (self.active, other),
                };
                if self.tab(id).is_none() {
                    return Some(Err("sekme bulunamadı".into()));
                }
                if let Some(u) = props["url"].as_str() {
                    let url = self.resolve_ext_url(ext_id, u);
                    if let Some(t) = self.tab_mut(id) {
                        t.url = url.clone();
                        let _ = t.wv.load_url(&url);
                    }
                }
                if props["active"].as_bool() == Some(true) {
                    self.switch_tab(id);
                }
                Some(Ok(self.tab(id).map(|t| self.tab_json(t)).unwrap_or(Value::Null)))
            }
            "tabs.remove" => {
                let ids: Vec<u64> = match arg(0) {
                    Value::Array(a) => a.iter().filter_map(Value::as_u64).collect(),
                    v => v.as_u64().into_iter().collect(),
                };
                for id in ids {
                    self.close_tab(id);
                }
                Some(Ok(Value::Null))
            }
            "tabs.reload" => {
                let id = arg(0).as_u64().unwrap_or(self.active);
                if let Some(t) = self.tab(id) {
                    let _ = t.wv.reload();
                }
                Some(Ok(Value::Null))
            }
            "tabs.sendMessage" => {
                let Some(tab_id) = arg(0).as_u64() else { return Some(Err("geçersiz tabId".into())) };
                let sender = self.sender_for(src, ext_id);
                self.forward(src, Src::Tab(tab_id), ext_id, arg(1), sender)
            }
            "tabs.executeScript" | "scripting.executeScript" | "tabs.insertCSS" | "scripting.insertCSS" => {
                Some(self.inject(ext_id, op, &args))
            }
            _ => Some(Err(format!("desteklenmeyen işlem: {op}"))),
        }
    }

    fn inject(&mut self, ext_id: &str, op: &str, args: &[Value]) -> Result<Value, String> {
        // tabs.*(tabId?, details) ya da scripting.*({target, ...})
        let (tab_id, details) = match args {
            [Value::Number(n), d, ..] => (n.as_u64(), d.clone()),
            [d, ..] => (d["target"]["tabId"].as_u64(), d.clone()),
            [] => (None, Value::Null),
        };
        let tab_id = tab_id.unwrap_or(self.active);
        let read = |p: &Value| -> Option<String> {
            let name = p.as_str()?;
            self.ext_map.read().ok()?.get(ext_id)?.read_string(name)
        };
        let is_css = op.ends_with("insertCSS");
        let mut code = String::new();
        if is_css {
            let css = details["css"].as_str().map(String::from).or_else(|| read(&details["file"])).unwrap_or_default();
            code = format!(
                "(function(){{var s=document.createElement('style');s.textContent={};(document.head||document.documentElement).appendChild(s);}})();",
                json!(css)
            );
        } else {
            if let Some(c) = details["code"].as_str() {
                code.push_str(c);
                code.push_str(";\n");
            }
            let files = match (&details["files"], &details["file"]) {
                (Value::Array(a), _) => a.clone(),
                (_, f) if f.is_string() => vec![f.clone()],
                _ => vec![],
            };
            for f in &files {
                code.push_str(&read(f).ok_or("betik dosyası okunamadı")?);
                code.push_str("\n;\n");
            }
        }
        let tab = self.tab(tab_id).ok_or("sekme bulunamadı")?;
        // Kullanıcı betiği hatası sayfayı bozmasın
        tab.wv
            .evaluate_script(&format!("try{{(function(){{\n{code}\n}})();}}catch(e){{console.error(e);}}"))
            .map_err(|e| e.to_string())?;
        Ok(if is_css { Value::Null } else { json!([{"frameId": 0, "result": null}]) })
    }

    fn broadcast_storage(&self, ext_id: &str, area: &str, changes: &Value) {
        let args = [json!(ext_id), changes.clone(), json!(area)];
        let mut targets: Vec<Src> = self.tabs.iter().map(|t| Src::Tab(t.id)).collect();
        targets.push(Src::Bg(ext_id.to_string()));
        for t in targets {
            if self.ext_level(&t, ext_id).is_some() {
                self.eval_in(&t, "storage", &args);
            }
        }
    }

    // ---------------------------------------------------------------- IPC (arayüz ve iç sayfalar)
    fn on_ipc(&mut self, src: Src, body: String) {
        let Ok(v) = serde_json::from_str::<Value>(&body) else { return };
        if v.get("z").is_some() {
            return self.on_ext_req(src, v);
        }
        // Tarayıcı komutları yalnızca arayüz çubuğundan ve ozi:// iç sayfalarından kabul edilir.
        let trusted = match &src {
            Src::Ui => true,
            Src::Tab(id) => self.tab(*id).is_some_and(|t| ext::is_internal(&t.wv.url().unwrap_or_default())),
            Src::Bg(_) => false,
        };
        if !trusted {
            return;
        }
        let from_tab = if let Src::Tab(id) = src { id } else { self.active };
        let id_arg = v["id"].as_u64();
        let s_arg = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match v["cmd"].as_str().unwrap_or("") {
            "ready" => self.push_ui(),
            "go" => self.navigate(from_tab, &s_arg("input")),
            "new_tab" => {
                let home = self.settings.read().map(|s| s.homepage.clone()).unwrap_or_default();
                self.create_tab(&home, true);
                let _ = self.ui.evaluate_script("window.oziUi&&window.oziUi.focusOmni()");
            }
            "close_tab" => {
                if let Some(id) = id_arg {
                    self.close_tab(id)
                }
            }
            "switch" => {
                if let Some(id) = id_arg {
                    self.switch_tab(id)
                }
            }
            "back" => self.eval_active("history.back()"),
            "forward" => self.eval_active("history.forward()"),
            "reload" => {
                if let Some(t) = self.tab(self.active) {
                    let _ = t.wv.reload();
                }
            }
            "ext_click" => self.ext_click(&s_arg("id")),
            "ext_options" => {
                let id = s_arg("id");
                let path = self.ext_map.read().ok().and_then(|m| m.get(&id).and_then(|e| e.manifest.options_page_path().map(String::from)));
                if let Some(p) = path {
                    self.create_tab(&ext_url(&id, &p), true);
                }
            }
            "ext_toggle" => {
                let id = s_arg("id");
                let on = self.ext_map.read().ok().and_then(|m| m.get(&id).map(|e| e.enabled)).unwrap_or(false);
                let _ = self.store.set_enabled(&id, !on);
                self.reload_extensions();
                self.reload_tab(from_tab);
            }
            "ext_remove" => {
                let id = s_arg("id");
                let _ = self.store.remove(&id);
                self.storage.forget(&id);
                self.reload_extensions();
                self.reload_tab(from_tab);
            }
            "install_file" => {
                let p = rfd::FileDialog::new()
                    .set_title("Uzantı paketi seç")
                    .add_filter("WebExtension", &["xpi", "crx", "zip"])
                    .pick_file();
                self.install(p, from_tab);
            }
            "install_dir" => {
                let p = rfd::FileDialog::new().set_title("Açılmış uzantı klasörünü seç").pick_folder();
                self.install(p, from_tab);
            }
            _ => {}
        }
    }

    fn eval_active(&self, js: &str) {
        if let Some(t) = self.tab(self.active) {
            let _ = t.wv.evaluate_script(js);
        }
    }

    fn reload_tab(&self, id: u64) {
        if let Some(t) = self.tab(id) {
            let _ = t.wv.reload();
        }
    }

    fn install(&mut self, path: Option<PathBuf>, from_tab: u64) {
        let Some(path) = path else { return };
        match self.store.install(&path) {
            Ok(e) => {
                self.just_installed.insert(e.id.clone());
                self.reload_extensions();
            }
            Err(err) => {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Uzantı yüklenemedi")
                    .set_description(err.to_string())
                    .show();
            }
        }
        self.reload_tab(from_tab);
    }

    fn ext_click(&mut self, id: &str) {
        let Some(e) = self.ext_map.read().ok().and_then(|m| m.get(id).cloned()) else { return };
        if let Some(popup) = e.manifest.toolbar_action().and_then(|a| a.default_popup.clone()).filter(|p| !p.is_empty()) {
            self.create_tab(&ext_url(id, &popup), true);
        } else if let Some(t) = self.tab(self.active) {
            let tj = self.tab_json(t);
            if let Some(b) = self.bg.get(id) {
                let _ = b.wv.evaluate_script(&b.ctx.call("fire", &[json!(id), json!("action.onClicked"), json!([tj])]));
            }
        }
    }

    fn on_user_event(&mut self, ev: UserEvent) {
        match ev {
            UserEvent::Ipc(src, body) => self.on_ipc(src, body),
            UserEvent::Title(id, title) => {
                if let Some(t) = self.tab_mut(id) {
                    t.title = title;
                }
                self.push_ui();
            }
            UserEvent::Nav(id, url) => {
                if let Some(t) = self.tab_mut(id) {
                    if !url.starts_with("about:") {
                        t.url = url;
                    }
                    t.loading = true;
                }
                self.push_ui();
            }
            UserEvent::Load(id, started, url) => {
                if let Some(t) = self.tab_mut(id) {
                    if !url.is_empty() && !url.starts_with("about:") {
                        t.url = url;
                    }
                    t.loading = started;
                    if started {
                        t.title.clear();
                    }
                }
                self.push_ui();
                if !started {
                    if let Some(t) = self.tab(id) {
                        let tj = self.tab_json(t);
                        self.fire_bg_all("tabs.onUpdated", json!([id, {"status": "complete", "url": t.url}, tj]));
                    }
                }
            }
            UserEvent::NewTab(url) => {
                self.create_tab(&url, true);
            }
            UserEvent::BgLoaded(id) => {
                let Some(b) = self.bg.get(&id) else { return };
                let fresh = self.just_installed.remove(&id);
                let (name, arg) = if fresh {
                    ("runtime.onInstalled", json!([{"reason": "install"}]))
                } else {
                    ("runtime.onStartup", json!([]))
                };
                let _ = b.wv.evaluate_script(&b.ctx.call("fire", &[json!(id), json!(name), arg]));
            }
        }
    }
}

fn main() {
    let paths = Paths::detect();
    for d in [&paths.config, &paths.data, &paths.profile_dir(), &paths.extensions_dir()] {
        let _ = std::fs::create_dir_all(d);
    }
    let settings_file = paths.settings_file();
    let settings = Settings::load(&settings_file);
    if !settings_file.exists() {
        let _ = settings.save(&settings_file);
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Ozi Browser")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .build(&event_loop)
        .expect("pencere oluşturulamadı");

    #[cfg(target_os = "linux")]
    let fixed = {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let fixed = gtk::Fixed::new();
        window.default_vbox().expect("GTK kutusu yok").pack_start(&fixed, true, true, 0);
        fixed.show_all();
        fixed
    };
    let scale = window.scale_factor();
    let phys = window.inner_size();
    let size = (phys.width as f64 / scale, phys.height as f64 / scale);
    let host = Host {
        window,
        #[cfg(target_os = "linux")]
        fixed,
    };

    let mut web_ctx = WebContext::new(Some(paths.profile_dir()));
    let ext_map: ext::ExtMap = Arc::new(RwLock::new(HashMap::new()));
    let settings_ref: pages::SettingsRef = Arc::new(RwLock::new(settings));

    let p_ui = proxy.clone();
    let need = protocols_needed(&web_ctx);
    let ui = build(&host, &mut web_ctx, |b| {
        with_protocols(b, need, &ext_map, &settings_ref)
            .with_html(include_str!("ui.html"))
            .with_bounds(rect(0.0, 0.0, size.0, UI_H))
            .with_ipc_handler(move |req| {
                let _ = p_ui.send_event(UserEvent::Ipc(Src::Ui, req.into_body()));
            })
    })
    .expect("arayüz oluşturulamadı");

    let mut app = App {
        host,
        web_ctx,
        proxy,
        ui,
        tabs: vec![],
        active: 0,
        next_id: 1,
        size,
        settings: settings_ref.clone(),
        store: ExtensionStore::new(paths.extensions_dir()),
        storage: ExtStorage::new(paths.storage_dir()),
        ext_map,
        bg: HashMap::new(),
        pending: HashMap::new(),
        just_installed: HashSet::new(),
        quit: false,
    };
    // Komut satırı: `ozi-browser [--install UZANTI]... [URL]...`
    let mut urls: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--install" {
            if let Some(p) = args.next() {
                match app.store.install(std::path::Path::new(&p)) {
                    Ok(e) => {
                        println!("uzantı yüklendi: {}", e.display_name());
                        app.just_installed.insert(e.id);
                    }
                    Err(err) => eprintln!("uzantı yüklenemedi ({p}): {err}"),
                }
            }
        } else if !a.starts_with("--") {
            urls.push(a);
        }
    }
    app.reload_extensions();
    if urls.is_empty() {
        urls.push(settings_ref.read().map(|s| s.homepage.clone()).unwrap_or_else(|_| "ozi://newtab".into()));
    }
    for u in urls {
        let s = settings_ref.read().map(|s| s.clone()).unwrap_or_default();
        let url = search::resolve(&u, &s.engines, &s.default_engine);
        app.create_tab(&url, true);
    }

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::Resized(sz), .. } => app.resize(sz),
            Event::UserEvent(ev) => {
                app.on_user_event(ev);
                if app.quit {
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
