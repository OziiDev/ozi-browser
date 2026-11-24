//! WebKitGTK davranış ayarları (Linux). Diğer platformlarda no-op.
//!
//! Donanım hızlandırma ilkesine bilerek dokunulmaz: `Always` bazı sürücülerde boş/bozuk çizime
//! yol açabilir; varsayılan "gerektiğinde" ilkesi güvenli ve yeterlidir.

#[cfg(target_os = "linux")]
pub fn tune(wv: &wry::WebView) {
    use webkit2gtk::{CacheModel, SettingsExt, WebContextExt, WebViewExt};
    use wry::WebViewExtUnix;
    let w = wv.webview();
    if let Some(s) = w.settings() {
        s.set_enable_smooth_scrolling(true);
        s.set_enable_page_cache(true); // geri/ileri anında
        s.set_enable_back_forward_navigation_gestures(true);
    }
    if let Some(c) = w.context() {
        // Tarayıcı önbellek modeli: daha büyük bellek/disk önbelleği, tekrar ziyaretler hızlı
        c.set_cache_model(CacheModel::WebBrowser);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn tune(_: &wry::WebView) {}
