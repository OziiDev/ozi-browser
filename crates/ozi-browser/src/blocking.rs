//! WebKitGTK içerik filtresi (reklam/izleyici engelleme). Yalnızca Linux; diğer platformlarda no-op.

#[cfg(target_os = "linux")]
mod imp {
    use gtk::glib::{self, translate::*};
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::{Duration, Instant};
    use webkit2gtk::WebViewExt;
    use webkit2gtk_sys as ffi;
    use wry::WebViewExtUnix;

    /// Derlenmiş WebKit içerik filtresi (yalnızca ana iş parçacığında kullanılır).
    #[derive(Clone, Copy)]
    pub struct Filter(*mut ffi::WebKitUserContentFilter);

    type Slot = Rc<RefCell<Option<Option<Filter>>>>;

    struct Ctx {
        cb: Box<dyn FnOnce(Option<Filter>)>,
        store: *mut ffi::WebKitUserContentFilterStore,
        _bytes: glib::Bytes,
    }

    impl Filter {
        pub fn to_raw(self) -> usize {
            self.0 as usize
        }
        /// # Safety
        /// `raw`, `to_raw` ile aynı süreçte üretilmiş geçerli bir filtre olmalı.
        pub unsafe fn from_raw(raw: usize) -> Self {
            Filter(raw as *mut ffi::WebKitUserContentFilter)
        }
    }

    unsafe extern "C" fn done(
        _src: *mut glib::gobject_ffi::GObject,
        res: *mut gtk::gio::ffi::GAsyncResult,
        data: glib::ffi::gpointer,
    ) {
        let ctx = Box::from_raw(data as *mut Ctx);
        let mut err: *mut glib::ffi::GError = std::ptr::null_mut();
        let f = ffi::webkit_user_content_filter_store_save_finish(ctx.store, res, &mut err);
        let out = if f.is_null() {
            if !err.is_null() {
                let e: glib::Error = from_glib_full(err);
                eprintln!("içerik filtresi derlenemedi: {e}");
            }
            None
        } else {
            Some(Filter(f))
        };
        glib::gobject_ffi::g_object_unref(ctx.store as *mut _);
        (ctx.cb)(out);
    }

    /// Kuralları eşzamansız derler; `cb` GLib ana döngüsünde çağrılır.
    pub fn compile(store_dir: &Path, json: &str, cb: impl FnOnce(Option<Filter>) + 'static) {
        let prepared = (|| {
            std::fs::create_dir_all(store_dir).ok()?;
            Some((CString::new(store_dir.to_str()?).ok()?, CString::new("ozi-rules").ok()?))
        })();
        let Some((path, ident)) = prepared else { return cb(None) };
        let bytes = glib::Bytes::from_owned(json.as_bytes().to_vec());
        unsafe {
            let store = ffi::webkit_user_content_filter_store_new(path.as_ptr());
            let stash = bytes.to_glib_none();
            let ctx = Box::new(Ctx { cb: Box::new(cb), store, _bytes: bytes.clone() });
            ffi::webkit_user_content_filter_store_save(
                store,
                ident.as_ptr(),
                stash.0,
                std::ptr::null_mut(),
                Some(done),
                Box::into_raw(ctx) as *mut _,
            );
        }
    }

    /// Başlangıçta (olay döngüsü başlamadan önce) güvenle kullanılabilir: ana döngüyü
    /// döndürerek bekler. Çalışma sırasında kullanmayın (yeniden giriş riski) — `compile` kullanın.
    pub fn compile_blocking(store_dir: &Path, json: &str) -> Option<Filter> {
        let slot: Slot = Rc::new(RefCell::new(None));
        let s2 = slot.clone();
        compile(store_dir, json, move |f| *s2.borrow_mut() = Some(f));
        let start = Instant::now();
        while slot.borrow().is_none() && start.elapsed() < Duration::from_secs(5) {
            gtk::main_iteration_do(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        let r = *slot.borrow();
        r.flatten()
    }

    /// Webview'daki filtreleri değiştirir (None = hepsini kaldır).
    pub fn apply(wv: &wry::WebView, filter: Option<Filter>) {
        let Some(mgr) = wv.webview().user_content_manager() else { return };
        let p: *mut ffi::WebKitUserContentManager = mgr.to_glib_none().0;
        unsafe {
            ffi::webkit_user_content_manager_remove_all_filters(p);
            if let Some(Filter(f)) = filter {
                ffi::webkit_user_content_manager_add_filter(p, f);
            }
        }
    }

    pub fn release(f: Filter) {
        unsafe { ffi::webkit_user_content_filter_unref(f.0) }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    #[derive(Clone, Copy)]
    pub struct Filter;
    impl Filter {
        pub fn to_raw(self) -> usize {
            0
        }
        /// # Safety
        /// Windows'ta filtre yoktur; çağrı güvenlidir.
        pub unsafe fn from_raw(_: usize) -> Self {
            Filter
        }
    }
    pub fn compile(_: &std::path::Path, _: &str, cb: impl FnOnce(Option<Filter>) + 'static) {
        cb(None)
    }
    pub fn compile_blocking(_: &std::path::Path, _: &str) -> Option<Filter> {
        None
    }
    pub fn apply(_: &wry::WebView, _: Option<Filter>) {}
    pub fn release(_: Filter) {}
}

pub use imp::*;
