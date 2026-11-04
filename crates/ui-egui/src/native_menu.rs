//! macOS: the menus live in the system menu bar instead of the window's strip, built from the same
//! catalog (`menus::CATALOG`) and run through the same `menus::invoke`.
//!
//! The menu bar is rebuilt when the language, the keymap, the recent projects or the hidden items
//! change; enabled and checked marks follow the app every frame. About, Settings and Quit move to
//! the app menu, as on every Mac app. Only shortcuts with ⌘ become the menu bar's key equivalents
//! (a bare key such as M or Tab would stop typing in text fields): those the menu bar runs, so
//! `menus::shortcuts` skips them; the rest stay egui's. `TEXTURECRAFT_MENUS=window` keeps the
//! menus in the window.

use std::str::FromStr as _;
use std::sync::mpsc;

use muda::accelerator::Accelerator;
use muda::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

use crate::TextureCraftApp;
use crate::i18n::tr;
use crate::keymap::Keymap;
use crate::menus::{CATALOG, Item, TOP};
use crate::state::Mode;

/// Menu ids of the recent projects (`recent:<index>`) and of the item that clears them.
const RECENT: &str = "native.recent:";
const CLEAR_RECENT: &str = "native.recent.clear";
/// The catalog items that move to the app menu.
const APP_MENU: [&str; 3] = ["ui.about", "ui.settings", "ui.quit"];

/// What the menu bar was built for: a change rebuilds it.
#[derive(Clone, PartialEq)]
struct Built {
    language: &'static str,
    keymap: Keymap,
    recent: Vec<String>,
    hidden: Vec<&'static str>,
    /// Settings › Keyboard is waiting for a key: the menu bar gives up its key equivalents.
    capturing: bool,
}

enum Entry {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

/// A catalog item in the menu bar, with the enabled state it was last given.
struct Live {
    id: &'static str,
    entry: Entry,
    enabled: bool,
}

/// The app's menu bar and the clicks it reports.
pub struct NativeMenu {
    events: mpsc::Receiver<String>,
    built: Option<Built>,
    /// Kept alive: AppKit's menu items call back into it.
    menu: Option<Menu>,
    items: Vec<Live>,
    /// Ids whose shortcut is a key equivalent of the menu bar.
    keyed: Vec<&'static str>,
}

/// Whether the menus go to the system menu bar (`TEXTURECRAFT_MENUS=window` keeps them in the window).
pub fn wanted() -> bool {
    !std::env::var("TEXTURECRAFT_MENUS").is_ok_and(|v| v.eq_ignore_ascii_case("window"))
}

/// Move the menus to the system menu bar; call once on the main thread after the window opens.
pub fn install(app: &mut TextureCraftApp, ctx: &egui::Context) {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        // The app is gone when the receiver is: nothing left to tell.
        let _ = tx.send(e.id().0.clone());
        ctx.request_repaint();
    }));
    let mut menu = NativeMenu { events: rx, built: None, menu: None, items: Vec::new(), keyed: Vec::new() };
    menu.sync_shape(app, false);
    app.native_menu = Some(menu);
}

/// Whether the menu bar runs the shortcut of `id` (so egui must not run it too).
pub fn owns_shortcut(app: &TextureCraftApp, id: &str) -> bool {
    app.native_menu.as_ref().is_some_and(|m| m.keyed.contains(&id))
}

/// Once per frame before the shortcuts: run the clicked items, then rebuild the menu bar if what
/// it shows changed and refresh its enabled and checked marks.
pub fn sync(app: &mut TextureCraftApp, ctx: &egui::Context) {
    let Some(mut menu) = app.native_menu.take() else { return };
    let clicked: Vec<String> = menu.events.try_iter().collect();
    for id in clicked {
        activate(app, ctx, &id);
    }
    menu.sync_shape(app, crate::keymap::capturing(ctx));
    menu.sync_state(app);
    app.native_menu = Some(menu);
}

/// A click on (or the key equivalent of) a menu bar item.
fn activate(app: &mut TextureCraftApp, ctx: &egui::Context, id: &str) {
    if let Some(i) = id.strip_prefix(RECENT) {
        if let Some(path) = i.parse::<usize>().ok().and_then(|i| app.ui.recent.get(i).cloned()) {
            crate::windows::open_path(app, &path);
        }
        return;
    }
    if id == CLEAR_RECENT {
        app.ui.recent.clear();
        return;
    }
    // Only the catalog's ids: the menu bar never runs anything the window's menus could not.
    let Some(item) = CATALOG.iter().find(|i| i.id == id && i.label != "-") else { return };
    // In a text field Edit ▸ Undo and Redo (and their keys) act on the text, as on any Mac.
    let typing = ctx.memory(|m| m.focused().is_some()) && ctx.egui_wants_keyboard_input();
    if typing && matches!(item.id, "edit.undo" | "edit.redo") {
        if let Some(sc) = crate::keymap::shortcut_of(&app.ui.keymap, item.id).and_then(crate::menus::parse_shortcut) {
            let key = egui::Event::Key { key: sc.logical_key, physical_key: None, pressed: true, repeat: false, modifiers: sc.modifiers };
            ctx.input_mut(|i| i.events.push(key));
        }
        return;
    }
    if crate::menus::enabled(app, item.id) {
        crate::menus::invoke(app, ctx, item.id);
    }
}

