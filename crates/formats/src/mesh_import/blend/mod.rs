//! Blender (`.blend`) files, read without Blender: every mesh object of the file's current
//! scene (the collection tree, minus collections and objects hidden in the viewport or excluded
//! from the active view layer, plus collection instances), in world space through the object
//! and parent transforms, converted from Blender's Z up and scene unit scale to meters, Y up.
//! Meshes come from Blender 5's attribute storage, the attribute layers of 3.x and 4.x, or the
//! `MVert` / `MPoly` / `MLoop` (2.63 to 3.3) and `MFace` (older) structs, with polygons, UVs
//! (the active map), vertex colors, material slots (object overrides included) and normals from
//! the sharp faces and edges (custom normals stored as vectors are kept; modifiers are not
//! applied, the file says which objects have them). Materials bring their Principled BSDF base
//! color and image (packed in the file, or a file beside it that [`package`] folds in) for
//! high-poly color map bakes, and cameras come along. Saves compressed with gzip or zstd are
//! inflated first.
//!
//! The container is read through the file's own SDNA struct catalog, so every Blender version
//! that writes the fields used here is understood (see [`file`]).

mod file;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, hash_map::Entry};

use texturecraft_geom::glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

use self::file::{BlendFile, BlockRef, View};
use super::external::below;
use super::{MAX_MESH_FILE_BYTES, MAX_TEXTURE_BYTES, MAX_ZIP_ENTRIES, RawBaseColor, RawCamera, RawMesh, check_triangles, check_vertices, lens_fov, zip_entry};
use crate::{FormatError, Result, malformed};

use self::file::KIND;

/// A zip package of a `.blend` and its texture files ([`package`]).
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
/// Deepest collection nesting and parent chain followed.
const MAX_DEPTH: usize = 64;
/// Objects visited, instances included.
const MAX_OBJECTS: usize = 1_000_000;
const DEFAULT_MATERIAL: &str = "Default";

// Object types and flags (DNA_object_types.h).
const OB_EMPTY: i64 = 0;
const OB_MESH: i64 = 1;
const OB_CAMERA: i64 = 11;
const OB_HIDE_VIEWPORT: i64 = 1;
const OB_DUPLICOLLECTION: i64 = 1 << 8;
const ROT_MODE_AXISANGLE: i64 = -1;
const ROT_MODE_QUAT: i64 = 0;
// Collections and view layers (DNA_collection_types.h, DNA_layer_types.h).
const COLLECTION_HIDE_VIEWPORT: i64 = 1;
const LAYER_COLLECTION_EXCLUDE: i64 = 1 << 4;
const LAYER_COLLECTION_HIDE: i64 = 1 << 7;
const BASE_HIDDEN: i64 = 1 << 8;
// Meshes (DNA_mesh_types.h, DNA_meshdata_types.h).
const ME_AUTOSMOOTH: i64 = 1 << 5;
const ME_SMOOTH: i64 = 1;
const ME_SHARP: i64 = 1 << 9;
// Cameras.
const CAM_ORTHO: i64 = 1;
const SENSOR_FIT_HOR: i64 = 1;
const SENSOR_FIT_VERT: i64 = 2;
// Shader nodes (idnames, and the numeric types older files rely on).
const SH_NODE_OUTPUT_MATERIAL: i64 = 124;
const SH_NODE_TEX_IMAGE: i64 = 143;
const SH_NODE_BSDF_PRINCIPLED: i64 = 193;
// CustomData layer types (DNA_customdata_types.h).
const CD_MVERT: i64 = 0;
const CD_MEDGE: i64 = 3;
const CD_MFACE: i64 = 4;
const CD_MTFACE: i64 = 5;
const CD_MCOL: i64 = 6;
const CD_PROP_FLOAT: i64 = 10;
const CD_PROP_INT32: i64 = 11;
const CD_MLOOPUV: i64 = 16;
const CD_PROP_BYTE_COLOR: i64 = 17;
const CD_MPOLY: i64 = 25;
const CD_MLOOP: i64 = 26;
const CD_CUSTOMLOOPNORMAL: i64 = 41;
const CD_PROP_INT8: i64 = 45;
const CD_PROP_INT32_2D: i64 = 46;
const CD_PROP_COLOR: i64 = 47;
const CD_PROP_FLOAT3: i64 = 48;
const CD_PROP_FLOAT2: i64 = 49;
const CD_PROP_BOOL: i64 = 50;

/// Element kinds of mesh attributes, over Blender 5's `AttrType` and the CustomData types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Bool,
    Int8,
    Int32,
    Int32x2,
    Int16x2,
    Float,
    Float2,
    Float3,
    ColorByte,
    ColorFloat,
    /// A legacy struct layer (`MVert`, `MPoly`, …), by CustomData type.
    Struct(i64),
}

impl Kind {
    fn from_attr_type(t: i64) -> Option<Kind> {
        Some(match t {
            0 => Kind::Bool,
            1 => Kind::Int8,
            2 => Kind::Int16x2,
            3 => Kind::Int32,
            4 => Kind::Int32x2,
            5 => Kind::Float,
            6 => Kind::Float2,
            7 => Kind::Float3,
            9 => Kind::ColorByte,
            10 => Kind::ColorFloat,
            _ => return None,
        })
    }

    fn from_cd_type(t: i64) -> Option<Kind> {
        Some(match t {
            CD_PROP_FLOAT => Kind::Float,
            CD_PROP_INT32 => Kind::Int32,
            CD_PROP_BYTE_COLOR => Kind::ColorByte,
            CD_CUSTOMLOOPNORMAL => Kind::Int16x2,
            CD_PROP_INT8 => Kind::Int8,
            CD_PROP_INT32_2D => Kind::Int32x2,
            CD_PROP_COLOR => Kind::ColorFloat,
            CD_PROP_FLOAT3 => Kind::Float3,
            CD_PROP_FLOAT2 => Kind::Float2,
            CD_PROP_BOOL => Kind::Bool,
            CD_MVERT | CD_MEDGE | CD_MFACE | CD_MTFACE | CD_MCOL | CD_MLOOPUV | CD_MPOLY | CD_MLOOP => Kind::Struct(t),
            _ => return None,
        })
    }

