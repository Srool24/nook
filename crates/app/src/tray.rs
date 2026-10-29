//! System tray icon and menu.
//!
//! On Linux the icon is an AppIndicator (GNOME shows it with the
//! "AppIndicator and KStatusNotifierItem Support" extension, which Fedora
//! packages as `gnome-shell-extension-appindicator`). tray-icon drives it
//! through GTK, so the icon and its menu live on a GTK thread of their own
//! and [`Tray`] forwards changes there.

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use nook_ui::geometry::Squircle;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Settings,
    TogglePin,
    ToggleOnTop,
    ToggleStartup,
    /// Download and install the release offered in the menu.
    Update,
    Quit,
}

#[cfg(windows)]
pub struct Tray {
    _icon: TrayIcon,
    menu: Menu,
    pin: CheckMenuItem,
    on_top: CheckMenuItem,
    startup: CheckMenuItem,
    /// "Update to x.y.z", at the top of the menu while an update is offered.
    update: MenuItem,
    update_sep: PredefinedMenuItem,
    /// Version the update item currently shows.
    update_shown: Option<String>,
}

#[cfg(windows)]
impl Tray {
    /// Must run on the thread that pumps Win32 messages (the UI thread).
    pub fn new(
        tx: Sender<TrayCommand>,
        ctx: egui::Context,
        startup_on: bool,
        on_top_on: bool,
    ) -> Result<Self> {
        let settings = MenuItem::with_id("settings", "Settings", true, None);
        let pin = CheckMenuItem::with_id("pin", "Pin open", true, false, None);
        let on_top = CheckMenuItem::with_id("ontop", "Always on top", true, on_top_on, None);
        let startup =
            CheckMenuItem::with_id("startup", "Launch at startup", true, startup_on, None);
        let quit = MenuItem::with_id("quit", "Quit Nook", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &settings,
            &pin,
            &on_top,
            &startup,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .context("building tray menu")?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let cmd = match event.id.as_ref() {
                "settings" => TrayCommand::Settings,
                "pin" => TrayCommand::TogglePin,
                "ontop" => TrayCommand::ToggleOnTop,
                "startup" => TrayCommand::ToggleStartup,
                "update" => TrayCommand::Update,
                "quit" => TrayCommand::Quit,
                _ => return,
            };
            if tx.send(cmd).is_ok() {
                ctx.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip("Nook")
            .with_icon(tray_icon()?)
            .build()
            .context("creating tray icon")?;
        Ok(Self {
            _icon: icon,
            menu,
            pin,
            on_top,
            startup,
            update: MenuItem::with_id("update", "Update", true, None),
            update_sep: PredefinedMenuItem::separator(),
            update_shown: None,
        })
    }

    /// Shows "Update to `version`" at the top of the menu, or removes it.
    pub fn set_update(&mut self, version: Option<&str>) {
        if self.update_shown.as_deref() == version {
            return;
        }
        let was_shown = self.update_shown.is_some();
        match version {
            Some(v) => {
                self.update.set_text(format!("Update to {v}"));
                if !was_shown
                    && let Err(e) = self.menu.insert_items(&[&self.update, &self.update_sep], 0)
                {
                    tracing::warn!("tray update item: {e}");
                    return;
                }
            }
            None => {
                let _ = self.menu.remove(&self.update);
                let _ = self.menu.remove(&self.update_sep);
            }
        }
        self.update_shown = version.map(str::to_owned);
    }

    pub fn set_pinned(&self, pinned: bool) {
        self.pin.set_checked(pinned);
    }

    pub fn set_on_top(&self, on: bool) {
        self.on_top.set_checked(on);
    }

    pub fn set_startup(&self, on: bool) {
        self.startup.set_checked(on);
    }
}

/// Resource id of the app icon `build.rs` embeds (winresource's default).
#[cfg(windows)]
const APP_ICON_RESOURCE: u16 = 1;

/// The app icon embedded in nook.exe, picked at the notification area's
/// size so it stays sharp at any scale. Falls back to a drawn pill if the
/// resource is missing (a build without it).
#[cfg(windows)]
fn tray_icon() -> Result<Icon> {
    let px = nook_platform::system::small_icon_size();
    match Icon::from_resource(APP_ICON_RESOURCE, Some((px, px))) {
        Ok(icon) => Ok(icon),
        Err(e) => {
            tracing::warn!("tray icon resource: {e}; using the drawn glyph");
            tray_glyph()
        }
    }
}

/// The app icon, built into the binary as PNG, at the indicator's size.
#[cfg(not(windows))]
fn tray_icon() -> Result<Icon> {
    const PNG: &[u8] = include_bytes!("../../../assets/icon/nook-48.png");
    let px = nook_platform::system::small_icon_size();
    match image::load_from_memory(PNG) {
        Ok(img) => {
            let img = img
                .resize(px, px, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            let (w, h) = img.dimensions();
            Icon::from_rgba(img.into_raw(), w, h).context("tray icon pixels")
        }
        Err(e) => {
            tracing::warn!("tray icon image: {e}; using the drawn glyph");
            tray_glyph()
        }
    }
}

/// A small white pill on a transparent background, drawn in code.
fn tray_glyph() -> Result<Icon> {
    const N: u32 = 32;
    let pill = Squircle::new(
        egui::Rect::from_min_max(egui::pos2(3.0, 10.0), egui::pos2(29.0, 22.0)),
        6.0,
    );
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            // 4x4 supersampling for smooth edges.
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = egui::pos2(
                        x as f32 + (sx as f32 + 0.5) / 4.0,
                        y as f32 + (sy as f32 + 0.5) / 4.0,
                    );
                    if pill.contains(p) {
                        hits += 1;
                    }
                }
            }
            let a = (hits * 255 / 16) as u8;
            rgba.extend_from_slice(&[255, 255, 255, a]);
        }
    }
    Icon::from_rgba(rgba, N, N).context("tray icon pixels")
}