/// `Ctrl+Shift+E` (the keymap's notation, Ctrl being ⌘ on a Mac) as a menu bar key equivalent;
/// None for a shortcut without ⌘ or with a key the menu bar cannot show.
fn accelerator(shortcut: &str) -> Option<Accelerator> {
    let parts: Vec<&str> = shortcut.split('+').collect();
    let (key, mods) = parts.split_last()?;
    if !mods.iter().any(|m| matches!(*m, "Ctrl" | "Cmd")) {
        return None;
    }
    let mods: Vec<&str> = mods.iter().map(|m| if matches!(*m, "Ctrl" | "Cmd") { "Cmd" } else { m }).collect();
    let key = match *key {
        "OpenBracket" => "BracketLeft",
        "CloseBracket" => "BracketRight",
        "Equals" | "Plus" => "Equal",
        k => k,
    };
    Accelerator::from_str(&format!("{}+{key}", mods.join("+"))).ok()
}

/// Drop separators at the ends of a menu and doubled ones (left by moved or hidden items).
fn tidy(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for i in items {
        let sep = i.label == "-";
        if sep && out.last().is_none_or(|l| l.label == "-") {
            continue;
        }
        out.push(i);
    }
    if out.last().is_some_and(|l| l.label == "-") {
        out.pop();
    }
    out
}

impl NativeMenu {
    /// Rebuild the menu bar when what it shows changed.
    fn sync_shape(&mut self, app: &TextureCraftApp, capturing: bool) {
        let built = Built {
            language: crate::i18n::current().code,
            keymap: app.ui.keymap.clone(),
            recent: app.ui.recent.clone(),
            hidden: CATALOG.iter().filter(|i| crate::menus::hidden(app, i)).map(|i| i.id).collect(),
            capturing,
        };
        if self.built.as_ref() == Some(&built) {
            return;
        }
        self.items.clear();
        self.keyed.clear();
        let menu = Menu::new();
        // Every append below goes to a menu made here: an error only leaves an item out.
        let _ = menu.append(&self.app_menu(app, &built));
        for top in TOP {
            let sub = Submenu::new(tr(top), true);
            let items = tidy(CATALOG.iter().filter(|i| i.menu == top && !built.hidden.contains(&i.id) && !APP_MENU.contains(&i.id)).copied().collect());
            let mut groups: Vec<&str> = Vec::new();
            for item in &items {
                if let Some(s) = item.sub {
                    if groups.contains(&s) {
                        continue;
                    }
                    groups.push(s);
                    let group = Submenu::new(tr(s), true);
                    for x in items.iter().filter(|x| x.sub == Some(s)) {
                        self.add(app, &built, &group, x);
                    }
                    let _ = sub.append(&group);
                    continue;
                }
                if item.label == "-" {
                    let _ = sub.append(&PredefinedMenuItem::separator());
                    continue;
                }
                self.add(app, &built, &sub, item);
                if item.id == "file.open" {
                    let _ = sub.append(&recent_menu(&built.recent));
                }
            }
            if top == "Window" {
                let _ = sub.append(&PredefinedMenuItem::separator());
                sub.set_as_windows_menu_for_nsapp();
            }
            if top == "Help" {
                sub.set_as_help_menu_for_nsapp();
            }
            let _ = menu.append(&sub);
        }
        menu.init_for_nsapp();
        self.menu = Some(menu);
        self.built = Some(built);
    }

