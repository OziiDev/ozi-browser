//! ozi-core: arayüzden bağımsız tarayıcı mantığı (arama, uzantılar, depolama, ayarlar).
//! WebKit/WebView2 gerektirmez; tek başına derlenir ve test edilir.

pub mod extension;
pub mod manifest;
pub mod matcher;
pub mod search;
pub mod settings;
pub mod storage;
pub mod store;
