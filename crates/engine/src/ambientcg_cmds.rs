//! ambientCG (<https://ambientcg.com>, CC0 materials, decals and HDRIs) in the engine.
//!
//! An imported asset is recorded in the project ([`ImportedAsset`]: its download, size and
//! SHA-1, and the image resources it became). Applying it again reuses those resources: a
//! material becomes another fill layer, a decal another decal layer, an HDRI the environment.
//! When ambientCG has a newer version (another size or SHA-1), importing it again stores the new
//! maps and points every layer at them in one undo step.
//!
//! - Blocking network helpers ([`lookup`], [`download`], [`remote_sha1`]) and [`unpack`] (unzip
//!   and decode) touch no session, so the UI runs them on worker threads.
//! - [`Session::import_ambientcg`] stores an unpacked download (and applies it);
//!   [`Session::apply_ambientcg`] applies a recorded asset.
//! - `assets.ambientcg.search`, `.import` and `.list` do the same for agents (control channel,
//!   CLI, MCP): they block while they fetch.
//!
//! The engine does no networking of its own: the app and the CLI install a fetcher
//! ([`install_fetcher`]) and, on the desktop, a download cache folder ([`install_cache`]).
//! Without a fetcher (tests, the browser build) the commands report that the network is
//! unavailable. Only ambientCG's API and download addresses are ever fetched.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use texturecraft_core::{Channel, Fill, ImportedAsset, Layer, LayerKind, Projection, Source};
use texturecraft_formats::ambientcg::{self, Asset, Download, Kind, Query, Sort};
use texturecraft_formats::materialx_read::Value as Mtlx;

use crate::commands::{CommandSpec, always, bad, strp};
use crate::{EngineError, Result, Session, cmd};

/// The library name recorded on imported assets.
pub const SITE: &str = "ambientCG";

// ---------------------------------------------------------------------------------------------
// Network.

/// A fetched body (empty for a HEAD) and the SHA-1 the host reported for it, if any.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub sha1: Option<String>,
}

/// Fetches `url` (GET, or HEAD when the flag is set), refusing bodies over `max` bytes.
pub type Fetcher = dyn Fn(&str, u64, bool) -> std::result::Result<Fetched, String> + Send + Sync;

static FETCHER: OnceLock<Arc<Fetcher>> = OnceLock::new();

/// Let the ambientCG commands reach the network (the first call wins).
pub fn install_fetcher(fetcher: Arc<Fetcher>) {
    let _ = FETCHER.set(fetcher);
}

/// The SHA-1 header of ambientCG's file host (Backblaze B2).
pub const SHA1_HEADER: &str = "x-bz-content-sha1";

fn fetch(url: &str, max: u64, head: bool) -> std::result::Result<Fetched, String> {
    let f = FETCHER.get().ok_or("network access is not available here (the desktop app and texturecraft-cli have it)")?;
    let got = f(url, max, head)?;
    if got.bytes.len() as u64 > max {
        return Err(format!("the reply is larger than {} MB", max >> 20));
    }
    Ok(got)
}

/// Look asset `id` up (blocking).
pub fn lookup(id: &str) -> std::result::Result<Asset, String> {
    if !ambientcg::valid_id(id) {
        return Err(format!("`{id}` is not an ambientCG asset id"));
    }
    let reply = fetch(&ambientcg::asset_url(id), ambientcg::MAX_REPLY as u64, false)?;
    let page = ambientcg::parse_page(&reply.bytes).map_err(|e| e.to_string())?;
    page.assets.into_iter().find(|a| a.id == id).ok_or_else(|| format!("ambientCG has no asset `{id}`"))
}

/// The SHA-1 ambientCG's file host reports for download `url`, without downloading it.
pub fn remote_sha1(url: &str) -> Option<String> {
    fetch(url, 0, true).ok()?.sha1.map(|s| s.to_ascii_lowercase())
}

/// Download `d` of asset `id` (blocking), from the download cache when it holds this version.
pub fn download(id: &str, d: &Download) -> std::result::Result<Fetched, String> {
    if d.size > ambientcg::MAX_DOWNLOAD {
        return Err(format!("{id} {} is {} MB (limit {} MB): pick a lower resolution", d.attributes, d.size >> 20, ambientcg::MAX_DOWNLOAD >> 20));
    }
    if let Some(hit) = cache::load(id, d) {
        return Ok(hit);
    }
    let got = fetch(&d.url, ambientcg::MAX_DOWNLOAD, false)?;
    let got = Fetched { sha1: got.sha1.map(|s| s.to_ascii_lowercase()), ..got };
    cache::store(id, d, &got);
    Ok(got)
}

