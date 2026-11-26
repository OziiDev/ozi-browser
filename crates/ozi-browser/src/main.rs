//! Ozi Browser — Rust ile yazılmış, Chrome ve Firefox uzantılarını çalıştırabilen hafif tarayıcı.
//!
//! Katmanlar: `tao` (pencere) + `wry` (Linux: WebKitGTK, Windows: WebView2) + `ozi-core`
//! (arama, uzantı yükleme/eşleştirme/depolama).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod blocking;
mod ext;
mod keys;
mod layout;
mod pages;
mod tune;
mod webstate;

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

pub(crate) type Wv = WebView;

/// Üstteki arayüz çubuğunun (sekmeler + adres) mantıksal yüksekliği; sayfada ara çubuğu +40.
const UI_BASE_H: f64 = 86.0;
const FIND_H: f64 = 40.0;
const NO_RECEIVER: &str = "Could not establish connection. Receiving end does not exist.";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Src {
    Ui,
    Tab(u64),
    Bg(String),
    /// Adres çubuğu öneri listesi (üst katman webview'u)
    Popup,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
enum UserEvent {
    Ipc(Src, String),
    /// Linux: WebKit sinyali (sekme, yalnızca-ilerleme)
    TabState(u64, bool),
    /// Linux: webview'ların konteyneri (GtkFixed) yeniden yerleşti: x, y, genişlik, yükseklik
    FixedAlloc(i32, i32, i32, i32),
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
    progress: f64,
    can_back: bool,
    can_forward: bool,
    private: bool,
}

struct Bg {
    wv: WebView,
    ctx: WebCtx,
}

/// Adres çubuğu öneri satırı
struct Sug {
    kind: &'static str,
    title: String,
    sub: String,
    /// Ok tuşlarıyla seçilince adres çubuğuna yazılan metin
    text: String,
    /// Seçilince gidilecek adres
    url: String,
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

/// Platforma göre durum işleyicileri: Linux'ta WebKit sinyalleri (`webstate`), diğerlerinde wry olayları.
trait StateHandlers<'a> {
    fn pipe_state_handlers(
        self,
        id: u64,
        nav: EventLoopProxy<UserEvent>,
        title: EventLoopProxy<UserEvent>,
        load: EventLoopProxy<UserEvent>,
    ) -> WebViewBuilder<'a>;
}

impl<'a> StateHandlers<'a> for WebViewBuilder<'a> {
    #[cfg(target_os = "linux")]
    fn pipe_state_handlers(self, _: u64, _: EventLoopProxy<UserEvent>, _: EventLoopProxy<UserEvent>, _: EventLoopProxy<UserEvent>) -> WebViewBuilder<'a> {
        self
    }

    #[cfg(not(target_os = "linux"))]
    fn pipe_state_handlers(
        self,
        id: u64,
        nav: EventLoopProxy<UserEvent>,
        title: EventLoopProxy<UserEvent>,
        load: EventLoopProxy<UserEvent>,
    ) -> WebViewBuilder<'a> {
        self.with_navigation_handler(move |u| {
            let _ = nav.send_event(UserEvent::Nav(id, u));
            true
        })
        .with_document_title_changed_handler(move |t| {
            let _ = title.send_event(UserEvent::Title(id, t));
        })
        .with_on_page_load_handler(move |ev, u| {
            let _ = load.send_event(UserEvent::Load(id, matches!(ev, PageLoadEvent::Started), u));
        })
    }
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
    /// Konteynerin pencere içindeki konumu (GNOME gölge/başlık kenarlıkları yüzünden sıfır değildir)
    origin: (f64, f64),
    settings: pages::SettingsRef,
    user: pages::UserRef,
    paths: Paths,
    filter: Option<blocking::Filter>,
    find_open: bool,
    popup: Option<WebView>,
    /// Panel sayfası yüklenip `pop_ready` gönderdi mi
    popup_ready: bool,
    /// Panelin istenen konumu (konteyner koordinatları): x, y, genişlik, yükseklik
    popup_rect: Option<(f64, f64, f64, f64)>,
    /// Panelden sonra başka widget eklendi mi (panelin yeniden en üste alınması gerekir)
    popup_z_dirty: bool,
    /// GTK yeniden yerleştirince eşzamanlı uygulanacak webview hedefleri
    targets: layout::Targets,
    /// Hazır olmadan istenen son içerik betiği (hazır olunca çalıştırılır)
    popup_pending: Option<String>,
    sug: Vec<Sug>,
    sug_sel: Option<usize>,
    sug_q: String,
    sug_open: bool,
    /// Üst katman şu an menü mü gösteriyor (aksi halde öneri listesi)
    popup_menu: bool,
    menu_hidden_at: std::time::Instant,
    last_ui_push: std::cell::Cell<std::time::Instant>,
    /// Başlangıçtaki GTK koyu tema tercihi ("sistem" temasına dönmek için)
    orig_dark: bool,
    /// Son kapatılan (gizli olmayan) sekme adresleri; Ctrl+Shift+T ile geri açılır
    closed: Vec<String>,
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

    /// Konteyner koordinatlarını pencere koordinatlarına çevirir: wry, GtkFixed içindeki webview'ları
    /// doğrudan `size_allocate` ile yerleştirir ve konteynerin kendi ofsetini eklemez.
    fn at(&self, x: f64, y: f64, w: f64, h: f64) -> Rect {
        rect(x + self.origin.0, y + self.origin.1, w, h)
    }

    fn content_rect(&self) -> Rect {
        self.at(0.0, self.ui_h(), self.size.0, self.size.1 - self.ui_h())
    }

    fn layout(&self) {
        let (ox, oy) = self.origin;
        let (w, h) = self.size;
        let uh = self.ui_h();
        let r = |v: f64| v.round() as i32;
        let mut items: Vec<(&Wv, layout::Rect4)> = vec![];
        let _ = self.ui.set_bounds(self.at(0.0, 0.0, w, uh));
        items.push((&self.ui, (r(ox), r(oy), r(w), r(uh))));
        let content = (r(ox), r(oy + uh), r(w), r((h - uh).max(1.0)));
        for t in &self.tabs {
            let _ = t.wv.set_bounds(self.content_rect());
            items.push((&t.wv, content));
        }
        if let (Some(p), Some((x, y, pw, ph))) = (&self.popup, self.popup_rect) {
            let _ = p.set_bounds(self.at(x, y, pw, ph));
            items.push((p, (r(ox + x), r(oy + y), r(pw), r(ph))));
        }
        layout::set(&self.targets, items);
    }

    fn resize(&mut self, sz: PhysicalSize<u32>) {
        self.hide_suggestions();
        // Linux'ta boyut `FixedAlloc` ile konteynerden okunur (pencere boyutu gölge/başlığı da içerir)
        #[cfg(not(target_os = "linux"))]
        {
            let s = self.host.window.scale_factor();
            self.size = (sz.width as f64 / s, sz.height as f64 / s);
            self.layout();
        }
        #[cfg(target_os = "linux")]
        let _ = sz;
    }

    fn target(&self, src: &Src) -> Option<(&WebView, &WebCtx)> {
        match src {
            Src::Tab(id) => self.tab(*id).map(|t| (&t.wv, &t.ctx)),
            Src::Bg(id) => self.bg.get(id).map(|b| (&b.wv, &b.ctx)),
            Src::Ui | Src::Popup => None,
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
            .map(|t| json!({"id": t.id, "title": t.title, "url": ext::to_display_url(&t.url), "loading": t.loading, "progress": t.progress, "private": t.private,
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
        let (can_back, can_fwd) = self.tab(self.active).map(|t| (t.can_back, t.can_forward)).unwrap_or_default();
        let (theme, accent) = self.settings.read().map(|s| (s.theme.clone(), s.accent.clone())).unwrap_or_default();
        let state = json!({"tabs": tabs, "active": self.active, "exts": exts, "engines": engines,
            "bookmarked": bookmarked, "find": self.find_open, "back": can_back, "fwd": can_fwd,
            "theme": theme, "accent": accent, "accentHex": ozi_core::settings::accent_hex(&accent)});
        self.last_ui_push.set(std::time::Instant::now());
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
        self.hide_suggestions();
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
                .pipe_state_handlers(id, p_nav, p_title, p_load)
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
        self.popup_z_dirty = true;
        blocking::apply(&wv, self.filter);
        tune::tune(&wv);
        webstate::watch(&wv, id, self.proxy.clone());
        self.tabs.push(Tab { id, wv, ctx, url: real, title: String::new(), loading: true, progress: 0.0, can_back: false, can_forward: false, private });
        self.layout();
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
        self.hide_suggestions();
        for t in &self.tabs {
            let _ = t.wv.set_visible(t.id == id);
        }
        self.active = id;
        self.layout();
        if let Some(t) = self.tab(id) {
            let _ = t.wv.focus();
        }
        self.push_ui();
        self.fire_bg_all("tabs.onActivated", json!([{"tabId": id, "windowId": 1}]));
    }

    fn close_tab(&mut self, id: u64) {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else { return };
        let gone = self.tabs.remove(pos);
        if !gone.private && !is_internal_url(&gone.url) && !gone.url.is_empty() {
            self.closed.push(gone.url.clone());
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
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
        self.hide_suggestions();
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
            Src::Ui | Src::Popup => None,
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
        if src == Src::Popup {
            // Öneri listesi yalnızca "bu satırı seç" diyebilir.
            match v["cmd"].as_str() {
                Some("sug_pick") => {
                    if let Some(i) = v["i"].as_u64() {
                        self.sug_pick(i as usize);
                    }
                }
                Some("pop_ready") => {
                    self.popup_ready = true;
                    if let (Some(js), Some(p)) = (self.popup_pending.take(), &self.popup) {
                        let _ = p.evaluate_script(&js);
                    }
                }
                Some("menu_pick") => self.menu_pick(v["id"].as_str().unwrap_or("")),
                Some("menu_hide") => self.hide_suggestions(),
                _ => {}
            }
            return;
        }
        if v.get("z").is_some() {
            return self.on_ext_req(src, v);
        }
        // Tarayıcı komutları yalnızca arayüz çubuğundan ve ozi:// iç sayfalarından kabul edilir.
        let trusted = match &src {
            Src::Ui => true,
            Src::Tab(id) => self.tab(*id).is_some_and(|t| ext::is_internal(&t.wv.url().unwrap_or_default())),
            Src::Bg(_) | Src::Popup => false,
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
            "stop" => {
                if let Some(t) = self.tab(self.active) {
                    webstate::stop(&t.wv);
                }
            }
            "suggest" => {
                let q = s_arg("q");
                let list: Vec<Value> = self
                    .user
                    .lock()
                    .map(|u| u.suggest(&q, 12).into_iter().map(|(url, title)| json!({"url": url, "title": title})).collect())
                    .unwrap_or_default();
                if v["inline"].as_bool().unwrap_or(false) {
                    let _ = self.ui.evaluate_script(&format!(
                        "window.oziUi&&window.oziUi.suggest({},{})",
                        json!(q),
                        Value::Array(list)
                    ));
                }
                let (x, y, w) = (v["x"].as_f64().unwrap_or(70.0), v["y"].as_f64().unwrap_or(80.0), v["w"].as_f64().unwrap_or(500.0));
                self.show_suggestions(&q, x, y, w);
            }
            "sug_move" => self.sug_move(v["d"].as_i64().unwrap_or(1)),
            "hard_reload" => self.reload_active(true),
            "menu" => {
                // Düğmeye ikinci tıklama: açık menü önce odak kaybıyla kapanmış olabilir
                if (self.sug_open && self.popup_menu) || self.menu_hidden_at.elapsed().as_millis() < 250 {
                    self.hide_suggestions();
                } else {
                    self.show_menu(v["x"].as_f64().unwrap_or(self.size.0 - 10.0), v["y"].as_f64().unwrap_or(84.0));
                }
            }
            "sug_hide" => self.hide_suggestions(),
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
            "back" => self.nav_back(),
            "forward" => self.nav_forward(),
            "reload" => self.reload_active(false),
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
            UserEvent::TabState(id, progress_only) => self.sync_tab(id, progress_only),
            UserEvent::FixedAlloc(x, y, w, h) => {
                #[cfg(debug_assertions)]
                if std::env::var_os("OZI_DEBUG_GEOM").is_some() {
                    eprintln!("[alloc] {x},{y} {w}x{h}");
                }
                if w > 1 && h > 1 {
                    let new_size = (w as f64, h as f64);
                    if new_size != self.size {
                        self.hide_suggestions();
                    }
                    self.origin = (x as f64, y as f64);
                    self.size = new_size;
                    self.layout();
                }
            }
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

impl App {
    /// Panele betik gönder; sayfa henüz yüklenmediyse hazır olana kadar beklet.
    fn popup_eval(&mut self, js: String) {
        if self.popup_ready {
            if let Some(p) = &self.popup {
                let _ = p.evaluate_script(&js);
            }
        } else {
            self.popup_pending = Some(js);
        }
    }

    fn ensure_popup(&mut self) -> bool {
        if self.popup.is_some() {
            return true;
        }
        let p = self.proxy.clone();
        let need = protocols_needed(&self.web_ctx);
        let pctx = self.page_ctx();
        let result = build(&self.host, &mut self.web_ctx, |b| {
            with_protocols(b, need, &pctx)
                .with_html(include_str!("popup.html"))
                .with_visible(false)
                .with_bounds(rect(0.0, 0.0, 10.0, 10.0))
                // Kendi içeriğinden başka hiçbir yere gidemez
                .with_navigation_handler(|u| u.is_empty() || u.starts_with("about:") || u.starts_with("data:"))
                .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
                .with_ipc_handler(move |req| {
                    let _ = p.send_event(UserEvent::Ipc(Src::Popup, req.into_body()));
                })
        });
        match result {
            Ok(w) => {
                self.popup = Some(w);
                true
            }
            Err(e) => {
                eprintln!("öneri listesi oluşturulamadı: {e}");
                false
            }
        }
    }

    /// Paneli en üste al ve konumlandır (Fixed yeniden yerleşir; `layout` hedefleri yeniden uygular).
    fn place_popup(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.popup_rect = Some((x, y, w, h));
        // Her tuş vuruşunda çıkarıp yeniden eklemek WebKit yüzeyini yeniden kurup titretir:
        // yalnızca panel gizliyken ya da sonradan başka widget eklendiyse yükselt.
        if !self.sug_open || self.popup_z_dirty {
            self.raise_popup();
            self.popup_z_dirty = false;
        }
        self.layout();
    }

    /// GTK'de çocuk widget'lar eklenme sırasıyla çizilir; öneri listesini en üste almak için
    /// çıkarıp yeniden ekleriz (sonradan açılan sekmeler listeyi örtmesin).
    fn raise_popup(&self) {
        #[cfg(target_os = "linux")]
        if let Some(p) = &self.popup {
            use gtk::prelude::*;
            use wry::WebViewExtUnix;
            let w = p.webview();
            self.host.fixed.remove(&w);
            self.host.fixed.put(&w, 0, 0);
            #[cfg(debug_assertions)]
            {
                let kids = self.host.fixed.children();
                let pos = kids.iter().position(|c| c == w.upcast_ref::<gtk::Widget>());
                eprintln!("[debug] öneri listesi z-sırası: {:?}/{}", pos.map(|p| p + 1), kids.len());
            }
        }
    }

    fn hide_suggestions(&mut self) {
        if self.sug_open {
            if let Some(p) = &self.popup {
                let _ = p.set_visible(false);
            }
            self.sug_open = false;
            if self.popup_menu {
                self.menu_hidden_at = std::time::Instant::now();
                if let Some(t) = self.tab(self.active) {
                    let _ = t.wv.focus();
                }
            }
            self.popup_menu = false;
        }
        self.sug.clear();
        self.sug_sel = None;
    }

    fn show_suggestions(&mut self, q: &str, x: f64, y: f64, w: f64) {
        let q = q.trim();
        if q.is_empty() {
            return self.hide_suggestions();
        }
        let s = self.settings.read().map(|s| s.clone()).unwrap_or_default();
        let engine = s.engines.iter().find(|e| e.id == s.default_engine).or(s.engines.first()).map(|e| e.name.clone()).unwrap_or_default();
        let mut items = vec![Sug {
            kind: "search",
            title: q.to_string(),
            sub: format!("{engine} ile ara"),
            text: q.to_string(),
            url: q.to_string(),
        }];
        if let Ok(u) = self.user.lock() {
            for (url, title) in u.suggest(q, 6) {
                let kind = if u.is_bookmarked(&url) { "bookmark" } else { "history" };
                let shown = url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string();
                items.push(Sug {
                    kind,
                    title: if title.is_empty() { shown.clone() } else { title },
                    sub: shown,
                    text: url.clone(),
                    url,
                });
            }
        }
        if !self.ensure_popup() {
            return;
        }
        let h = items.len() as f64 * 37.0 + 10.0;
        let payload: Vec<Value> = items.iter().map(|i| json!({"kind": i.kind, "title": i.title, "sub": i.sub})).collect();
        self.sug = items;
        self.sug_sel = None;
        self.sug_q = q.to_string();
        self.place_popup(x, y, w.max(240.0), h);
        let theme = self.popup_theme_js();
        if let Some(p) = &self.popup {
            let _ = p.set_visible(true);
        }
        self.popup_eval(format!("{theme}window.oziPop&&window.oziPop.set({},-1)", Value::Array(payload)));
        self.popup_menu = false;
        self.sug_open = true;
    }

    fn sug_move(&mut self, d: i64) {
        let n = self.sug.len();
        if n == 0 || !self.sug_open {
            return;
        }
        self.sug_sel = match (self.sug_sel, d > 0) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) if i + 1 < n => Some(i + 1),
            (Some(i), false) if i > 0 => Some(i - 1),
            _ => None,
        };
        let text = self.sug_sel.map(|i| self.sug[i].text.clone()).unwrap_or_else(|| self.sug_q.clone());
        if let Some(p) = &self.popup {
            let _ = p.evaluate_script(&format!("window.oziPop&&window.oziPop.select({})", self.sug_sel.map(|i| i as i64).unwrap_or(-1)));
        }
        let _ = self.ui.evaluate_script(&format!("window.oziUi&&window.oziUi.fill({})", json!(text)));
    }

    fn sug_pick(&mut self, i: usize) {
        let Some(url) = self.sug.get(i).map(|s| s.url.clone()) else { return };
        self.navigate(self.active, &url);
        let _ = self.ui.evaluate_script("window.oziUi&&window.oziUi.blurOmni()");
    }
}

impl App {
    fn nav_back(&self) {
        if let Some(t) = self.tab(self.active) {
            webstate::back(&t.wv);
        }
    }

    fn nav_forward(&self) {
        if let Some(t) = self.tab(self.active) {
            webstate::forward(&t.wv);
        }
    }

    fn reload_active(&self, hard: bool) {
        if let Some(t) = self.tab(self.active) {
            webstate::reload(&t.wv, hard);
        }
    }

    /// WebKit sinyali geldiğinde sekme durumunu gerçek değerlerden yeniden oku.
    fn sync_tab(&mut self, id: u64, progress_only: bool) {
        let Some(t) = self.tab(id) else { return };
        let snap = webstate::snapshot(&t.wv);
        let (old_url, old_loading) = (t.url.clone(), t.loading);
        let Some(t) = self.tab_mut(id) else { return };
        if let Some(u) = snap.url.filter(|u| !u.is_empty() && u != "about:blank") {
            t.url = u;
        }
        t.title = snap.title.clone();
        t.loading = snap.loading;
        t.progress = snap.progress;
        t.can_back = snap.can_back;
        t.can_forward = snap.can_forward;
        let url = t.url.clone();
        let private = t.private;
        #[cfg(debug_assertions)]
        if std::env::var_os("OZI_DEBUG_STATE").is_some() {
            eprintln!(
                "[state] tab {id} loading={} progress={:.2} back={} fwd={} url={url}",
                snap.loading, snap.progress, snap.can_back, snap.can_forward
            );
        }
        let finished = old_loading && !snap.loading;
        let url_changed = url != old_url;
        if !private && !is_internal_url(&url) && !snap.title.is_empty() {
            if let Ok(mut u) = self.user.lock() {
                u.set_title(&url, &snap.title);
            }
        }
        if finished {
            self.record_visit(id);
        }
        if finished || url_changed {
            if let Some(t) = self.tab(id) {
                let tj = self.tab_json(t);
                let change = if finished { json!({"status": "complete", "url": t.url}) } else { json!({"url": t.url}) };
                self.fire_bg_all("tabs.onUpdated", json!([id, change, tj]));
            }
        }
        // İlerleme çubuğu güncellemelerini kıs; diğer her değişiklik hemen yansır
        if progress_only && self.last_ui_push.get().elapsed().as_millis() < 50 {
            return;
        }
        self.push_ui();
    }

    /// Tema: GTK'nin koyu tercihi sayfalardaki `prefers-color-scheme`'i ve yerel pencereleri etkiler.
    fn apply_theme(&self) {
        if let Ok(s) = self.settings.read() {
            pages::set_theme(&s.theme, &s.accent);
        }
        #[cfg(target_os = "linux")]
        {
            use gtk::prelude::GtkSettingsExt;
            if let Some(gs) = gtk::Settings::default() {
                let theme = self.settings.read().map(|s| s.theme.clone()).unwrap_or_default();
                gs.set_gtk_application_prefer_dark_theme(match theme.as_str() {
                    "dark" => true,
                    "light" => false,
                    _ => self.orig_dark,
                });
            }
        }
    }
}

impl App {
    fn popup_theme_js(&self) -> String {
        let (theme, accent) = self.settings.read().map(|s| (s.theme.clone(), s.accent.clone())).unwrap_or_default();
        format!("window.oziPop&&window.oziPop.theme({},{});", json!(theme), json!(ozi_core::settings::accent_hex(&accent)))
    }

    /// Hamburger menüsü: (kimlik, etiket, kısayol, simge); kimlik "-" ayırıcıdır.
    fn show_menu(&mut self, x_right: f64, y: f64) {
        const MENU: &[(&str, &str, &str, &str)] = &[
            ("newtab", "Yeni sekme", "Ctrl+T", "plus"),
            ("private", "Yeni gizli sekme", "Ctrl+Shift+N", "private"),
            ("-", "", "", ""),
            ("history", "Geçmiş", "Ctrl+H", "history"),
            ("bookmarks", "Yer imleri", "", "bookmark"),
            ("downloads", "İndirmeler", "Ctrl+J", "download"),
            ("extensions", "Uzantılar", "", "ext"),
            ("-", "", "", ""),
            ("find", "Sayfada ara", "Ctrl+F", "find"),
            ("print", "Yazdır…", "", "print"),
            ("devtools", "Geliştirici araçları", "F12", "code"),
            ("fullscreen", "Tam ekran", "F11", "full"),
            ("-", "", "", ""),
            ("settings", "Ayarlar", "", "settings"),
            ("quit", "Çık", "", "quit"),
        ];
        if !self.ensure_popup() {
            return;
        }
        self.hide_suggestions();
        let rows = MENU.iter().filter(|m| m.0 != "-").count() as f64;
        let seps = MENU.len() as f64 - rows;
        let (w, h) = (270.0, rows * 32.0 + seps * 11.0 + 16.0);
        let x = (x_right - w).max(6.0);
        let payload: Vec<Value> = MENU
            .iter()
            .map(|(id, label, key, icon)| {
                if *id == "-" {
                    json!({"sep": true})
                } else {
                    json!({"id": id, "label": label, "key": key, "icon": icon})
                }
            })
            .collect();
        self.place_popup(x, y, w, h);
        let theme = self.popup_theme_js();
        if let Some(p) = &self.popup {
            let _ = p.set_visible(true);
            let _ = p.focus();
        }
        self.popup_eval(format!("{theme}window.oziPop&&window.oziPop.menu({})", Value::Array(payload)));
        self.popup_menu = true;
        self.sug_open = true;
    }

    fn menu_pick(&mut self, id: &str) {
        self.hide_suggestions();
        use keys::Action::*;
        match id {
            "newtab" => self.on_shortcut(NewTab),
            "private" => self.on_shortcut(PrivateTab),
            "history" => self.open_internal("ozi://history"),
            "bookmarks" => self.open_internal("ozi://bookmarks"),
            "downloads" => self.open_internal("ozi://downloads"),
            "extensions" => self.open_internal("ozi://extensions"),
            "settings" => self.open_internal("ozi://settings"),
            "find" => self.on_shortcut(Find),
            "print" => {
                if let Some(t) = self.tab(self.active) {
                    let _ = t.wv.print();
                }
            }
            "devtools" => self.on_shortcut(DevTools),
            "fullscreen" => self.on_shortcut(Fullscreen),
            "quit" => self.quit = true,
            _ => {}
        }
    }
}

fn is_internal_url(u: &str) -> bool {
    ext::is_internal(u) || u.starts_with(ext::EXT_BASE)
}

impl App {
    /// Kapanış: pencereyi hemen gizle (dock/görev çubuğu simgesi anında kalksın), verileri kaydet ve
    /// WebKit'in yavaş temizliğine (birkaç webview + uzantı arka planı ~6 sn sürebiliyordu) en fazla
    /// 2 sn tanı; sonra süreci kesin sonlandır.
    fn shutdown(&mut self) {
        self.host.window.set_visible(false);
        self.save_window_state();
        self.flush();
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(2));
            std::process::exit(0);
        });
    }

    fn flush(&mut self) {
        if let Ok(mut u) = self.user.lock() {
            u.flush();
        }
        self.save_session();
        self.last_flush = std::time::Instant::now();
    }

    /// Pencere boyutunu kaydet (bir sonraki açılışta aynı boyutta açılır).
    fn save_window_state(&self) {
        let (w, h) = self.size;
        let v = json!({"w": w.round(), "h": h.round(), "max": self.host.window.is_maximized()});
        let _ = std::fs::create_dir_all(&self.paths.config);
        let _ = std::fs::write(self.paths.window_file(), v.to_string());
    }

    /// Gizli olmayan sekmeleri kaydet (ayar açıksa).
    fn save_session(&self) {
        if !self.settings.read().is_ok_and(|s| s.restore_session) {
            return;
        }
        let keep = |t: &&Tab| {
            !t.private && {
                let u = ext::to_display_url(&t.url);
                u.starts_with("http://") || u.starts_with("https://") || u.starts_with("file://") || u.starts_with("ozi://")
            }
        };
        let urls: Vec<String> = self.tabs.iter().filter(keep).map(|t| ext::to_display_url(&t.url)).collect();
        let active = self.tabs.iter().filter(keep).position(|t| t.id == self.active).unwrap_or(0);
        let _ = std::fs::create_dir_all(self.paths.data.join("userdata"));
        let _ = std::fs::write(self.paths.session_file(), json!({"tabs": urls, "active": active}).to_string());
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
            Reload => self.reload_active(false),
            HardReload => self.reload_active(true),
            Back => self.nav_back(),
            Forward => self.nav_forward(),
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
            ReopenTab => {
                if let Some(url) = self.closed.pop() {
                    self.create_tab(&url, true);
                }
            }
            MoveTabLeft | MoveTabRight => {
                if let Some(i) = self.tabs.iter().position(|t| t.id == self.active) {
                    let j = if a == MoveTabLeft { i.checked_sub(1) } else { (i + 1 < self.tabs.len()).then_some(i + 1) };
                    if let Some(j) = j {
                        self.tabs.swap(i, j);
                        self.push_ui();
                    }
                }
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
            restore_session: v["restore_session"].as_bool().unwrap_or(false),
            theme: v["theme"].as_str().filter(|t| ozi_core::settings::THEMES.contains(t)).unwrap_or("system").to_string(),
            rendering: v["rendering"]
                .as_str()
                .filter(|r| ozi_core::settings::RENDER_MODES.contains(r))
                .unwrap_or("auto")
                .to_string(),
            accent: v["accent"]
                .as_str()
                .filter(|a| ozi_core::settings::ACCENTS.iter().any(|(id, _)| id == a))
                .unwrap_or("blue")
                .to_string(),
        };
        new.save(&self.paths.settings_file()).map_err(|e| e.to_string())?;
        if let Ok(mut s) = self.settings.write() {
            *s = new;
        }
        self.apply_theme();
        self.push_ui();
        self.rebuild_filter(false);
        Ok(())
    }
}

/// WebKit çizim yolunu seçer; WebKit başlamadan önce çağrılmalıdır. Kullanıcı ortam değişkenini
/// kendisi verdiyse dokunulmaz; `OZI_RENDER=auto|compat|fast` ayarı geçersiz kılar.
///
/// Neden: WebKitGTK'nın DMABUF çizim yolu, Wayland'da NVIDIA'nın kapalı kaynak sürücüsüyle (özellikle
/// Intel+NVIDIA melez ekran kartlarında) bozuk, titreyen ya da yanlış arka plan çizimlerine yol açıyor.
fn apply_render_env(setting: &str) {
    let mode = std::env::var("OZI_RENDER").unwrap_or_else(|_| setting.to_string());
    let set = |k: &str| {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, "1");
        }
    };
    match mode.as_str() {
        "fast" => {}
        "compat" => {
            set("WEBKIT_DISABLE_DMABUF_RENDERER");
            set("WEBKIT_DISABLE_COMPOSITING_MODE");
        }
        _ => set("WEBKIT_DISABLE_DMABUF_RENDERER"),
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

    let (win_w, win_h, win_max) = std::fs::read(paths.window_file())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| {
            (
                v["w"].as_f64().unwrap_or(1280.0).clamp(640.0, 7680.0),
                v["h"].as_f64().unwrap_or(800.0).clamp(420.0, 4320.0),
                v["max"].as_bool().unwrap_or(false),
            )
        })
        .unwrap_or((0.0, 0.0, false));
    apply_render_env(&settings.rendering);
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Varsayılan boyut ekrana sığmalı (1366x768 gibi küçük ekranlarda 1280x800 taşıyordu)
    let (win_w, win_h) = if win_w > 0.0 {
        (win_w, win_h)
    } else {
        event_loop
            .primary_monitor()
            .map(|m| {
                let sc = m.scale_factor();
                (m.size().width as f64 / sc, m.size().height as f64 / sc)
            })
            .map(|(mw, mh)| ((mw - 80.0).clamp(640.0, 1280.0), (mh - 150.0).clamp(420.0, 800.0)))
            .unwrap_or((1280.0, 800.0))
    };
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Ozi Browser")
        .with_inner_size(LogicalSize::new(win_w, win_h))
        .with_maximized(win_max)
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
    let size = (win_w, win_h);
    let host = Host {
        window,
        #[cfg(target_os = "linux")]
        fixed,
    };

    // Konteyner yerleştikçe (pencere boyutu, GNOME gölgeleri, büyütme) yerleşimi güncelle
    let targets = layout::new();
    #[cfg(target_os = "linux")]
    {
        let p = proxy.clone();
        layout::install(&host.fixed, targets.clone(), move |x, y, w, h| {
            let _ = p.send_event(UserEvent::FixedAlloc(x, y, w, h));
        });
    }

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
        size: (win_w, win_h),
        origin: (0.0, 0.0),
        settings: settings_ref.clone(),
        user,
        paths: paths.clone(),
        filter: None,
        find_open: false,
        popup: None,
        popup_ready: false,
        popup_rect: None,
        popup_z_dirty: true,
        targets,
        popup_pending: None,
        sug: vec![],
        sug_sel: None,
        sug_q: String::new(),
        sug_open: false,
        popup_menu: false,
        menu_hidden_at: std::time::Instant::now(),
        last_ui_push: std::cell::Cell::new(std::time::Instant::now()),
        orig_dark: false,
        closed: vec![],
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
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::GtkSettingsExt;
        app.orig_dark = gtk::Settings::default().is_some_and(|s| s.is_gtk_application_prefer_dark_theme());
    }
    app.apply_theme();
    app.ensure_popup(); // sayfa ilk kullanımdan önce yüklensin
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
    let mut restore_active = 0usize;
    if urls.is_empty() && settings_ref.read().is_ok_and(|s| s.restore_session) {
        if let Some(v) = std::fs::read(paths.session_file()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            urls = v["tabs"]
                .as_array()
                .map(|a| a.iter().filter_map(|u| u.as_str().map(String::from)).take(60).collect())
                .unwrap_or_default();
            restore_active = v["active"].as_u64().unwrap_or(0) as usize;
        }
    }
    if urls.is_empty() {
        urls.push(settings_ref.read().map(|s| s.homepage.clone()).unwrap_or_else(|_| "ozi://newtab".into()));
    }
    let restored = urls.len() > 1;
    for (i, u) in urls.into_iter().enumerate() {
        let s = settings_ref.read().map(|s| s.clone()).unwrap_or_default();
        let url = search::resolve(&u, &s.engines, &s.default_engine);
        app.create_tab_ex(&url, !restored || i == restore_active, private);
    }

    // Menü / öneri listesini gerçek IPC yolundan, pencere yerleştikten sonra aç (yalnızca hata ayıklama)
    #[cfg(all(debug_assertions, target_os = "linux"))]
    {
        let cmd = if std::env::var_os("OZI_DEBUG_MENU").is_some() {
            Some(r#"{"cmd":"menu","x":1260,"y":84}"#.to_string())
        } else {
            std::env::var("OZI_DEBUG_SUGGEST")
                .ok()
                .map(|q| format!(r#"{{"cmd":"suggest","q":{},"x":70,"y":82,"w":640,"inline":false}}"#, json!(q)))
        };
        if let Some(c) = cmd {
            let p = app.proxy.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                let _ = p.send_event(UserEvent::Ipc(Src::Ui, c));
            });
        }
    }

    // Gerçek WebKit çizimini PNG'ye kaydet (yalnızca hata ayıklama): OZI_DEBUG_SHOT=<klasör>
    // Masaüstünü değil, yalnızca uygulamanın kendi webview içeriğini yakalar.
    #[cfg(all(debug_assertions, target_os = "linux"))]
    if let Ok(dir) = std::env::var("OZI_DEBUG_SHOT") {
        use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};
        use wry::WebViewExtUnix;
        let mut views: Vec<(String, webkit2gtk::WebView)> = vec![("ui".into(), app.ui.webview())];
        for (i, t) in app.tabs.iter().enumerate() {
            views.push((format!("tab{i}"), t.wv.webview()));
        }
        if let Some(p) = &app.popup {
            views.push(("popup".into(), p.webview()));
        }
        let _ = std::fs::create_dir_all(&dir);
        // OZI_DEBUG_UI_JS: arayüzde betik çalıştırıp animasyonun ara karelerini kaydet
        if let Ok(js) = std::env::var("OZI_DEBUG_UI_JS") {
            let ui_w = app.ui.webview();
            let dir2 = dir.clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || {
                let _ = ui_w.run_javascript(&js, None::<&gtk::gio::Cancellable>, |_| {});
                for (ms, tag) in [(70u64, "a"), (200, "b"), (700, "c")] {
                    let (w2, d2) = (ui_w.clone(), dir2.clone());
                    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
                        let path = format!("{d2}/anim_{tag}.png");
                        w2.snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&gtk::gio::Cancellable>, move |res| {
                            if let Ok(surf) = res {
                                if let Ok(img) = gtk::cairo::ImageSurface::try_from(surf) {
                                    if let Ok(mut f) = std::fs::File::create(&path) {
                                        let _ = img.write_to_png(&mut f);
                                    }
                                }
                            }
                        });
                    });
                }
            });
        }
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            for (name, w) in views {
                let path = format!("{dir}/{name}.png");
                w.snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&gtk::gio::Cancellable>, move |res| match res {
                    Ok(surf) => match gtk::cairo::ImageSurface::try_from(surf) {
                        Ok(img) => match std::fs::File::create(&path).map(|mut f| img.write_to_png(&mut f)) {
                            Ok(Ok(())) => eprintln!("[shot] {path} {}x{}", img.width(), img.height()),
                            other => eprintln!("[shot] yazılamadı {path}: {other:?}"),
                        },
                        Err(_) => eprintln!("[shot] {path}: görüntü yüzeyi değil"),
                    },
                    Err(e) => eprintln!("[shot] {path}: {e}"),
                });
            }
        });
    }

    // Kapanış yolu denemesi (yalnızca hata ayıklama): OZI_DEBUG_CLOSE=close|quit
    #[cfg(all(debug_assertions, target_os = "linux"))]
    if let Ok(mode) = std::env::var("OZI_DEBUG_CLOSE") {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let win = app.host.window.gtk_window().clone();
        let p = app.proxy.clone();
        gtk::glib::timeout_add_seconds_local_once(4, move || {
            eprintln!("[close] {mode} tetikleniyor");
            if mode == "close" {
                win.close();
            } else {
                let _ = p.send_event(UserEvent::Ipc(Src::Popup, r#"{"cmd":"menu_pick","id":"quit"}"#.into()));
            }
        });
    }

    // Yerleşim ölçümü (yalnızca hata ayıklama): pencere ve widget konumları
    #[cfg(all(debug_assertions, target_os = "linux"))]
    if std::env::var_os("OZI_DEBUG_GEOM").is_some() {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let win = app.host.window.gtk_window().clone();
        let vbox = app.host.window.default_vbox().cloned();
        let fixed = app.host.fixed.clone();
        gtk::glib::timeout_add_seconds_local_once(6, move || {
            let a = |w: &gtk::Widget| {
                let al = w.allocation();
                let (x, y) = w.translate_coordinates(&win, 0, 0).unwrap_or((-9999, -9999));
                format!("alloc=({},{} {}x{}) win-rel=({x},{y})", al.x(), al.y(), al.width(), al.height())
            };
            eprintln!("[geom] pencere {}x{} ", win.allocated_width(), win.allocated_height());
            if let Some(v) = &vbox {
                eprintln!("[geom] vbox   {}", a(v.upcast_ref()));
            }
            eprintln!("[geom] fixed  {}", a(fixed.upcast_ref()));
            for (i, c) in fixed.children().iter().enumerate() {
                eprintln!("[geom] cocuk{i} {} visible={}", a(c), c.is_visible());
            }
        });
    }

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                app.shutdown();
                *flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::Resized(sz), .. } => app.resize(sz),
            Event::UserEvent(ev) => {
                app.on_user_event(ev);
                if app.quit {
                    app.shutdown();
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
