//! Geometry decals: an image resource projected onto the surface from a frame placed on it
//! (a decal fill with a planar projection), as a fill layer with a decal projection.

use serde_json::{Value, json};
use texturecraft_core::{Channel, Decal, Fill, Layer, LayerId, LayerKind, Projection, Source};
use texturecraft_geom::glam::{Vec2, Vec3};

use crate::commands::{CommandSpec, always, bad, f32p, has_layer, layer_param, strp};
use crate::tool_cmds::viewport_of;
use crate::{Result, Session, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "layer.add.decal",
            "Add decal",
            [],
            None,
            "{image: resource name, x?, y?: viewport pixels (default: the view center), viewport?: [w,h] | position?: [x,y,z], normal?: [x,y,z], up?: [x,y,z] (the image's up; default the view's up, or the view direction when that is along the normal), size?: height | [w,h] (world units; default a quarter of the mesh), rotation?: degrees, depth?, cutout?: image resource whose brightness cuts the decal out (default: the image's alpha), name?} — a fill layer projecting the image from a frame placed on the surface; seen from the front, the image reads the right way round",
            always,
            add
        ),
        cmd!(
            "layer.decal.set",
            "Edit decal",
            [],
            None,
            "{layer?, x?, y?, viewport? | position?, normal? (re-place it), up?: [x,y,z], size?: height | [w,h], rotation?: degrees, offset?: [right, up] (world units from where it was placed), depth?, image?, cutout?: image resource | null (back to the image's alpha), merge?: true continues the previous Edit decal undo step (drags)}",
            has_layer,
            set
        ),
    ]
}

/// A `[x, y, z]` param of finite numbers.
fn vec3(p: &Value, key: &str) -> Option<Vec3> {
    let a = p.get(key)?.as_array()?;
    let v: Vec<f32> = a.iter().filter_map(Value::as_f64).map(|x| x as f32).filter(|x| x.is_finite()).collect();
    match v.as_slice() {
        [x, y, z] => Some(Vec3::new(*x, *y, *z)),
        _ => None,
    }
}

/// Where a decal goes: a surface point and its normal, from `x`/`y` (viewport pixels, the view
/// center by default) or `position` + `normal`. None when `optional` and neither is given.
fn placement(s: &Session, cmd: &str, p: &Value, optional: bool) -> Result<Option<(Vec3, Vec3)>> {
    if let Some(pos) = vec3(p, "position") {
        let n = vec3(p, "normal").and_then(Vec3::try_normalize).ok_or_else(|| bad(cmd, "`position` needs a non-zero `normal`"))?;
        return Ok(Some((pos, n)));
    }
    let vp = viewport_of(s, p);
    let at = match (f32p(p, "x"), f32p(p, "y")) {
        (Some(x), Some(y)) => Vec2::new(x, y),
        _ if optional => return Ok(None),
        _ => vp * 0.5,
    };
    let hit = s.pick(at, vp).ok_or_else(|| bad(cmd, "nothing under that point: place the decal on the mesh"))?;
    if s.doc.project.texture_set(s.ts()).ok().map(|t| t.material) != Some(hit.material) {
        return Err(bad(cmd, "that surface belongs to another texture set"));
    }
    Ok(Some((hit.pos, hit.nrm)))
}

/// The mesh's diagonal (decal sizes are relative to it).
fn mesh_size(s: &Session) -> f32 {
    let (lo, hi) = s.mesh.bounds();
    (hi - lo).length().max(1e-3)
}

/// Width and height from a `size` param (a height keeping `aspect`, or `[w, h]`).
fn size_param(p: &Value, aspect: f32, diag: f32) -> Option<[f32; 2]> {
    let v = p.get("size")?;
    let size = match v.as_array() {
        Some(a) => [a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32],
        None => {
            let h = v.as_f64()? as f32;
            [h * aspect, h]
        }
    };
    size.iter().all(|x| x.is_finite()).then(|| size.map(|x| x.clamp(diag * 1e-4, diag * 10.0)))
}

/// The decal's up direction: `up` when given, else the camera's (decals are placed upright on
/// screen), or where the camera looks when that is along `normal` (a top face seen from the
/// front: its far edge is up, as on a table).
fn decal_up(s: &Session, cmd: &str, p: &Value, normal: Vec3) -> Result<Vec3> {
    if p.get("up").is_some() {
        return vec3(p, "up").filter(|u| u.cross(normal).length_squared() > 1e-8).ok_or_else(|| bad(cmd, "`up` must be [x, y, z], not along the normal"));
    }
    let to_world = s.camera.view().inverse();
    let up = to_world.transform_vector3(Vec3::Y);
    Ok(if up.normalize_or_zero().cross(normal).length_squared() > 1e-4 { up } else { to_world.transform_vector3(Vec3::NEG_Z) })
}

/// A new decal frame from the placement params of `layer.add.decal` (`x`/`y`/`viewport` or
/// `position`/`normal`, `up`, `size`, `rotation`, `depth`) for an image `aspect` wide per unit
/// of height.
pub(crate) fn new_decal(s: &Session, cmd: &str, p: &Value, aspect: f32) -> Result<Decal> {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect.clamp(1e-3, 1e3) } else { 1.0 };
    let (center, normal) = placement(s, cmd, p, false)?.ok_or_else(|| bad(cmd, "no placement"))?;
    let diag = mesh_size(s);
    let size = size_param(p, aspect, diag).unwrap_or([diag * 0.25 * aspect, diag * 0.25]);
    Ok(Decal {
        center: center.to_array(),
        normal: normal.to_array(),
        up: decal_up(s, cmd, p, normal)?.to_array(),
        size,
        rotation: f32p(p, "rotation").unwrap_or(0.0).rem_euclid(360.0),
        offset: [0.0; 2],
        depth: f32p(p, "depth").map_or(size[0].max(size[1]) * 0.5, |d| d.clamp(diag * 1e-4, diag * 10.0)),
    })
}

