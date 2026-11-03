//! Mesh-map baking commands (the "Bake Mesh Maps" window): bake settings, the
//! high-definition mesh list, the cage, and running a bake. Bakes cast their rays on the GPU
//! tracer the app installs ([`install_gpu_tracer`]) and on the CPU otherwise.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use texturecraft_bake::rays::Tracer;
use texturecraft_bake::{BakeOutput, BakeRequest, HighPoly, MAX_RAYS, Progress, SkewMap, bake, bake_on};
use texturecraft_core::{BakeMatch, BakeSettings, BakedMap, DirectionMode, PositionMode, RayDistribution, RaySettings, SelfOcclusion};
use texturecraft_geom::{BaseColor, Bvh, Mesh};

use crate::commands::{CommandSpec, always, bad, boolp, f32p, strp, u64p};
use crate::{EngineError, Result, Session, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "bake.run",
            "Bake mesh maps",
            ["Mode"],
            None,
            "{ts?: index|[indices]|\"all\"|\"selected\", cpu?: true (skip the GPU), ...any bake.settings parameter} → {baked, tracer: [where each set's rays ran]}",
            always,
            bake_run
        ),
        cmd!(
            "bake.settings",
            "Bake settings",
            [],
            None,
            "{ts?: index|[indices]|\"all\"|\"selected\" (default: the active set), resolution?, antialiasing?: 1|2|4|8, dilation?, ao?: {rays?, maxDistance?, spread?: degrees (180 = hemisphere), distribution?: cosine|uniform, ignoreBackface?, selfOcclusion?: always|sameMeshName}, thickness?: {rays?, maxDistance?, spread?, distribution?}, bentNormal?: {rays?, maxDistance?, spread?, distribution?}, rays?, maxDistance? (all three ray maps), curvatureStrength?, curvatureFromNormal?, curvatureRadius?: texels (0 = auto), positionMode?: boundingBox|boundingSphere, direction?: [x,y,z], directionMode?: signed|positive, maps?: [ids], enabled?, useLowAsHigh?, maxFrontal?, maxRear?, averageNormals?, matching?: always|byMeshName, lowSuffix?, highSuffix?}",
            always,
            settings
        ),
        cmd!(
            "bake.highPoly.add",
            "Add high definition mesh",
            [],
            None,
            "{path} — a mesh file (glb, gltf, fbx, dae, usd, usda, usdc, usdz, abc, blend, obj, ply, stl; base color textures from glTF, embedded FBX media, usdz packages, images packed in a .blend and image files next to a .gltf, USD layer, FBX or .blend feed the color map)",
            always,
            high_add
        ),
        cmd!("bake.highPoly.remove", "Remove high definition mesh", [], None, "{index? | name? | all?: true}", always, high_remove),
        cmd!("bake.highPoly.list", "List high definition meshes", [], None, "{}", always, |s, _| Ok(meshes_json(s))),
        cmd!("bake.cage.set", "Load cage mesh", [], None, "{path} — a mesh with the low poly's triangles", always, cage_set),
        cmd!("bake.cage.clear", "Remove cage mesh", [], None, "{}", always, |s, _| {
            s.bake_meshes.cage = None;
            s.modified = true;
            Ok(meshes_json(s))
        }),
        cmd!(
            "bake.skew.set",
            "Set skew correction",
            [],
            None,
            "{value?: 0..1 (a constant map, default 1) | path: a greyscale image over the UV atlas (white turns the rays onto the base normal, black keeps the cage's averaged direction), edgeDistance?: fraction of the mesh diagonal over which hard edges hold the correction back (default 0), edgeContrast?: falloff exponent (default 1), base?: mesh|triangle (default mesh)} → {skew}. Applies to the transfer rays of a bake from the built-in distance cage; a cage mesh ignores it",
            always,
            skew_set
        ),
        cmd!(
            "bake.skew.paint",
            "Paint skew correction",
            [],
            None,
            "{points: [[u, v], ...] (atlas uv, a stroke), radius?: uv units (default 0.03), value?: 0..1 (1 turns rays onto the base normal, 0 erases; default 1), hardness?: 0..1 (default 0.5), strength?: 0..1 per dab (default 1), size?: px of a new or constant map (default 1024, 16..4096)} → {skew}. Paints into the skew map; a missing or constant map is first widened to `size`",
            always,
            skew_paint
        ),
        cmd!("bake.skew.clear", "Remove skew correction", [], None, "{}", always, |s, _| {
            s.bake_meshes.skew = None;
            s.bake_meshes.skew_rev += 1;
            s.modified = true;
            Ok(meshes_json(s))
        }),
    ]
}

