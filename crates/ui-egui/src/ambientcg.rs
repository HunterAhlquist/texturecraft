//! The ambientCG tab of the Assets shelf: search ambientCG's CC0 library of materials, decals and
//! HDRIs, with thumbnails, and use one with a click. A material becomes a fill layer above the
//! selection, a decal a decal layer at the center of the 3D view (or where its tile is dropped
//! on it), an HDRI the viewport environment.
//!
//! The project remembers what it imported ([`texturecraft_engine::core::ImportedAsset`]). An asset
//! it already holds applies at once from its own images; a check in the background then asks
//! ambientCG whether the file changed (size, SHA-1) and, when it did, downloads it and replaces
//! the images in every layer. "In this project" lists only those assets, offline.
//!
//! Searches and thumbnails load asynchronously (ehttp). Downloads, the download cache, unzipping
//! and decoding run on a worker thread ([`texturecraft_engine::ambientcg_cmds`]), so the window
//! never waits on the network or on decoding.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use egui::{Color32, RichText, Ui, vec2};
use egui_phosphor::regular as ic;
use serde_json::{Value, json};
use texturecraft_engine::ambientcg_cmds::{self as acg, Unpacked, Version};
use texturecraft_engine::core::ImportedAsset;
use texturecraft_engine::formats::ambientcg::{self, Asset, Download, Kind, Page, Query, Sort};

use crate::TextureCraftApp;
use crate::i18n::{tr, trf};
use crate::theme::Tokens;

/// Results per request.
const PAGE: u32 = 30;
/// Thumbnails kept before the cache starts over.
const MAX_THUMBS: usize = 600;
/// Who is asking (desktop only: browsers set their own).
#[cfg(not(target_arch = "wasm32"))]
const USER_AGENT: &str = concat!("TextureCraft/", env!("CARGO_PKG_VERSION"), " (+https://github.com/storytold/texturecraft)");
/// The download cache's size limit (desktop).
#[cfg(not(target_arch = "wasm32"))]
const CACHE_BYTES: u64 = 2 << 30;

enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

/// A download on its way.
struct Pending {
    id: String,
    title: String,
}

enum Msg {
    Page {
        generation: u64,
        append: bool,
        result: Result<Page, String>,
    },
    Thumb {
        id: String,
        result: Result<egui::ColorImage, String>,
    },
    /// A download, unzipped and decoded on a worker thread. `place`: apply it too (a decal's
    /// placement, pinned when it was asked for); None for an update of a held asset.
    Ready {
        id: String,
        title: String,
        result: Result<Box<(Version, Unpacked)>, String>,
        place: Option<Value>,
    },
    /// A held asset's background check found ambientCG's file unchanged (or couldn't tell).
    Current {
        id: String,
    },
}

/// The tab's results, thumbnails and requests in flight (not saved; the search terms are, in
/// [`crate::state::AmbientCgUi`]).
pub struct Browser {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    results: Vec<Asset>,
    total: u64,
    /// The kind the results are (a click imports by it).
    results_kind: Kind,
    /// Counts searches: a reply for an older one is dropped.
    generation: u64,
    searching: bool,
    error: Option<String>,
    thumbs: HashMap<String, Thumb>,
    downloads: Vec<Pending>,
    /// Held assets being checked for a newer version.
    checking: Vec<String>,
    /// The first search runs when the tab first shows.
    started: bool,
    /// For requests started outside the tab's drawing (a tile dropped on the 3D view).
    ctx: Option<egui::Context>,
}

impl Default for Browser {
    fn default() -> Self {
        let (tx, rx) = channel();
        Browser {
            tx,
            rx,
            results: Vec::new(),
            total: 0,
            results_kind: Kind::Material,
            generation: 0,
            searching: false,
            error: None,
            thumbs: HashMap::new(),
            downloads: Vec::new(),
            checking: Vec::new(),
            started: false,
            ctx: None,
        }
    }
}

