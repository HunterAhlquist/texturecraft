//! ambientCG (<https://ambientcg.com>, CC0 materials and HDRIs) in the engine.
//!
//! - [`Session::import_ambientcg_zip`] turns a downloaded zip into a fill layer (a material) or
//!   the viewport environment (an HDRI). The UI downloads asynchronously and calls it.
//! - `assets.ambientcg.search` and `assets.ambientcg.import` do the same for agents (control
//!   channel, CLI, MCP): they block while they fetch.
//!
//! The engine does no networking of its own: the app and the CLI install a fetcher
//! ([`install_fetcher`]). Without one (tests, the browser build) the commands report that the
//! network is unavailable. Only ambientCG's API and download addresses are ever fetched.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use texturecraft_formats::ambientcg::{self, Asset, Kind, Query, Sort};

use crate::commands::{CommandSpec, always, bad, strp};
use crate::{EngineError, Result, Session, cmd};

/// Fetches `url` with a GET and returns the body, refusing bodies over `max_bytes`.
pub type Fetcher = dyn Fn(&str, u64) -> std::result::Result<Vec<u8>, String> + Send + Sync;

static FETCHER: OnceLock<Arc<Fetcher>> = OnceLock::new();

/// Let the ambientCG commands reach the network (the first call wins).
pub fn install_fetcher(fetcher: Arc<Fetcher>) {
    let _ = FETCHER.set(fetcher);
}

fn fetch(cmd: &str, url: &str, max: u64) -> Result<Vec<u8>> {
    let f = FETCHER.get().ok_or_else(|| bad(cmd, "network access is not available here (the desktop app and texturecraft-cli have it)"))?;
    let bytes = f(url, max).map_err(|e| bad(cmd, format!("{url}: {e}")))?;
    if bytes.len() as u64 > max {
        return Err(bad(cmd, format!("{url}: the reply is larger than {} MB", max >> 20)));
    }
    Ok(bytes)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "assets.ambientcg.search",
            "Search ambientCG",
            [],
            None,
            "{query?: keywords, type?: material|hdri (material), sort?: popular|latest|downloads|alphabet, limit?: 1-100 (30), offset?} → {total, assets: [{id, title, type, tags, maps, page, thumbnail, downloads: [{attributes, size}]}]}: ambientCG's CC0 library (needs network access)",
            always,
            search
        ),
        cmd!(
            "assets.ambientcg.import",
            "Import from ambientCG",
            [],
            None,
            "{id, resolution?: 1|2|4|8 (K, default 2), format?: jpg|png (jpg)} → a material becomes a fill layer {layer, channels, resources, warnings, source}; an HDRI becomes the environment {environment, resource, source} (needs network access)",
            always,
            import
        ),
    ]
}

fn asset_json(a: &Asset) -> Value {
    json!({
        "id": a.id,
        "title": a.title,
        "type": a.kind,
        "tags": a.tags,
        "maps": a.maps,
        "page": a.page,
        "thumbnail": a.thumbnail,
        "downloads": a.downloads.iter().map(|d| json!({"attributes": d.attributes, "size": d.size})).collect::<Vec<_>>(),
    })
}

fn search(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.ambientcg.search";
    let kind = match strp(p, "type") {
        Some(t) => Kind::from_id(t).ok_or_else(|| bad(cmd, format!("unknown type `{t}` (material or hdri)")))?,
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
    let bytes = fetch(cmd, &query.url(), ambientcg::MAX_REPLY as u64)?;
    let page = ambientcg::parse_page(&bytes).map_err(|e| bad(cmd, e.to_string()))?;
    Ok(json!({"total": page.total, "assets": page.assets.iter().map(asset_json).collect::<Vec<_>>()}))
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "assets.ambientcg.import";
    let id = strp(p, "id").ok_or_else(|| bad(cmd, "`id` is required (an ambientCG asset id such as Wood096)"))?;
    if !ambientcg::valid_id(id) {
        return Err(bad(cmd, format!("`{id}` is not an ambientCG asset id")));
    }
    let resolution = p.get("resolution").and_then(Value::as_u64).unwrap_or(2).clamp(1, 16) as u32;
    let format = match strp(p, "format").map(str::to_ascii_uppercase) {
        Some(f) if f == "JPG" || f == "PNG" => f,
        Some(f) => return Err(bad(cmd, format!("unknown format `{f}` (jpg or png)"))),
        None => "JPG".into(),
    };
    let reply = fetch(cmd, &ambientcg::asset_url(id), ambientcg::MAX_REPLY as u64)?;
    let page = ambientcg::parse_page(&reply).map_err(|e| bad(cmd, e.to_string()))?;
    let asset = page.assets.into_iter().find(|a| a.id == id).ok_or_else(|| bad(cmd, format!("ambientCG has no asset `{id}`")))?;
    let kind = Kind::from_id(&asset.kind).ok_or_else(|| bad(cmd, format!("{id} is a {} (only materials and HDRIs can be imported)", asset.kind)))?;
    let download = asset.download(resolution, &format).ok_or_else(|| bad(cmd, format!("{id} has no download")))?.clone();
    if download.size > ambientcg::MAX_DOWNLOAD {
        return Err(bad(
            cmd,
            format!("{id} {} is {} MB (limit {} MB): pick a lower resolution", download.attributes, download.size >> 20, ambientcg::MAX_DOWNLOAD >> 20),
        ));
    }
    let bytes = fetch(cmd, &download.url, ambientcg::MAX_DOWNLOAD)?;
    s.import_ambientcg_zip(kind, &asset.id, &asset.title, &download.attributes, &bytes)
}

impl Session {
    /// Import a downloaded ambientCG zip: a material becomes one fill layer above the selection
    /// (one undo step), an HDRI becomes a resource and the viewport environment. `id`, `title`
    /// and `attributes` (`2K-JPG`) describe the asset; the reply names them under `source`.
    pub fn import_ambientcg_zip(&mut self, kind: Kind, id: &str, title: &str, attributes: &str, bytes: &[u8]) -> Result<Value> {
        let source = json!({"site": "ambientCG", "id": id, "title": title, "download": attributes, "license": ambientcg::LICENSE, "page": format!("{}/a/{id}", ambientcg::SITE_URL)});
        let mut reply = match kind {
            Kind::Material => {
                let z = ambientcg::read_material_zip(bytes, title).map_err(|e| EngineError::Other(format!("{title}: {e}")))?;
                let files = RefCell::new(z.files);
                let load = |f: &str| files.borrow_mut().remove(f).ok_or_else(|| format!("{f} is not in the zip"));
                // The asset's title as is ("Wood 096"): a counter after its number would confuse.
                let name = z.material.name.clone();
                self.import_material(z.material, Some(&name), &load)?
            }
            Kind::Hdri => {
                let (file, image) = ambientcg::read_hdri_zip(bytes).map_err(|e| EngineError::Other(format!("{title}: {e}")))?;
                let name = self.import_resource_bytes(&file, image)?;
                let environment = format!("{}{name}", texturecraft_core::project::ENV_RESOURCE_PREFIX);
                crate::execute(self, "display.set", &json!({ "environment": environment }))?;
                json!({"environment": environment, "resource": name})
            }
        };
        if let Some(o) = reply.as_object_mut() {
            o.insert("source".into(), source);
        }
        Ok(reply)
    }
}