/// A mesh file as saved in a project: (file name, bytes).
type MeshFile = (String, Vec<u8>);

/// A mesh file the baker reads (high poly or cage): its name, original bytes (saved in the
/// project), parsed mesh and its materials' base colors (high polys).
#[derive(Clone, Debug)]
pub struct BakeMesh {
    pub name: String,
    pub bytes: Arc<Vec<u8>>,
    pub mesh: Arc<Mesh>,
    pub base_color: Vec<Option<Arc<BaseColor>>>,
}

/// Every high poly appended into one mesh, its BVH and per-material base colors.
#[derive(Clone, Debug)]
pub struct MergedHigh {
    pub mesh: Arc<Mesh>,
    pub bvh: Arc<Bvh>,
    pub base_color: Arc<Vec<Option<Arc<BaseColor>>>>,
}

static GPU_TRACER: OnceLock<Arc<dyn Tracer>> = OnceLock::new();

/// Cast bake rays with `tracer` (the apps install the render crate's GPU baker at start-up);
/// the first call wins.
pub fn install_gpu_tracer(tracer: Arc<dyn Tracer>) {
    let _ = GPU_TRACER.set(tracer);
}

/// The high-definition meshes and the cage. Like the low-poly mesh they are not part of the
/// undo history.
#[derive(Clone, Debug, Default)]
pub struct BakeMeshes {
    pub high: Vec<BakeMesh>,
    pub cage: Option<BakeMesh>,
    /// Skew correction of the transfer rays (saved in the project as `skew.bin`).
    pub skew: Option<Arc<SkewMap>>,
    /// Counts changes to `skew`, so a view knows when to redraw it.
    pub skew_rev: u64,
    /// Every high poly appended into one mesh (rebuilt when the list changes).
    merged: Option<MergedHigh>,
}

impl BakeMeshes {
    // shortcut: the merged copy doubles high-poly memory; build the BVH over the parts in place if
    // multi-million-triangle sets run out of memory.
    fn rebuild(&mut self) {
        self.merged = (!self.high.is_empty()).then(|| {
            let (mut all, mut base_color) = (Mesh::default(), Vec::new());
            for h in &self.high {
                all.append(&h.mesh);
                base_color.extend(h.base_color.iter().cloned().chain(std::iter::repeat(None)).take(h.mesh.materials.len()));
            }
            let bvh = Bvh::new(&all);
            MergedHigh { mesh: Arc::new(all), bvh: Arc::new(bvh), base_color: Arc::new(base_color) }
        });
    }
}

/// A self-contained bake task that can run on another thread.
pub struct BakeTask {
    pub ts: usize,
    pub mesh: Arc<Mesh>,
    pub bvh: Arc<Bvh>,
    pub material: usize,
    pub settings: BakeSettings,
    /// The merged high polys, unless the set bakes from the low poly.
    pub high: Option<MergedHigh>,
    pub cage: Option<Arc<Mesh>>,
    pub skew: Option<Arc<SkewMap>>,
    /// UV tile grid (columns, rows) of the set; (1, 1) without UV tiles.
    pub tiles: (u32, u32),
    /// The GPU ray tracer, if installed and not turned off (`bake.run {cpu: true}`).
    pub tracer: Option<Arc<dyn Tracer>>,
}

impl BakeTask {
    pub fn run(&self, progress: &Progress) -> Result<BakeOutput> {
        let high = self.high.as_ref().map(|h| HighPoly { mesh: &h.mesh, bvh: &h.bvh, base_color: &h.base_color });
        let req = BakeRequest {
            mesh: &self.mesh,
            bvh: &self.bvh,
            material: self.material,
            settings: &self.settings,
            high,
            cage: self.cage.as_deref(),
            skew: self.skew.as_deref(),
            tiles: self.tiles,
        };
        match &self.tracer {
            Some(t) => bake_on(&req, progress, t.as_ref()),
            None => bake(&req, progress),
        }
        .map_err(|e| EngineError::Other(format!("bake failed: {e}")))
    }
}

