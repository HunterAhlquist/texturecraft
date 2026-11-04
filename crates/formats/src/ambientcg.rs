//! [ambientCG](https://ambientcg.com): its public asset API (v3) and its download zips.
//!
//! ambientCG publishes PBR materials, decals and HDRIs under CC0 1.0 (public domain). This module is
//! pure: it builds request URLs, reads the API's JSON replies and reads downloaded zips from
//! bytes. The caller does the HTTP (the engine through an installed fetcher, the UI
//! asynchronously), so everything here also runs in the browser build and in offline tests.
//!
//! - [`Query::url`] builds an `/api/v3/assets` search; [`asset_url`] looks up one asset by id.
//! - [`parse_page`] reads a reply into [`Asset`]s with their thumbnails and downloads.
//! - [`Asset::download`] picks the zip for a resolution and file format.
//! - [`read_material_zip`] turns a material (or decal) zip into a [`Material`] (from the `.mtlx` it ships
//!   with, completed by ambientCG's file name suffixes) plus the bytes of the images it names.
//! - [`read_hdri_zip`] finds the HDR image (`.exr`, else `.hdr`) in an HDRI zip.
//!
//! Replies and zips are untrusted: sizes, counts and string lengths are capped, ids and
//! download URLs are validated, zip entry names may not leave the archive, and every failure is
//! an error, never a panic.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use serde_json::Value;
use texturecraft_core::Channel;
use zip::ZipArchive;

use crate::materialx_read::{self, Material, Value as Mtlx};
use crate::{FormatError, Result, malformed};

const KIND: &str = "ambientCG reply";
const ZIP_KIND: &str = "ambientCG download";

/// The asset search endpoint.
pub const API_URL: &str = "https://ambientcg.com/api/v3/assets";
/// The site, for credits and asset pages.
pub const SITE_URL: &str = "https://ambientcg.com";
/// ambientCG's license for every asset.
pub const LICENSE: &str = "CC0 1.0";
/// Download links must start with this (the API's links redirect from here to its file host).
const DOWNLOAD_PREFIX: &str = "https://ambientcg.com/get?file=";
/// Largest API reply read.
pub const MAX_REPLY: usize = 32 << 20;
/// Largest zip downloaded.
pub const MAX_DOWNLOAD: u64 = 1 << 30;
/// Largest page size asked for (the API allows 500; a shelf page needs far fewer).
pub const MAX_LIMIT: u32 = 100;
/// Largest single file read out of a zip.
const MAX_ENTRY: u64 = 512 << 20;
/// Largest total read out of a zip.
const MAX_UNZIPPED: u64 = 2 << 30;
const MAX_ENTRIES: usize = 512;
/// Longest string kept from a reply (titles, tags, URLs).
const MAX_STRING: usize = 2048;
const MAX_TAGS: usize = 64;
const MAX_DOWNLOADS: usize = 64;

/// The asset types TextureCraft can use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A PBR material: one fill layer with its maps.
    #[default]
    Material,
    /// A PBR decal: maps plus an opacity map, placed on the surface as a decal layer.
    Decal,
    /// An HDR environment: the viewport's lighting.
    Hdri,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Material, Kind::Decal, Kind::Hdri];

    /// The API's `type` value.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Material => "material",
            Kind::Decal => "decal",
            Kind::Hdri => "hdri",
        }
    }

    pub fn from_id(id: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.id().eq_ignore_ascii_case(id))
    }
}

/// Result order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Sort {
    #[default]
    Popular,
    Latest,
    Downloads,
    Alphabet,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Popular, Sort::Latest, Sort::Downloads, Sort::Alphabet];

    /// The API's `sort` value.
    pub fn id(self) -> &'static str {
        match self {
            Sort::Popular => "popular",
            Sort::Latest => "latest",
            Sort::Downloads => "downloads",
            Sort::Alphabet => "alphabet",
        }
    }

    pub fn from_id(id: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|s| s.id().eq_ignore_ascii_case(id))
    }
}

/// An asset search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// Keywords (all must match), separated by spaces or commas.
    pub text: String,
    pub kind: Kind,
    pub sort: Sort,
    /// Page size, 1..=[`MAX_LIMIT`].
    pub limit: u32,
    pub offset: u32,
}