impl Browser {
    /// Whether a search, thumbnail, download or update check is still on its way.
    pub fn busy(&self) -> bool {
        self.searching || !self.downloads.is_empty() || !self.checking.is_empty() || self.thumbs.values().any(|t| matches!(t, Thumb::Loading))
    }

    /// The results shown: (id, title).
    pub fn results(&self) -> Vec<(String, String)> {
        self.results.iter().map(|a| (a.id.clone(), a.title.clone())).collect()
    }
}

/// GET `url` in the background; `done` turns the body (or why there is none) into a message.
fn get(ctx: &egui::Context, tx: &Sender<Msg>, url: &str, timeout: Duration, done: impl FnOnce(Result<Vec<u8>, String>) -> Msg + Send + 'static) {
    let mut req = ehttp::Request::get(url);
    req.timeout = Some(timeout);
    #[cfg(not(target_arch = "wasm32"))]
    req.headers.insert("User-Agent", USER_AGENT);
    let (tx, ctx) = (tx.clone(), ctx.clone());
    ehttp::fetch(req, move |r| {
        let r = r.and_then(|resp| if resp.ok { Ok(resp.bytes) } else { Err(format!("HTTP {} {}", resp.status, resp.status_text)) });
        let _ = tx.send(done(r.map_err(explain)));
        ctx.request_repaint();
    });
}

/// Run `work` on a worker thread (inline in the browser, which has none) and send its message.
fn spawn(app: &TextureCraftApp, work: impl FnOnce() -> Msg + Send + 'static) {
    let (tx, ctx) = (app.ambientcg.tx.clone(), app.ambientcg.ctx.clone());
    let run = move || {
        let _ = tx.send(work());
        if let Some(c) = ctx {
            c.request_repaint();
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A failed spawn drops the job; the error shows as nothing happening, never a crash.
        let _ = std::thread::Builder::new().name("texturecraft-ambientcg".into()).spawn(run);
    }
    #[cfg(target_arch = "wasm32")]
    run();
}

/// A failed request's message, with the likely cause in the browser build.
fn explain(e: String) -> String {
    if cfg!(target_arch = "wasm32") {
        format!("{e}. {}", tr("The browser build can't reach ambientCG (its API doesn't accept requests from web pages): use the desktop app"))
    } else {
        e
    }
}

/// The blocking fetcher behind downloads and the `assets.ambientcg.*` commands, and the
/// download cache in the data folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn install() {
    acg::install_fetcher(std::sync::Arc::new(|url: &str, max: u64, head: bool| {
        let mut req = if head { ehttp::Request::head(url) } else { ehttp::Request::get(url) };
        req.timeout = Some(Duration::from_secs(600));
        req.headers.insert("User-Agent", USER_AGENT);
        let resp = ehttp::fetch_blocking(&req)?;
        if !resp.ok {
            return Err(format!("HTTP {} {}", resp.status, resp.status_text));
        }
        if resp.bytes.len() as u64 > max {
            return Err(format!("the reply is larger than {} MB", max >> 20));
        }
        let sha1 = resp.headers.get(acg::SHA1_HEADER).map(str::to_string);
        Ok(acg::Fetched { bytes: resp.bytes, sha1 })
    }));
    acg::install_cache(crate::user_dir("cache").join("ambientcg"), CACHE_BYTES);
}

/// Start a search with the tab's terms: a new one, or the next page of the current one.
fn search(app: &mut TextureCraftApp, ctx: &egui::Context, append: bool) {
    let prefs = &app.ui.ambientcg;
    let b = &mut app.ambientcg;
    let query = Query {
        text: prefs.query.clone(),
        kind: Kind::from_id(&prefs.kind).unwrap_or_default(),
        sort: Sort::from_id(&prefs.sort).unwrap_or_default(),
        limit: PAGE,
        offset: if append { b.results.len() as u32 } else { 0 },
    };
    b.generation += 1;
    b.searching = true;
    b.started = true;
    b.error = None;
    if !append {
        b.results.clear();
        b.total = 0;
        b.results_kind = query.kind;
    }
    if b.thumbs.len() > MAX_THUMBS {
        b.thumbs.clear();
    }
    let generation = b.generation;
    get(ctx, &b.tx, &query.url(), Duration::from_secs(30), move |r| Msg::Page {
        generation,
        append,
        result: r.and_then(|bytes| ambientcg::parse_page(&bytes).map_err(|e| e.to_string())),
    });
}