/// A mesh file name made unique among `taken` (and safe as a project file entry).
fn unique_name(file_name: &str, taken: &[&str]) -> String {
    let leaf = std::path::Path::new(file_name).file_name().and_then(|n| n.to_str()).unwrap_or("mesh");
    let base = texturecraft_formats::sanitize_resource_name(leaf);
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name.as_str()) {
        name = format!("{n}_{base}");
        n += 1;
    }
    name
}

fn parse_mesh(file_name: &str, bytes: Vec<u8>) -> Result<BakeMesh> {
    let h = texturecraft_formats::import_high_poly_bytes(file_name, &bytes).map_err(|e| EngineError::Other(format!("{file_name}: {e}")))?;
    let base_color = h.base_color.into_iter().map(|c| c.map(Arc::new)).collect();
    Ok(BakeMesh { name: file_name.to_string(), bytes: Arc::new(bytes), mesh: Arc::new(h.mesh), base_color })
}

impl Session {
    pub fn bake_task(&self, ts: usize) -> Result<BakeTask> {
        let set = self.doc.project.texture_set(ts)?;
        let high = if set.bake.use_low_as_high { None } else { self.bake_meshes.merged.clone() };
        let cage = high.as_ref().and(self.bake_meshes.cage.as_ref()).map(|c| c.mesh.clone());
        let skew = high.as_ref().and(self.bake_meshes.skew.clone());
        let tiles = set.tile_grid().map_or((1, 1), |g| (g.cols, g.rows));
        let tracer = GPU_TRACER.get().cloned();
        Ok(BakeTask { ts, mesh: self.mesh.clone(), bvh: self.bvh.clone(), material: set.material, settings: set.bake.clone(), high, cage, skew, tiles, tracer })
    }

    /// Store a finished bake (one undo step).
    pub fn apply_bake(&mut self, ts: usize, out: BakeOutput) -> Result<()> {
        self.log.push(format!("Baked texture set {ts} ({})", out.tracer));
        let made: Vec<BakedMap> = out.maps.keys().copied().collect();
        self.edit("Bake mesh maps", |s| {
            let maps: BTreeSet<BakedMap> = out.maps.keys().copied().collect();
            let entry = s.doc.baked.entry(ts).or_default();
            entry.resolution = out.resolution;
            for (m, data) in out.maps {
                entry.maps.insert(m, Arc::new(data));
            }
            s.doc.project.texture_set_mut(ts)?.baked.extend(maps);
            Ok(())
        })?;
        self.note_baked(ts, made.into_iter());
        Ok(())
    }

    /// Bake texture set `ts` now (blocking), on the CPU when `cpu`; returns where the rays ran.
    pub fn bake_now(&mut self, ts: usize, cpu: bool) -> Result<String> {
        self.begin_bake(ts);
        let mut task = self.bake_task(ts)?;
        if cpu {
            task.tracer = None;
        }
        let out = task.run(&Progress::new())?;
        let tracer = out.tracer.clone();
        self.apply_bake(ts, out)?;
        Ok(tracer)
    }

    /// Add a high-definition mesh from file bytes (the web picker, the desktop dialog and
    /// `bake.highPoly.add`); every texture set then bakes from the high polys. Returns its name.
    pub fn add_high_poly_bytes(&mut self, file_name: &str, bytes: Vec<u8>) -> Result<String> {
        let mut m = parse_mesh(file_name, bytes)?;
        let taken: Vec<&str> = self.bake_meshes.high.iter().map(|h| h.name.as_str()).collect();
        m.name = unique_name(file_name, &taken);
        let textures = m.base_color.iter().flatten().filter(|c| !c.texels.is_empty()).count();
        self.log.push(format!("High poly {}: {} triangles, {textures} base color textures", m.name, m.mesh.triangle_count()));
        let name = m.name.clone();
        self.bake_meshes.high.push(m);
        self.bake_meshes.rebuild();
        for set in &mut self.doc.project.texture_sets {
            set.bake.use_low_as_high = false;
        }
        self.modified = true;
        Ok(name)
    }