impl Default for Query {
    fn default() -> Self {
        Query { text: String::new(), kind: Kind::Material, sort: Sort::Popular, limit: 30, offset: 0 }
    }
}

/// What every request asks the API to include.
const INCLUDE: &str = "title,type,tags,maps,url,thumbnails,downloads";

impl Query {
    /// The request URL.
    pub fn url(&self) -> String {
        let limit = self.limit.clamp(1, MAX_LIMIT);
        let text: String = self.text.trim().chars().take(200).collect();
        let mut url = format!("{API_URL}?type={}&sort={}&limit={limit}&offset={}&include={INCLUDE}", self.kind.id(), self.sort.id(), self.offset);
        if !text.is_empty() {
            url.push_str("&q=");
            url.push_str(&encode(&text));
        }
        url
    }
}

/// The URL that looks up the asset `id` (validate it with [`valid_id`] first).
pub fn asset_url(id: &str) -> String {
    format!("{API_URL}?id={}&include={INCLUDE}", encode(id))
}

/// Whether `id` looks like an ambientCG asset id (`Wood096`, `DayEnvironmentHDRI114`).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One downloadable file of an asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// Resolution and format, as the API names them: `2K-JPG`, `4K-PNG`, `1K` (HDRIs).
    pub attributes: String,
    pub url: String,
    /// Bytes (0 when the API didn't say).
    pub size: u64,
}

impl Download {
    /// The resolution in K (1, 2, 4…) and the format (`JPG`, `PNG`; none for HDRIs).
    pub fn resolution_format(&self) -> Option<(u32, Option<&str>)> {
        let (res, format) = match self.attributes.split_once('-') {
            Some((r, f)) => (r, Some(f)),
            None => (self.attributes.as_str(), None),
        };
        let k = res.strip_suffix('K').or_else(|| res.strip_suffix('k'))?.parse::<u32>().ok()?;
        Some((k, format))
    }
}

/// An asset from a search reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub id: String,
    pub title: String,
    /// The API's `type` (`material`, `hdri`…).
    pub kind: String,
    pub tags: Vec<String>,
    /// The maps a material has (`color`, `normal`, `roughness`…).
    pub maps: Vec<String>,
    /// Its page on ambientcg.com.
    pub page: String,
    /// A 128 px PNG thumbnail, when the reply has one.
    pub thumbnail: Option<String>,
    /// Downloadable zips (only ones whose link is ambientCG's download address).
    pub downloads: Vec<Download>,
}

impl Asset {
    /// The zip closest to `resolution` (in K) in `format` (`JPG` or `PNG`, ignored for HDRIs):
    /// the exact one, else the largest smaller one, else the smallest. Downloads in another
    /// format are used only when none is in `format`.
    pub fn download(&self, resolution: u32, format: &str) -> Option<&Download> {
        let parsed: Vec<(u32, &Download)> = self
            .downloads
            .iter()
            .filter_map(|d| d.resolution_format().map(|(k, f)| (k, f, d)))
            .filter(|(_, f, _)| f.is_none_or(|f| f.eq_ignore_ascii_case(format)))
            .map(|(k, _, d)| (k, d))
            .collect();
        let pool: Vec<(u32, &Download)> =
            if parsed.is_empty() { self.downloads.iter().filter_map(|d| d.resolution_format().map(|(k, _)| (k, d))).collect() } else { parsed };
        pool.iter().filter(|(k, _)| *k <= resolution).max_by_key(|(k, _)| *k).or_else(|| pool.iter().min_by_key(|(k, _)| *k)).map(|(_, d)| *d)
    }
}

/// One page of search results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    /// Matches in all pages.
    pub total: u64,
    pub assets: Vec<Asset>,
}

/// Read a search (or id lookup) reply.
pub fn parse_page(bytes: &[u8]) -> Result<Page> {
    if bytes.len() > MAX_REPLY {
        return Err(FormatError::TooLarge(format!("the ambientCG reply is over {} MB", MAX_REPLY >> 20)));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| malformed(KIND, e))?;
    let list = v.get("assets").and_then(Value::as_array).ok_or_else(|| malformed(KIND, "no `assets` list"))?;
    let total = v.get("totalResults").and_then(Value::as_u64).unwrap_or(list.len() as u64);
    let assets = list.iter().take(MAX_LIMIT as usize * 5).filter_map(parse_asset).collect();
    Ok(Page { total, assets })
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| s.len() <= MAX_STRING).map(str::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter(|s| s.len() <= 64).take(MAX_TAGS).map(str::to_string).collect())
        .unwrap_or_default()
}