/// Where a decal of kind `kind` goes (`at`: a point in the 3D view and its size, else its
/// center), pinned to the surface now so moving the view meanwhile doesn't move it; `{}` for
/// other kinds. None (after a toast) when nothing is there.
fn placement(app: &mut TextureCraftApp, kind: Kind, at: Option<(egui::Vec2, egui::Vec2)>) -> Option<Value> {
    if kind != Kind::Decal {
        return Some(json!({}));
    }
    let params = at.map_or(json!({}), |(p, vp)| json!({"x": p.x, "y": p.y, "viewport": [vp.x, vp.y]}));
    match app.session.decal_placement(&params) {
        Ok(place) => Some(place),
        Err(e) => {
            app.toast(e.to_string(), true);
            None
        }
    }
}

/// Use search result `asset` (the download nearest the tab's resolution and format, or
/// `attributes`). The project's copy applies at once when it is that download at the same size
/// (and a background check compares the SHA-1); anything else is downloaded first.
pub(crate) fn start(app: &mut TextureCraftApp, asset: &Asset, attributes: Option<&str>, at: Option<(egui::Vec2, egui::Vec2)>) {
    if app.ambientcg.downloads.iter().any(|d| d.id == asset.id) {
        return;
    }
    let prefs = &app.ui.ambientcg;
    let pick = match attributes {
        Some(a) => asset.downloads.iter().find(|d| d.attributes == a),
        None => {
            // Without an explicit choice, the download the project already holds.
            let held = app.session.ambientcg_asset(&asset.id).map(|r| r.download.clone());
            held.and_then(|h| asset.downloads.iter().find(|d| d.attributes == h))
                .or_else(|| asset.download(prefs.resolution, if prefs.png { "PNG" } else { "JPG" }))
        }
    };
    let Some(d) = pick.cloned() else {
        app.toast(trf("{name} has no download", &[("name", &asset.title)]), true);
        return;
    };
    if d.size > ambientcg::MAX_DOWNLOAD {
        app.toast(trf("{name} is too large at this resolution: pick a lower one", &[("name", &asset.title)]), true);
        return;
    }
    let kind = app.ambientcg.results_kind;
    let Some(place) = placement(app, kind, at) else { return };
    let held = app.session.ambientcg_asset(&asset.id).cloned();
    match held {
        Some(rec) if rec.download == d.attributes && rec.size == d.size => {
            if apply_held(app, &rec, &place) {
                check(app, rec, Some(d));
            }
        }
        _ => fetch(app, &asset.id, &asset.title, kind, d, Some(place)),
    }
}

/// Use held asset `id` (the "In this project" view): it applies at once, then a background
/// check asks ambientCG whether its file changed.
fn use_held(app: &mut TextureCraftApp, id: &str, at: Option<(egui::Vec2, egui::Vec2)>) {
    let Some(rec) = app.session.ambientcg_asset(id).cloned() else { return };
    let kind = Kind::from_id(&rec.kind).unwrap_or_default();
    let Some(place) = placement(app, kind, at) else { return };
    if apply_held(app, &rec, &place) {
        check(app, rec, None);
    }
}

/// Apply the project's copy of `rec`; false (after a toast) when that fails.
fn apply_held(app: &mut TextureCraftApp, rec: &ImportedAsset, place: &Value) -> bool {
    match app.session.apply_ambientcg(&rec.id, place) {
        Ok(_) => {
            app.toast(trf("Added \"{name}\" from ambientCG", &[("name", &rec.title)]), false);
            true
        }
        Err(e) => {
            app.toast(format!("{}: {e}", rec.title), true);
            false
        }
    }
}