/// Keep downloads in `dir` (at most `cap` bytes, oldest dropped first), so an asset used again in
/// another project isn't downloaded again. Desktop only; the first call wins.
pub fn install_cache(dir: std::path::PathBuf, cap: u64) {
    cache::install(dir, cap);
}

#[cfg(not(target_arch = "wasm32"))]
mod cache {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use serde_json::{Value, json};
    use texturecraft_formats::ambientcg::Download;

    use super::Fetched;

    static DIR: OnceLock<(PathBuf, u64)> = OnceLock::new();

    pub fn install(dir: PathBuf, cap: u64) {
        let _ = DIR.set((dir, cap));
    }

    /// `<id>_<attributes>.zip` and its `.json` (size, SHA-1); both parts are validated names.
    fn paths(dir: &Path, id: &str, d: &Download) -> Option<(PathBuf, PathBuf)> {
        let ok = |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        (ok(id) && ok(&d.attributes)).then(|| (dir.join(format!("{id}_{}.zip", d.attributes)), dir.join(format!("{id}_{}.json", d.attributes))))
    }

    pub fn load(id: &str, d: &Download) -> Option<Fetched> {
        let (dir, _) = DIR.get()?;
        let (zip, meta) = paths(dir, id, d)?;
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).ok()?).ok()?;
        // The API's size is the version check: a re-released file has another size.
        if d.size == 0 || m.get("size").and_then(Value::as_u64) != Some(d.size) || std::fs::metadata(&zip).ok()?.len() != d.size {
            return None;
        }
        let bytes = std::fs::read(&zip).ok()?;
        // Touch it, so pruning drops the least recently used first.
        let _ = std::fs::File::options().append(true).open(&zip).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        Some(Fetched { bytes, sha1: m.get("sha1").and_then(Value::as_str).map(str::to_string) })
    }

    pub fn store(id: &str, d: &Download, got: &Fetched) {
        let Some((dir, cap)) = DIR.get() else { return };
        let Some((zip, meta)) = paths(dir, id, d) else { return };
        if got.bytes.len() as u64 != d.size || std::fs::create_dir_all(dir).is_err() {
            return;
        }
        // A failed write just means no cache entry: the next use downloads again.
        let tmp = zip.with_extension("zip.tmp");
        if std::fs::write(&tmp, &got.bytes).and_then(|_| std::fs::rename(&tmp, &zip)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = std::fs::write(&meta, json!({"size": d.size, "sha1": got.sha1}).to_string());
        prune(dir, *cap);
    }

    /// Drop the least recently used zips until the folder holds at most `cap` bytes.
    fn prune(dir: &Path, cap: u64) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut zips: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "zip"))
            .filter_map(|e| e.metadata().ok().map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path())))
            .collect();
        zips.sort();
        let mut total: u64 = zips.iter().map(|z| z.1).sum();
        for (_, len, path) in zips {
            if total <= cap {
                break;
            }
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("json"));
            total = total.saturating_sub(len);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod cache {
    use texturecraft_formats::ambientcg::Download;

    use super::Fetched;

    pub fn install(_: std::path::PathBuf, _: u64) {}
    pub fn load(_: &str, _: &Download) -> Option<Fetched> {
        None
    }
    pub fn store(_: &str, _: &Download, _: &Fetched) {}
}

// ---------------------------------------------------------------------------------------------
// Unpacking.

/// One image of a download: its channel (maps), its resource name, bytes and decoded pixels.
pub struct Map {
    pub channel: Option<Channel>,
    pub name: String,
    pub bytes: Vec<u8>,
    pub image: texturecraft_formats::DecodedImage,
}

/// A download unzipped and decoded (the slow part, done off the UI thread), ready to store.
pub struct Unpacked {
    pub kind: Kind,
    pub maps: Vec<Map>,
    pub cutout: Option<Map>,
    pub environment: Option<Map>,
    pub uv_tiling: [f32; 2],
    pub warnings: Vec<String>,
}

/// The resource name of zip entry `file`: its file name without the download's attributes
/// (`Wood096_2K-JPG_Color.jpg` → `Wood096_Color.jpg`), so a version at another resolution keeps
/// readable names.
fn resource_name(file: &str, attributes: &str) -> String {
    let leaf = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let leaf = if attributes.is_empty() { leaf.to_string() } else { leaf.replacen(&format!("_{attributes}"), "", 1) };
    texturecraft_formats::sanitize_resource_name(&leaf)
}