/// An asset entry; entries without a valid id are dropped.
fn parse_asset(v: &Value) -> Option<Asset> {
    let id = text(v, "id").filter(|id| valid_id(id))?;
    let thumbs = v.get("thumbnails");
    let thumbnail = ["128-PNG", "256-PNG", "64-PNG"]
        .iter()
        .find_map(|k| thumbs.and_then(|t| t.get(*k)).and_then(Value::as_str))
        .filter(|u| u.starts_with("https://") && u.len() <= MAX_STRING)
        .map(str::to_string);
    let downloads = v
        .get("downloads")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    let url = text(d, "url").filter(|u| is_download_url(u))?;
                    let attributes = text(d, "attributes").filter(|a| a.len() <= 32)?;
                    if d.get("extension").and_then(Value::as_str).is_some_and(|e| !e.eq_ignore_ascii_case("zip")) {
                        return None;
                    }
                    let d = Download { attributes, url, size: d.get("size").and_then(Value::as_u64).unwrap_or(0) };
                    // Only plain maps (`2K-JPG`, `2K-PNG`, `2K` for HDRIs): not animated sequences.
                    let usable = d.resolution_format().is_some_and(|(_, f)| f.is_none_or(|f| f.eq_ignore_ascii_case("JPG") || f.eq_ignore_ascii_case("PNG")));
                    usable.then_some(d)
                })
                .take(MAX_DOWNLOADS)
                .collect()
        })
        .unwrap_or_default();
    let page = text(v, "url").filter(|u| u.starts_with(SITE_URL)).unwrap_or_else(|| format!("{SITE_URL}/a/{id}"));
    Some(Asset {
        title: text(v, "title").unwrap_or_else(|| id.clone()),
        kind: text(v, "type").unwrap_or_default(),
        tags: strings(v, "tags"),
        maps: strings(v, "maps"),
        page,
        thumbnail,
        downloads,
        id,
    })
}