    /// Remove high-definition mesh `index`.
    pub fn remove_high_poly(&mut self, index: usize) -> Result<BakeMesh> {
        if index >= self.bake_meshes.high.len() {
            return Err(EngineError::Other(format!("no high poly mesh {index}")));
        }
        let removed = self.bake_meshes.high.remove(index);
        self.bake_meshes.rebuild();
        self.modified = true;
        Ok(removed)
    }

    /// Use a cage mesh (same triangles as the low poly) from file bytes.
    pub fn set_cage_bytes(&mut self, file_name: &str, bytes: Vec<u8>) -> Result<String> {
        let mut m = parse_mesh(file_name, bytes)?;
        if m.mesh.triangle_count() != self.mesh.triangle_count() {
            return Err(EngineError::Other(format!(
                "the cage has {} triangles but the low poly has {}; export the cage from the low poly",
                m.mesh.triangle_count(),
                self.mesh.triangle_count()
            )));
        }
        m.name = unique_name(file_name, &[]);
        m.base_color.clear();
        let name = m.name.clone();
        self.bake_meshes.cage = Some(m);
        self.modified = true;
        Ok(name)
    }

    /// Add mesh file `f` as a high-definition mesh, or set it as the cage, logging what it could
    /// not fold in. Replies like `bake.highPoly.add` / `bake.cage.set`: `{name, meshes}`.
    pub fn add_bake_mesh(&mut self, f: texturecraft_formats::MeshFile, cage: bool) -> Result<Value> {
        let name = if cage { self.set_cage_bytes(&f.name, f.bytes)? } else { self.add_high_poly_bytes(&f.name, f.bytes)? };
        self.log.extend(f.warnings);
        Ok(json!({ "name": name, "meshes": meshes_json(self) }))
    }

    /// Restore the bake meshes saved in a project file; unreadable ones are logged and skipped.
    pub(crate) fn load_bake_meshes(&mut self, high: Vec<MeshFile>, cage: Option<MeshFile>) {
        for (name, bytes) in high {
            match parse_mesh(&name, bytes) {
                Ok(m) => self.bake_meshes.high.push(m),
                Err(e) => self.log.push(format!("skipped high poly mesh: {e}")),
            }
        }
        self.bake_meshes.rebuild();
        if let Some((name, bytes)) = cage
            && let Err(e) = self.set_cage_bytes(&name, bytes)
        {
            self.log.push(format!("skipped cage mesh: {e}"));
        }
    }

    /// The skew map of a project file, if it passes the baker's checks.
    pub(crate) fn load_skew(&mut self, skew: Option<texturecraft_formats::SkewData>) {
        let Some(k) = skew else { return };
        let map = SkewMap {
            width: k.width,
            height: k.height,
            values: k.values,
            edge_distance: k.edge_distance,
            edge_contrast: k.edge_contrast,
            per_triangle: k.per_triangle,
        };
        match map.validate() {
            Ok(()) => {
                self.bake_meshes.skew = Some(Arc::new(map));
                self.bake_meshes.skew_rev += 1;
            }
            Err(e) => self.log.push(format!("skipped skew map: {e}")),
        }
    }

    /// The skew map for a project file.
    pub(crate) fn saved_skew(&self) -> Option<texturecraft_formats::SkewData> {
        let k = self.bake_meshes.skew.as_ref()?;
        Some(texturecraft_formats::SkewData {
            width: k.width,
            height: k.height,
            values: k.values.clone(),
            edge_distance: k.edge_distance,
            edge_contrast: k.edge_contrast,
            per_triangle: k.per_triangle,
        })
    }

    /// The bake meshes for a project file: (high polys, cage).
    pub(crate) fn saved_bake_meshes(&self) -> (Vec<MeshFile>, Option<MeshFile>) {
        let entry = |m: &BakeMesh| (m.name.clone(), m.bytes.as_ref().clone());
        (self.bake_meshes.high.iter().map(entry).collect(), self.bake_meshes.cage.as_ref().map(entry))
    }
}

