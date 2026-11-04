//! ambientCG: search and import through an installed fetcher that serves canned replies and
//! zips made here (no network).

use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use texturecraft_engine::ambientcg_cmds::install_fetcher;
use texturecraft_engine::core::{Channel, LayerKind, Source};
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

fn material_zip() -> Vec<u8> {
    // No .mtlx: the maps are matched by ambientCG's file name suffixes.
    zip_of(&[
        ("Brick001_1K-PNG_Color.png", png([180, 60, 40])),
        ("Brick001_1K-PNG_NormalGL.png", png([128, 128, 255])),
        ("Brick001_1K-PNG_NormalDX.png", png([128, 127, 255])),
        ("Brick001_1K-PNG_Roughness.png", png([200, 200, 200])),
        ("Brick001_1K-PNG_Displacement.png", png([90, 90, 90])),
        ("Brick001.png", png([0, 0, 0])),
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

fn reply(assets: Value) -> Vec<u8> {
    json!({"totalResults": 2, "assets": assets}).to_string().into_bytes()
}

fn brick() -> Value {
    json!({"id": "Brick001", "title": "Bricks 001", "type": "material", "maps": ["color", "normal", "roughness", "displacement"],
           "thumbnails": {"128-PNG": "https://media.example/Brick001.png"},
           "downloads": [
               {"attributes": "1K-PNG", "extension": "zip", "url": "https://ambientcg.com/get?file=Brick001_1K-PNG.zip", "size": 100},
               {"attributes": "2K-PNG", "extension": "zip", "url": "https://ambientcg.com/get?file=Brick001_2K-PNG.zip", "size": 4_000_000_000u64}]})
}

fn sky() -> Value {
    json!({"id": "Sky002", "title": "Sky 002", "type": "hdri",
           "downloads": [{"attributes": "1K", "extension": "zip", "url": "https://ambientcg.com/get?file=Sky002_1K.zip", "size": 100}]})
}

/// Every URL the fake fetcher was asked for, in order.
static REQUESTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn install() {
    install_fetcher(Arc::new(|url: &str, _max: u64| {
        REQUESTS.lock().unwrap().push(url.to_string());
        if url.contains("/api/v3/assets?id=Brick001&") {
            Ok(reply(json!([brick()])))
        } else if url.contains("/api/v3/assets?id=Sky002&") {
            Ok(reply(json!([sky()])))
        } else if url.contains("/api/v3/assets?id=Model1&") {
            Ok(reply(json!([{"id": "Model1", "type": "3d-model", "downloads": []}])))
        } else if url.contains("/api/v3/assets?id=RoadLines019A&") {
            Ok(reply(json!([{"id": "RoadLines019A", "title": "Road Lines 019 A", "type": "decal",
                "downloads": [{"attributes": "1K-JPG", "extension": "zip", "url": "https://ambientcg.com/get?file=RoadLines019A_1K-JPG.zip", "size": 100}]}])))
        } else if url == "https://ambientcg.com/get?file=RoadLines019A_1K-JPG.zip" {
            Ok(decal_zip())
        } else if url.contains("/api/v3/assets?") && url.contains("id=") {
            Ok(reply(json!([])))
        } else if url.contains("/api/v3/assets?") {
            Ok(reply(json!([brick(), sky()])))
        } else if url == "https://ambientcg.com/get?file=Brick001_1K-PNG.zip" {
            Ok(material_zip())
        } else if url == "https://ambientcg.com/get?file=Sky002_1K.zip" {
            Ok(zip_of(&[("Sky002_1K_TONEMAPPED.jpg", vec![1, 2, 3]), ("Sky002_1K_HDR.exr", exr())]))
        } else {
            Err(format!("unexpected request {url}"))
        }
    }));
}

fn requested(part: &str) -> bool {
    REQUESTS.lock().unwrap().iter().any(|u| u.contains(part))
}

#[test]
fn search_lists_assets_with_their_downloads() {
    install();
    let mut s = session();
    let r = execute(&mut s, "assets.ambientcg.search", &json!({"query": "red brick", "sort": "latest", "limit": 500})).unwrap();
    assert_eq!(r["total"], 2);
    assert_eq!(r["assets"][0]["id"], "Brick001");
    assert_eq!(r["assets"][0]["downloads"][0], json!({"attributes": "1K-PNG", "size": 100}));
    assert_eq!(r["assets"][1]["type"], "hdri");
    assert!(requested("type=material&sort=latest&limit=100&offset=0") && requested("&q=red%20brick"));
    for bad in [json!({"type": "model"}), json!({"sort": "best"})] {
        assert!(execute(&mut s, "assets.ambientcg.search", &bad).is_err(), "{bad}");
    }
}

#[test]
fn a_material_becomes_one_fill_layer_in_one_undo_step() {
    install();
    let mut s = session();
    let layers = s.doc.project.texture_sets[0].stack.len();
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Brick001", "resolution": 1, "format": "png"})).unwrap();
    assert_eq!(r["source"]["id"], "Brick001");
    assert_eq!(r["source"]["download"], "1K-PNG");
    assert_eq!(r["source"]["license"], "CC0 1.0");
    let layer = r["layer"].as_u64().unwrap();
    let l = s.doc.project.layer(texturecraft_engine::core::LayerId(layer)).unwrap();
    assert_eq!(l.name, "Bricks 001");
    let LayerKind::Fill(fill) = &l.kind else { panic!("not a fill: {:?}", l.kind) };
    for c in [Channel::BaseColor, Channel::Normal, Channel::Roughness] {
        assert!(matches!(fill.sources.get(&c), Some(Source::Image { .. })), "{c:?}: {fill:?}");
    }
    let normal = match fill.sources.get(&Channel::Normal) {
        Some(Source::Image { name, .. }) => name.clone(),
        other => panic!("{other:?}"),
    };
    assert!(normal.contains("NormalGL"), "the OpenGL normal is used: {normal}");
    for name in r["resources"].as_array().unwrap() {
        assert!(s.images.contains_key(name.as_str().unwrap()), "{name} is decoded");
    }
    assert_eq!(s.doc.project.texture_sets[0].stack.len(), layers + 1);
    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    assert_eq!(s.doc.project.texture_sets[0].stack.len(), layers, "one undo removes it");
    // The 2K zip is too large to download: refused before any request for it.
    let e = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Brick001", "resolution": 2, "format": "png"})).unwrap_err().to_string();
    assert!(e.contains("lower resolution"), "{e}");
    assert!(!requested("Brick001_2K-PNG.zip"));
}

#[test]
fn an_hdri_becomes_the_environment() {
    install();
    let mut s = session();
    let r = execute(&mut s, "assets.ambientcg.import", &json!({"id": "Sky002", "resolution": 4})).unwrap();
    let res = r["resource"].as_str().unwrap();
    assert!(res.ends_with(".exr"), "{r}");
    assert_eq!(s.doc.project.display.environment, format!("resource:{res}"));
    assert!(s.images.contains_key(res));
}

#[test]
fn bad_ids_types_and_formats_are_errors() {
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
    // A zip that isn't one: an error, and nothing changes.
    let layers = s.doc.project.texture_sets[0].stack.len();
    assert!(s.import_ambientcg_zip(Kind::Material, "X", "X", "1K-JPG", b"not a zip", &Value::Null).is_err());
    assert!(s.import_ambientcg_zip(Kind::Hdri, "X", "X", "1K", &zip_of(&[("a.jpg", vec![1])]), &Value::Null).is_err());
    assert!(s.import_ambientcg_zip(Kind::Decal, "X", "X", "1K-JPG", &zip_of(&[("a.txt", vec![1])]), &Value::Null).is_err());
    assert_eq!(s.doc.project.texture_sets[0].stack.len(), layers);
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
    let l = s.doc.project.layer(texturecraft_engine::core::LayerId(r["layer"].as_u64().unwrap())).unwrap();
    assert_eq!(l.name, "Road Lines 019 A");
    let LayerKind::Fill(fill) = &l.kind else { panic!("not a fill: {:?}", l.kind) };
    let texturecraft_engine::core::Projection::Decal(d) = &fill.projection else { panic!("not a decal: {:?}", fill.projection) };
    assert_eq!(d.rotation, 30.0);
    assert!((d.size[1] - 0.5).abs() < 1e-6, "{d:?}");
    let cutout = fill.cutout.clone().expect("the opacity map is the cutout");
    assert!(cutout.contains("Opacity") && s.images.contains_key(&cutout), "{cutout}");
    assert!(!fill.sources.contains_key(&Channel::Opacity), "the opacity map is not painted into the surface's opacity");
    for c in [Channel::BaseColor, Channel::Normal, Channel::Roughness] {
        assert!(matches!(fill.sources.get(&c), Some(Source::Image { .. })), "{c:?}: {fill:?}");
    }
    assert_eq!(r["cutout"], json!(cutout));
    // The inspector shows it, and removing unused resources keeps it.
    let inspect = execute(&mut s, "project.inspect", &Value::Null).unwrap().to_string();
    assert!(inspect.contains(&format!("\"cutout\":\"{cutout}\"")), "{inspect}");
    execute(&mut s, "file.removeUnused", &Value::Null).unwrap();
    assert!(s.doc.resources.contains_key(&cutout));
}

#[test]
fn decal_cutouts_are_set_and_cleared() {
    let mut s = session();
    let img = s.import_resource_bytes("mark.png", png([255, 255, 255])).unwrap();
    let cut = s.import_resource_bytes("mark_opacity.png", png([128, 128, 128])).unwrap();
    let at = json!({"image": img, "position": [0.0, 0.0, 0.0], "normal": [0.0, 1.0, 0.0], "cutout": cut});
    let id = execute(&mut s, "layer.add.decal", &at).unwrap()["layer"].as_u64().unwrap();
    let cutout_of = |s: &Session| match &s.doc.project.layer(texturecraft_engine::core::LayerId(id)).unwrap().kind {
        LayerKind::Fill(f) => f.cutout.clone(),
        other => panic!("{other:?}"),
    };
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
