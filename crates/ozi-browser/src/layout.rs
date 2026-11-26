//! Webview yerleşimini GTK yeniden yerleştirdikten hemen sonra eşzamanlı olarak yeniden uygular (Linux).
//!
//! `GtkFixed` her `size_allocate`'te çocuklarını doğal boyutlarına sıfırlar (çocuk eklenip çıkarılınca,
//! gösterilip gizlenince, pencere boyutu değişince). wry ise webview'ları doğrudan `size_allocate` ile
//! yerleştirir; sıfırlamayı bilmez. Sonuç: örneğin açılır panel pencere boyutunda dev bir dikdörtgen
//! olarak çizilip arayüzü örter. Burada her webview'ın istenen konum/boyutu saklanır ve Fixed'in
//! `size-allocate` işleyicisinde (sınıf işleyicisinden sonra, çizimden önce) tekrar uygulanır.

use crate::Wv;

/// (x, y, genişlik, yükseklik) — pencere koordinatlarında
pub type Rect4 = (i32, i32, i32, i32);

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use wry::WebViewExtUnix;

    pub type Targets = Rc<RefCell<Vec<(gtk::Widget, Rect4)>>>;

    pub fn new() -> Targets {
        Rc::new(RefCell::new(Vec::new()))
    }

    /// Fixed yeniden yerleşince: önce kayıtlı hedefleri uygula, sonra `on_alloc` ile bildir.
    pub fn install(fixed: &gtk::Fixed, targets: Targets, on_alloc: impl Fn(i32, i32, i32, i32) + 'static) {
        fixed.connect_size_allocate(move |_, a| {
            for (w, (x, y, wd, h)) in targets.borrow().iter() {
                if w.is_visible() {
                    w.size_allocate(&gtk::Allocation::new(*x, *y, *wd, *h));
                }
            }
            on_alloc(a.x(), a.y(), a.width(), a.height());
        });
    }

    pub fn set(targets: &Targets, items: Vec<(&Wv, Rect4)>) {
        *targets.borrow_mut() = items.into_iter().map(|(wv, r)| (wv.webview().upcast::<gtk::Widget>(), r)).collect();
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    pub type Targets = ();
    pub fn new() -> Targets {}
    pub fn set(_: &Targets, _: Vec<(&Wv, Rect4)>) {}
}

pub use imp::*;