    fn elem_size(self) -> usize {
        match self {
            Kind::Bool | Kind::Int8 => 1,
            Kind::Int32 | Kind::Float | Kind::ColorByte | Kind::Int16x2 => 4,
            Kind::Int32x2 | Kind::Float2 => 8,
            Kind::Float3 => 12,
            Kind::ColorFloat => 16,
            Kind::Struct(_) => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Domain {
    Point,
    Edge,
    Face,
    Corner,
    /// Legacy per-face data with four corners each (`MTFace`, `MCol`).
    Face4,
}

/// One attribute (or CustomData layer) of a mesh.
#[derive(Clone)]
struct Attr<'f> {
    name: String,
    domain: Domain,
    kind: Kind,
    block: BlockRef<'f>,
    /// One value for every element.
    single: bool,
    /// Elements available.
    count: usize,
    /// For CustomData layers: index of the active layer among those of this type.
    active: usize,
}

impl<'f> Attr<'f> {
    fn offset(&self, i: usize, c: usize, comp: usize) -> usize {
        let i = if self.single { 0 } else { i };
        i.saturating_mul(self.kind.elem_size()).saturating_add(c.saturating_mul(comp))
    }

    fn f32(&self, i: usize, c: usize) -> Option<f32> {
        self.block.file.f32(self.block.bytes(), self.offset(i, c, 4)).ok()
    }

    fn i32(&self, i: usize, c: usize) -> Option<i32> {
        self.block.file.i32(self.block.bytes(), self.offset(i, c, 4)).ok()
    }

    fn bool(&self, i: usize) -> Option<bool> {
        self.block.bytes().get(self.offset(i, 0, 1)).map(|b| *b != 0)
    }

    /// Linear RGBA of a color element.
    fn color(&self, i: usize) -> Option<Vec4> {
        match self.kind {
            Kind::ColorByte => {
                let o = self.offset(i, 0, 1);
                let b = self.block.bytes().get(o..o + 4)?;
                Some(byte_color([b[0], b[1], b[2], b[3]]))
            }
            Kind::ColorFloat => Some(Vec4::new(self.f32(i, 0)?, self.f32(i, 1)?, self.f32(i, 2)?, self.f32(i, 3)?)),
            _ => None,
        }
    }

    fn vec2(&self, i: usize) -> Option<Vec2> {
        Some(Vec2::new(self.f32(i, 0)?, self.f32(i, 1)?))
    }

    fn vec3(&self, i: usize) -> Option<Vec3> {
        Some(Vec3::new(self.f32(i, 0)?, self.f32(i, 1)?, self.f32(i, 2)?))
    }
}

/// sRGB-encoded RGBA bytes (Blender's byte colors) as linear RGBA.
fn byte_color(b: [u8; 4]) -> Vec4 {
    let c = |v: u8| texturecraft_texel::srgb_to_linear(f32::from(v) / 255.0);
    Vec4::new(c(b[0]), c(b[1]), c(b[2]), f32::from(b[3]) / 255.0)
}

/// Every attribute of mesh `me`: Blender 5's attribute storage, then the CustomData layers.
fn attributes<'f>(me: &View<'f>) -> Vec<Attr<'f>> {
    let mut out = Vec::new();
    if let Some(storage) = me.sub("attribute_storage")
        && let Some(arr) = storage.deref("dna_attributes")
    {
        let n = storage.i64("dna_attributes_num").and_then(|n| usize::try_from(n).ok()).unwrap_or(arr.count()).min(arr.count());
        for i in 0..n {
            let Some(a) = arr.view(i) else { break };
            let (Some(kind), Some(domain)) = (a.i64("data_type").and_then(Kind::from_attr_type), a.i64("domain")) else { continue };
            let domain = match domain {
                0 => Domain::Point,
                1 => Domain::Edge,
                2 => Domain::Face,
                3 => Domain::Corner,
                _ => continue,
            };
            let name = a.deref("name").map(|b| b.string()).unwrap_or_default();
            let Some(holder) = a.deref("data") else { continue };
            let (block, single, count) = if a.i64("storage_type") == Some(1) {
                let Some(b) = holder.view_named("AttributeSingle", 0).and_then(|h| h.deref("data")) else { continue };
                (b, true, usize::MAX)
            } else {
                let Some(h) = holder.view_named("AttributeArray", 0) else { continue };
                let Some(b) = h.deref("data") else { continue };
                let size = h.i64("size").and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX);
                (b, false, size.min(b.bytes().len() / kind.elem_size().max(1)))
            };
            out.push(Attr { name, domain, kind, block, single, count, active: 0 });
        }
    }
    for (field, domain) in [("vdata", Domain::Point), ("edata", Domain::Edge), ("pdata", Domain::Face), ("ldata", Domain::Corner), ("fdata", Domain::Face4)] {
        let Some(cd) = me.sub(field) else { continue };
        let Some(layers) = cd.deref("layers") else { continue };
        let n = cd.i64("totlayer").and_then(|n| usize::try_from(n).ok()).unwrap_or(0).min(layers.count());
        for i in 0..n {
            let Some(l) = layers.view(i) else { break };
            let Some(kind) = l.i64("type").and_then(Kind::from_cd_type) else { continue };
            let Some(block) = l.deref("data") else { continue };
            let count = match kind {
                Kind::Struct(_) => block.count(),
                k => block.bytes().len() / k.elem_size().max(1),
            };
            let active = l.i64("active").and_then(|a| usize::try_from(a).ok()).unwrap_or(0);
            out.push(Attr { name: l.string("name").unwrap_or_default(), domain, kind, block, single: false, count, active });
        }
    }
    out
}

/// The attribute `name` of `kind` on `domain`.
fn attr<'a, 'f>(attrs: &'a [Attr<'f>], domain: Domain, kind: Kind, name: &str) -> Option<&'a Attr<'f>> {
    attrs.iter().find(|a| a.domain == domain && a.kind == kind && a.name == name)
}

/// The active attribute of `kinds` on `domain`: the one named `name` when given, else the
/// active CustomData layer, else the first whose name is not internal (`.`-prefixed).
fn active_attr<'a, 'f>(attrs: &'a [Attr<'f>], domain: Domain, kinds: &[Kind], name: Option<&str>) -> Option<&'a Attr<'f>> {
    let candidates: Vec<&Attr<'f>> = attrs.iter().filter(|a| a.domain == domain && kinds.contains(&a.kind)).collect();
    if let Some(n) = name.filter(|n| !n.is_empty())
        && let Some(a) = candidates.iter().find(|a| a.name == n)
    {
        return Some(a);
    }
    let public: Vec<&&Attr<'f>> = candidates.iter().filter(|a| !a.name.starts_with('.')).collect();
    public.first().and_then(|first| public.get(first.active)).or(public.first()).map(|a| **a)
}

/// The data block of the first legacy struct layer of CustomData type `cd` on `domain`.
fn struct_layer<'f>(attrs: &[Attr<'f>], domain: Domain, cd: i64) -> Option<BlockRef<'f>> {
    attrs.iter().find(|a| a.domain == domain && a.kind == Kind::Struct(cd)).map(|a| a.block)
}

/// The string a `char *` field of `v` points to.
fn pointed_string(v: &View<'_>, field: &str) -> Option<String> {
    v.deref(field).map(|b| b.string()).filter(|s| !s.is_empty())
}

