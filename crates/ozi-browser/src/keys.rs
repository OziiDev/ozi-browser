//! Linux: pencere düzeyinde kısayol tuşları. GTK3'te üst düzey pencere tuş olayını odaktaki
//! widget'tan (WebKit sayfası dahil) önce görür; böylece Ctrl+T gibi kısayollar sayfa
//! odaktayken de çalışır. (Windows'ta yalnızca arayüz çubuğundaki JS kısayolları vardır.)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    PrivateTab,
    CloseTab,
    FocusOmni,
    Reload,
    Back,
    Forward,
    NextTab,
    PrevTab,
    GotoTab(usize),
    GotoLastTab,
    Bookmark,
    History,
    Downloads,
    Find,
    DevTools,
    Fullscreen,
    Escape,
}

#[cfg(target_os = "linux")]
pub fn install(window: &tao::window::Window, send: impl Fn(Action) + 'static) {
    use gtk::gdk::{keys::constants as k, ModifierType};
    use gtk::glib::Propagation;
    use gtk::prelude::*;
    use tao::platform::unix::WindowExtUnix;

    window.gtk_window().connect_key_press_event(move |_, ev| {
        let st = ev.state();
        let ctrl = st.contains(ModifierType::CONTROL_MASK);
        let shift = st.contains(ModifierType::SHIFT_MASK);
        let alt = st.contains(ModifierType::MOD1_MASK);
        let key = ev.keyval();
        let ch = key.to_unicode().map(|c| c.to_ascii_lowercase());
        let act = if ctrl && !alt {
            match (ch, shift) {
                (Some('t'), false) => Some(Action::NewTab),
                (Some('n'), true) => Some(Action::PrivateTab),
                (Some('w'), _) => Some(Action::CloseTab),
                (Some('l'), _) => Some(Action::FocusOmni),
                (Some('r'), _) => Some(Action::Reload),
                (Some('d'), false) => Some(Action::Bookmark),
                (Some('h'), false) => Some(Action::History),
                (Some('j'), false) => Some(Action::Downloads),
                (Some('f'), false) => Some(Action::Find),
                (Some('9'), false) => Some(Action::GotoLastTab),
                (Some(c @ '1'..='8'), false) => Some(Action::GotoTab(c as usize - '1' as usize)),
                _ if key == k::Tab || key == k::ISO_Left_Tab => {
                    Some(if shift { Action::PrevTab } else { Action::NextTab })
                }
                _ if key == k::Page_Down => Some(Action::NextTab),
                _ if key == k::Page_Up => Some(Action::PrevTab),
                _ => None,
            }
        } else if alt && !ctrl {
            if key == k::Left {
                Some(Action::Back)
            } else if key == k::Right {
                Some(Action::Forward)
            } else {
                None
            }
        } else if key == k::F5 {
            Some(Action::Reload)
        } else if key == k::F11 {
            Some(Action::Fullscreen)
        } else if key == k::F12 {
            Some(Action::DevTools)
        } else if key == k::Escape {
            Some(Action::Escape) // tüketilmez: sayfa da Escape'i görsün
        } else {
            None
        };
        match act {
            Some(Action::Escape) => {
                send(Action::Escape);
                Propagation::Proceed
            }
            Some(a) => {
                send(a);
                Propagation::Stop
            }
            None => Propagation::Proceed,
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn install(_window: &tao::window::Window, _send: impl Fn(Action) + 'static) {}