/// Ask ambientCG in the background whether held asset `rec` changed (its download's size, then
/// its SHA-1): when it did, download it and replace its images in every layer. `known`: the
/// download as a fresh search reported it (saves looking it up).
fn check(app: &mut TextureCraftApp, rec: ImportedAsset, known: Option<Download>) {
    if app.ambientcg.checking.contains(&rec.id) || app.ambientcg.downloads.iter().any(|d| d.id == rec.id) {
        return;
    }
    app.ambientcg.checking.push(rec.id.clone());
    let kind = Kind::from_id(&rec.kind).unwrap_or_default();
    spawn(app, move || {
        let (id, title) = (rec.id.clone(), rec.title.clone());
        let d = match known {
            Some(d) => Some(d),
            None => acg::lookup(&id).ok().and_then(|a| a.downloads.into_iter().find(|d| d.attributes == rec.download)),
        };
        // Offline, or the download is gone: keep what the project has.
        let Some(d) = d else { return Msg::Current { id } };
        let sha1 = if rec.size == d.size && rec.sha1.is_some() { acg::remote_sha1(&d.url) } else { None };
        if acg::is_current(&rec, &d, sha1.as_deref()) {
            return Msg::Current { id };
        }
        Msg::Ready { result: fetch_unpack(&id, &title, kind, &d), id, title, place: None }
    });
}

/// Download (or take from the cache), unzip and decode download `d`: blocking.
fn fetch_unpack(id: &str, title: &str, kind: Kind, d: &Download) -> Result<Box<(Version, Unpacked)>, String> {
    let got = acg::download(id, d)?;
    let u = acg::unpack(kind, title, &d.attributes, &got.bytes)?;
    Ok(Box::new((Version { download: d.attributes.clone(), size: d.size, sha1: got.sha1 }, u)))
}

/// Download `d` of asset `id` on a worker thread; `place`: apply it once it is in.
fn fetch(app: &mut TextureCraftApp, id: &str, title: &str, kind: Kind, d: Download, place: Option<Value>) {
    app.ambientcg.downloads.push(Pending { id: id.into(), title: title.into() });
    let (id, title) = (id.to_string(), title.to_string());
    spawn(app, move || Msg::Ready { result: fetch_unpack(&id, &title, kind, &d), id, title, place });
}

/// Place decal `id` where it was dropped on the 3D view (a search result, or held by the project).
pub(crate) fn drop_decal(app: &mut TextureCraftApp, id: &str, at: egui::Vec2, viewport: egui::Vec2) {
    if app.ui.ambientcg.in_project {
        use_held(app, id, Some((at, viewport)));
    } else if let Some(asset) = app.ambientcg.results.iter().find(|a| a.id == id).cloned() {
        start(app, &asset, None, Some((at, viewport)));
    }
}