fn decode(file: &str, attributes: &str, bytes: Vec<u8>, channel: Option<Channel>) -> std::result::Result<Map, String> {
    let image = texturecraft_formats::decode_image(&bytes).map_err(|e| format!("{file}: {e}"))?;
    Ok(Map { channel, name: resource_name(file, attributes), bytes, image })
}

/// Unzip and decode download `bytes` of `kind` (`attributes` is the download, `2K-JPG`).
pub fn unpack(kind: Kind, title: &str, attributes: &str, bytes: &[u8]) -> std::result::Result<Unpacked, String> {
    let mut u = Unpacked { kind, maps: Vec::new(), cutout: None, environment: None, uv_tiling: [1.0, 1.0], warnings: Vec::new() };
    if kind == Kind::Hdri {
        let (file, image) = ambientcg::read_hdri_zip(bytes).map_err(|e| e.to_string())?;
        u.environment = Some(decode(&file, attributes, image, None)?);
        return Ok(u);
    }
    let z = ambientcg::read_material_zip(bytes, title).map_err(|e| e.to_string())?;
    let mut files = z.files;
    u.uv_tiling = z.material.uv_tiling;
    u.warnings = z.material.warnings;
    for (channel, value) in z.material.channels {
        let Mtlx::Image(file) = value else {
            u.warnings.push(format!("{} was skipped: a constant, not a map", channel.id()));
            continue;
        };
        let Some(b) = files.remove(&file) else { continue };
        match decode(&file, attributes, b, Some(channel)) {
            // A decal's opacity map shapes it; it is not painted into the surface's opacity.
            Ok(m) if kind == Kind::Decal && channel == Channel::Opacity => u.cutout = Some(m),
            Ok(m) => u.maps.push(m),
            Err(e) => u.warnings.push(format!("{} was skipped: {e}", channel.id())),
        }
    }
    if u.maps.is_empty() {
        return Err("no maps could be read from the download".into());
    }
    if kind == Kind::Decal && u.cutout.is_none() {
        u.warnings.push("no opacity map: the decal covers its whole frame".into());
    }
    Ok(u)
}

/// Whether `rec` is download `d` as ambientCG serves it now: the same download and size, and the
/// same SHA-1 when both are known.
pub fn is_current(rec: &ImportedAsset, d: &Download, remote_sha1: Option<&str>) -> bool {
    rec.download == d.attributes && rec.size == d.size && !matches!((rec.sha1.as_deref(), remote_sha1), (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b))
}

// ---------------------------------------------------------------------------------------------
// Session.

/// The download an import came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub download: String,
    pub size: u64,
    pub sha1: Option<String>,
}

/// A resource name not in use: `name`, else `2_name`, `3_name`…
fn unique(s: &Session, taken: &[String], name: &str) -> String {
    (1..)
        .map(|n| if n == 1 { name.to_string() } else { format!("{n}_{name}") })
        .find(|n| !s.doc.resources.contains_key(n) && !taken.contains(n))
        .unwrap_or_else(|| name.to_string())
}

/// Every image resource something in the document uses (layers, the environment, recorded assets).
fn used_images(s: &Session) -> Vec<String> {
    let mut used = Vec::new();
    for set in &s.doc.project.texture_sets {
        for root in &set.stack {
            crate::material_lib::images_used(&s.doc.project, *root, &mut used, 0);
        }
    }
    used.extend(s.doc.project.display.environment_resource().map(str::to_string));
    used.extend(s.tool.stencil.as_ref().map(|st| st.image.clone()));
    used.extend(s.doc.project.assets.iter().flat_map(|a| a.resources().cloned()));
    used
}

impl Session {
    /// The recorded ambientCG asset `id`, when all its resources are still in the project.
    pub fn ambientcg_asset(&self, id: &str) -> Option<&ImportedAsset> {
        self.doc.project.assets.iter().find(|a| a.site == SITE && a.id == id).filter(|a| a.resources().all(|r| self.doc.resources.contains_key(r)))
    }