/// The menu entries that change, on the GTK thread.
#[cfg(not(windows))]
struct Items {
    _icon: TrayIcon,
    menu: Menu,
    pin: CheckMenuItem,
    on_top: CheckMenuItem,
    startup: CheckMenuItem,
    update: MenuItem,
    update_sep: PredefinedMenuItem,
}

#[cfg(not(windows))]
thread_local! {
    static ITEMS: std::cell::RefCell<Option<Items>> = const { std::cell::RefCell::new(None) };
}

/// What was last sent to the GTK thread, so per-frame calls cost nothing
/// when nothing changed.
#[cfg(not(windows))]
#[derive(Default)]
struct Sent {
    pin: Option<bool>,
    on_top: Option<bool>,
    startup: Option<bool>,
    update: Option<Option<String>>,
}

#[cfg(not(windows))]
pub struct Tray {
    sent: std::cell::RefCell<Sent>,
}

#[cfg(not(windows))]
impl Tray {
    /// Starts the GTK thread with the indicator and its menu.
    pub fn new(
        tx: Sender<TrayCommand>,
        ctx: egui::Context,
        startup_on: bool,
        on_top_on: bool,
    ) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();
        std::thread::Builder::new()
            .name("nook-tray".into())
            .spawn(move || {
                if let Err(e) = gtk::init() {
                    let _ = ready_tx.send(Err(format!("GTK: {e}")));
                    return;
                }
                match build_items(tx, ctx, startup_on, on_top_on) {
                    Ok(items) => {
                        ITEMS.with(|i| *i.borrow_mut() = Some(items));
                        let _ = ready_tx.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e:#}")));
                        return;
                    }
                }
                gtk::main();
            })
            .context("spawning the tray thread")?;
        ready_rx
            .recv()
            .context("tray thread exited")?
            .map_err(|e| anyhow::anyhow!(e))?;
        let sent = Sent {
            on_top: Some(on_top_on),
            startup: Some(startup_on),
            ..Sent::default()
        };
        Ok(Self {
            sent: std::cell::RefCell::new(sent),
        })
    }

    /// Runs `f` with the menu items on the GTK thread.
    fn on_gtk(f: impl FnOnce(&mut Items) + Send + 'static) {
        glib::MainContext::default().invoke(move || {
            ITEMS.with(|i| {
                if let Some(items) = i.borrow_mut().as_mut() {
                    f(items);
                }
            });
        });
    }

    /// Shows "Update to `version`" at the top of the menu, or removes it.
    pub fn set_update(&mut self, version: Option<&str>) {
        let want = version.map(str::to_owned);
        let mut sent = self.sent.borrow_mut();
        if sent.update.as_ref() == Some(&want) {
            return;
        }
        let was_shown = sent.update.as_ref().is_some_and(Option::is_some);
        sent.update = Some(want.clone());
        Self::on_gtk(move |items| match want {
            Some(v) => {
                items.update.set_text(format!("Update to {v}"));
                if !was_shown
                    && let Err(e) = items
                        .menu
                        .insert_items(&[&items.update, &items.update_sep], 0)
                {
                    tracing::warn!("tray update item: {e}");
                }
            }
            None => {
                let _ = items.menu.remove(&items.update);
                let _ = items.menu.remove(&items.update_sep);
            }
        });
    }

    pub fn set_pinned(&self, pinned: bool) {
        if std::mem::replace(&mut self.sent.borrow_mut().pin, Some(pinned)) != Some(pinned) {
            Self::on_gtk(move |items| items.pin.set_checked(pinned));
        }
    }

    pub fn set_on_top(&self, on: bool) {
        if std::mem::replace(&mut self.sent.borrow_mut().on_top, Some(on)) != Some(on) {
            Self::on_gtk(move |items| items.on_top.set_checked(on));
        }
    }

    pub fn set_startup(&self, on: bool) {
        if std::mem::replace(&mut self.sent.borrow_mut().startup, Some(on)) != Some(on) {
            Self::on_gtk(move |items| items.startup.set_checked(on));
        }
    }
}

#[cfg(not(windows))]
fn build_items(
    tx: Sender<TrayCommand>,
    ctx: egui::Context,
    startup_on: bool,
    on_top_on: bool,
) -> Result<Items> {
    let settings = MenuItem::with_id("settings", "Settings", true, None);
    let pin = CheckMenuItem::with_id("pin", "Pin open", true, false, None);
    let on_top = CheckMenuItem::with_id("ontop", "Always on top", true, on_top_on, None);
    let startup = CheckMenuItem::with_id("startup", "Launch at startup", true, startup_on, None);
    let quit = MenuItem::with_id("quit", "Quit Nook", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &settings,
        &pin,
        &on_top,
        &startup,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .context("building tray menu")?;
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let cmd = match event.id.as_ref() {
            "settings" => TrayCommand::Settings,
            "pin" => TrayCommand::TogglePin,
            "ontop" => TrayCommand::ToggleOnTop,
            "startup" => TrayCommand::ToggleStartup,
            "update" => TrayCommand::Update,
            "quit" => TrayCommand::Quit,
            _ => return,
        };
        if tx.send(cmd).is_ok() {
            ctx.request_repaint();
        }
    }));
    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_tooltip("Nook")
        .with_title("")
        .with_icon(tray_icon()?)
        .build()
        .context("creating tray icon")?;
    Ok(Items {
        _icon: icon,
        menu,
        pin,
        on_top,
        startup,
        update: MenuItem::with_id("update", "Update", true, None),
        update_sep: PredefinedMenuItem::separator(),
    })
}