fn decode_thumb(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if img.width() > 1024 || img.height() > 1024 {
        return Err("thumbnail too large".into());
    }
    let rgba = img.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// Handle finished requests (every frame, so downloads land with the tab closed too).
pub fn poll(app: &mut TextureCraftApp, ctx: &egui::Context) {
    if app.ambientcg.ctx.is_none() {
        app.ambientcg.ctx = Some(ctx.clone());
    }
    while let Ok(msg) = app.ambientcg.rx.try_recv() {
        let b = &mut app.ambientcg;
        match msg {
            Msg::Page { generation, append, result } => {
                if generation != b.generation {
                    continue;
                }
                b.searching = false;
                match result {
                    Ok(page) => {
                        b.total = page.total;
                        if !append {
                            b.results.clear();
                        }
                        // A page can overlap the last when the library changed in between.
                        for a in page.assets {
                            if !b.results.iter().any(|r| r.id == a.id) {
                                b.results.push(a);
                            }
                        }
                    }
                    Err(e) => b.error = Some(e),
                }
            }
            Msg::Thumb { id, result } => {
                let t = match result {
                    Ok(img) => Thumb::Ready(ctx.load_texture(format!("ambientcg:{id}"), img, egui::TextureOptions::LINEAR)),
                    Err(_) => Thumb::Failed,
                };
                b.thumbs.insert(id, t);
            }
            Msg::Current { id } => b.checking.retain(|c| *c != id),
            Msg::Ready { id, title, result, place } => {
                b.downloads.retain(|d| d.id != id);
                b.checking.retain(|c| *c != id);
                let done = result.and_then(|b| {
                    let (version, u) = *b;
                    app.session.import_ambientcg(&id, &title, version, u, place.as_ref()).map_err(|e| e.to_string())
                });
                match (done, place) {
                    (Ok(_), Some(_)) => app.toast(trf("Added \"{name}\" from ambientCG", &[("name", &title)]), false),
                    (Ok(_), None) => app.toast(trf("Updated \"{name}\" to the latest version from ambientCG", &[("name", &title)]), false),
                    (Err(e), _) => app.toast(format!("{title}: {e}"), true),
                }
            }
        }
    }
}

/// The tab's search terms, in place of the shelf's own search box.
pub fn controls(app: &mut TextureCraftApp, ui: &mut Ui) {
    let tk = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let mut go = false;
    let in_project = app.ui.ambientcg.in_project;
    ui.horizontal(|ui| {
        ui.label(RichText::new(ic::MAGNIFYING_GLASS).color(tk.text_dim));
        let r = ui.add(egui::TextEdit::singleline(&mut app.ui.ambientcg.query).hint_text(tr("Search ambientCG")).desired_width(f32::INFINITY));
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        let prefs = &mut app.ui.ambientcg;
        let kinds = [(Kind::Material, tr("Materials")), (Kind::Decal, tr("Decals")), (Kind::Hdri, tr("Environments"))];
        let mut kind = Kind::from_id(&prefs.kind).unwrap_or_default();
        egui::ComboBox::from_id_salt("acg_kind").width(96.0).selected_text(kinds.iter().find(|k| k.0 == kind).map(|k| k.1).unwrap_or_default()).show_ui(
            ui,
            |ui| {
                for (k, label) in kinds {
                    go |= ui.selectable_value(&mut kind, k, label).changed();
                }
            },
        );
        prefs.kind = kind.id().into();
        if !in_project {
            let sorts =
                [(Sort::Popular, tr("Popular")), (Sort::Latest, tr("Newest")), (Sort::Downloads, tr("Most downloaded")), (Sort::Alphabet, tr("A to Z"))];
            let mut sort = Sort::from_id(&prefs.sort).unwrap_or_default();
            egui::ComboBox::from_id_salt("acg_sort").width(96.0).selected_text(sorts.iter().find(|s| s.0 == sort).map(|s| s.1).unwrap_or_default()).show_ui(
                ui,
                |ui| {
                    for (s, label) in sorts {
                        go |= ui.selectable_value(&mut sort, s, label).changed();
                    }
                },
            );
            prefs.sort = sort.id().into();
            let format = if kind == Kind::Hdri {
                ""
            } else if prefs.png {
                " PNG"
            } else {
                " JPG"
            };
            egui::ComboBox::from_id_salt("acg_res")
                .width(64.0)
                .selected_text(format!("{}K{format}", prefs.resolution))
                .show_ui(ui, |ui| {
                    for k in [1, 2, 4, 8] {
                        ui.selectable_value(&mut prefs.resolution, k, format!("{k}K"));
                    }
                    if kind != Kind::Hdri {
                        ui.separator();
                        ui.selectable_value(&mut prefs.png, false, "JPG");
                        ui.selectable_value(&mut prefs.png, true, "PNG");
                    }
                })
                .response
                .on_hover_text(tr("Download resolution and file format"));
        }
        ui.toggle_value(&mut prefs.in_project, format!("{} {}", ic::FOLDER_SIMPLE, tr("In this project")))
            .on_hover_text(tr("Only the ambientCG assets this project already holds (no download needed)"));
    });
    // Coming back from the project view runs the search its terms may have changed.
    if !app.ui.ambientcg.in_project && (go || !app.ambientcg.started || in_project) {
        search(app, &ctx, false);
    }
}

/// The results (or the project's assets): thumbnails to click, more on demand, and the credit.
pub fn results(app: &mut TextureCraftApp, ui: &mut Ui, ctx: &egui::Context) {
    for d in &app.ambientcg.downloads {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(trf("Downloading {name}...", &[("name", &d.title)])).size(11.0));
        });
    }
    if app.ui.ambientcg.in_project {
        held_assets(app, ui, ctx);
    } else {
        search_results(app, ui, ctx);
    }
    ui.add_space(6.0);
    ui.hyperlink_to(RichText::new(tr("Assets from ambientCG.com: CC0, free for any use")).size(10.0), ambientcg::SITE_URL);
}