    /// Store unpacked download `u` of asset `id` (`title`) as project resources and record it;
    /// when the project already holds the asset, its new maps replace the old ones in every
    /// layer. With `place`, also apply it (the placement params of `layer.add.decal` for a decal).
    /// One undo step.
    pub fn import_ambientcg(&mut self, id: &str, title: &str, version: Version, u: Unpacked, place: Option<&Value>) -> Result<Value> {
        let existing = self.doc.project.assets.iter().position(|a| a.site == SITE && a.id == id);
        let label = if existing.is_some() { "Update ambientCG asset" } else { "Import ambientCG asset" };
        let aspect = u.maps.iter().find(|m| m.channel == Some(Channel::BaseColor)).map(|m| m.image.width as f32 / m.image.height.max(1) as f32);
        // Fresh names for every image, so undo, redo and the decoded-image cache never disagree.
        let mut names: Vec<String> = Vec::new();
        let all = u.maps.iter().chain(u.cutout.iter()).chain(u.environment.iter());
        for m in all {
            let n = unique(self, &names, &m.name);
            names.push(n);
        }
        let mut name_of = names.iter().cloned();
        let maps: BTreeMap<Channel, String> = u.maps.iter().filter_map(|m| Some((m.channel?, name_of.next()?))).collect();
        let cutout = u.cutout.as_ref().and_then(|_| name_of.next());
        let environment = u.environment.as_ref().and_then(|_| name_of.next());
        let record = ImportedAsset {
            site: SITE.into(),
            id: id.into(),
            title: title.into(),
            kind: u.kind.id().into(),
            download: version.download.clone(),
            size: version.size,
            sha1: version.sha1.clone(),
            maps,
            cutout,
            environment,
            uv_tiling: u.uv_tiling,
        };
        let images: Vec<_> = u.maps.into_iter().chain(u.cutout).chain(u.environment).collect();
        let mut warnings = u.warnings;
        let (reply, replaced) = self.edit(label, |s| {
            for (n, m) in names.iter().zip(&images) {
                s.doc.resources.insert(n.clone(), Arc::new(m.bytes.clone()));
            }
            let mut replaced = Vec::new();
            match existing {
                Some(i) => {
                    let old = s.doc.project.assets.get(i).cloned().ok_or_else(|| EngineError::Other("asset record vanished".into()))?;
                    // The same role (channel, cutout, environment) moves to its new image.
                    let mut renames = BTreeMap::new();
                    for (c, n) in &old.maps {
                        if let Some(new) = record.maps.get(c) {
                            renames.insert(n.clone(), new.clone());
                        }
                    }
                    for (o, n) in [(&old.cutout, &record.cutout), (&old.environment, &record.environment)] {
                        if let (Some(o), Some(n)) = (o, n) {
                            renames.insert(o.clone(), n.clone());
                        }
                    }
                    s.doc.project.rename_images(&renames);
                    if let Some(slot) = s.doc.project.assets.get_mut(i) {
                        *slot = record.clone();
                    }
                    if let Some(st) = s.tool.stencil.as_mut()
                        && let Some(n) = renames.get(&st.image)
                    {
                        st.image = n.clone();
                    }
                    // The old images go unless something still shows them.
                    let used = used_images(s);
                    for n in old.resources().filter(|n| !used.contains(n)) {
                        s.doc.resources.remove(n);
                        replaced.push(n.clone());
                    }
                }
                None => s.doc.project.assets.push(record.clone()),
            }
            let reply = match place {
                Some(p) => apply(s, &record, p, aspect)?,
                None => json!({}),
            };
            Ok((reply, replaced))
        })?;
        for (n, m) in names.iter().zip(images) {
            self.images.insert(n.clone(), crate::resource_cmds::decoded(m.image));
        }
        self.sync_images();
        let mut reply = reply;
        if let Some(o) = reply.as_object_mut() {
            if let Some(w) = o.remove("warnings").and_then(|w| serde_json::from_value::<Vec<String>>(w).ok()) {
                warnings.extend(w);
            }
            o.insert("warnings".into(), json!(warnings));
            o.insert("resources".into(), json!(names));
            o.insert("updated".into(), json!(existing.is_some()));
            o.insert("replaced".into(), json!(replaced));
            o.insert("reused".into(), json!(false));
            o.insert("source".into(), source_json(&record));
        }
        Ok(reply)
    }