/// Geometry of one mesh datablock in its local space.
struct MeshData {
    positions: Vec<Vec3>,
    /// Corner ranges per face into `corner_verts`.
    faces: Vec<(u32, u32)>,
    corner_verts: Vec<u32>,
    /// Material slot per face.
    material: Vec<u32>,
    smooth: Vec<bool>,
    sharp_edges: HashSet<(u32, u32)>,
    /// Legacy auto smooth: edges sharper than this angle (radians) are sharp.
    autosmooth: Option<f32>,
    /// Per corner, Blender convention (v up).
    uvs: Option<Vec<Vec2>>,
    custom_normals: Option<Vec<Vec3>>,
    /// Per corner, linear.
    colors: Option<Vec<Vec4>>,
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// The geometry of mesh `me` (None when it has no polygons).
fn mesh_data(me: BlockRef<'_>, version: u32) -> Result<Option<MeshData>> {
    let Some(v) = me.view(0) else { return Ok(None) };
    let file = v.file;
    let attrs = attributes(&v);
    let dna = &file.dna;
    let bad = |what: &str| malformed(KIND, format!("mesh {}: {what}", me.id_name().unwrap_or_default()));

    // Positions.
    let positions: Vec<Vec3> = if let Some(a) = attr(&attrs, Domain::Point, Kind::Float3, "position") {
        check_vertices(a.count)?;
        (0..a.count).map(|i| a.vec3(i)).collect::<Option<_>>().ok_or_else(|| bad("positions end early"))?
    } else if let (Some(b), Some(st)) = (struct_layer(&attrs, Domain::Point, CD_MVERT), dna.struct_index("MVert")) {
        check_vertices(b.count())?;
        (0..b.count())
            .map(|i| b.view_as(st, i)?.f32s("co", 3).map(|c| Vec3::new(c[0], c[1], c[2])))
            .collect::<Option<_>>()
            .ok_or_else(|| bad("MVert array ends early"))?
    } else {
        return Ok(None);
    };

    // Faces and corners.
    let mut faces: Vec<(u32, u32)> = Vec::new();
    let mut corner_verts: Vec<u32> = Vec::new();
    let mut material: Vec<u32> = Vec::new();
    let mut smooth: Vec<bool> = Vec::new();
    let mut uvs: Option<Vec<Vec2>> = None;
    let mut colors: Option<Vec<Vec4>> = None;
    let corner_vert_attr = attr(&attrs, Domain::Corner, Kind::Int32, ".corner_vert");
    let mloop = struct_layer(&attrs, Domain::Corner, CD_MLOOP).zip(dna.struct_index("MLoop"));
    let read_corners = |corner_verts: &mut Vec<u32>| -> Result<()> {
        if let Some(a) = corner_vert_attr {
            check_triangles(0, a.count / 3)?;
            for i in 0..a.count {
                corner_verts.push(u32::try_from(a.i32(i, 0).ok_or_else(|| bad("corner vertices end early"))?).map_err(|_| bad("negative corner vertex"))?);
            }
        } else if let Some((b, st)) = mloop {
            check_triangles(0, b.count() / 3)?;
            for i in 0..b.count() {
                let l = b.view_as(st, i).ok_or_else(|| bad("MLoop array ends early"))?;
                corner_verts.push(l.i64("v").and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad("MLoop vertex"))?);
            }
        }
        Ok(())
    };
    if let Some(offsets) = v.deref("poly_offset_indices") {
        read_corners(&mut corner_verts)?;
        let n = offsets.bytes().len() / 4;
        let mut prev = 0u32;
        for i in 0..n {
            let o = u32::try_from(file.i32(offsets.bytes(), i * 4)?).map_err(|_| bad("negative face offset"))?;
            if i > 0 {
                if o < prev {
                    return Err(bad("face offsets are not increasing"));
                }
                faces.push((prev, o - prev));
            }
            prev = o;
        }
    } else if let (Some(b), Some(st)) = (struct_layer(&attrs, Domain::Face, CD_MPOLY), dna.struct_index("MPoly")) {
        read_corners(&mut corner_verts)?;
        for i in 0..b.count() {
            let p = b.view_as(st, i).ok_or_else(|| bad("MPoly array ends early"))?;
            let start = p.i64("loopstart").and_then(|s| u32::try_from(s).ok()).ok_or_else(|| bad("MPoly loopstart"))?;
            let len = p.i64("totloop").and_then(|s| u32::try_from(s).ok()).ok_or_else(|| bad("MPoly totloop"))?;
            faces.push((start, len));
            material.push(p.i64("mat_nr").and_then(|m| u32::try_from(m).ok()).unwrap_or(0));
            smooth.push(p.i64("flag").unwrap_or(0) & ME_SMOOTH != 0);
        }
    } else if let (Some(b), Some(st)) = (struct_layer(&attrs, Domain::Face4, CD_MFACE), dna.struct_index("MFace")) {
        // Blender 2.62 and older: triangles and quads, UVs and colors per face corner.
        check_triangles(0, b.count().saturating_mul(2))?;
        let tface = struct_layer(&attrs, Domain::Face4, CD_MTFACE).zip(dna.struct_index("MTFace"));
        let mcol = struct_layer(&attrs, Domain::Face4, CD_MCOL);
        let (mut face_uvs, mut face_colors) = (Vec::new(), Vec::new());
        for i in 0..b.count() {
            let f = b.view_as(st, i).ok_or_else(|| bad("MFace array ends early"))?;
            let vs: Vec<u32> = ["v1", "v2", "v3", "v4"].iter().filter_map(|n| f.i64(n).and_then(|v| u32::try_from(v).ok())).collect();
            let n = if vs.get(3).is_some_and(|v| *v != 0) { 4 } else { 3 };
            if vs.len() < n {
                return Err(bad("MFace vertices"));
            }
            faces.push((corner_verts.len() as u32, n as u32));
            corner_verts.extend_from_slice(&vs[..n]);
            material.push(f.i64("mat_nr").and_then(|m| u32::try_from(m).ok()).unwrap_or(0));
            smooth.push(f.i64("flag").unwrap_or(0) & ME_SMOOTH != 0);
            if let Some((tb, tst)) = tface
                && let Some(t) = tb.view_as(tst, i)
                && let Some(uv) = t.f32s("uv", 8)
            {
                face_uvs.extend((0..n).map(|c| Vec2::new(uv[c * 2], uv[c * 2 + 1])));
            }
            if let Some(cb) = mcol
                && let Some(c) = cb.bytes().get(i * 16..i * 16 + 16)
            {
                // MCol is a, r, g, b per corner.
                face_colors.extend((0..n).map(|k| byte_color([c[k * 4 + 1], c[k * 4 + 2], c[k * 4 + 3], c[k * 4]])));
            }
        }
        uvs = (face_uvs.len() == corner_verts.len()).then_some(face_uvs);
        colors = (face_colors.len() == corner_verts.len()).then_some(face_colors);
    } else {
        return Ok(None);
    }
    if faces.is_empty() {
        return Ok(None);
    }
    let corners = corner_verts.len();
    for &(s, l) in &faces {
        if (s as usize).saturating_add(l as usize) > corners {
            return Err(bad("a face reaches past its corners"));
        }
    }
    if let Some(bad_v) = corner_verts.iter().find(|v| **v as usize >= positions.len()) {
        return Err(bad(&format!("corner vertex {bad_v} out of range ({} vertices)", positions.len())));
    }
    check_triangles(0, faces.iter().map(|(_, l)| (*l as usize).saturating_sub(2)).sum())?;

    // Per-face attributes of 3.4 and newer take precedence over the struct fields.
    if let Some(a) = attr(&attrs, Domain::Face, Kind::Int32, "material_index") {
        material = (0..faces.len()).map(|i| a.i32(i, 0).and_then(|m| u32::try_from(m).ok()).unwrap_or(0)).collect();
    } else if material.is_empty() {
        material = vec![0; faces.len()];
    }
    if let Some(a) = attr(&attrs, Domain::Face, Kind::Bool, "sharp_face") {
        smooth = (0..faces.len()).map(|i| !a.bool(i).unwrap_or(false)).collect();
    } else if smooth.is_empty() {
        smooth = vec![true; faces.len()];
    }

    // Sharp edges.
    let mut sharp_edges = HashSet::new();
    if let Some(sharp) = attr(&attrs, Domain::Edge, Kind::Bool, "sharp_edge") {
        if let Some(ev) = attr(&attrs, Domain::Edge, Kind::Int32x2, ".edge_verts") {
            for i in 0..ev.count {
                if sharp.bool(i).unwrap_or(false)
                    && let (Some(a), Some(b)) = (ev.i32(i, 0), ev.i32(i, 1))
                {
                    sharp_edges.insert(edge_key(a as u32, b as u32));
                }
            }
        } else if let (Some(b), Some(st)) = (struct_layer(&attrs, Domain::Edge, CD_MEDGE), dna.struct_index("MEdge")) {
            for i in 0..b.count() {
                if sharp.bool(i).unwrap_or(false)
                    && let Some(e) = b.view_as(st, i)
                    && let (Some(a), Some(c)) = (e.i64("v1"), e.i64("v2"))
                {
                    sharp_edges.insert(edge_key(a as u32, c as u32));
                }
            }
        }
    } else if let (Some(b), Some(st)) = (struct_layer(&attrs, Domain::Edge, CD_MEDGE), dna.struct_index("MEdge")) {
        for i in 0..b.count() {
            if let Some(e) = b.view_as(st, i)
                && e.i64("flag").unwrap_or(0) & ME_SHARP != 0
                && let (Some(a), Some(c)) = (e.i64("v1"), e.i64("v2"))
            {
                sharp_edges.insert(edge_key(a as u32, c as u32));
            }
        }
    }
    let autosmooth = (version < 401 && v.i64("flag").unwrap_or(0) & ME_AUTOSMOOTH != 0).then(|| v.f32("smoothresh").unwrap_or(std::f32::consts::FRAC_PI_6));