fn hint(kind: Kind) -> &'static str {
    match kind {
        Kind::Material => tr("Click: add as a fill layer above the selected layer. Right-click: other resolutions"),
        Kind::Decal => tr("Click: place as a decal at the center of the 3D view, or drag it onto the 3D view. Right-click: other resolutions"),
        Kind::Hdri => tr("Click: use as the environment. Right-click: other resolutions"),
    }
}

/// A check mark on tiles of assets the project holds.
fn held_badge(ui: &Ui, rect: egui::Rect) {
    let tk = Tokens::get(ui.ctx());
    let c = rect.right_top() + vec2(-11.0, 11.0);
    ui.painter().circle_filled(c, 7.0, tk.accent);
    ui.painter().text(c, egui::Align2::CENTER_CENTER, ic::CHECK, egui::FontId::proportional(10.0), Color32::WHITE);
}

fn search_results(app: &mut TextureCraftApp, ui: &mut Ui, ctx: &egui::Context) {
    let tk = Tokens::get(ctx);
    let hint = hint(app.ambientcg.results_kind);
    let assets = app.ambientcg.results.clone();
    let mut clicked: Option<(Asset, Option<String>)> = None;
    crate::shelf::tiles(ui, |ui| {
        for a in &assets {
            let busy = app.ambientcg.downloads.iter().any(|d| d.id == a.id);
            let tex = match app.ambientcg.thumbs.get(&a.id) {
                Some(Thumb::Ready(t)) => Some(t.clone()),
                Some(_) => None,
                None => {
                    // Thumbnails load as their tiles first show.
                    if let Some(url) = &a.thumbnail
                        && ui.is_rect_visible(egui::Rect::from_min_size(ui.cursor().min, vec2(72.0, 86.0)))
                    {
                        app.ambientcg.thumbs.insert(a.id.clone(), Thumb::Loading);
                        let id = a.id.clone();
                        get(ctx, &app.ambientcg.tx, url, Duration::from_secs(30), move |r| Msg::Thumb { id, result: r.and_then(|b| decode_thumb(&b)) });
                    }
                    None
                }
            };
            // Decals drag onto the 3D view, to be placed where they drop.
            let drag = (app.ambientcg.results_kind == Kind::Decal && !busy).then(|| crate::shelf::Asset::AmbientCgDecal(a.id.clone()));
            let r = crate::shelf::tile(ui, tex.as_ref(), &a.title, hint, drag.as_ref());
            if app.session.ambientcg_asset(&a.id).is_some() {
                held_badge(ui, r.rect);
            }
            if busy {
                ui.painter().rect_filled(r.rect, 3.0, Color32::from_black_alpha(110));
                ui.put(egui::Rect::from_center_size(r.rect.center_top() + vec2(0.0, 32.0), vec2(20.0, 20.0)), egui::Spinner::new());
            }
            if r.clicked() && !busy {
                clicked = Some((a.clone(), None));
            }
            r.context_menu(|ui| {
                for d in &a.downloads {
                    let mb = d.size as f64 / (1024.0 * 1024.0);
                    let label = if d.size > 0 { format!("{}  ({mb:.1} MB)", d.attributes) } else { d.attributes.clone() };
                    if ui.add_enabled(!busy && d.size <= ambientcg::MAX_DOWNLOAD, egui::Button::new(label)).clicked() {
                        clicked = Some((a.clone(), Some(d.attributes.clone())));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(format!("{}  {}", ic::ARROW_SQUARE_OUT, tr("Open on ambientCG.com"))).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(&a.page));
                    ui.close();
                }
            });
        }
    });
    if let Some((asset, attributes)) = clicked {
        start(app, &asset, attributes.as_deref(), None);
    }
    let b = &app.ambientcg;
    if b.searching {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(tr("Searching...")).color(tk.text_dim));
        });
    } else if let Some(e) = b.error.clone() {
        ui.label(RichText::new(e).size(11.0).color(tk.danger));
        if ui.button(tr("Retry")).clicked() {
            let append = !app.ambientcg.results.is_empty();
            search(app, ctx, append);
        }
    } else if b.started && b.results.is_empty() {
        ui.label(RichText::new(tr("No results")).color(tk.text_dim));
    } else if (b.results.len() as u64) < b.total && ui.button(tr("Load more")).clicked() {
        search(app, ctx, true);
    }
}