impl Session {
    /// Resolve the placement params of `layer.add.decal` (`x`/`y`/`viewport`, the view center by
    /// default, or `position`/`normal`, plus `up`) to `{position, normal, up}` at the current
    /// camera, so a decal can be placed later (after a download) where it was asked for now.
    pub fn decal_placement(&self, p: &Value) -> Result<Value> {
        let cmd = "layer.add.decal";
        let (position, normal) = placement(self, cmd, p, false)?.ok_or_else(|| bad(cmd, "no placement"))?;
        let up = decal_up(self, cmd, p, normal)?;
        Ok(json!({ "position": position.to_array(), "normal": normal.to_array(), "up": up.to_array() }))
    }
}

/// The `cutout` param: an image resource name (checked), or None when absent.
fn cutout_param(s: &Session, cmd: &str, p: &Value) -> Result<Option<String>> {
    match strp(p, "cutout") {
        Some(name) if s.doc.resources.contains_key(name) => Ok(Some(name.to_string())),
        Some(name) => Err(bad(cmd, format!("no image resource `{name}` (import it first)"))),
        None => Ok(None),
    }
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "layer.add.decal";
    let image = strp(p, "image").ok_or_else(|| bad(cmd, "`image` is required"))?.to_string();
    let img = s.images.get(&image).ok_or_else(|| bad(cmd, format!("no image resource `{image}` (import it first)")))?;
    let aspect = img.width as f32 / img.height.max(1) as f32;
    let decal = new_decal(s, cmd, p, aspect)?;
    let cutout = cutout_param(s, cmd, p)?;
    let ts = s.ts();
    let base =
        strp(p, "name").map(str::to_string).unwrap_or_else(|| std::path::Path::new(&image).file_stem().and_then(|n| n.to_str()).unwrap_or("Decal").to_string());
    let name = s.doc.project.next_layer_name(ts, &base);
    let id = s.edit("Add decal", |s| {
        let id = s.doc.project.alloc_id();
        let mut fill = Fill { projection: Projection::Decal(decal), cutout, ..Fill::default() };
        fill.sources.insert(Channel::BaseColor, Source::image(image.clone()));
        crate::layer_cmds::insert_selected(s, Layer::new(id, name, LayerKind::Fill(fill), &[Channel::BaseColor]))
    })?;
    Ok(json!({ "layer": id.0, "decal": decal }))
}

/// The decal of fill layer `id`.
fn decal_of(s: &Session, cmd: &str, id: LayerId) -> Result<Decal> {
    match &s.doc.project.layer(id)?.kind {
        LayerKind::Fill(Fill { projection: Projection::Decal(d), .. }) => Ok(*d),
        _ => Err(bad(cmd, "the layer is not a decal (layer.add.decal)")),
    }
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "layer.decal.set";
    let id = s.doc.project.content_layer(layer_param(s, cmd, p)?).map(|l| l.id).ok_or_else(|| bad(cmd, "no layer"))?;
    let mut d = decal_of(s, cmd, id)?;
    let diag = mesh_size(s);
    if let Some((center, normal)) = placement(s, cmd, p, true)? {
        (d.center, d.normal, d.offset) = (center.to_array(), normal.to_array(), [0.0; 2]);
    }
    if p.get("up").is_some() {
        d.up = decal_up(s, cmd, p, Vec3::from(d.normal))?.to_array();
    }
    if let Some(size) = size_param(p, d.size[0] / d.size[1].max(1e-6), diag) {
        d.size = size;
    }
    if let Some(r) = f32p(p, "rotation") {
        d.rotation = r.rem_euclid(360.0);
    }
    if let Some(o) = p.get("offset").and_then(Value::as_array) {
        let v: Vec<f32> = o.iter().filter_map(Value::as_f64).map(|x| x as f32).filter(|x| x.is_finite()).collect();
        let [x, y] = v.as_slice() else { return Err(bad(cmd, "`offset` must be [right, up]")) };
        d.offset = [x.clamp(-diag, diag), y.clamp(-diag, diag)];
    }
    if let Some(v) = f32p(p, "depth") {
        d.depth = v.clamp(diag * 1e-4, diag * 10.0);
    }
    let image = strp(p, "image").map(str::to_string);
    if let Some(name) = &image
        && !s.doc.resources.contains_key(name)
    {
        return Err(bad(cmd, format!("no image resource `{name}`")));
    }
    // `cutout: null` removes it; a name sets it.
    let cutout = match p.get("cutout") {
        Some(Value::Null) => Some(None),
        Some(_) => Some(Some(cutout_param(s, cmd, p)?.ok_or_else(|| bad(cmd, "`cutout` must be an image resource name or null"))?)),
        None => None,
    };
    s.edit("Edit decal", |s| {
        if let LayerKind::Fill(f) = &mut s.doc.project.layer_mut(id)?.kind {
            f.projection = Projection::Decal(d);
            if let Some(name) = image {
                f.sources.insert(Channel::BaseColor, Source::image(name));
            }
            if let Some(c) = cutout {
                f.cutout = c;
            }
        }
        Ok(())
    })?;
    Ok(json!({ "layer": id.0, "decal": d }))
}
