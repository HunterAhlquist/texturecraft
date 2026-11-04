//! ambientCG: search, import, reuse and update through an installed fetcher that serves canned
//! replies and zips made here (no network).

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use texturecraft_engine::ambientcg_cmds::{Fetched, install_fetcher, unpack};
use texturecraft_engine::core::{Channel, LayerId, LayerKind, Source};
use texturecraft_engine::formats::ambientcg::Kind;
use texturecraft_engine::geom::primitives;
use texturecraft_engine::{Session, execute};

fn session() -> Session {
    Session::new_project("t", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap()
}

fn png(rgb: [u8; 3]) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(4, 4, image::Rgb(rgb)).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn exr() -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let img = image::Rgb32FImage::from_pixel(8, 4, image::Rgb([2.0, 1.5, 1.0]));
    image::DynamicImage::ImageRgb32F(img).write_to(&mut out, image::ImageFormat::OpenExr).unwrap();
    out.into_inner()
}

fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (n, b) in files {
        w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// A material zip without a .mtlx (maps matched by ambientCG's file name suffixes).
fn material_zip(id: &str, color: [u8; 3]) -> Vec<u8> {
    zip_of(&[
        (&format!("{id}_1K-PNG_Color.png"), png(color)),
        (&format!("{id}_1K-PNG_NormalGL.png"), png([128, 128, 255])),
        (&format!("{id}_1K-PNG_NormalDX.png"), png([128, 127, 255])),
        (&format!("{id}_1K-PNG_Roughness.png"), png([200, 200, 200])),
        (&format!("{id}_1K-PNG_Displacement.png"), png([90, 90, 90])),
        (&format!("{id}.png"), png([0, 0, 0])),
    ])
}

/// A road marking: an opaque (JPG-like) color map, a normal map and a separate opacity map.
fn decal_zip() -> Vec<u8> {
    let mut opacity = Cursor::new(Vec::new());
    image::GrayImage::from_fn(8, 4, |x, _| image::Luma([if x < 4 { 255 } else { 0 }])).write_to(&mut opacity, image::ImageFormat::Png).unwrap();
    zip_of(&[
        ("RoadLines019A_1K-JPG_Color.png", png([240, 240, 240])),
        ("RoadLines019A_1K-JPG_NormalGL.png", png([128, 128, 255])),
        ("RoadLines019A_1K-JPG_Roughness.png", png([150, 150, 150])),
        ("RoadLines019A_1K-JPG_Opacity.png", opacity.into_inner()),
    ])
}

/// What ambientCG serves for a material: download size, SHA-1, base color.
type Remote = (u64, String, [u8; 3]);
/// What ambientCG serves for each material id.
static REMOTE: Mutex<BTreeMap<String, Remote>> = Mutex::new(BTreeMap::new());
/// Every URL the fake fetcher was asked for (`HEAD ` before a HEAD).
static REQUESTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn set_remote(id: &str, size: u64, sha1: &str, color: [u8; 3]) {
    REMOTE.lock().unwrap().insert(id.to_string(), (size, sha1.to_string(), color));
}

fn reply(assets: Value) -> Vec<u8> {
    json!({"totalResults": 2, "assets": assets}).to_string().into_bytes()
}

fn material(id: &str, size: u64) -> Value {
    json!({"id": id, "title": format!("Bricks {id}"), "type": "material", "thumbnails": {"128-PNG": "https://media.example/t.png"},
           "downloads": [
               {"attributes": "1K-PNG", "extension": "zip", "url": format!("https://ambientcg.com/get?file={id}_1K-PNG.zip"), "size": size},
               {"attributes": "2K-PNG", "extension": "zip", "url": format!("https://ambientcg.com/get?file={id}_2K-PNG.zip"), "size": 4_000_000_000u64}]})
}

fn sky() -> Value {
    json!({"id": "Sky002", "title": "Sky 002", "type": "hdri",
           "downloads": [{"attributes": "1K", "extension": "zip", "url": "https://ambientcg.com/get?file=Sky002_1K.zip", "size": 100}]})
}

fn decal() -> Value {
    json!({"id": "RoadLines019A", "title": "Road Lines 019 A", "type": "decal",
           "downloads": [{"attributes": "1K-JPG", "extension": "zip", "url": "https://ambientcg.com/get?file=RoadLines019A_1K-JPG.zip", "size": 100}]})
}

fn serve(url: &str, head: bool) -> Result<Fetched, String> {
    REQUESTS.lock().unwrap().push(if head { format!("HEAD {url}") } else { url.to_string() });
    let remote = REMOTE.lock().unwrap().clone();
    let api_id = url.strip_prefix("https://ambientcg.com/api/v3/assets?id=").and_then(|r| r.split('&').next());
    let body = |bytes: Vec<u8>| Fetched { bytes, sha1: None };
    if let Some(id) = api_id {
        return Ok(body(match id {
            "Sky002" => reply(json!([sky()])),
            "RoadLines019A" => reply(json!([decal()])),
            "Model1" => reply(json!([{"id": "Model1", "type": "3d-model", "downloads": []}])),
            id => match remote.get(id) {
                Some((size, _, _)) => reply(json!([material(id, *size)])),
                None => reply(json!([])),
            },
        }));
    }
    if url.starts_with("https://ambientcg.com/api/v3/assets?") {
        return Ok(body(reply(json!([material("Brick001", 100), sky()]))));
    }
    let file = url.strip_prefix("https://ambientcg.com/get?file=").ok_or_else(|| format!("unexpected request {url}"))?;
    let (bytes, sha1) = match file {
        "Sky002_1K.zip" => (zip_of(&[("Sky002_1K_TONEMAPPED.jpg", vec![1, 2, 3]), ("Sky002_1K_HDR.exr", exr())]), None),
        "RoadLines019A_1K-JPG.zip" => (decal_zip(), None),
        f => {
            let id = f.split('_').next().unwrap_or_default();
            let (_, sha1, color) = remote.get(id).ok_or_else(|| format!("unexpected request {url}"))?;
            (material_zip(id, *color), Some(sha1.clone()))
        }
    };
    Ok(Fetched { bytes: if head { Vec::new() } else { bytes }, sha1 })
}

fn install() {
    install_fetcher(Arc::new(|url: &str, _max: u64, head: bool| serve(url, head)));
}

fn requests(part: &str) -> usize {
    REQUESTS.lock().unwrap().iter().filter(|u| u.contains(part)).count()
}

/// Downloads (GETs) of exactly `url`.
fn gets(url: &str) -> usize {
    REQUESTS.lock().unwrap().iter().filter(|u| *u == url).count()
}

fn fill_of(s: &Session, layer: u64) -> texturecraft_engine::core::Fill {
    match &s.doc.project.layer(LayerId(layer)).unwrap().kind {
        LayerKind::Fill(f) => f.clone(),
        other => panic!("not a fill: {other:?}"),
    }
}

fn image_of(f: &texturecraft_engine::core::Fill, c: Channel) -> String {
    match f.sources.get(&c) {
        Some(Source::Image { name, .. }) => name.clone(),
        other => panic!("{c:?}: {other:?}"),
    }
}

#[test]
fn search_lists_assets_with_their_downloads() {
    install();
    let mut s = session();
    let r = execute(&mut s, "assets.ambientcg.search", &json!({"query": "red brick", "sort": "latest", "limit": 500})).unwrap();
    assert_eq!(r["total"], 2);
    assert_eq!(r["assets"][0]["id"], "Brick001");
    assert_eq!(r["assets"][0]["downloads"][0], json!({"attributes": "1K-PNG", "size": 100}));
    assert_eq!(r["assets"][0]["inProject"], Value::Null);
    assert_eq!(r["assets"][1]["type"], "hdri");
    assert!(requests("type=material&sort=latest&limit=100&offset=0") > 0 && requests("&q=red%20brick") > 0);
    for bad in [json!({"type": "model"}), json!({"sort": "best"})] {
        assert!(execute(&mut s, "assets.ambientcg.search", &bad).is_err(), "{bad}");
    }
}

#[test]
fn a_material_is_imported_once_then_reused() {
    install();
    set_remote("Reuse1", 100, "aaaa", [180, 60, 40]);
    let mut s = session();
    let layers = s.doc.project.texture_sets[0].stack.len();
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Reuse1", "resolution": 1, "format": "png"})).unwrap();
    assert_eq!((r["reused"].as_bool(), r["updated"].as_bool()), (Some(false), Some(false)), "{r}");
    assert_eq!(r["source"]["download"], "1K-PNG");
    assert_eq!(r["source"]["license"], "CC0 1.0");
    let first = r["layer"].as_u64().unwrap();
    assert_eq!(s.doc.project.layer(LayerId(first)).unwrap().name, "Bricks Reuse1");
    let f = fill_of(&s, first);
    assert_eq!(image_of(&f, Channel::BaseColor), "Reuse1_Color.png", "the resolution leaves the names");
    assert!(image_of(&f, Channel::Normal).contains("NormalGL"), "the OpenGL normal is used");
    let rec = s.ambientcg_asset("Reuse1").unwrap().clone();
    assert_eq!((rec.download.as_str(), rec.size, rec.sha1.as_deref()), ("1K-PNG", 100, Some("aaaa")));
    assert_eq!(rec.maps.len(), 4, "{rec:?}");
    let resources = s.doc.resources.len();
    let zip = "https://ambientcg.com/get?file=Reuse1_1K-PNG.zip";
    assert_eq!(gets(zip), 1);

    // Again: unchanged on ambientCG (size, then SHA-1 by HEAD), so nothing is downloaded and the
    // new layer shows the same images.
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Reuse1"})).unwrap();
    assert_eq!(r["reused"], true, "{r}");
    assert_eq!(gets(zip), 1, "no download: {:?}", REQUESTS.lock().unwrap());
    assert_eq!(gets(&format!("HEAD {zip}")), 1, "the SHA-1 is checked");
    let second = r["layer"].as_u64().unwrap();
    assert_ne!(first, second);
    assert_eq!(fill_of(&s, second).sources, f.sources);
    assert_eq!(s.doc.resources.len(), resources, "no second copy of the maps");
    assert_eq!(s.doc.project.texture_sets[0].stack.len(), layers + 2);

    // Without the network a held asset still applies.
    let r = s.apply_ambientcg("Reuse1", &Value::Null).unwrap();
    assert_eq!(r["reused"], true);
    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    assert_eq!(s.doc.project.texture_sets[0].stack.len(), layers + 1, "each apply is one undo step");
    let inspect = execute(&mut s, "project.inspect", &Value::Null).unwrap();
    assert_eq!(inspect["importedAssets"][0]["id"], "Reuse1");
    let list = execute(&mut s, "assets.ambientcg.list", &Value::Null).unwrap();
    assert_eq!(list[0]["download"], "1K-PNG");
}

#[test]
fn a_new_version_replaces_the_maps_in_every_layer() {
    install();
    set_remote("Change1", 100, "aaaa", [180, 60, 40]);
    let mut s = session();
    let a = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Change1", "resolution": 1})).unwrap()["layer"].as_u64().unwrap();
    let b = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Change1"})).unwrap()["layer"].as_u64().unwrap();
    let old = image_of(&fill_of(&s, a), Channel::BaseColor);
    let resources = s.doc.resources.len();

    // ambientCG re-releases it (another size): it downloads again and both layers move over.
    set_remote("Change1", 120, "bbbb", [20, 200, 20]);
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Change1"})).unwrap();
    assert_eq!((r["reused"].as_bool(), r["updated"].as_bool()), (Some(false), Some(true)), "{r}");
    assert_eq!(gets("https://ambientcg.com/get?file=Change1_1K-PNG.zip"), 2);
    let new = image_of(&fill_of(&s, a), Channel::BaseColor);
    assert_ne!(old, new);
    assert_eq!(image_of(&fill_of(&s, b), Channel::BaseColor), new);
    assert!(!s.doc.resources.contains_key(&old), "the old maps are gone");
    assert_eq!(s.doc.resources.len(), resources, "replaced, not added");
    assert_eq!(s.images.get(&new).map(|i| i.rgba[0][1] > 0.5), Some(true), "the new (green) map is decoded");
    assert_eq!(s.ambientcg_asset("Change1").unwrap().size, 120);
    // One undo brings back the old version everywhere (and its decoded image).
    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    assert_eq!(image_of(&fill_of(&s, a), Channel::BaseColor), old);
    assert_eq!(image_of(&fill_of(&s, b), Channel::BaseColor), old);
    assert!(s.images.contains_key(&old));
    assert_eq!(s.ambientcg_asset("Change1").unwrap().size, 100);
    execute(&mut s, "edit.redo", &Value::Null).unwrap();

    // Same size, another SHA-1: also a new version.
    set_remote("Change1", 120, "cccc", [20, 20, 200]);
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Change1"})).unwrap();
    assert_eq!(r["updated"], true, "{r}");
    assert_eq!(s.ambientcg_asset("Change1").unwrap().sha1.as_deref(), Some("cccc"));
}

#[test]
fn removing_unused_resources_forgets_assets_no_layer_uses() {
    install();
    set_remote("Prune1", 100, "aaaa", [180, 60, 40]);
    let mut s = session();
    let layer = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Prune1", "resolution": 1})).unwrap()["layer"].as_u64().unwrap();
    execute(&mut s, "file.removeUnused", &Value::Null).unwrap();
    assert!(s.ambientcg_asset("Prune1").is_some(), "still in use");
    execute(&mut s, "layer.remove", &json!({"layer": layer})).unwrap();
    execute(&mut s, "file.removeUnused", &Value::Null).unwrap();
    assert!(s.ambientcg_asset("Prune1").is_none());
    assert!(s.doc.project.assets.is_empty());
    assert!(s.apply_ambientcg("Prune1", &Value::Null).is_err());
}

#[test]
fn an_hdri_becomes_the_environment() {
    install();
    let mut s = session();
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Sky002", "resolution": 4})).unwrap();
    let res = r["resource"].as_str().unwrap().to_string();
    assert_eq!(res, "Sky002_HDR.exr", "{r}");
    assert_eq!(s.doc.project.display.environment, format!("resource:{res}"));
    assert!(s.images.contains_key(&res));
    execute(&mut s, "display.set", &json!({"environment": "Night"})).unwrap();
    let r = s.apply_ambientcg("Sky002", &Value::Null).unwrap();
    assert_eq!(r["environment"], json!(format!("resource:{res}")));
}

#[test]
fn a_decal_becomes_a_decal_layer_cut_out_by_its_opacity_map() {
    install();
    let mut s = session();
    let place = json!({"position": [0.0, 0.0, 0.0], "normal": [0.0, 1.0, 0.0], "up": [0.0, 0.0, -1.0], "size": 0.5, "rotation": 30});
    let mut p = json!({"id": "RoadLines019A", "resolution": 1});
    p.as_object_mut().unwrap().extend(place.as_object().unwrap().clone());
    let r = execute(&mut s, "assets.ambientcg.import", &p).unwrap();
    assert_eq!(r["source"]["id"], "RoadLines019A");
    let id = r["layer"].as_u64().unwrap();
    assert_eq!(s.doc.project.layer(LayerId(id)).unwrap().name, "Road Lines 019 A");
    let fill = fill_of(&s, id);
    let texturecraft_engine::core::Projection::Decal(d) = &fill.projection else { panic!("not a decal: {:?}", fill.projection) };
    assert_eq!(d.rotation, 30.0);
    assert!((d.size[1] - 0.5).abs() < 1e-6, "{d:?}");
    let cutout = fill.cutout.clone().expect("the opacity map is the cutout");
    assert_eq!(cutout, "RoadLines019A_Opacity.png");
    assert!(s.images.contains_key(&cutout));
    assert!(!fill.sources.contains_key(&Channel::Opacity), "the opacity map is not painted into the surface's opacity");
    for c in [Channel::BaseColor, Channel::Normal, Channel::Roughness] {
        assert!(matches!(fill.sources.get(&c), Some(Source::Image { .. })), "{c:?}: {fill:?}");
    }
    assert_eq!(r["cutout"], json!(cutout));
    let inspect = execute(&mut s, "project.inspect", &Value::Null).unwrap().to_string();
    assert!(inspect.contains(&format!("\"cutout\":\"{cutout}\"")), "{inspect}");
    execute(&mut s, "file.removeUnused", &Value::Null).unwrap();
    assert!(s.doc.resources.contains_key(&cutout));
    // Placed again from the project, somewhere else.
    let r = s.apply_ambientcg("RoadLines019A", &json!({"position": [0.3, 0.0, 0.0], "normal": [0.0, 1.0, 0.0]})).unwrap();
    let again = fill_of(&s, r["layer"].as_u64().unwrap());
    assert_eq!(again.cutout.as_deref(), Some(cutout.as_str()));
    assert_eq!(gets("https://ambientcg.com/get?file=RoadLines019A_1K-JPG.zip"), 1);
}

#[test]
fn bad_ids_types_formats_and_zips_are_errors() {
    install();
    let mut s = session();
    for (p, want) in [
        (json!({}), "required"),
        (json!({"id": "../../etc/passwd"}), "not an ambientCG asset id"),
        (json!({"id": "Nope404"}), "no asset"),
        (json!({"id": "Model1"}), "only materials, decals and HDRIs"),
        (json!({"id": "Brick001", "format": "tif"}), "unknown format"),
    ] {
        let e = execute(&mut s, "assets.ambientcg.import", &p).unwrap_err().to_string();
        assert!(e.contains(want), "{p}: {e}");
    }
    set_remote("Big1", 100, "aaaa", [1, 2, 3]);
    let e = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Big1", "resolution": 2})).unwrap_err().to_string();
    assert!(e.contains("lower resolution"), "{e}");
    assert_eq!(requests("Big1_2K-PNG.zip"), 0, "refused before downloading");
    assert!(unpack(Kind::Material, "X", "1K-JPG", b"not a zip").is_err());
    assert!(unpack(Kind::Hdri, "X", "1K", &zip_of(&[("a.jpg", vec![1])])).is_err());
    assert!(unpack(Kind::Decal, "X", "1K-JPG", &zip_of(&[("a.txt", vec![1])])).is_err());
    assert!(unpack(Kind::Material, "X", "1K-JPG", &zip_of(&[("X_1K-JPG_Color.jpg", b"not an image".to_vec())])).is_err());
}

#[test]
fn decal_cutouts_are_set_and_cleared() {
    let mut s = session();
    let img = s.import_resource_bytes("mark.png", png([255, 255, 255])).unwrap();
    let cut = s.import_resource_bytes("mark_opacity.png", png([128, 128, 128])).unwrap();
    let at = json!({"image": img, "position": [0.0, 0.0, 0.0], "normal": [0.0, 1.0, 0.0], "cutout": cut});
    let id = execute(&mut s, "layer.add.decal", &at).unwrap()["layer"].as_u64().unwrap();
    let cutout_of = |s: &Session| fill_of(s, id).cutout;
    assert_eq!(cutout_of(&s), Some(cut.clone()));
    execute(&mut s, "layer.decal.set", &json!({"layer": id, "cutout": null})).unwrap();
    assert_eq!(cutout_of(&s), None, "null goes back to the image alpha");
    execute(&mut s, "layer.decal.set", &json!({"layer": id, "cutout": cut})).unwrap();
    assert_eq!(cutout_of(&s), Some(cut.clone()));
    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    assert_eq!(cutout_of(&s), None, "undoable");
    for bad in [json!({"layer": id, "cutout": "missing.png"}), json!({"layer": id, "cutout": 3})] {
        assert!(execute(&mut s, "layer.decal.set", &bad).is_err(), "{bad}");
    }
    let mut missing = at.clone();
    missing["cutout"] = json!("missing.png");
    assert!(execute(&mut s, "layer.add.decal", &missing).is_err());
    // The placement of a decal can be pinned now and used later.
    let pinned = s.decal_placement(&json!({"position": [0.1, 0.0, 0.0], "normal": [0.0, 1.0, 0.0]})).unwrap();
    let x = pinned["position"][0].as_f64().unwrap();
    assert!((x - 0.1).abs() < 1e-6, "{pinned}");
    assert!(pinned["up"].as_array().is_some_and(|u| u.len() == 3));
}
