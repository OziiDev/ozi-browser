//! Ozi Browser — Rust ile yazılmış, Chrome ve Firefox uzantılarını çalıştırabilen hafif tarayıcı.
//!
//! Katmanlar: `tao` (pencere) + `wry` (Linux: WebKitGTK, Windows: WebView2) + `ozi-core`
//! (arama, uzantı yükleme/eşleştirme/depolama).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod blocking;
mod ext;
mod keys;
mod pages;

use ext::{ext_url, WebCtx, BACKGROUND_PAGE};
use ozi_core::{
    extension::Extension,
    matcher,
    search,
    settings::{Paths, Settings},
    storage::ExtStorage,
    store::ExtensionStore,
    userdata::UserData,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tao::{
    dpi::{LogicalSize, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

/// Üstteki arayüz çubuğunun (sekmeler + adres) mantıksal yüksekliği; sayfada ara çubuğu +40.
const UI_BASE_H: f64 = 86.0;
const FIND_H: f64 = 40.0;
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
    DownloadStart(String, String),
    DownloadDone(String, Option<String>, bool),
    Shortcut(keys::Action),
    FilterReady(Option<usize>),
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
    private: bool,
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

fn with_protocols<'a>(b: WebViewBuilder<'a>, need: bool, pages_ctx: &pages::PageCtx) -> WebViewBuilder<'a> {
    if !need {
        return b;
    }
    let (c1, m2) = (pages_ctx.clone(), pages_ctx.exts.clone());
    b.with_custom_protocol("ozi".into(), move |_, req| pages::serve(&c1, &req))
        .with_custom_protocol("ozi-ext".into(), move |_, req| ext::serve_ext(&m2, &req))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    user: pages::UserRef,
    paths: Paths,
    filter: Option<blocking::Filter>,
    find_open: bool,
    last_flush: std::time::Instant,
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

    fn ui_h(&self) -> f64 {
        UI_BASE_H + if self.find_open { FIND_H } else { 0.0 }
    }

    fn content_rect(&self) -> Rect {
        rect(0.0, self.ui_h(), self.size.0, self.size.1 - self.ui_h())
    }

    fn layout(&self) {
        let _ = self.ui.set_bounds(rect(0.0, 0.0, self.size.0, self.ui_h()));
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
            .map(|t| json!({"id": t.id, "title": t.title, "url": ext::to_display_url(&t.url), "loading": t.loading, "private": t.private,
                "favicon": !t.private && (t.url.starts_with("http://") || t.url.starts_with("https://"))}))
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
        let bookmarked = self
            .tab(self.active)
            .zip(self.user.lock().ok())
            .is_some_and(|(t, u)| u.is_bookmarked(&t.url));
        let engines: Vec<Value> = self
            .settings
            .read()
            .map(|s| {
                s.engines
                    .iter()
                    .map(|e| json!({"name": e.name, "keyword": e.keyword, "default": e.id == s.default_engine}))
                    .collect()
            })
            .unwrap_or_default();
        let state = json!({"tabs": tabs, "active": self.active, "exts": exts, "engines": engines,
            "bookmarked": bookmarked, "find": self.find_open});
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
    fn page_ctx(&self) -> pages::PageCtx {
        pages::PageCtx { exts: self.ext_map.clone(), settings: self.settings.clone(), user: self.user.clone() }
    }

    fn create_tab(&mut self, url: &str, activate: bool) -> u64 {
        self.create_tab_ex(url, activate, false)
    }

    /// `private`: kalıcı veri yok (çerez/önbellek), geçmişe yazılmaz, uzantı çalışmaz.
    fn create_tab_ex(&mut self, url: &str, activate: bool, private: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let real = ext::to_real_url(url);
        let ctx = WebCtx::new();
        let enabled = if private { vec![] } else { self.enabled_exts() };
        let refs: Vec<&Extension> = enabled.iter().collect();
        let scripts = if private { vec![] } else { ext::init_scripts(&refs, &ctx) };
        let bounds = self.content_rect();
        let p = self.proxy.clone();
        let (p_ipc, p_nav, p_title, p_load, p_new) = (p.clone(), p.clone(), p.clone(), p.clone(), p.clone());
        let (p_dl, p_dl2) = (p.clone(), p);
        let need = protocols_needed(&self.web_ctx);
        let pctx = self.page_ctx();
        let result = build(&self.host, &mut self.web_ctx, |mut b| {
            for sc in scripts {
                b = b.with_initialization_script(sc);
            }
            with_protocols(b, need, &pctx)
                .with_incognito(private)
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
                .with_download_started_handler(move |url, path| {
                    if let (Some(dir), Some(name)) = (dirs::download_dir(), path.file_name()) {
                        *path = unique_path(dir.join(name));
                    }
                    let _ = p_dl.send_event(UserEvent::DownloadStart(url, path.to_string_lossy().to_string()));
                    true
                })
                .with_download_completed_handler(move |url, path, ok| {
                    let _ = p_dl2.send_event(UserEvent::DownloadDone(
                        url,
                        path.map(|p| p.to_string_lossy().to_string()),
                        ok,
                    ));
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
        blocking::apply(&wv, self.filter);
        self.tabs.push(Tab { id, wv, ctx, url: real, title: String::new(), loading: true, private });
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
    fn reload_extensions(&mut self, startup: bool) {
        let all = self.store.load_all();
        if let Ok(mut m) = self.ext_map.write() {
            *m = all.into_iter().map(|e| (e.id.clone(), e)).collect();
        }
        self.bg.clear();
        self.pending.clear();
        for e in self.enabled_exts() {
            self.start_background(&e);
        }
        self.rebuild_filter(startup);
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
        let pctx = self.page_ctx();
        let result = build(&self.host, &mut self.web_ctx, |mut b| {
            for sc in scripts {
                b = b.with_initialization_script(sc);
            }
            with_protocols(b, need, &pctx)
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
        // `None` = yanıt ertelendi (mesaj iletimi); `__resp` geldiğinde `rid` ile cevaplanır.
        if let Some(res) = self.run_op(&src, &ext_id, level, &op, rid, args) {
            self.reply(&src, rid, res);
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
        rid: u64,
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
        self.pending.insert(token.clone(), Pending { origin: origin.clone(), rid, target: target.clone() });
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

    fn run_op(&mut self, src: &Src, ext_id: &str, level: Level, op: &str, rid: u64, args: Vec<Value>) -> Option<Result<Value, String>> {
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
                self.forward(src, rid, Src::Bg(ext_id.to_string()), ext_id, arg(0), sender)
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
                self.forward(src, rid, Src::Tab(tab_id), ext_id, arg(1), sender)
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
            "private_tab" => self.on_shortcut(keys::Action::PrivateTab),
            "stop" => self.eval_active("window.stop()"),
            "suggest" => {
                let q = s_arg("q");
                let list: Vec<Value> = self
                    .user
                    .lock()
                    .map(|u| u.suggest(&q, 12).into_iter().map(|(url, title)| json!({"url": url, "title": title})).collect())
                    .unwrap_or_default();
                let _ = self.ui.evaluate_script(&format!(
                    "window.oziUi&&window.oziUi.suggest({},{})",
                    json!(q),
                    Value::Array(list)
                ));
            }
            "bookmark" => self.toggle_bookmark(),
            "find_toggle" => self.set_find(!self.find_open),
            "find" => self.find(&s_arg("q"), v["back"].as_bool().unwrap_or(false), v["fresh"].as_bool().unwrap_or(false)),
            "bm_remove" => {
                if let Ok(mut u) = self.user.lock() {
                    u.remove_bookmark(&s_arg("url"));
                }
                self.reload_tab(from_tab);
            }
            "hist_remove" => {
                if let Ok(mut u) = self.user.lock() {
                    u.remove_history(&s_arg("url"));
                }
                self.reload_tab(from_tab);
            }
            "hist_clear" => {
                if let Ok(mut u) = self.user.lock() {
                    u.clear_history();
                }
                self.reload_tab(from_tab);
            }
            "dl_clear" => {
                if let Ok(mut u) = self.user.lock() {
                    u.clear_downloads();
                }
                self.reload_tab(from_tab);
            }
            "dl_show" => self.show_download(&s_arg("path")),
            "settings_save" => match self.save_settings(&v["settings"]) {
                Ok(()) => self.reload_tab(from_tab),
                Err(e) => {
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Ayarlar kaydedilemedi")
                        .set_description(e)
                        .show();
                }
            },
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
                self.reload_extensions(false);
                self.reload_tab(from_tab);
            }
            "ext_remove" => {
                let id = s_arg("id");
                let _ = self.store.remove(&id);
                self.storage.forget(&id);
                self.reload_extensions(false);
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
                self.reload_extensions(false);
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
                let mut record = None;
                if let Some(t) = self.tab_mut(id) {
                    t.title = title.clone();
                    if !t.private {
                        record = Some(t.url.clone());
                    }
                }
                if let Some(url) = record.filter(|u| !is_internal_url(u)) {
                    if let Ok(mut u) = self.user.lock() {
                        u.set_title(&url, &title);
                    }
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
                    self.record_visit(id);
                    if let Some(t) = self.tab(id) {
                        let tj = self.tab_json(t);
                        self.fire_bg_all("tabs.onUpdated", json!([id, {"status": "complete", "url": t.url}, tj]));
                    }
                }
            }
            UserEvent::DownloadStart(url, path) => {
                if let Ok(mut u) = self.user.lock() {
                    u.download_started(&url, &path, now_secs());
                }
            }
            UserEvent::DownloadDone(url, path, ok) => {
                if let Ok(mut u) = self.user.lock() {
                    u.download_finished(&url, path.as_deref(), ok);
                }
            }
            UserEvent::Shortcut(a) => self.on_shortcut(a),
            UserEvent::FilterReady(raw) => {
                // SAFETY: işaretçi bu süreçte `blocking::compile` ile üretildi ve ana iş parçacığına taşındı.
                let f = raw.map(|r| unsafe { blocking::Filter::from_raw(r) });
                self.set_filter(f);
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

fn is_internal_url(u: &str) -> bool {
    ext::is_internal(u) || u.starts_with(ext::EXT_BASE)
}

impl App {
    fn flush(&mut self) {
        if let Ok(mut u) = self.user.lock() {
            u.flush();
        }
        self.last_flush = std::time::Instant::now();
    }

    fn record_visit(&mut self, id: u64) {
        let Some(t) = self.tab(id) else { return };
        if t.private || is_internal_url(&t.url) {
            return;
        }
        let (url, title) = (t.url.clone(), t.title.clone());
        if let Ok(mut u) = self.user.lock() {
            u.add_visit(&url, &title, now_secs());
        }
        if self.last_flush.elapsed().as_secs() > 10 {
            self.flush();
        }
    }

    fn toggle_bookmark(&mut self) {
        let Some(t) = self.tab(self.active) else { return };
        if is_internal_url(&t.url) {
            return;
        }
        let (url, title) = (t.url.clone(), t.title.clone());
        if let Ok(mut u) = self.user.lock() {
            u.toggle_bookmark(&url, &title, now_secs());
        }
        self.push_ui();
    }

    fn set_find(&mut self, open: bool) {
        if self.find_open == open {
            if open {
                let _ = self.ui.focus();
                self.push_ui();
            }
            return;
        }
        self.find_open = open;
        self.layout();
        if open {
            let _ = self.ui.focus();
        } else {
            self.eval_active("window.getSelection&&window.getSelection().removeAllRanges()");
            if let Some(t) = self.tab(self.active) {
                let _ = t.wv.focus();
            }
        }
        self.push_ui();
    }

    fn find(&self, q: &str, back: bool, fresh: bool) {
        let clear = "window.getSelection&&window.getSelection().removeAllRanges();";
        if q.is_empty() {
            return self.eval_active(clear);
        }
        let js = format!(
            "{}window.find({},false,{},true,false,false,false);",
            if fresh { clear } else { "" },
            json!(q),
            back
        );
        self.eval_active(&js);
    }

    fn show_download(&self, path: &str) {
        // Yalnızca indirme listesindeki yollar açılır.
        let known = self.user.lock().is_ok_and(|u| u.downloads.iter().any(|d| d.path == path));
        let Some(dir) = std::path::Path::new(path).parent().filter(|_| known) else { return };
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }

    fn open_internal(&mut self, url: &str) {
        let real = ext::to_real_url(url);
        match self.tabs.iter().find(|t| t.url == real).map(|t| t.id) {
            Some(id) => self.switch_tab(id),
            None => {
                self.create_tab(url, true);
            }
        }
    }

    fn on_shortcut(&mut self, a: keys::Action) {
        use keys::Action::*;
        let focus_omni = |s: &Self| {
            let _ = s.ui.focus();
            let _ = s.ui.evaluate_script("window.oziUi&&window.oziUi.focusOmni()");
        };
        let rotate = |s: &mut Self, delta: isize| {
            let n = s.tabs.len() as isize;
            if let Some(i) = s.tabs.iter().position(|t| t.id == s.active) {
                let id = s.tabs[(i as isize + delta).rem_euclid(n) as usize].id;
                s.switch_tab(id);
            }
        };
        match a {
            NewTab | PrivateTab => {
                let home = self.settings.read().map(|s| s.homepage.clone()).unwrap_or_else(|_| "ozi://newtab".into());
                if a == PrivateTab {
                    self.create_tab_ex("ozi://newtab", true, true);
                } else {
                    self.create_tab(&home, true);
                }
                focus_omni(self);
            }
            CloseTab => self.close_tab(self.active),
            FocusOmni => focus_omni(self),
            Reload => self.reload_tab(self.active),
            Back => self.eval_active("history.back()"),
            Forward => self.eval_active("history.forward()"),
            NextTab => rotate(self, 1),
            PrevTab => rotate(self, -1),
            GotoTab(i) => {
                if let Some(id) = self.tabs.get(i).map(|t| t.id) {
                    self.switch_tab(id);
                }
            }
            GotoLastTab => {
                if let Some(id) = self.tabs.last().map(|t| t.id) {
                    self.switch_tab(id);
                }
            }
            Bookmark => self.toggle_bookmark(),
            History => self.open_internal("ozi://history"),
            Downloads => self.open_internal("ozi://downloads"),
            Find => self.set_find(true),
            DevTools => {
                if let Some(t) = self.tab(self.active) {
                    t.wv.open_devtools();
                }
            }
            Fullscreen => {
                let on = self.host.window.fullscreen().is_some();
                self.host
                    .window
                    .set_fullscreen(if on { None } else { Some(tao::window::Fullscreen::Borderless(None)) });
            }
            Escape => {
                if self.find_open {
                    self.set_find(false);
                }
            }
        }
    }

    /// Yerleşik liste + uzantıların declarativeNetRequest statik kurallarından WebKit filtresi kurar.
    /// `startup`: olay döngüsü başlamadan önce eşzamanlı derle; sonra eşzamansız (yeniden giriş yok).
    fn rebuild_filter(&mut self, startup: bool) {
        let block = self.settings.read().map(|s| s.block_ads).unwrap_or(true);
        let mut rulesets: Vec<Value> = vec![];
        for e in self.enabled_exts() {
            let Some(d) = &e.manifest.declarative_net_request else { continue };
            for r in d.rule_resources.iter().filter(|r| r.enabled) {
                if let Some(v) = e.read_string(&r.path).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                    rulesets.push(v);
                }
            }
        }
        let dir = self.paths.data.join("filters");
        match ozi_core::blocklist::build_filter(block, &rulesets) {
            None => self.set_filter(None),
            Some(json) if startup => {
                let f = blocking::compile_blocking(&dir, &json);
                self.set_filter(f);
            }
            Some(json) => {
                let p = self.proxy.clone();
                blocking::compile(&dir, &json, move |f| {
                    let _ = p.send_event(UserEvent::FilterReady(f.map(|f| f.to_raw())));
                });
            }
        }
    }

    fn set_filter(&mut self, f: Option<blocking::Filter>) {
        let old = std::mem::replace(&mut self.filter, f);
        for t in &self.tabs {
            blocking::apply(&t.wv, f);
        }
        if let Some(o) = old {
            blocking::release(o);
        }
    }

    fn save_settings(&mut self, v: &Value) -> Result<(), String> {
        let list = v["engines"].as_array().ok_or("arama motoru listesi yok")?;
        let mut engines: Vec<search::SearchEngine> = vec![];
        for e in list {
            let name = e["name"].as_str().unwrap_or("").trim().to_string();
            let kw = e["keyword"].as_str().unwrap_or("").trim().to_string();
            let url = e["url"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() || kw.is_empty() || kw.len() > 12 || !kw.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(format!("'{name}': ad boş olamaz, anahtar harf/rakam olmalı (en çok 12)"));
            }
            if !url.contains("%s") || !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("'{name}': URL http(s) ile başlamalı ve %s içermeli"));
            }
            if engines.iter().any(|x| x.keyword.eq_ignore_ascii_case(&kw)) {
                return Err(format!("'{kw}' anahtarı birden fazla motorda kullanılıyor"));
            }
            let mut id = e["id"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| {
                let slug: String = name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                if slug.is_empty() { "ozel".into() } else { slug }
            });
            while engines.iter().any(|x| x.id == id) {
                id.push('2');
            }
            engines.push(search::SearchEngine { id, name, keyword: kw, url });
        }
        if engines.is_empty() {
            return Err("en az bir arama motoru gerekli".into());
        }
        let default_engine = v["default_engine"]
            .as_str()
            .filter(|d| engines.iter().any(|e| e.id == *d))
            .map(String::from)
            .unwrap_or_else(|| engines[0].id.clone());
        let homepage = v["homepage"]
            .as_str()
            .map(str::trim)
            .filter(|h| *h == "ozi://newtab" || h.starts_with("https://") || h.starts_with("http://"))
            .unwrap_or("ozi://newtab")
            .to_string();
        let new = Settings {
            default_engine,
            engines,
            homepage,
            block_ads: v["block_ads"].as_bool().unwrap_or(true),
        };
        new.save(&self.paths.settings_file()).map_err(|e| e.to_string())?;
        if let Ok(mut s) = self.settings.write() {
            *s = new;
        }
        self.rebuild_filter(false);
        Ok(())
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
    let user: pages::UserRef = Arc::new(Mutex::new(UserData::load(paths.userdata_dir())));
    let pctx = pages::PageCtx { exts: ext_map.clone(), settings: settings_ref.clone(), user: user.clone() };

    let p_ui = proxy.clone();
    let need = protocols_needed(&web_ctx);
    let ui = build(&host, &mut web_ctx, |b| {
        with_protocols(b, need, &pctx)
            .with_html(include_str!("ui.html"))
            .with_bounds(rect(0.0, 0.0, size.0, UI_BASE_H))
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
        user,
        paths: paths.clone(),
        filter: None,
        find_open: false,
        last_flush: std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap_or_else(std::time::Instant::now),
        store: ExtensionStore::new(paths.extensions_dir()),
        storage: ExtStorage::new(paths.storage_dir()),
        ext_map,
        bg: HashMap::new(),
        pending: HashMap::new(),
        just_installed: HashSet::new(),
        quit: false,
    };
    let p_keys = app.proxy.clone();
    keys::install(&app.host.window, move |a| {
        let _ = p_keys.send_event(UserEvent::Shortcut(a));
    });
    // Komut satırı: `ozi-browser [--private] [--install UZANTI]... [URL]...`
    let mut urls: Vec<String> = Vec::new();
    let mut private = false;
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
        } else if a == "--private" {
            private = true;
        } else if !a.starts_with("--") {
            urls.push(a);
        }
    }
    app.reload_extensions(true);
    if urls.is_empty() {
        urls.push(settings_ref.read().map(|s| s.homepage.clone()).unwrap_or_else(|_| "ozi://newtab".into()));
    }
    for u in urls {
        let s = settings_ref.read().map(|s| s.clone()).unwrap_or_default();
        let url = search::resolve(&u, &s.engines, &s.default_engine);
        app.create_tab_ex(&url, true, private);
    }

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                app.flush();
                *flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::Resized(sz), .. } => app.resize(sz),
            Event::UserEvent(ev) => {
                app.on_user_event(ev);
                if app.quit {
                    app.flush();
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