/// `bake.highPoly.list`: the high polys and the cage (also part of `project.inspect`).
pub fn meshes_json(s: &Session) -> Value {
    let one = |m: &BakeMesh| json!({ "name": m.name, "triangles": m.mesh.triangle_count(), "parts": m.mesh.submeshes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>() });
    let skew = s.bake_meshes.skew.as_ref().map(|k| {
        json!({ "width": k.width, "height": k.height, "edgeDistance": k.edge_distance, "edgeContrast": k.edge_contrast, "base": if k.per_triangle { "triangle" } else { "mesh" } })
    });
    json!({ "highPoly": s.bake_meshes.high.iter().map(one).collect::<Vec<_>>(), "cage": s.bake_meshes.cage.as_ref().map(one), "skew": skew })
}

#[cfg(not(target_arch = "wasm32"))]
fn read_mesh_file(cmd: &str, p: &Value) -> Result<texturecraft_formats::MeshFile> {
    let path = strp(p, "path").ok_or_else(|| bad(cmd, "`path` is required"))?;
    crate::file_cmds::read_mesh(cmd, path)
}

#[cfg(target_arch = "wasm32")]
fn read_mesh_file(_: &str, _: &Value) -> Result<texturecraft_formats::MeshFile> {
    Err(EngineError::Other("file paths are not available in the browser; use the bake panel's file picker".into()))
}

fn high_add(s: &mut Session, p: &Value) -> Result<Value> {
    s.add_bake_mesh(read_mesh_file("bake.highPoly.add", p)?, false)
}

fn high_remove(s: &mut Session, p: &Value) -> Result<Value> {
    if boolp(p, "all") == Some(true) {
        s.bake_meshes.high.clear();
        s.bake_meshes.rebuild();
        s.modified = true;
        return Ok(meshes_json(s));
    }
    let index = match (u64p(p, "index"), strp(p, "name")) {
        (Some(i), _) => usize::try_from(i).unwrap_or(usize::MAX),
        (None, Some(n)) => s.bake_meshes.high.iter().position(|h| h.name == n).ok_or_else(|| bad("bake.highPoly.remove", format!("no high poly `{n}`")))?,
        (None, None) => return Err(bad("bake.highPoly.remove", "`index`, `name` or `all` is required")),
    };
    s.remove_high_poly(index)?;
    Ok(meshes_json(s))
}

/// The greyscale skew map of an image file (the mean of its colour channels, as stored).
#[cfg(not(target_arch = "wasm32"))]
fn skew_image(path: &str) -> Result<(u32, u32, Vec<f32>)> {
    const MAX_BYTES: u64 = 256 << 20;
    let cmd = "bake.skew.set";
    let len = std::fs::metadata(path).map_err(|e| bad(cmd, format!("cannot read {path}: {e}")))?.len();
    if len > MAX_BYTES {
        return Err(bad(cmd, format!("{path} is over 256 MiB")));
    }
    let bytes = std::fs::read(path).map_err(|e| bad(cmd, format!("cannot read {path}: {e}")))?;
    // The size is checked from the header, before the pixels are decoded.
    let img = texturecraft_formats::decode_image_within(&bytes, texturecraft_bake::MAX_TEXELS).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
    Ok((img.width, img.height, img.rgba.iter().map(|p| (p[0] + p[1] + p[2]) / 3.0).collect()))
}

#[cfg(target_arch = "wasm32")]
fn skew_image(_: &str) -> Result<(u32, u32, Vec<f32>)> {
    Err(EngineError::Other("file paths are not available in the browser".into()))
}

fn skew_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bake.skew.set";
    let mut map = match strp(p, "path") {
        Some(path) => {
            let (width, height, values) = skew_image(path)?;
            SkewMap { width, height, values: values.into_iter().map(|v| v.clamp(0.0, 1.0)).collect(), ..SkewMap::constant(0.0) }
        }
        None => SkewMap::constant(f32p(p, "value").unwrap_or(1.0).clamp(0.0, 1.0)),
    };
    map.edge_distance = f32p(p, "edgeDistance").unwrap_or(0.0);
    map.edge_contrast = f32p(p, "edgeContrast").unwrap_or(1.0);
    map.per_triangle = match strp(p, "base") {
        None | Some("mesh") => false,
        Some("triangle") => true,
        Some(other) => return Err(bad(cmd, format!("unknown base `{other}`"))),
    };
    map.validate().map_err(|e| bad(cmd, e.to_string()))?;
    s.bake_meshes.skew = Some(Arc::new(map));
    s.bake_meshes.skew_rev += 1;
    s.modified = true;
    Ok(meshes_json(s))
}