/// Whether `url` is an ambientCG download link (the only addresses downloads are fetched from).
pub fn is_download_url(url: &str) -> bool {
    url.strip_prefix(DOWNLOAD_PREFIX)
        .is_some_and(|file| !file.is_empty() && file.len() <= 200 && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
}

// ---------------------------------------------------------------------------------------------
// Zips.

/// A material read from a zip: the material (image values are entry names in `files`) and the
/// bytes of every image it uses.
#[derive(Debug)]
pub struct ZipMaterial {
    pub material: Material,
    pub files: HashMap<String, Vec<u8>>,
}

/// ambientCG's map file suffixes (`Wood096_2K-JPG_Roughness.jpg`) and their channels.
/// `NormalDX` is not here: the OpenGL normal ships alongside it.
const SUFFIXES: [(&str, Channel); 8] = [
    ("Color", Channel::BaseColor),
    ("NormalGL", Channel::Normal),
    ("Roughness", Channel::Roughness),
    ("Metalness", Channel::Metallic),
    ("Displacement", Channel::Height),
    ("AmbientOcclusion", Channel::AmbientOcclusion),
    ("Opacity", Channel::Opacity),
    ("Emission", Channel::Emissive),
];

const IMAGE_EXTS: [&str; 7] = ["jpg", "jpeg", "png", "exr", "tif", "tiff", "hdr"];

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn zip_err(e: zip::result::ZipError) -> FormatError {
    malformed(ZIP_KIND, e)
}

/// Normalize an entry name or a path relative to `dir` inside the archive: forward slashes, no
/// `.` parts. None when it would leave the archive.
fn inside(dir: &str, rel: &str) -> Option<String> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The archive's file names, leaving out directories and names that aren't plain relative paths
/// (`..`, absolute, `./`), so each can be opened with `by_name`.
fn entry_names<R: Read + std::io::Seek>(zip: &ZipArchive<R>) -> Result<Vec<String>> {
    if zip.len() > MAX_ENTRIES {
        return Err(FormatError::TooLarge(format!("the zip has {} files (limit {MAX_ENTRIES})", zip.len())));
    }
    // Names that don't decode are left out, like unsafe ones.
    Ok(zip.file_names().filter_map(|n| n.ok()).filter(|n| !n.ends_with('/')).filter_map(|n| inside("", &n).filter(|m| *m == *n)).collect())
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str, cap: u64, total: &mut u64) -> Result<Vec<u8>> {
    let f = zip.by_name(name).map_err(zip_err)?;
    let mut out = Vec::new();
    f.take(cap.saturating_add(1)).read_to_end(&mut out).map_err(|e| malformed(ZIP_KIND, format!("{name}: {e}")))?;
    if out.len() as u64 > cap {
        return Err(FormatError::TooLarge(format!("{name} is larger than {} MB", cap >> 20)));
    }
    *total = total.saturating_add(out.len() as u64);
    if *total > MAX_UNZIPPED {
        return Err(FormatError::TooLarge(format!("the zip unpacks to more than {} MB", MAX_UNZIPPED >> 20)));
    }
    Ok(out)
}

/// Read a material zip: the `.mtlx` it ships with (when there is one and it reads) gives the
/// channels; ambientCG's file name suffixes add channels it leaves out (ambient occlusion) or
/// stand in for it. `name` names the material (the asset's title).
pub fn read_material_zip(bytes: &[u8], name: &str) -> Result<ZipMaterial> {
    crate::guard(ZIP_KIND, || read_material(bytes, name))
}

fn read_material(bytes: &[u8], name: &str) -> Result<ZipMaterial> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
    let names = entry_names(&zip)?;
    let mut total = 0u64;
    let mut warnings = Vec::new();
    let mut channels: Vec<(Channel, Mtlx)> = Vec::new();
    let mut uv_tiling = [1.0, 1.0];
    if let Some(doc) = names.iter().find(|n| ext(n) == "mtlx") {
        let dir = doc.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let read = read_entry(&mut zip, doc, materialx_read::MAX_BYTES as u64, &mut total).and_then(|b| materialx_read::read_materialx(&b, None));
        match read {
            Ok(m) => {
                uv_tiling = m.uv_tiling;
                warnings.extend(m.warnings);
                for (channel, value) in m.channels {
                    match value {
                        Mtlx::Image(file) => match inside(dir, &file).filter(|f| names.contains(f)) {
                            Some(f) => channels.push((channel, Mtlx::Image(f))),
                            None => warnings.push(format!("{} was skipped: {file} is not in the zip", channel.id())),
                        },
                        constant => channels.push((channel, constant)),
                    }
                }
            }
            Err(e) => warnings.push(format!("{doc} was not read ({e}); maps were matched by file name")),
        }
    }
    for (suffix, channel) in SUFFIXES {
        if channels.iter().any(|(c, _)| *c == channel) {
            continue;
        }
        let file = names.iter().find(|n| {
            let stem = n.rsplit('/').next().unwrap_or(n);
            let stem = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
            IMAGE_EXTS.contains(&ext(n).as_str()) && stem.rsplit_once('_').is_some_and(|(_, s)| s.eq_ignore_ascii_case(suffix))
        });
        if let Some(f) = file {
            channels.push((channel, Mtlx::Image(f.clone())));
        }
    }
    if channels.is_empty() {
        return Err(malformed(ZIP_KIND, "no material maps found (it should hold a .mtlx or *_Color, *_NormalGL… images)"));
    }
    // A fixed order, whichever way each channel was found.
    channels.sort_by_key(|(c, _)| Channel::ALL.iter().position(|a| a == c).unwrap_or(usize::MAX));
    let mut files = HashMap::new();
    for (_, v) in &channels {
        if let Mtlx::Image(f) = v
            && !files.contains_key(f)
        {
            let b = read_entry(&mut zip, f, MAX_ENTRY, &mut total)?;
            files.insert(f.clone(), b);
        }
    }
    let name = if name.trim().is_empty() { "ambientCG material".to_string() } else { name.trim().to_string() };
    Ok(ZipMaterial { material: Material { name, channels, uv_tiling, warnings }, files })
}

