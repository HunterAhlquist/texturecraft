//! Stanford PLY (ascii, binary little and big endian): vertex positions, normals, UVs and colors,
//! and faces of any size (triangulated by `RawMesh::polygon`). Other elements and properties are skipped.

use texturecraft_geom::glam::{Vec2, Vec3, Vec4};

use super::{RawMesh, check_triangles, check_vertices};
use crate::{Result, malformed};

const KIND: &str = "PLY";
/// Longest polygon accepted (a list count above this is treated as corruption).
const MAX_FACE_VERTICES: usize = 4096;

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Option<Ty> {
        Some(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return None,
        })
    }
    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
}

struct Property {
    name: String,
    ty: Ty,
    /// Count type of a list property.
    list: Option<Ty>,
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Le,
    Be,
}

/// Reads scalars from the body in either encoding.
struct Body<'a> {
    data: &'a [u8],
    pos: usize,
    enc: Encoding,
}

impl Body<'_> {
    fn next_token(&mut self) -> Option<&str> {
        let rest = self.data.get(self.pos..)?;
        let start = rest.iter().position(|b| !b.is_ascii_whitespace())?;
        let len = rest.get(start..)?.iter().position(|b| b.is_ascii_whitespace()).unwrap_or(rest.len() - start);
        self.pos += start + len;
        std::str::from_utf8(rest.get(start..start + len)?).ok()
    }

    fn read(&mut self, ty: Ty) -> Result<f64> {
        if self.enc == Encoding::Ascii {
            let tok = self.next_token().ok_or_else(|| malformed(KIND, "unexpected end of data"))?;
            return tok.parse::<f64>().map_err(|_| malformed(KIND, format!("bad number \"{tok}\"")));
        }
        let n = ty.size();
        let bytes = self.data.get(self.pos..self.pos + n).ok_or_else(|| malformed(KIND, "unexpected end of data"))?;
        self.pos += n;
        let mut b = [0u8; 8];
        b.get_mut(..n).unwrap_or_default().copy_from_slice(bytes);
        if self.enc == Encoding::Be {
            b.get_mut(..n).unwrap_or_default().reverse();
        }
        Ok(match ty {
            Ty::I8 => b[0] as i8 as f64,
            Ty::U8 => b[0] as f64,
            Ty::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Ty::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Ty::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Ty::F64 => f64::from_le_bytes(b),
        })
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

pub(crate) fn read(bytes: &[u8]) -> Result<RawMesh> {
    let (elements, enc, body_start) = header(bytes)?;
    let mut body = Body { data: bytes.get(body_start..).unwrap_or_default(), pos: 0, enc };
    let mut raw = RawMesh::default();
    let (mut normals, mut uvs, mut colors) = (Vec::new(), Vec::new(), Vec::new());
    // Elements may appear in any order, so take the vertex count from the header.
    let nverts: usize = elements.iter().filter(|e| e.name == "vertex").map(|e| e.count).fold(0, usize::saturating_add);
    for el in &elements {
        // Every item takes at least one byte, so the count can never exceed what is left.
        if el.count > body.remaining() {
            return Err(malformed(KIND, format!("element \"{}\" claims {} items but the file is too short", el.name, el.count)));
        }
        let find = |names: &[&str]| el.props.iter().position(|p| p.list.is_none() && names.contains(&p.name.as_str()));
        let (px, py, pz) = (find(&["x"]), find(&["y"]), find(&["z"]));
        let (nx, ny, nz) = (find(&["nx"]), find(&["ny"]), find(&["nz"]));
        let (tu, tv) = (find(&["s", "u", "texture_u"]), find(&["t", "v", "texture_v"]));
        let (cr, cg, cb, ca) = (find(&["red", "r"]), find(&["green", "g"]), find(&["blue", "b"]), find(&["alpha", "a"]));
        let has_color = cr.is_some() && cg.is_some() && cb.is_some();
        let vertex = el.name == "vertex";
        let has_n = nx.is_some() && ny.is_some() && nz.is_some();
        let has_uv = tu.is_some() && tv.is_some();
        if vertex {
            if px.is_none() || py.is_none() || pz.is_none() {
                return Err(malformed(KIND, "vertices have no x, y, z"));
            }
            check_vertices(el.count)?;
            raw.positions.reserve(el.count);
        }
        let face_list =
            (el.name == "face").then(|| el.props.iter().position(|p| p.list.is_some() && (p.name == "vertex_indices" || p.name == "vertex_index"))).flatten();
        let mut vals = vec![0.0f64; el.props.len()];
        let mut poly: Vec<u32> = Vec::new();
        for _ in 0..el.count {
            for (k, p) in el.props.iter().enumerate() {
                match p.list {
                    None => {
                        if let Some(v) = vals.get_mut(k) {
                            *v = body.read(p.ty)?;
                        }
                    }
                    Some(ct) => {
                        let len = body.read(ct)?;
                        if !(0.0..=MAX_FACE_VERTICES as f64).contains(&len) {
                            return Err(malformed(KIND, format!("bad list length {len}")));
                        }
                        poly.clear();
                        for _ in 0..len as usize {
                            let v = body.read(p.ty)?;
                            if face_list == Some(k) {
                                if !(0.0..nverts as f64).contains(&v) {
                                    return Err(malformed(KIND, format!("face index {v} out of range")));
                                }
                                poly.push(v as u32);
                            }
                        }
                        if face_list == Some(k) && poly.len() >= 3 {
                            check_triangles(raw.triangle_count(), poly.len() - 2)?;
                            raw.polygon("Default", "", &poly);
                        }
                    }
                }
            }
            if vertex {
                let g = |i: Option<usize>| i.and_then(|i| vals.get(i)).copied().unwrap_or(0.0) as f32;
                raw.positions.push(Vec3::new(g(px), g(py), g(pz)));
                if has_n {
                    normals.push(Vec3::new(g(nx), g(ny), g(nz)));
                }
                if has_uv {
                    // PLY texture coordinates follow OpenGL (origin bottom-left).
                    uvs.push(Vec2::new(g(tu), 1.0 - g(tv)));
                }
                if has_color {
                    // Integer channels are normalized by their type's maximum; floats are 0..1.
                    let c = |i: Option<usize>, default: f32| match i.and_then(|i| el.props.get(i)) {
                        Some(p) => g(i) / color_scale(p.ty),
                        None => default,
                    };
                    colors.push(Vec4::new(c(cr, 1.0), c(cg, 1.0), c(cb, 1.0), c(ca, 1.0)));
                }
            }
        }
    }
    let n = raw.positions.len();
    raw.normals = (normals.len() == n).then_some(normals);
    raw.uvs = (uvs.len() == n).then_some(uvs);
    raw.colors = (colors.len() == n).then_some(colors);
    Ok(raw)
}

fn color_scale(ty: Ty) -> f32 {
    match ty {
        Ty::U8 | Ty::I8 => 255.0,
        Ty::U16 | Ty::I16 => 65535.0,
        Ty::U32 | Ty::I32 => u32::MAX as f32,
        Ty::F32 | Ty::F64 => 1.0,
    }
}

fn header(bytes: &[u8]) -> Result<(Vec<Element>, Encoding, usize)> {
    if !bytes.starts_with(b"ply") {
        return Err(malformed(KIND, "missing \"ply\" signature"));
    }
    let mut pos = 0usize;
    let mut enc = None;
    let mut elements: Vec<Element> = Vec::new();
    loop {
        let rest = bytes.get(pos..).unwrap_or_default();
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else { return Err(malformed(KIND, "header has no end_header")) };
        let line = std::str::from_utf8(rest.get(..nl).unwrap_or_default()).map_err(|_| malformed(KIND, "header is not text"))?.trim();
        pos += nl + 1;
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["end_header"] => break,
            ["format", f, ..] => {
                enc = Some(match *f {
                    "ascii" => Encoding::Ascii,
                    "binary_little_endian" => Encoding::Le,
                    "binary_big_endian" => Encoding::Be,
                    other => return Err(malformed(KIND, format!("unknown format {other}"))),
                })
            }
            ["element", name, count] => {
                let count = count.parse::<usize>().map_err(|_| malformed(KIND, format!("bad element count {count}")))?;
                elements.push(Element { name: name.to_string(), count, props: Vec::new() });
            }
            ["property", "list", ct, ty, name] => {
                let el = elements.last_mut().ok_or_else(|| malformed(KIND, "property before element"))?;
                let (ct, ty) = (Ty::parse(ct), Ty::parse(ty));
                let (Some(ct), Some(ty)) = (ct, ty) else { return Err(malformed(KIND, format!("bad list types in \"{line}\""))) };
                el.props.push(Property { name: name.to_string(), ty, list: Some(ct) });
            }
            ["property", ty, name] => {
                let el = elements.last_mut().ok_or_else(|| malformed(KIND, "property before element"))?;
                let ty = Ty::parse(ty).ok_or_else(|| malformed(KIND, format!("bad property type {ty}")))?;
                el.props.push(Property { name: name.to_string(), ty, list: None });
            }
            ["ply"] | [] | ["comment", ..] | ["obj_info", ..] => {}
            _ => return Err(malformed(KIND, format!("unexpected header line \"{line}\""))),
        }
    }
    let enc = enc.ok_or_else(|| malformed(KIND, "header has no format line"))?;
    Ok((elements, enc, pos))
}