    /// Apply recorded ambientCG asset `id` again from the project's resources (no download): a
    /// material as a fill layer above the selection, a decal as a decal layer placed by `place`,
    /// an HDRI as the environment. One undo step.
    pub fn apply_ambientcg(&mut self, id: &str, place: &Value) -> Result<Value> {
        let rec = self.ambientcg_asset(id).cloned().ok_or_else(|| EngineError::Other(format!("ambientCG asset {id} is not in this project")))?;
        let aspect = rec.maps.get(&Channel::BaseColor).and_then(|n| self.images.get(n)).map(|i| i.width as f32 / i.height.max(1) as f32);
        let mut reply = self.edit("Apply ambientCG asset", |s| apply(s, &rec, place, aspect))?;
        if let Some(o) = reply.as_object_mut() {
            o.insert("reused".into(), json!(true));
            o.insert("source".into(), source_json(&rec));
        }
        Ok(reply)
    }
}

fn source_json(a: &ImportedAsset) -> Value {
    json!({"site": a.site, "id": a.id, "title": a.title, "download": a.download, "size": a.size, "license": ambientcg::LICENSE, "page": format!("{}/a/{}", ambientcg::SITE_URL, a.id)})
}

/// Apply recorded asset `rec` inside an edit.
fn apply(s: &mut Session, rec: &ImportedAsset, place: &Value, aspect: Option<f32>) -> Result<Value> {
    let cmd = "assets.ambientcg.import";
    let kind = Kind::from_id(&rec.kind).ok_or_else(|| bad(cmd, format!("unknown asset type `{}`", rec.kind)))?;
    if kind == Kind::Hdri {
        let env = rec.environment.as_ref().ok_or_else(|| bad(cmd, format!("{} has no environment image", rec.title)))?;
        let environment = format!("{}{env}", texturecraft_core::project::ENV_RESOURCE_PREFIX);
        s.doc.project.display.environment = environment.clone();
        return Ok(json!({"environment": environment, "resource": env, "warnings": []}));
    }
    let ts = s.ts();
    let available = s.doc.project.texture_set(ts).map(|t| t.channel_list()).unwrap_or_default();
    let mut warnings = Vec::new();
    let mut fill = Fill::default();
    for (c, n) in &rec.maps {
        if available.contains(c) {
            fill.sources.insert(*c, Source::image(n.clone()));
        } else {
            warnings.push(format!("{} was skipped: this texture set has no such channel", c.id()));
        }
    }
    if fill.sources.is_empty() {
        return Err(bad(cmd, format!("{}: none of its maps fits this texture set's channels", rec.title)));
    }
    let channels: Vec<Channel> = fill.sources.keys().copied().collect();
    let mut decal = None;
    fill.projection = match kind {
        Kind::Decal => {
            let d = crate::decal_cmds::new_decal(s, cmd, place, aspect.unwrap_or(1.0))?;
            fill.cutout = rec.cutout.clone();
            decal = Some(d);
            Projection::Decal(d)
        }
        _ => Projection::Uv { scale: rec.uv_tiling, rotation: 0.0, offset: [0.0, 0.0] },
    };
    let id = s.doc.project.alloc_id();
    let layer = crate::layer_cmds::insert_selected(s, Layer::new(id, rec.title.clone(), LayerKind::Fill(fill), &channels))?;
    let mut reply = json!({"layer": layer.0, "channels": channels.iter().map(|c| c.id()).collect::<Vec<_>>(), "warnings": warnings});
    if let Some(d) = decal {
        reply["decal"] = json!(d);
        reply["cutout"] = json!(rec.cutout);
    }
    Ok(reply)
}

// ---------------------------------------------------------------------------------------------
// Commands.

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "assets.ambientcg.search",
            "Search ambientCG",
            [],
            None,
            "{query?: keywords, type?: material|decal|hdri (material), sort?: popular|latest|downloads|alphabet, limit?: 1-100 (30), offset?} → {total, assets: [{id, title, type, tags, maps, page, thumbnail, downloads: [{attributes, size}], inProject}]}: ambientCG's CC0 library (needs network access)",
            always,
            search
        ),
        cmd!(
            "assets.ambientcg.import",
            "Import from ambientCG",
            [],
            None,
            "{id, resolution?: 1|2|4|8 (K, default 2, or the download the project holds), format?: jpg|png, and for decals the placement of layer.add.decal: x?, y?, viewport? | position?, normal?, up?, size?, rotation?, depth?} → a material becomes a fill layer, a decal a decal layer cut out by its opacity map, an HDRI the environment: {layer?, channels?, decal?, cutout?, environment?, resources, warnings, reused, updated, source}. An asset the project already holds is reused when ambientCG's file is unchanged (size and SHA-1), else downloaded and its maps replaced in every layer (needs network access)",
            always,
            import
        ),
        cmd!(
            "assets.ambientcg.list",
            "List ambientCG assets in the project",
            [],
            None,
            "{} → [{site, id, title, kind, download, size, sha1?, maps, cutout?, environment?, uv_tiling}]: the assets this project holds and their image resources",
            always,
            |s, _| Ok(json!(s.doc.project.assets))
        ),
    ]
}