/// The HDR image of an HDRI zip: its entry name and bytes (`.exr` preferred, else `.hdr`).
pub fn read_hdri_zip(bytes: &[u8]) -> Result<(String, Vec<u8>)> {
    crate::guard(ZIP_KIND, || {
        let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
        let names = entry_names(&zip)?;
        let pick = names.iter().find(|n| ext(n) == "exr").or_else(|| names.iter().find(|n| ext(n) == "hdr"));
        let name = pick.ok_or_else(|| malformed(ZIP_KIND, "no .exr or .hdr image in the HDRI zip"))?.clone();
        let mut total = 0;
        let b = read_entry(&mut zip, &name, MAX_ENTRY, &mut total)?;
        let leaf = name.rsplit('/').next().unwrap_or(&name).to_string();
        Ok((leaf, b))
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (n, b) in files {
            w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const REPLY: &str = r#"{
        "totalResults": 295,
        "nextPageHttp": "https://ambientcg.com/api/v3/assets?offset=2",
        "assets": [
            {"id": "Wood096", "title": "Wood 096", "type": "material", "tags": ["wood", "grain"],
             "maps": ["color", "normal", "roughness"], "url": "https://ambientcg.com/a/Wood096",
             "thumbnails": {"64-PNG": "https://media.example/64/Wood096.png", "128-PNG": "https://media.example/128/Wood096.png"},
             "downloads": [
                {"attributes": "1K-JPG", "extension": "zip", "url": "https://ambientcg.com/get?file=Wood096_1K-JPG.zip", "size": 5358165},
                {"attributes": "2K-JPG", "extension": "zip", "url": "https://ambientcg.com/get?file=Wood096_2K-JPG.zip", "size": 19620619},
                {"attributes": "8K-JPG", "extension": "zip", "url": "https://ambientcg.com/get?file=Wood096_8K-JPG.zip", "size": 1},
                {"attributes": "1K-PNG", "extension": "zip", "url": "https://ambientcg.com/get?file=Wood096_1K-PNG.zip", "size": 15153371},
                {"attributes": "1K-JPG-ANIMATED", "extension": "zip", "url": "https://ambientcg.com/get?file=Wood096_1K-JPG-ANIMATED.zip", "size": 9},
                {"attributes": "2K-PNG", "extension": "zip", "url": "https://evil.example/get?file=Wood096_2K-PNG.zip", "size": 1}
             ]},
            {"id": "../etc", "title": "bad"},
            {"id": "DayEnvironmentHDRI114", "type": "hdri",
             "downloads": [{"attributes": "1K", "extension": "zip", "url": "https://ambientcg.com/get?file=DayEnvironmentHDRI114_1K.zip", "size": 4}]}
        ]
    }"#;

    #[test]
    fn query_urls_are_encoded_and_clamped() {
        let q = Query { text: " red brick & tile ".into(), kind: Kind::Material, sort: Sort::Latest, limit: 9999, offset: 60 };
        let u = q.url();
        assert!(u.starts_with("https://ambientcg.com/api/v3/assets?type=material&sort=latest&limit=100&offset=60&include="), "{u}");
        assert!(u.ends_with("&q=red%20brick%20%26%20tile"), "{u}");
        assert!(!Query::default().url().contains("&q="));
        assert_eq!(asset_url("Wood096"), format!("{API_URL}?id=Wood096&include={INCLUDE}"));
        assert!(valid_id("Wood096") && valid_id("DayEnvironmentHDRI114"));
        assert!(!valid_id("") && !valid_id("../x") && !valid_id("a b") && !valid_id(&"a".repeat(65)));
        assert_eq!(Kind::from_id("HDRI"), Some(Kind::Hdri));
        assert_eq!(Kind::from_id("decal"), Some(Kind::Decal));
        assert!(Query { kind: Kind::Decal, ..Query::default() }.url().contains("type=decal&"));
        assert_eq!(Sort::from_id("nope"), None);
    }

    #[test]
    fn replies_parse_and_drop_bad_entries() {
        let p = parse_page(REPLY.as_bytes()).unwrap();
        assert_eq!(p.total, 295);
        assert_eq!(p.assets.len(), 2, "the entry with a bad id is dropped");
        let w = &p.assets[0];
        assert_eq!((w.id.as_str(), w.title.as_str(), w.kind.as_str()), ("Wood096", "Wood 096", "material"));
        assert_eq!(w.thumbnail.as_deref(), Some("https://media.example/128/Wood096.png"));
        assert_eq!(w.downloads.len(), 4, "a download off ambientCG and an animated one are dropped");
        assert_eq!(w.tags, ["wood", "grain"]);
        let h = &p.assets[1];
        assert_eq!(h.title, "DayEnvironmentHDRI114", "the id stands in for a missing title");
        assert_eq!(h.page, "https://ambientcg.com/a/DayEnvironmentHDRI114");
        for bad in ["", "[]", "{}", "{\"assets\": 3}", "nul"] {
            assert!(parse_page(bad.as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(parse_page(b"{\"assets\": [1, null, {}]}").unwrap().assets.len(), 0);
    }

    #[test]
    fn downloads_pick_the_nearest_resolution() {
        let p = parse_page(REPLY.as_bytes()).unwrap();
        let w = &p.assets[0];
        let pick = |r, f| w.download(r, f).map(|d| d.attributes.as_str());
        assert_eq!(pick(2, "JPG"), Some("2K-JPG"));
        assert_eq!(pick(4, "jpg"), Some("2K-JPG"), "the largest below 4K");
        assert_eq!(pick(8, "JPG"), Some("8K-JPG"));
        assert_eq!(pick(2, "PNG"), Some("1K-PNG"), "the off-site 2K-PNG is gone");
        assert_eq!(pick(2, "TIF"), Some("2K-JPG"), "another format when none is in this one");
        let h = &p.assets[1];
        assert_eq!(h.download(4, "JPG").map(|d| d.attributes.as_str()), Some("1K"), "HDRIs have no format");
        assert!(is_download_url("https://ambientcg.com/get?file=Wood096_1K-JPG.zip"));
        for bad in [
            "https://ambientcg.com/get?file=",
            "https://ambientcg.com/get?file=a/../b",
            "http://ambientcg.com/get?file=a.zip",
            "https://ambientcg.com.evil/get?file=a",
        ] {
            assert!(!is_download_url(bad), "{bad}");
        }
    }

    const MTLX: &str = r#"<?xml version="1.0"?>
<materialx version="1.39" fileprefix="./">
  <open_pbr_surface name="S" type="surfaceshader">
    <input nodename="C" name="base_color" type="color3" />
    <input nodename="N" name="geometry_normal" type="vector3" />
    <input nodename="R" name="specular_roughness" type="float" />
  </open_pbr_surface>
  <surfacematerial name="M" type="material">
    <input nodename="S" name="surfaceshader" type="surfaceshader" />
    <input nodename="D" name="displacementshader" type="displacementshader" />
  </surfacematerial>
  <tiledimage name="C" type="color3"><input colorspace="srgb_texture" value="W_1K-JPG_Color.jpg" name="file" type="filename" /><input value="2.0, 2.0" name="uvtiling" type="vector2" /></tiledimage>
  <tiledimage name="Dimg" type="float"><input value="W_1K-JPG_Displacement.jpg" name="file" type="filename" /></tiledimage>
  <displacement name="D" type="displacementshader"><input nodename="Dimg" name="displacement" type="float" /></displacement>
  <tiledimage name="Nimg" type="vector3"><input value="W_1K-JPG_NormalGL.jpg" name="file" type="filename" /></tiledimage>
  <normalmap name="N" type="vector3"><input nodename="Nimg" name="in" type="vector3" /></normalmap>
  <tiledimage name="R" type="float"><input value="../outside.jpg" name="file" type="filename" /></tiledimage>
</materialx>"#;

    #[test]
    fn material_zips_read_the_mtlx_and_fill_in_by_suffix() {
        let z = zip_of(&[
            ("W_1K-JPG.mtlx", MTLX.as_bytes()),
            ("W_1K-JPG_Color.jpg", b"c"),
            ("W_1K-JPG_Displacement.jpg", b"d"),
            ("W_1K-JPG_NormalGL.jpg", b"n"),
            ("W_1K-JPG_NormalDX.jpg", b"x"),
            ("W_1K-JPG_Roughness.jpg", b"r"),
            ("W_1K-JPG_AmbientOcclusion.jpg", b"a"),
            ("W_1K-JPG.blend", b"blend"),
            ("W.png", b"preview"),
        ]);
        let m = read_material_zip(&z, " Wood 096 ").unwrap();
        assert_eq!(m.material.name, "Wood 096");
        assert_eq!(m.material.uv_tiling, [2.0, 2.0]);
        let got: Vec<(Channel, String)> =
            m.material.channels.iter().map(|(c, v)| (*c, if let Mtlx::Image(f) = v { f.clone() } else { String::new() })).collect();
        let want = [
            (Channel::BaseColor, "W_1K-JPG_Color.jpg"),
            (Channel::Roughness, "W_1K-JPG_Roughness.jpg"),
            (Channel::Normal, "W_1K-JPG_NormalGL.jpg"),
            (Channel::Height, "W_1K-JPG_Displacement.jpg"),
            (Channel::AmbientOcclusion, "W_1K-JPG_AmbientOcclusion.jpg"),
        ];
        for (c, f) in want {
            assert!(got.contains(&(c, f.to_string())), "{c:?} {f} in {got:?}");
        }
        assert_eq!(got.len(), want.len());
        assert!(m.material.warnings.iter().any(|w| w.contains("outside.jpg")), "{:?}", m.material.warnings);
        assert_eq!(m.files.len(), 5, "only the images in use are read");
        assert_eq!(m.files["W_1K-JPG_Color.jpg"], b"c");
    }

    #[test]
    fn material_zips_without_a_readable_mtlx_use_suffixes() {
        let z = zip_of(&[("X.mtlx", b"<not xml"), ("sub/X_2K-PNG_Color.png", b"c"), ("sub/X_2K-PNG_Metalness.png", b"m")]);
        let m = read_material_zip(&z, "").unwrap();
        assert_eq!(m.material.name, "ambientCG material");
        assert_eq!(
            m.material.channels,
            vec![(Channel::BaseColor, Mtlx::Image("sub/X_2K-PNG_Color.png".into())), (Channel::Metallic, Mtlx::Image("sub/X_2K-PNG_Metalness.png".into()))]
        );
        assert!(m.material.warnings.iter().any(|w| w.contains("X.mtlx")));
        assert!(read_material_zip(&zip_of(&[("readme.txt", b"hi")]), "x").is_err());
        assert!(read_material_zip(b"not a zip", "x").is_err());
        assert!(read_material_zip(&[], "x").is_err());
    }

    #[test]
    fn hdri_zips_give_their_hdr_image() {
        let z = zip_of(&[("H_1K_TONEMAPPED.jpg", b"j"), ("H_1K.hdr", b"h"), ("H_1K_HDR.exr", b"e")]);
        assert_eq!(read_hdri_zip(&z).unwrap(), ("H_1K_HDR.exr".to_string(), b"e".to_vec()));
        assert_eq!(read_hdri_zip(&zip_of(&[("d/H.hdr", b"h")])).unwrap().0, "H.hdr");
        assert!(read_hdri_zip(&zip_of(&[("H.jpg", b"j")])).is_err());
    }

    #[test]
    fn entry_names_stay_inside_the_archive() {
        assert_eq!(inside("", "./a/b.jpg").as_deref(), Some("a/b.jpg"));
        assert_eq!(inside("sub", "../a.jpg").as_deref(), Some("a.jpg"));
        assert_eq!(inside("", "../a.jpg"), None);
        assert_eq!(inside("", "/etc/passwd"), None);
        assert_eq!(inside("", "C:/x"), None);
        assert_eq!(inside("", "a\\b.jpg").as_deref(), Some("a/b.jpg"));
        let z = zip_of(&[("../evil_Color.jpg", b"e"), ("ok_Color.jpg", b"c")]);
        let m = read_material_zip(&z, "m").unwrap();
        assert_eq!(m.material.channels, vec![(Channel::BaseColor, Mtlx::Image("ok_Color.jpg".into()))]);
    }
}