fn cage_set(s: &mut Session, p: &Value) -> Result<Value> {
    s.add_bake_mesh(read_mesh_file("bake.cage.set", p)?, true)
}

/// Texture sets named by `ts` (default: the active one).
fn sets_param(s: &Session, cmd: &str, p: &Value) -> Result<Vec<usize>> {
    let n = s.set_count();
    let one = |v: &Value| v.as_u64().map(|t| t as usize).filter(|t| *t < n).ok_or_else(|| bad(cmd, format!("invalid texture set {v}")));
    match p.get("ts") {
        Some(Value::String(a)) if a == "all" => Ok((0..n).collect()),
        Some(Value::String(a)) if a == "selected" => Ok((0..n).filter(|t| s.doc.project.texture_sets.get(*t).is_some_and(|set| set.bake.enabled)).collect()),
        Some(Value::Array(a)) => a.iter().map(one).collect(),
        Some(v) => Ok(vec![one(v)?]),
        None => Ok(vec![s.ts()]),
    }
}

/// `b` as `bake.settings` parameters (what the command returns and the UI sends back).
pub fn settings_params(b: &BakeSettings) -> Value {
    let mut ao = rays_params(&b.ao);
    ao["ignoreBackface"] = b.ao_ignore_backface.into();
    ao["selfOcclusion"] = SELF_OCCLUSION.name(b.ao_self_occlusion).into();
    json!({
        "resolution": b.resolution,
        "antialiasing": b.antialiasing,
        "dilation": b.dilation,
        "ao": ao,
        "thickness": rays_params(&b.thickness),
        "bentNormal": rays_params(&b.bent_normal),
        "curvatureStrength": b.curvature_strength,
        "curvatureFromNormal": b.curvature_from_normal,
        "curvatureRadius": b.curvature_radius,
        "positionMode": POSITION_MODE.name(b.position_mode),
        "direction": b.direction,
        "directionMode": DIRECTION_MODE.name(b.direction_mode),
        "maps": b.maps.iter().map(|m| m.id()).collect::<Vec<_>>(),
        "enabled": b.enabled,
        "useLowAsHigh": b.use_low_as_high,
        "maxFrontal": b.max_frontal,
        "maxRear": b.max_rear,
        "averageNormals": b.average_normals,
        "matching": match b.matching { BakeMatch::Always => "always", BakeMatch::ByMeshName => "byMeshName" },
        "lowSuffix": b.low_suffix,
        "highSuffix": b.high_suffix,
    })
}