    // UVs: the active map.
    if uvs.is_none() {
        let name = pointed_string(&v, "active_uv_map_attribute").or_else(|| pointed_string(&v, "default_uv_map_attribute"));
        if let Some(a) = active_attr(&attrs, Domain::Corner, &[Kind::Float2], name.as_deref()) {
            uvs = (0..corners).map(|i| a.vec2(i)).collect::<Option<Vec<_>>>();
        } else if let (Some(a), Some(st)) = (active_attr(&attrs, Domain::Corner, &[Kind::Struct(CD_MLOOPUV)], None), dna.struct_index("MLoopUV")) {
            uvs = (0..corners).map(|i| a.block.view_as(st, i)?.f32s("uv", 2).map(|u| Vec2::new(u[0], u[1]))).collect::<Option<Vec<_>>>();
        }
    }

    // Custom normals stored as vectors (Blender 4.5 and newer).
    let mut custom_normals = None;
    if let Some(a) = attr(&attrs, Domain::Corner, Kind::Float3, "custom_normal") {
        custom_normals = (0..corners).map(|i| a.vec3(i)).collect::<Option<Vec<_>>>();
    } else if let Some(a) = attr(&attrs, Domain::Point, Kind::Float3, "custom_normal") {
        custom_normals = corner_verts.iter().map(|&cv| a.vec3(cv as usize)).collect::<Option<Vec<_>>>();
    }

    // Vertex colors: the default (render) color attribute, else the active, else the first.
    if colors.is_none() {
        let name = pointed_string(&v, "default_color_attribute").or_else(|| pointed_string(&v, "active_color_attribute"));
        let kinds = [Kind::ColorByte, Kind::ColorFloat];
        let named = name.as_deref().and_then(|n| {
            attr(&attrs, Domain::Corner, Kind::ColorByte, n)
                .or_else(|| attr(&attrs, Domain::Corner, Kind::ColorFloat, n))
                .map(|a| (a, true))
                .or_else(|| attr(&attrs, Domain::Point, Kind::ColorByte, n).or_else(|| attr(&attrs, Domain::Point, Kind::ColorFloat, n)).map(|a| (a, false)))
        });
        let pick = named
            .or_else(|| active_attr(&attrs, Domain::Corner, &kinds, None).map(|a| (a, true)))
            .or_else(|| active_attr(&attrs, Domain::Point, &kinds, None).map(|a| (a, false)));
        if let Some((a, per_corner)) = pick {
            colors = if per_corner { (0..corners).map(|i| a.color(i)).collect() } else { corner_verts.iter().map(|&cv| a.color(cv as usize)).collect() };
        }
    }
    Ok(Some(MeshData { positions, faces, corner_verts, material, smooth, sharp_edges, autosmooth, uvs, custom_normals, colors }))
}

/// Collects polygons into a [`RawMesh`] with one vertex per unique corner (position, normal,
/// UV and color); UVs and colors are kept only when every corner has them.
#[derive(Default)]
struct Sink {
    raw: RawMesh,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    colors: Vec<Vec4>,
    ids: HashMap<[u32; 12], u32>,
    polygon: Vec<u32>,
    missing_uvs: bool,
    some_uvs: bool,
    some_colors: bool,
    triangles: usize,
}

/// One polygon corner in world space.
struct Corner {
    p: Vec3,
    n: Vec3,
    uv: Option<Vec2>,
    color: Option<Vec4>,
}

impl Sink {
    fn polygon(&mut self, material: &str, part: &str, corners: &[Corner]) -> Result<()> {
        if corners.len() < 3 {
            return Ok(());
        }
        check_triangles(self.triangles, corners.len() - 2)?;
        self.triangles += corners.len() - 2;
        self.polygon.clear();
        for c in corners {
            self.missing_uvs |= c.uv.is_none();
            self.some_uvs |= c.uv.is_some();
            self.some_colors |= c.color.is_some();
            let uv = c.uv.unwrap_or_default();
            let color = c.color.unwrap_or(Vec4::ONE);
            let key = [c.p.x, c.p.y, c.p.z, c.n.x, c.n.y, c.n.z, uv.x, uv.y, color.x, color.y, color.z, color.w].map(f32::to_bits);
            let id = match self.ids.entry(key) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => {
                    let id = self.raw.positions.len();
                    check_vertices(id + 1)?;
                    self.raw.positions.push(c.p);
                    self.normals.push(c.n);
                    self.uvs.push(uv);
                    self.colors.push(color);
                    *e.insert(id as u32)
                }
            };
            self.polygon.push(id);
        }
        self.raw.polygon(material, part, &self.polygon);
        Ok(())
    }

    fn warn(&mut self, msg: String) {
        if !self.raw.warnings.contains(&msg) {
            self.raw.warnings.push(msg);
        }
    }

    fn finish(mut self) -> RawMesh {
        if self.missing_uvs && self.some_uvs {
            self.warn("some objects have no UVs; the whole mesh was unwrapped automatically".into());
        }
        self.raw.normals = Some(self.normals);
        self.raw.uvs = (!self.missing_uvs && self.some_uvs).then_some(self.uvs);
        self.raw.colors = self.some_colors.then_some(self.colors);
        self.raw
    }
}