fn asset_json(s: &Session, a: &Asset) -> Value {
    json!({
        "id": a.id,
        "title": a.title,
        "type": a.kind,
        "tags": a.tags,
        "maps": a.maps,
        "page": a.page,
        "thumbnail": a.thumbnail,
        "downloads": a.downloads.iter().map(|d| json!({"attributes": d.attributes, "size": d.size})).collect::<Vec<_>>(),
        "inProject": s.ambientcg_asset(&a.id).map(|r| &r.download),
    })
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.ambientcg.search";
    let kind = match strp(p, "type") {
        Some(t) => Kind::from_id(t).ok_or_else(|| bad(cmd, format!("unknown type `{t}` (material, decal or hdri)")))?,
        None => Kind::Material,
    };
    let sort = match strp(p, "sort") {
        Some(t) => Sort::from_id(t).ok_or_else(|| bad(cmd, format!("unknown sort `{t}` (popular, latest, downloads or alphabet)")))?,
        None => Sort::Popular,
    };
    let num = |k: &str, default: u64, max: u64| p.get(k).and_then(Value::as_u64).unwrap_or(default).min(max) as u32;
    let query = Query {
        text: strp(p, "query").unwrap_or_default().to_string(),
        kind,
        sort,
        limit: num("limit", 30, ambientcg::MAX_LIMIT as u64).max(1),
        offset: num("offset", 0, 1_000_000),
    };
    let reply = fetch(&query.url(), ambientcg::MAX_REPLY as u64, false).map_err(|e| bad(cmd, e))?;
    let page = ambientcg::parse_page(&reply.bytes).map_err(|e| bad(cmd, e.to_string()))?;
    Ok(json!({"total": page.total, "assets": page.assets.iter().map(|a| asset_json(s, a)).collect::<Vec<_>>()}))
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.ambientcg.import";
    let id = strp(p, "id").ok_or_else(|| bad(cmd, "`id` is required (an ambientCG asset id such as Wood096)"))?;
    if !ambientcg::valid_id(id) {
        return Err(bad(cmd, format!("`{id}` is not an ambientCG asset id")));
    }
    let format = match strp(p, "format").map(str::to_ascii_uppercase) {
        Some(f) if f == "JPG" || f == "PNG" => Some(f),
        Some(f) => return Err(bad(cmd, format!("unknown format `{f}` (jpg or png)"))),
        None => None,
    };
    let resolution = p.get("resolution").and_then(Value::as_u64).map(|r| r.clamp(1, 16) as u32);
    let asset = lookup(id).map_err(|e| bad(cmd, e))?;
    let kind = Kind::from_id(&asset.kind).ok_or_else(|| bad(cmd, format!("{id} is a {} (only materials, decals and HDRIs can be imported)", asset.kind)))?;
    let held = s.ambientcg_asset(id).cloned();
    // Without a resolution or format, the download the project holds; else the one asked for.
    let held_download =
        held.as_ref().filter(|_| resolution.is_none() && format.is_none()).and_then(|r| asset.downloads.iter().find(|d| d.attributes == r.download));
    let d = match held_download {
        Some(d) => d.clone(),
        None => asset.download(resolution.unwrap_or(2), format.as_deref().unwrap_or("JPG")).ok_or_else(|| bad(cmd, format!("{id} has no download")))?.clone(),
    };
    if let Some(rec) = &held
        && rec.download == d.attributes
        && rec.size == d.size
    {
        let sha1 = rec.sha1.as_ref().and_then(|_| remote_sha1(&d.url));
        if is_current(rec, &d, sha1.as_deref()) {
            return s.apply_ambientcg(id, p);
        }
    }
    let got = download(id, &d).map_err(|e| bad(cmd, e))?;
    let u = unpack(kind, &asset.title, &d.attributes, &got.bytes).map_err(|e| bad(cmd, format!("{}: {e}", asset.title)))?;
    s.import_ambientcg(id, &asset.title, Version { download: d.attributes.clone(), size: d.size, sha1: got.sha1 }, u, Some(p))
}