/// The ambientCG assets the project holds, of the chosen kind, matching the search words.
fn held_assets(app: &mut TextureCraftApp, ui: &mut Ui, ctx: &egui::Context) {
    let tk = Tokens::get(ctx);
    let kind = Kind::from_id(&app.ui.ambientcg.kind).unwrap_or_default();
    let words: Vec<String> = app.ui.ambientcg.query.split([' ', ',']).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    let held: Vec<ImportedAsset> = app
        .session
        .doc
        .project
        .assets
        .iter()
        .filter(|a| a.site == acg::SITE && a.kind == kind.id() && app.session.ambientcg_asset(&a.id).is_some())
        .filter(|a| words.iter().all(|w| a.title.to_lowercase().contains(w) || a.id.to_lowercase().contains(w)))
        .cloned()
        .collect();
    if held.is_empty() {
        ui.label(RichText::new(tr("No ambientCG assets in this project yet")).color(tk.text_dim));
        return;
    }
    let hint = tr("Click: use it again from this project (a newer version on ambientCG replaces it)");
    let mut clicked = None;
    crate::shelf::tiles(ui, |ui| {
        for a in &held {
            // The project's own images make the thumbnails, so this works offline.
            let image = a.maps.get(&texturecraft_engine::core::Channel::BaseColor).or(a.environment.as_ref());
            let tex = image.and_then(|n| app.session.images.get(n).cloned().map(|img| app.thumbs.image(ctx, n, &img)));
            let drag = (kind == Kind::Decal).then(|| crate::shelf::Asset::AmbientCgDecal(a.id.clone()));
            let busy = app.ambientcg.downloads.iter().any(|d| d.id == a.id);
            let r = crate::shelf::tile(ui, tex.as_ref(), &a.title, hint, drag.as_ref());
            if busy || app.ambientcg.checking.contains(&a.id) {
                ui.put(egui::Rect::from_center_size(r.rect.right_top() + vec2(-11.0, 11.0), vec2(14.0, 14.0)), egui::Spinner::new());
            } else {
                held_badge(ui, r.rect);
            }
            if r.clicked() && !busy {
                clicked = Some(a.id.clone());
            }
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("{} · {}", a.download, a.id)).size(10.5).color(tk.text_dim));
                if ui.button(format!("{}  {}", ic::ARROW_SQUARE_OUT, tr("Open on ambientCG.com"))).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(format!("{}/a/{}", ambientcg::SITE_URL, a.id)));
                    ui.close();
                }
            });
        }
    });
    if let Some(id) = clicked {
        use_held(app, &id, None);
    }
}
