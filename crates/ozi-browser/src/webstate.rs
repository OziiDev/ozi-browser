//! Sekme durumunu WebKit'in kendi sinyalleriyle izler (Linux).
//!
//! wry'nin `navigation_handler`'ı alt çerçeve (iframe) gezinmelerinde de tetiklenir ve sayfa
//! yüklemesi olmayan gezinmelerde (`#bağlantı`, indirme) "bitti" olayı gelmez; bu yüzden adres ve
//! yükleme durumu bu olaylardan türetilirse yanlış adres / takılı yükleme göstergesi çıkar.
//! WebKit'in `uri` / `is-loading` / `estimated-load-progress` özellikleri yalnızca ana çerçeveyi
//! yansıtır; geri/ileri durumu da buradan okunur.

use crate::{UserEvent, Wv};
use tao::event_loop::EventLoopProxy;

/// Anlık sekme durumu
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub url: Option<String>,
    pub title: String,
    pub loading: bool,
    pub progress: f64,
    pub can_back: bool,
    pub can_forward: bool,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use webkit2gtk::WebViewExt;
    use wry::WebViewExtUnix;

    pub fn watch(wv: &Wv, id: u64, proxy: EventLoopProxy<UserEvent>) {
        let w = wv.webview();
        // `true` = yalnızca ilerleme değişti (arayüz güncellemesi kısılabilir)
        let send = move |p: &EventLoopProxy<UserEvent>, progress_only: bool| {
            let _ = p.send_event(UserEvent::TabState(id, progress_only));
        };
        let p = proxy.clone();
        w.connect_uri_notify(move |_| send(&p, false));
        let p = proxy.clone();
        w.connect_title_notify(move |_| send(&p, false));
        let p = proxy.clone();
        w.connect_is_loading_notify(move |_| send(&p, false));
        let p = proxy.clone();
        w.connect_estimated_load_progress_notify(move |_| send(&p, true));
        // Geri/ileri listesi commit'te değişir
        w.connect_load_changed(move |_, _| send(&proxy, false));
    }

    pub fn snapshot(wv: &Wv) -> Snapshot {
        let w = wv.webview();
        Snapshot {
            url: w.uri().map(|u| u.to_string()),
            title: w.title().map(|t| t.to_string()).unwrap_or_default(),
            loading: w.is_loading(),
            progress: w.estimated_load_progress(),
            can_back: w.can_go_back(),
            can_forward: w.can_go_forward(),
        }
    }

    pub fn back(wv: &Wv) {
        wv.webview().go_back();
    }
    pub fn forward(wv: &Wv) {
        wv.webview().go_forward();
    }
    pub fn reload(wv: &Wv, hard: bool) {
        let w = wv.webview();
        if hard {
            w.reload_bypass_cache();
        } else {
            w.reload();
        }
    }
    pub fn stop(wv: &Wv) {
        wv.webview().stop_loading();
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    pub fn watch(_: &Wv, _: u64, _: EventLoopProxy<UserEvent>) {}
    pub fn snapshot(_: &Wv) -> Snapshot {
        Snapshot::default()
    }
    pub fn back(wv: &Wv) {
        let _ = wv.evaluate_script("history.back()");
    }
    pub fn forward(wv: &Wv) {
        let _ = wv.evaluate_script("history.forward()");
    }
    pub fn reload(wv: &Wv, _hard: bool) {
        let _ = wv.reload();
    }
    pub fn stop(wv: &Wv) {
        let _ = wv.evaluate_script("window.stop()");
    }
}

pub use imp::*;