/// Apply the `bake.settings` parameters present in `p` to `b`.
fn apply_settings(b: &mut BakeSettings, p: &Value, cmd: &str) -> Result<()> {
    if let Some(r) = u64p(p, "resolution") {
        b.resolution = crate::session::check_resolution(u32::try_from(r).unwrap_or(0))?;
    }
    if let Some(a) = u64p(p, "antialiasing") {
        if ![1, 2, 4, 8].contains(&a) {
            return Err(bad(cmd, "antialiasing must be 1, 2, 4 or 8"));
        }
        b.antialiasing = a as u32;
    }
    if let Some(d) = u64p(p, "dilation") {
        b.dilation = d.min(256) as u32;
    }
    // `rays` / `maxDistance` set every ray map; the per-map objects override.
    for r in [&mut b.ao, &mut b.thickness, &mut b.bent_normal] {
        apply_rays(r, p, cmd)?;
    }
    for (k, r) in [("ao", &mut b.ao), ("thickness", &mut b.thickness), ("bentNormal", &mut b.bent_normal)] {
        if let Some(o) = p.get(k) {
            apply_rays(r, o, cmd)?;
        }
    }
    if let Some(ao) = p.get("ao") {
        if let Some(v) = boolp(ao, "ignoreBackface") {
            b.ao_ignore_backface = v;
        }
        if let Some(v) = strp(ao, "selfOcclusion") {
            b.ao_self_occlusion = SELF_OCCLUSION.parse(cmd, "selfOcclusion", v)?;
        }
    }
    if let Some(v) = strp(p, "positionMode") {
        b.position_mode = POSITION_MODE.parse(cmd, "positionMode", v)?;
    }
    if let Some(v) = strp(p, "directionMode") {
        b.direction_mode = DIRECTION_MODE.parse(cmd, "directionMode", v)?;
    }
    if let Some(r) = u64p(p, "curvatureRadius") {
        b.curvature_radius = r.min(64) as u32;
    }
    for (k, slot, lo, hi) in
        [("curvatureStrength", &mut b.curvature_strength, 0.0, 16.0), ("maxFrontal", &mut b.max_frontal, 0.0, 1.0), ("maxRear", &mut b.max_rear, 0.0, 1.0)]
    {
        if let Some(v) = f32p(p, k).filter(|v| v.is_finite()) {
            *slot = v.clamp(lo, hi);
        }
    }
    for (k, slot) in [
        ("curvatureFromNormal", &mut b.curvature_from_normal),
        ("enabled", &mut b.enabled),
        ("useLowAsHigh", &mut b.use_low_as_high),
        ("averageNormals", &mut b.average_normals),
    ] {
        if let Some(v) = boolp(p, k) {
            *slot = v;
        }
    }
    if let Some(d) = p.get("direction") {
        let v: Vec<f32> = d.as_array().into_iter().flatten().filter_map(Value::as_f64).map(|x| x as f32).collect();
        match v[..] {
            [x, y, z] if [x, y, z].iter().all(|c| c.is_finite()) && [x, y, z] != [0.0; 3] => b.direction = [x, y, z],
            _ => return Err(bad(cmd, "`direction` must be three finite numbers, not all zero")),
        }
    }
    if let Some(a) = p.get("maps").and_then(Value::as_array) {
        b.maps = a.iter().map(|v| v.as_str().and_then(BakedMap::parse).ok_or_else(|| bad(cmd, format!("unknown mesh map {v}")))).collect::<Result<_>>()?;
    }
    match strp(p, "matching") {
        Some("always") => b.matching = BakeMatch::Always,
        Some("byMeshName") | Some("byName") => b.matching = BakeMatch::ByMeshName,
        Some(o) => return Err(bad(cmd, format!("unknown matching `{o}` (always|byMeshName)"))),
        None => {}
    }
    for (k, slot) in [("lowSuffix", &mut b.low_suffix), ("highSuffix", &mut b.high_suffix)] {
        if let Some(v) = strp(p, k) {
            *slot = v.chars().take(64).collect();
        }
    }
    Ok(())
}