    /// The app menu: About, Settings, the system's Services and Hide items, Quit.
    fn app_menu(&mut self, app: &TextureCraftApp, built: &Built) -> Submenu {
        // AppKit titles the app menu with the app's name whatever this says.
        let sub = Submenu::new("TextureCraft", true);
        let find = |id: &str| CATALOG.iter().find(|i| i.id == id).copied();
        if let Some(about) = find("ui.about") {
            self.add(app, built, &sub, &about);
        }
        let _ = sub.append(&PredefinedMenuItem::separator());
        if let Some(settings) = find("ui.settings") {
            // ⌘, as on every Mac, unless the keymap gives Settings a shortcut of its own.
            let shortcut = Item { shortcut: Some("Cmd+Comma"), ..settings };
            self.add(app, built, &sub, &shortcut);
        }
        let _ = sub.append(&PredefinedMenuItem::separator());
        let _ = sub.append(&PredefinedMenuItem::services(Some(tr("Services"))));
        let _ = sub.append(&PredefinedMenuItem::separator());
        let _ = sub.append(&PredefinedMenuItem::hide(Some(tr("Hide TextureCraft"))));
        let _ = sub.append(&PredefinedMenuItem::hide_others(Some(tr("Hide Others"))));
        let _ = sub.append(&PredefinedMenuItem::show_all(Some(tr("Show All"))));
        let _ = sub.append(&PredefinedMenuItem::separator());
        if let Some(quit) = find("ui.quit") {
            let quit = Item { label: crate::i18n::n_("Quit TextureCraft"), ..quit };
            self.add(app, built, &sub, &quit);
        }
        sub
    }

    /// One catalog item into `menu`: a check item for modes and views, its ⌘ shortcut as the key
    /// equivalent.
    fn add(&mut self, app: &TextureCraftApp, built: &Built, menu: &Submenu, item: &Item) {
        let shortcut = match crate::keymap::shortcut_of(&built.keymap, item.id) {
            Some(s) => Some(s),
            None if item.id == "ui.settings" => item.shortcut,
            None => None,
        };
        let key = if built.capturing { None } else { shortcut.and_then(accelerator) };
        if key.is_some() && !self.keyed.contains(&item.id) {
            self.keyed.push(item.id);
        }
        let label = tr(item.label);
        let on = crate::menus::enabled(app, item.id);
        let entry = match checked(app, item.id) {
            Some(c) => Entry::Check(CheckMenuItem::with_id(item.id, label, on, c, key)),
            None => Entry::Plain(MenuItem::with_id(item.id, label, on, key)),
        };
        let added = match &entry {
            Entry::Plain(m) => menu.append(m as &dyn IsMenuItem),
            Entry::Check(m) => menu.append(m as &dyn IsMenuItem),
        };
        if added.is_ok() {
            self.items.push(Live { id: item.id, entry, enabled: on });
        }
    }

    /// Enabled and checked marks as the app is now.
    fn sync_state(&mut self, app: &TextureCraftApp) {
        for live in &mut self.items {
            let on = crate::menus::enabled(app, live.id);
            if on != live.enabled {
                live.enabled = on;
                match &live.entry {
                    Entry::Plain(m) => m.set_enabled(on),
                    Entry::Check(m) => m.set_enabled(on),
                }
            }
            // AppKit flips a check item on click: put back what the app says.
            if let (Entry::Check(m), Some(c)) = (&live.entry, checked(app, live.id))
                && m.is_checked() != c
            {
                m.set_checked(c);
            }
        }
    }
}

/// File ▸ Recent projects: one item per project, then Clear.
fn recent_menu(recent: &[String]) -> Submenu {
    let sub = Submenu::new(tr("Recent projects"), !recent.is_empty());
    for (i, path) in recent.iter().enumerate() {
        let _ = sub.append(&MenuItem::with_id(format!("{RECENT}{i}"), path, true, None));
    }
    let _ = sub.append(&PredefinedMenuItem::separator());
    let _ = sub.append(&MenuItem::with_id(CLEAR_RECENT, tr("Clear recent projects"), true, None));
    sub
}

/// The check mark of a mode or view item; None for the other items.
fn checked(app: &TextureCraftApp, id: &str) -> Option<bool> {
    match id {
        "ui.mode.paint" => Some(app.ui.mode == Mode::Painting),
        "ui.mode.bake" => Some(app.ui.mode == Mode::Baking),
        "ui.mode.render" => Some(app.ui.mode == Mode::Rendering),
        id => crate::docking::view_panel(id).map(|p| app.ui.dock.is_open(p)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_command_shortcuts_become_key_equivalents() {
        assert!(accelerator("Ctrl+S").is_some());
        assert!(accelerator("Ctrl+Shift+E").is_some());
        assert!(accelerator("Ctrl+F4").is_some());
        assert!(accelerator("Cmd+Comma").is_some());
        // Bare keys and function keys stay egui's.
        assert!(accelerator("M").is_none());
        assert!(accelerator("Shift+C").is_none());
        assert!(accelerator("F8").is_none());
        assert!(accelerator("Tab").is_none());
        assert!(accelerator("").is_none());
    }

    #[test]
    fn separators_never_lead_trail_or_double() {
        let items = tidy(CATALOG.iter().filter(|i| i.menu == "File" && i.id != "ui.quit").copied().collect());
        assert_ne!(items.first().map(|i| i.label), Some("-"));
        assert_ne!(items.last().map(|i| i.label), Some("-"));
        assert!(items.windows(2).all(|w| !(w[0].label == "-" && w[1].label == "-")));
    }
}