/// Corner normals of `data` in world space: custom normals when stored, else the face
/// normal for flat faces and, for smooth ones, the angle-weighted average over the fan of
/// smooth faces around the vertex that no sharp edge (or auto-smooth angle) splits. `order`
/// holds the corner vertices as emitted (each face reversed when `mirrored`).
fn corner_normals(data: &MeshData, world: &[Vec3], order: &[u32], normal_m: Mat3, mirrored: bool) -> Vec<Vec3> {
    let corners = order.len();
    let nv = world.len();
    let pos = |c: u32| world.get(c as usize).copied().unwrap_or_default();
    // Per corner: face, the previous and next vertex around the face, corner angle.
    let mut corner_face = vec![0u32; corners];
    let mut prev_next = vec![(0u32, 0u32); corners];
    let mut angle = vec![0f32; corners];
    let mut face_n = vec![Vec3::Y; data.faces.len()];
    for (f, &(s, l)) in data.faces.iter().enumerate() {
        let (s, l) = (s as usize, l as usize);
        let Some(ring) = order.get(s..s + l) else { continue };
        let mut n = Vec3::ZERO;
        for i in 0..l {
            let (a, b) = (pos(ring[i]), pos(ring[(i + 1) % l]));
            n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
        }
        face_n[f] = n.try_normalize().unwrap_or(Vec3::Y);
        for i in 0..l {
            let (p, q, r) = (ring[(i + l - 1) % l], ring[i], ring[(i + 1) % l]);
            let (e1, e2) = (pos(p) - pos(q), pos(r) - pos(q));
            corner_face[s + i] = f as u32;
            prev_next[s + i] = (p, r);
            angle[s + i] = e1.angle_between(e2);
            if !angle[s + i].is_finite() {
                angle[s + i] = 0.0;
            }
        }
    }
    if let Some(custom) = &data.custom_normals {
        let mut out = Vec::with_capacity(corners);
        for (i, &f) in corner_face.iter().enumerate() {
            // Mirrored objects emit each face's corners reversed: read the stored corner.
            let src = match data.faces.get(f as usize) {
                Some(&(s, l)) if mirrored => (s as usize).saturating_add((l as usize).saturating_sub(1)).saturating_sub(i.saturating_sub(s as usize)),
                _ => i,
            };
            out.push(custom.get(src).and_then(|n| (normal_m * *n).try_normalize()).unwrap_or(face_n.get(f as usize).copied().unwrap_or(Vec3::Y)));
        }
        return out;
    }
    // Sharp edges: stored flags plus the legacy auto-smooth angle (and non-manifold edges).
    let mut sharp = data.sharp_edges.clone();
    if let Some(thresh) = data.autosmooth {
        let cos_t = thresh.clamp(0.0, std::f32::consts::PI).cos();
        let mut edge_faces: HashMap<(u32, u32), (u32, bool)> = HashMap::new();
        for (c, &(_, next)) in prev_next.iter().enumerate() {
            let key = edge_key(order[c], next);
            let f = corner_face[c];
            match edge_faces.entry(key) {
                Entry::Vacant(e) => {
                    e.insert((f, false));
                }
                Entry::Occupied(mut e) => {
                    let (other, seen_twice) = *e.get();
                    if seen_twice || face_n[f as usize].dot(face_n[other as usize]) < cos_t {
                        sharp.insert(key);
                    }
                    e.get_mut().1 = true;
                }
            }
        }
    }
    // Corners around each vertex.
    let mut start = vec![0u32; nv + 1];
    for &v in order {
        start[v as usize + 1] += 1;
    }
    for i in 0..nv {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut around = vec![0u32; corners];
    for (c, &v) in order.iter().enumerate() {
        around[fill[v as usize] as usize] = c as u32;
        fill[v as usize] += 1;
    }
    let mut out = vec![Vec3::Y; corners];
    let mut parent: Vec<u32> = Vec::new();
    let mut by_neighbor: HashMap<u32, u32> = HashMap::new();
    for v in 0..nv {
        let cs = &around[start[v] as usize..start[v + 1] as usize];
        parent.clear();
        parent.extend(0..cs.len() as u32);
        fn find(parent: &mut [u32], mut i: u32) -> u32 {
            while parent[i as usize] != i {
                parent[i as usize] = parent[parent[i as usize] as usize];
                i = parent[i as usize];
            }
            i
        }
        by_neighbor.clear();
        for (k, &c) in cs.iter().enumerate() {
            let f = corner_face[c as usize] as usize;
            if !data.smooth.get(f).copied().unwrap_or(true) {
                continue;
            }
            let (p, n) = prev_next[c as usize];
            for w in [p, n] {
                if sharp.contains(&edge_key(v as u32, w)) {
                    continue;
                }
                match by_neighbor.entry(w) {
                    Entry::Vacant(e) => {
                        e.insert(k as u32);
                    }
                    Entry::Occupied(e) => {
                        let (a, b) = (find(&mut parent, *e.get()), find(&mut parent, k as u32));
                        if a != b {
                            parent[a as usize] = b;
                        }
                    }
                }
            }
        }
        let mut acc: HashMap<u32, Vec3> = HashMap::new();
        for (k, &c) in cs.iter().enumerate() {
            let f = corner_face[c as usize] as usize;
            if data.smooth.get(f).copied().unwrap_or(true) {
                *acc.entry(find(&mut parent, k as u32)).or_default() += face_n[f] * angle[c as usize].max(1e-6);
            }
        }
        for (k, &c) in cs.iter().enumerate() {
            let f = corner_face[c as usize] as usize;
            let n = if data.smooth.get(f).copied().unwrap_or(true) { acc.get(&find(&mut parent, k as u32)).and_then(|n| n.try_normalize()) } else { None };
            out[c as usize] = n.unwrap_or(face_n[f]);
        }
    }
    out
}

/// Add one object's mesh to `sink`: `world` places it in meters, Y up.
fn add_object(sink: &mut Sink, data: &MeshData, world: Mat4, part: &str, slots: &[String]) -> Result<()> {
    let m3 = Mat3::from_mat4(world);
    let mirrored = m3.determinant() < 0.0;
    let normal_m = if m3.determinant().abs() > 1e-20 { m3.inverse().transpose() } else { Mat3::IDENTITY };
    let world_pos: Vec<Vec3> = data.positions.iter().map(|p| world.transform_point3(*p)).collect();
    // Corner order per face, reversed under mirroring so the winding stays front-facing.
    let mut order = data.corner_verts.clone();
    let remap: Vec<usize> = (0..data.corner_verts.len()).collect();
    let mut remap = remap;
    if mirrored {
        for &(s, l) in &data.faces {
            let (s, l) = (s as usize, l as usize);
            if let Some(r) = order.get_mut(s..s + l) {
                r.reverse();
            }
            if let Some(r) = remap.get_mut(s..s + l) {
                r.reverse();
            }
        }
    }
    let normals = corner_normals(data, &world_pos, &order, normal_m, mirrored);
    let mut corners = Vec::new();
    for (f, &(s, l)) in data.faces.iter().enumerate() {
        let (s, l) = (s as usize, l as usize);
        corners.clear();
        for i in s..s + l {
            let stored = remap.get(i).copied().unwrap_or(i);
            corners.push(Corner {
                p: world_pos.get(order.get(i).copied().unwrap_or(0) as usize).copied().unwrap_or_default(),
                n: normals.get(i).copied().unwrap_or(Vec3::Y),
                uv: data.uvs.as_ref().and_then(|u| u.get(stored)).map(|uv| Vec2::new(uv.x, 1.0 - uv.y)),
                color: data.colors.as_ref().and_then(|c| c.get(stored)).copied(),
            });
        }
        let slot = data.material.get(f).copied().unwrap_or(0) as usize;
        let material = slots.get(slot).map_or(DEFAULT_MATERIAL, String::as_str);
        sink.polygon(material, part, &corners)?;
    }
    Ok(())
}

/// An object's transform relative to its parent (`BKE_object_to_mat4`): scale, then rotation
/// (with the delta transforms), then translation.
fn local_matrix(ob: &View<'_>) -> Mat4 {
    let v3 = |name: &str, default: Vec3| ob.f32s(name, 3).map_or(default, |v| Vec3::new(v[0], v[1], v[2]));
    let loc = v3("loc", Vec3::ZERO) + v3("dloc", Vec3::ZERO);
    let scale = v3("size", Vec3::ONE) * v3("dscale", Vec3::ONE);
    let rotmode = ob.i64("rotmode").unwrap_or(1);
    let rot = |rot: &str, quat: &str, axis: &str, angle: &str| -> Mat3 {
        match rotmode {
            ROT_MODE_QUAT => {
                let q = ob.f32s(quat, 4).unwrap_or(vec![1.0, 0.0, 0.0, 0.0]);
                Mat3::from_quat(Quat::from_xyzw(q[1], q[2], q[3], q[0]).normalize())
            }
            ROT_MODE_AXISANGLE => {
                let a = v3(axis, Vec3::Y);
                Mat3::from_axis_angle(a.try_normalize().unwrap_or(Vec3::Y), ob.f32(angle).unwrap_or(0.0))
            }
            mode => {
                let e = v3(rot, Vec3::ZERO);
                let (x, y, z) = (Mat3::from_rotation_x(e.x), Mat3::from_rotation_y(e.y), Mat3::from_rotation_z(e.z));
                // Blender applies the axes in the order's sequence: XYZ is X first.
                match mode {
                    2 => y * z * x, // XZY
                    3 => z * x * y, // YXZ
                    4 => x * z * y, // YZX
                    5 => y * x * z, // ZXY
                    6 => x * y * z, // ZYX
                    _ => z * y * x, // XYZ
                }
            }
        }
    };
    let r = rot("drot", "dquat", "drotAxis", "drotAngle") * rot("rot", "quat", "rotAxis", "rotAngle");
    let m = Mat4::from_mat3(r * Mat3::from_diagonal(scale));
    Mat4::from_translation(loc) * m
}

/// A 4×4 float field as a matrix (Blender stores rows of columns like glam).
fn mat4_field(v: &View<'_>, name: &str) -> Option<Mat4> {
    let m = v.f32s(name, 16)?;
    let mut a = [0f32; 16];
    a.copy_from_slice(&m);
    Some(Mat4::from_cols_array(&a))
}

/// An object's world transform through its parent chain.
fn world_matrix(ob: BlockRef<'_>, depth: usize, cache: &mut HashMap<usize, Mat4>) -> Mat4 {
    if let Some(m) = cache.get(&ob.index) {
        return *m;
    }
    let Some(v) = ob.view(0) else { return Mat4::IDENTITY };
    let local = local_matrix(&v);
    let m = match v.id("parent").filter(|p| p.code()[..2] == *b"OB" && depth < MAX_DEPTH) {
        Some(parent) => world_matrix(parent, depth + 1, cache) * mat4_field(&v, "parentinv").unwrap_or(Mat4::IDENTITY) * local,
        None => local,
    };
    cache.insert(ob.index, m);
    m
}

/// Objects the scene shows: collected through the collection tree with the transform of the
/// collection instances they come through.
struct Walk<'f> {
    file: &'f BlendFile<'f>,
    /// Each object with the transform and name chain of the collection instances it comes
    /// through (identity and empty for objects placed directly).
    objects: Vec<(BlockRef<'f>, Mat4, String)>,
    seen: HashSet<(usize, [u32; 16])>,
    /// Collections excluded from (or hidden in) the active view layer, and objects hidden there.
    excluded: HashSet<usize>,
    hidden: HashSet<usize>,
    visits: usize,
    cache: HashMap<usize, Mat4>,
}

impl<'f> Walk<'f> {
    fn collection(&mut self, col: BlockRef<'f>, prefix: Mat4, chain: &str, depth: usize) {
        if depth > MAX_DEPTH || self.visits > MAX_OBJECTS || self.excluded.contains(&col.index) {
            return;
        }
        let Some(v) = col.view(0) else { return };
        if v.i64("flag").unwrap_or(0) & COLLECTION_HIDE_VIEWPORT != 0 {
            return;
        }
        for co in v.list("gobject") {
            if let Some(ob) = co.id("ob") {
                self.object(ob, prefix, chain, depth);
            }
        }
        for ch in v.list("children") {
            if let Some(c) = ch.id("collection").filter(|c| c.code()[..2] == *b"GR") {
                self.collection(c, prefix, chain, depth + 1);
            }
        }
    }

    fn object(&mut self, ob: BlockRef<'f>, prefix: Mat4, chain: &str, depth: usize) {
        self.visits += 1;
        if self.visits > MAX_OBJECTS || ob.code()[..2] != *b"OB" || self.hidden.contains(&ob.index) {
            return;
        }
        let Some(v) = ob.view(0) else { return };
        if v.i64("restrictflag").unwrap_or(0) & OB_HIDE_VIEWPORT != 0 {
            return;
        }
        if !self.seen.insert((ob.index, prefix.to_cols_array().map(f32::to_bits))) {
            return;
        }
        match v.i64("type") {
            Some(OB_MESH | OB_CAMERA) => self.objects.push((ob, prefix, chain.to_string())),
            Some(OB_EMPTY) if v.i64("transflag").unwrap_or(0) & OB_DUPLICOLLECTION != 0 => {
                if let Some(col) = v.id("dup_group").filter(|c| c.code()[..2] == *b"GR") {
                    let offset = col.view(0).and_then(|c| c.f32s("dupli_ofs", 3)).map_or(Vec3::ZERO, |o| Vec3::new(o[0], o[1], o[2]));
                    let world = prefix * world_matrix(ob, 0, &mut self.cache) * Mat4::from_translation(-offset);
                    let name = ob.id_name().unwrap_or_default();
                    let chain = if chain.is_empty() { name } else { format!("{chain}/{name}") };
                    self.collection(col, world, &chain, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Note what the view layer excludes or hides.
    fn view_layer(&mut self, scene: BlockRef<'f>, layer: Option<BlockRef<'f>>) {
        let layer = layer.or_else(|| scene.view(0)?.list("view_layers").next().and_then(|v| self.file.data_block(scene.index, v.addr)));
        let Some(layer) = layer.and_then(|l| l.view(0)) else { return };
        for base in layer.list("object_bases") {
            if base.i64("flag").unwrap_or(0) & BASE_HIDDEN != 0
                && let Some(ob) = base.id("object")
            {
                self.hidden.insert(ob.index);
            }
        }
        let mut stack: Vec<(View<'f>, usize)> = layer.list("layer_collections").map(|lc| (lc, 0)).collect();
        while let Some((lc, depth)) = stack.pop() {
            if lc.i64("flag").unwrap_or(0) & (LAYER_COLLECTION_EXCLUDE | LAYER_COLLECTION_HIDE) != 0
                && let Some(c) = lc.id("collection")
            {
                self.excluded.insert(c.index);
            }
            if depth < MAX_DEPTH {
                stack.extend(lc.list("layer_collections").map(|c| (c, depth + 1)));
            }
        }
    }
}

/// Material slot names of `ob` with mesh `me`: the object's own materials where its `matbits`
/// say so, else the mesh's; empty slots are [`DEFAULT_MATERIAL`]. Also the material blocks.
fn material_slots<'f>(ob: &View<'f>, me: &View<'f>) -> (Vec<String>, Vec<BlockRef<'f>>) {
    let file = me.file;
    let totcol = me.i64("totcol").and_then(|n| usize::try_from(n).ok()).unwrap_or(0).min(4096);
    let mesh_mats = me.deref("mat").map(|b| b.ptrs()).unwrap_or_default();
    let ob_mats = ob.deref("mat").map(|b| b.ptrs()).unwrap_or_default();
    let matbits = ob.deref("matbits").map(|b| b.bytes().to_vec()).unwrap_or_default();
    let mut names = Vec::new();
    let mut blocks = Vec::new();
    for i in 0..totcol {
        let from_object = matbits.get(i).is_some_and(|b| b & 1 != 0) && ob_mats.get(i).is_some_and(|p| *p != 0);
        let ptr = if from_object { ob_mats.get(i) } else { mesh_mats.get(i) }.copied().unwrap_or(0);
        match file.id_block(ptr).filter(|b| b.code()[..2] == *b"MA") {
            Some(ma) => {
                names.push(ma.id_name().filter(|n| !n.is_empty()).unwrap_or_else(|| DEFAULT_MATERIAL.to_string()));
                blocks.push(ma);
            }
            None => names.push(DEFAULT_MATERIAL.to_string()),
        }
    }
    (names, blocks)
}

/// Where an image's file lives: the stored path (Blender's `//` means beside the file) and
/// its bare file name, for [`package`] and the texture lookup.
fn image_paths(path: &str) -> Vec<String> {
    let p = path.trim_start_matches("//").replace('\\', "/");
    let mut out = vec![p.clone()];
    if let Some(leaf) = p.rsplit('/').next().filter(|l| *l != p) {
        out.push(leaf.to_string());
    }
    out
}

/// The bytes of image `im`: packed in the file, else the file `textures` finds.
fn image_bytes(im: BlockRef<'_>, textures: &mut dyn FnMut(&[String]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let v = im.view(0)?;
    let packed = v.deref("packedfile").or_else(|| v.list("packedfiles").next().and_then(|p| p.deref("packedfile")));
    if let Some(pf) = packed.and_then(|b| b.view_named("PackedFile", 0)) {
        let size = pf.i64("size").and_then(|s| usize::try_from(s).ok()).unwrap_or(0);
        if let Some(data) = pf.deref("data") {
            let bytes = data.bytes();
            let bytes = bytes.get(..size.min(bytes.len())).unwrap_or(bytes);
            if !bytes.is_empty() && bytes.len() as u64 <= MAX_TEXTURE_BYTES {
                return Some(bytes.to_vec());
            }
        }
    }
    let path = v.string("filepath").or_else(|| v.string("name")).filter(|p| !p.is_empty())?;
    textures(&image_paths(&path))
}

/// The Principled BSDF base color of material `ma` and the image feeding it, when any.
fn base_color(ma: BlockRef<'_>, textures: &mut dyn FnMut(&[String]) -> Option<Vec<u8>>) -> Option<RawBaseColor> {
    let v = ma.view(0)?;
    let name = ma.id_name()?;
    let mut factor = Vec4::new(v.f32("r").unwrap_or(1.0), v.f32("g").unwrap_or(1.0), v.f32("b").unwrap_or(1.0), 1.0);
    let mut image = None;
    let use_nodes = v.i64("use_nodes").unwrap_or(1) != 0;
    if use_nodes && let Some(tree) = v.deref("nodetree").and_then(|b| b.view(0)) {
        let is = |n: &View<'_>, idname: &str, ty: i64| n.string("idname").is_some_and(|s| s == idname) || n.i64("type") == Some(ty);
        let output = tree.list("nodes").find(|n| is(n, "ShaderNodeOutputMaterial", SH_NODE_OUTPUT_MATERIAL));
        let links: Vec<View<'_>> = tree.list("links").collect();
        let from = |sock: &View<'_>| links.iter().find(|l| l.ptr("tosock") == Some(sock.addr)).and_then(|l| l.deref("fromnode")).and_then(|b| b.view(0));
        // The Principled BSDF on the output's Surface, else the first in the tree.
        let principled = output
            .and_then(|o| o.list("inputs").find(|s| s.string("name").as_deref() == Some("Surface")))
            .and_then(|s| from(&s))
            .filter(|n| is(n, "ShaderNodeBsdfPrincipled", SH_NODE_BSDF_PRINCIPLED))
            .or_else(|| tree.list("nodes").find(|n| is(n, "ShaderNodeBsdfPrincipled", SH_NODE_BSDF_PRINCIPLED)));
        if let Some(bsdf) = principled
            && let Some(sock) = bsdf.list("inputs").find(|s| s.string("name").as_deref() == Some("Base Color"))
        {
            if let Some(rgba) = sock.deref("default_value").and_then(|b| b.view_named("bNodeSocketValueRGBA", 0)).and_then(|d| d.f32s("value", 4)) {
                factor = Vec4::new(rgba[0], rgba[1], rgba[2], rgba[3]);
            }
            if let Some(tex) = from(&sock).filter(|n| is(n, "ShaderNodeTexImage", SH_NODE_TEX_IMAGE))
                && let Some(im) = tex.id("id").filter(|b| b.code()[..2] == *b"IM")
            {
                image = image_bytes(im, textures);
                if image.is_some() {
                    factor = Vec4::ONE;
                }
            }
        }
    }
    let factor = if factor.is_finite() { factor.clamp(Vec4::ZERO, Vec4::ONE) } else { Vec4::ONE };
    (factor != Vec4::ONE || image.is_some()).then_some(RawBaseColor { material: name, factor, image })
}

/// The camera of object `ob` placed by `world` (meters, Y up); `render` is the scene's
/// `RenderData` for the sensor fit.
fn camera(ob: BlockRef<'_>, world: Mat4, render: Option<&View<'_>>) -> Option<RawCamera> {
    let v = ob.view(0)?;
    let cam = v.id("data").filter(|c| c.code()[..2] == *b"CA")?.view(0)?;
    let lens = cam.f32("lens").filter(|l| *l > 0.0).unwrap_or(50.0);
    let sensor_x = cam.f32("sensor_x").filter(|s| *s > 0.0).unwrap_or(36.0);
    let sensor_y = cam.f32("sensor_y").filter(|s| *s > 0.0).unwrap_or(24.0);
    let aspect = render
        .map(|r| {
            let (w, h) = (r.f32("xsch").unwrap_or(1920.0) * r.f32("xasp").unwrap_or(1.0), r.f32("ysch").unwrap_or(1080.0) * r.f32("yasp").unwrap_or(1.0));
            if w > 0.0 && h > 0.0 { w / h } else { 16.0 / 9.0 }
        })
        .unwrap_or(16.0 / 9.0);
    let vertical = match cam.i64("sensor_fit") {
        Some(SENSOR_FIT_VERT) => true,
        Some(SENSOR_FIT_HOR) => false,
        _ => aspect < 1.0,
    };
    // AUTO fit measures the sensor width across the larger render side.
    let sensor = if cam.i64("sensor_fit") == Some(SENSOR_FIT_VERT) { sensor_y } else { sensor_x };
    let fov = if vertical { lens_fov(lens, sensor) } else { 2.0 * ((lens_fov(lens, sensor).to_radians() / 2.0).tan() / aspect).atan().to_degrees() };
    let ortho = (cam.i64("type") == Some(CAM_ORTHO)).then(|| {
        let scale = cam.f32("ortho_scale").filter(|s| *s > 0.0).unwrap_or(6.0);
        if vertical { scale / 2.0 } else { scale / 2.0 / aspect }
    });
    Some(RawCamera::placed(ob.id_name().unwrap_or_default(), world, Vec3::NEG_Z, fov, ortho))
}

/// Blender's Z up to Y up.
fn to_y_up() -> Mat4 {
    Mat4::from_cols(Vec4::X, Vec4::new(0.0, 0.0, -1.0, 0.0), Vec4::new(0.0, 1.0, 0.0, 0.0), Vec4::W)
}

/// A `.blend`, or a package of one and its texture files.
pub(crate) fn read(bytes: &[u8]) -> Result<RawMesh> {
    if !bytes.starts_with(ZIP_MAGIC) {
        return crate::guard(KIND, || read_inner(bytes, &mut |_| None));
    }
    crate::guard(KIND, || {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| malformed(KIND, format!("package: {e}")))?;
        if zip.len() > MAX_ZIP_ENTRIES {
            return Err(FormatError::TooLarge(format!("the package has more than {MAX_ZIP_ENTRIES} entries")));
        }
        let name =
            zip.file_names().flatten().filter(|n| super::extension(n) == "blend").min_by_key(|n| zip.index_for_name(n)).map(std::borrow::Cow::into_owned);
        let name = name.ok_or_else(|| malformed(KIND, "the package contains no .blend file"))?;
        let blend = zip_entry(&mut zip, &name, MAX_MESH_FILE_BYTES)
            .ok_or_else(|| FormatError::TooLarge("the packaged .blend is unreadable or larger than 2 GiB".into()))?;
        if blend.starts_with(ZIP_MAGIC) {
            return Err(malformed(KIND, "the package holds another package"));
        }
        read_inner(&blend, &mut |paths| paths.iter().find_map(|p| zip_entry(&mut zip, &below(p)?, MAX_TEXTURE_BYTES)))
    })
}

/// A `.blend` whose base color textures are files, as a package holding it (first) and the
/// files `found` has, under their paths below its folder; None when it found none (or `bytes`
/// is a package already). Also the texture paths it could not find.
pub(crate) fn package(file_name: &str, bytes: &[u8], found: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<(Option<Vec<u8>>, Vec<String>)> {
    if bytes.starts_with(ZIP_MAGIC) {
        return Ok((None, Vec::new()));
    }
    crate::guard(KIND, || {
        let (mut files, mut missing): (Vec<(String, Vec<u8>)>, Vec<String>) = (Vec::new(), Vec::new());
        read_inner(bytes, &mut |paths| {
            if let Some((_, data)) = files.iter().find(|(n, _)| paths.iter().any(|p| below(p).as_ref() == Some(n))) {
                return Some(data.clone());
            }
            for p in paths {
                if let (Some(name), Some(data)) = (below(p), found(p).filter(|d| d.len() as u64 <= MAX_TEXTURE_BYTES)) {
                    files.push((name, data.clone()));
                    return Some(data);
                }
            }
            if let Some(first) = paths.first().filter(|f| !missing.iter().any(|m| m == *f)) {
                missing.push(first.to_string());
            }
            None
        })?;
        if files.is_empty() {
            return Ok((None, missing));
        }
        let total = files.iter().fold(bytes.len() as u64, |t, (_, d)| t.saturating_add(d.len() as u64));
        if total > MAX_MESH_FILE_BYTES {
            return Err(FormatError::TooLarge("the .blend and its textures exceed 2 GiB".into()));
        }
        let leaf = std::path::Path::new(file_name).file_name().and_then(|n| n.to_str()).unwrap_or("mesh.blend");
        Ok((Some(crate::mesh_export::usd::usdz(leaf, bytes, &files)?), missing))
    })
}

fn read_inner(bytes: &[u8], textures: &mut dyn FnMut(&[String]) -> Option<Vec<u8>>) -> Result<RawMesh> {
    let file = BlendFile::parse(bytes, MAX_MESH_FILE_BYTES)?;
    let file = &file;
    let glob = file.block_with_code(b"GLOB").and_then(|g| g.view(0));
    let scene = glob.as_ref().and_then(|g| g.id("curscene")).filter(|s| s.code()[..2] == *b"SC").or_else(|| file.blocks_with_code(b"SC").next());
    let scene_view = scene.and_then(|s| s.view(0));
    let unit_scale = scene_view.as_ref().and_then(|s| s.sub("unit")).and_then(|u| u.f32("scale_length")).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
    let render = scene_view.as_ref().and_then(|s| s.sub("r"));

    let mut walk = Walk { file, objects: Vec::new(), seen: HashSet::new(), excluded: HashSet::new(), hidden: HashSet::new(), visits: 0, cache: HashMap::new() };
    if let (Some(scene), Some(sv)) = (scene, &scene_view) {
        let layer = glob.as_ref().and_then(|g| g.ptr("cur_view_layer")).and_then(|p| file.data_block(scene.index, p));
        walk.view_layer(scene, layer);
        // The master collection is embedded in the scene (written as its data) since 2.80.
        let master = sv.deref("master_collection").or_else(|| sv.id("master_collection").filter(|c| c.code()[..2] == *b"GR"));
        if let Some(master) = master {
            walk.collection(master, Mat4::IDENTITY, "", 0);
        } else {
            // Blender 2.7x and older: the scene's base list.
            for base in sv.list("base") {
                if let Some(ob) = base.id("object") {
                    walk.object(ob, Mat4::IDENTITY, "", 0);
                }
            }
        }
    }
    let mut fallback = false;
    if walk.objects.is_empty() {
        // No scene (a library file), or one that shows nothing: every object that is not
        // hidden, else every mesh at the origin.
        for ob in file.blocks_with_code(b"OB") {
            walk.object(ob, Mat4::IDENTITY, "", 0);
        }
        fallback = scene.is_some() && !walk.objects.is_empty();
    }

    let conv = to_y_up() * Mat4::from_scale(Vec3::splat(unit_scale));
    let mut sink = Sink::default();
    let mut meshes: HashMap<usize, Option<MeshData>> = HashMap::new();
    let mut material_blocks: Vec<BlockRef<'_>> = Vec::new();
    let mut cameras = Vec::new();
    let (mut linked, mut with_modifiers) = (0usize, Vec::new());
    let mut cache = HashMap::new();
    if walk.objects.is_empty() {
        for me in file.blocks_with_code(b"ME") {
            if let Some(data) = mesh_data(me, file.version)? {
                let mv = me.view(0).ok_or_else(|| malformed(KIND, "mesh block"))?;
                let slots = mv.deref("mat").map(|b| b.ptrs()).unwrap_or_default();
                let names: Vec<String> =
                    slots.iter().map(|p| file.id_block(*p).and_then(|m| m.id_name()).unwrap_or_else(|| DEFAULT_MATERIAL.to_string())).collect();
                material_blocks.extend(slots.iter().filter_map(|p| file.id_block(*p)).filter(|b| b.code()[..2] == *b"MA"));
                add_object(&mut sink, &data, conv, &me.id_name().unwrap_or_default(), &names)?;
            }
        }
    }
    for (ob, prefix, chain) in &walk.objects {
        let Some(v) = ob.view(0) else { continue };
        let world = conv * *prefix * world_matrix(*ob, 0, &mut cache);
        let own = ob.id_name().unwrap_or_default();
        // Instanced objects are parts of their own, named by the instancers they come through.
        let name = if chain.is_empty() { own } else { format!("{chain}/{own}") };
        if v.i64("type") == Some(OB_CAMERA) {
            if cameras.len() < super::MAX_CAMERAS {
                cameras.extend(camera(*ob, world, render.as_ref()));
            }
            continue;
        }
        let Some(me) = v.id("data").filter(|m| m.code()[..2] == *b"ME" && !m.is_linked()) else {
            linked += 1;
            continue;
        };
        let data = match meshes.entry(me.index) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(mesh_data(me, file.version)?),
        };
        let Some(data) = data else { continue };
        let mv = me.view(0).ok_or_else(|| malformed(KIND, "mesh block"))?;
        let (slots, blocks) = material_slots(&v, &mv);
        material_blocks.extend(blocks);
        let mods: Vec<String> = v.list("modifiers").filter_map(|m| m.string("name")).filter(|n| !n.is_empty()).take(4).collect();
        if !mods.is_empty() {
            with_modifiers.push(format!("{name} ({})", mods.join(", ")));
        }
        add_object(&mut sink, data, world, &name, &slots)?;
    }
    if sink.triangles == 0 {
        return Err(FormatError::Invalid(format!("the Blender file (version {}.{}) has no visible mesh objects", file.version / 100, file.version % 100)));
    }
    if fallback {
        sink.warn("the current scene shows no mesh objects; every object of the file was imported".into());
    }
    if linked > 0 {
        sink.warn(format!("{linked} objects use mesh data linked from other files and were skipped"));
    }
    if !with_modifiers.is_empty() {
        let shown = with_modifiers.iter().take(6).cloned().collect::<Vec<_>>().join("; ");
        let more = if with_modifiers.len() > 6 { format!(" and {} more", with_modifiers.len() - 6) } else { String::new() };
        sink.warn(format!("modifiers are not applied: {shown}{more}; apply them in Blender to paint the final surface"));
    }
    let mut raw = sink.finish();
    raw.cameras = cameras;
    let mut seen = HashSet::new();
    for ma in material_blocks {
        if seen.insert(ma.index)
            && let Some(c) = base_color(ma, textures)
        {
            raw.base_colors.push(c);
        }
    }
    Ok(raw)
}