/// Parameter names of a settings enum.
struct Names<T: 'static>(&'static [(&'static str, T)]);

impl<T: Copy + PartialEq> Names<T> {
    fn name(&self, v: T) -> &'static str {
        self.0.iter().find(|(_, x)| *x == v).map_or("", |(n, _)| n)
    }

    fn parse(&self, cmd: &str, key: &str, s: &str) -> Result<T> {
        let all = || self.0.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("|");
        self.0.iter().find(|(n, _)| *n == s).map(|(_, v)| *v).ok_or_else(|| bad(cmd, format!("unknown {key} `{s}` ({})", all())))
    }
}

const SELF_OCCLUSION: Names<SelfOcclusion> = Names(&[("always", SelfOcclusion::Always), ("sameMeshName", SelfOcclusion::SameMeshName)]);
const DISTRIBUTION: Names<RayDistribution> = Names(&[("cosine", RayDistribution::Cosine), ("uniform", RayDistribution::Uniform)]);
const POSITION_MODE: Names<PositionMode> = Names(&[("boundingBox", PositionMode::BoundingBox), ("boundingSphere", PositionMode::BoundingSphere)]);
const DIRECTION_MODE: Names<DirectionMode> = Names(&[("signed", DirectionMode::Signed), ("positive", DirectionMode::Positive)]);

/// `r` as `bake.settings` parameters.
fn rays_params(r: &RaySettings) -> Value {
    json!({ "rays": r.rays, "maxDistance": r.max_distance, "spread": r.spread, "distribution": DISTRIBUTION.name(r.distribution) })
}

/// Apply the ray parameters present in `p` (`rays`, `maxDistance`, `spread`, `distribution`).
fn apply_rays(r: &mut RaySettings, p: &Value, cmd: &str) -> Result<()> {
    if let Some(n) = u64p(p, "rays") {
        r.rays = n.clamp(1, u64::from(MAX_RAYS)) as u32;
    }
    for (k, slot, lo, hi) in [("maxDistance", &mut r.max_distance, 0.001, 10.0), ("spread", &mut r.spread, 0.0, 180.0)] {
        if let Some(v) = f32p(p, k).filter(|v| v.is_finite()) {
            *slot = v.clamp(lo, hi);
        }
    }
    if let Some(v) = strp(p, "distribution") {
        r.distribution = DISTRIBUTION.parse(cmd, "distribution", v)?;
    }
    Ok(())
}

/// Validate and apply the settings parameters to every set first, so a bad parameter changes
/// nothing.
fn set_settings(s: &mut Session, p: &Value, cmd: &str) -> Result<Vec<usize>> {
    let sets = sets_param(s, cmd, p)?;
    let mut next = Vec::with_capacity(sets.len());
    for &ts in &sets {
        let mut b = s.doc.project.texture_set(ts)?.bake.clone();
        apply_settings(&mut b, p, cmd)?;
        next.push((ts, b));
    }
    for (ts, b) in next {
        let set = s.doc.project.texture_set_mut(ts)?;
        if set.bake != b {
            set.bake = b;
            s.modified = true;
        }
    }
    Ok(sets)
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    let sets = set_settings(s, p, "bake.settings")?;
    let out: Vec<Value> =
        sets.iter().filter_map(|t| s.doc.project.texture_sets.get(*t).map(|set| json!({ "ts": t, "settings": settings_params(&set.bake) }))).collect();
    Ok(json!(out))
}

fn bake_run(s: &mut Session, p: &Value) -> Result<Value> {
    let sets = set_settings(s, p, "bake.run")?;
    let cpu = boolp(p, "cpu") == Some(true);
    let tracer = sets.iter().map(|&ts| s.bake_now(ts, cpu)).collect::<Result<Vec<_>>>()?;
    s.update();
    Ok(json!({ "baked": sets, "tracer": tracer }))
}

fn skew_paint(s: &mut Session, p: &Value) -> Result<Value> {
    use texturecraft_geom::glam::Vec2;
    let cmd = "bake.skew.paint";
    const MAX_POINTS: usize = 100_000;
    let points: Vec<Vec2> = p
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(cmd, "`points` is required: [[u, v], ...]"))?
        .iter()
        .map(|q| match (q.get(0).and_then(Value::as_f64), q.get(1).and_then(Value::as_f64)) {
            (Some(u), Some(v)) => Ok(Vec2::new(u as f32, v as f32)),
            _ => Err(bad(cmd, "each point is [u, v]")),
        })
        .collect::<Result<_>>()?;
    if points.is_empty() || points.len() > MAX_POINTS {
        return Err(bad(cmd, format!("give 1 to {MAX_POINTS} points")));
    }
    let size = p.get("size").and_then(Value::as_u64).unwrap_or(1024);
    if !(16..=4096).contains(&size) {
        return Err(bad(cmd, "`size` is 16 to 4096"));
    }
    let (value, radius) = (f32p(p, "value").unwrap_or(1.0), f32p(p, "radius").unwrap_or(0.03));
    if !(radius.is_finite() && radius > 0.0 && radius <= 1.0) {
        return Err(bad(cmd, "`radius` is above 0 and at most 1 (uv units)"));
    }
    let (hardness, strength) = (f32p(p, "hardness").unwrap_or(0.5), f32p(p, "strength").unwrap_or(1.0));
    let stroke = |m: &mut SkewMap| m.paint(&points, radius, value, hardness, strength).map_err(|e| bad(cmd, e.to_string()));
    match s.bake_meshes.skew.as_mut() {
        // In place: the map is copied only while a running bake still holds it, and `paint`
        // refuses a stroke before it changes a texel.
        Some(k) if k.values.len() > 1 => stroke(Arc::make_mut(k))?,
        // A constant map has no resolution to paint at: widen it, keeping its value everywhere.
        k => {
            let mut map = match k {
                Some(k) => k.resized(size as u32, size as u32),
                None => SkewMap::constant(0.0).resized(size as u32, size as u32),
            }
            .map_err(|e| bad(cmd, e.to_string()))?;
            stroke(&mut map)?;
            s.bake_meshes.skew = Some(Arc::new(map));
        }
    }
    s.bake_meshes.skew_rev += 1;
    s.modified = true;
    Ok(meshes_json(s))
}
