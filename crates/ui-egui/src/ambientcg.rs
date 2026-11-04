//! The ambientCG tab of the Assets shelf: search ambientCG's CC0 library of materials, decals and
//! HDRIs, with thumbnails, and download one with a click. A material becomes a fill layer above
//! the selection, a decal a decal layer at the center of the 3D view (or where its tile is
//! dropped on it), an HDRI the viewport environment ([`Session::import_ambientcg_zip`]; agents
//! use the `assets.ambientcg.*` commands instead). Every request runs in the background (ehttp),
//! so the window never waits on the network.
//!
//! [`Session::import_ambientcg_zip`]: texturecraft_engine::Session::import_ambientcg_zip

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use egui::{Color32, RichText, Ui, vec2};
use egui_phosphor::regular as ic;
use serde_json::{Value, json};
use texturecraft_engine::formats::ambientcg::{self, Asset, Kind, Page, Query, Sort};

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
    /// `place`: a decal's placement, pinned to the surface when it was asked for.
    Zip {
        id: String,
        title: String,
        kind: Kind,
        attributes: String,
        place: Value,
        result: Result<Vec<u8>, String>,
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
            started: false,
            ctx: None,
        }
    }
}

impl Browser {
    /// Whether a search, thumbnail or download is still on its way.
    pub fn busy(&self) -> bool {
        self.searching || !self.downloads.is_empty() || self.thumbs.values().any(|t| matches!(t, Thumb::Loading))
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

/// A failed request's message, with the likely cause in the browser build.
fn explain(e: String) -> String {
    if cfg!(target_arch = "wasm32") {
        format!("{e}. {}", tr("The browser build can't reach ambientCG (its API doesn't accept requests from web pages): use the desktop app"))
    } else {
        e
    }
}

/// The blocking fetcher behind the `assets.ambientcg.*` commands (control channel), refusing
/// replies over `max` bytes.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_fetcher() {
    texturecraft_engine::ambientcg_cmds::install_fetcher(std::sync::Arc::new(|url: &str, max: u64| {
        let mut req = ehttp::Request::get(url);
        req.timeout = Some(Duration::from_secs(600));
        req.headers.insert("User-Agent", USER_AGENT);
        let resp = ehttp::fetch_blocking(&req)?;
        if !resp.ok {
            return Err(format!("HTTP {} {}", resp.status, resp.status_text));
        }
        if resp.bytes.len() as u64 > max {
            return Err(format!("the reply is larger than {} MB", max >> 20));
        }
        Ok(resp.bytes)
    }));
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

/// Import `asset` (the zip nearest the tab's resolution and format, or `attributes`). A decal
/// goes where `at` is in the 3D view (a point in it and its size), else at its center: the
/// spot on the surface is found now, so moving the view during the download doesn't move it.
pub(crate) fn start(app: &mut TextureCraftApp, asset: &Asset, attributes: Option<&str>, at: Option<(egui::Vec2, egui::Vec2)>) {
    if app.ambientcg.downloads.iter().any(|d| d.id == asset.id) {
        return;
    }
    let place = if app.ambientcg.results_kind == Kind::Decal {
        let params = at.map_or(json!({}), |(p, vp)| json!({"x": p.x, "y": p.y, "viewport": [vp.x, vp.y]}));
        match app.session.decal_placement(&params) {
            Ok(place) => place,
            Err(e) => {
                app.toast(e.to_string(), true);
                return;
            }
        }
    } else {
        Value::Null
    };
    download(app, asset, attributes, place);
}

/// Place the decal `id` of the results where it was dropped on the 3D view.
pub(crate) fn drop_decal(app: &mut TextureCraftApp, id: &str, at: egui::Vec2, viewport: egui::Vec2) {
    if let Some(asset) = app.ambientcg.results.iter().find(|a| a.id == id).cloned() {
        start(app, &asset, None, Some((at, viewport)));
    }
}

fn download(app: &mut TextureCraftApp, asset: &Asset, attributes: Option<&str>, place: Value) {
    let Some(ctx) = app.ambientcg.ctx.clone() else { return };
    let prefs = &app.ui.ambientcg;
    let pick = match attributes {
        Some(a) => asset.downloads.iter().find(|d| d.attributes == a),
        None => asset.download(prefs.resolution, if prefs.png { "PNG" } else { "JPG" }),
    };
    let Some(d) = pick.cloned() else {
        app.toast(trf("{name} has no download", &[("name", &asset.title)]), true);
        return;
    };
    if d.size > ambientcg::MAX_DOWNLOAD {
        app.toast(trf("{name} is too large at this resolution: pick a lower one", &[("name", &asset.title)]), true);
        return;
    }
    let (id, title, kind, attributes) = (asset.id.clone(), asset.title.clone(), app.ambientcg.results_kind, d.attributes);
    app.ambientcg.downloads.push(Pending { id: id.clone(), title: title.clone() });
    get(&ctx, &app.ambientcg.tx, &d.url, Duration::from_secs(600), move |result| Msg::Zip { id, title, kind, attributes, place, result });
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
            Msg::Zip { id, title, kind, attributes, place, result } => {
                b.downloads.retain(|d| d.id != id);
                let done = result.and_then(|bytes| app.session.import_ambientcg_zip(kind, &id, &title, &attributes, &bytes, &place).map_err(|e| e.to_string()));
                match done {
                    Ok(_) => app.toast(trf("Added \"{name}\" from ambientCG", &[("name", &title)]), false),
                    Err(e) => app.toast(format!("{title}: {e}"), true),
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
        let sorts = [(Sort::Popular, tr("Popular")), (Sort::Latest, tr("Newest")), (Sort::Downloads, tr("Most downloaded")), (Sort::Alphabet, tr("A to Z"))];
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
    });
    if go || !app.ambientcg.started {
        search(app, &ctx, false);
    }
}

/// The results: thumbnails to click, more on demand, and the credit.
pub fn results(app: &mut TextureCraftApp, ui: &mut Ui, ctx: &egui::Context) {
    let tk = Tokens::get(ctx);
    for d in &app.ambientcg.downloads {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(trf("Downloading {name}...", &[("name", &d.title)])).size(11.0));
        });
    }
    let hint = match app.ambientcg.results_kind {
        Kind::Material => tr("Click: add as a fill layer above the selected layer. Right-click: other resolutions"),
        Kind::Decal => tr("Click: place as a decal at the center of the 3D view, or drag it onto the 3D view. Right-click: other resolutions"),
        Kind::Hdri => tr("Click: use as the environment. Right-click: other resolutions"),
    };
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
    ui.add_space(6.0);
    ui.hyperlink_to(RichText::new(tr("Assets from ambientCG.com: CC0, free for any use")).size(10.0), ambientcg::SITE_URL);
}
