//! Wavefront OBJ (via `tobj`): polygons triangulated by `RawMesh::polygon`, one vertex per unique v/vt/vn.

use texturecraft_geom::glam::{Vec2, Vec3, Vec4};

use super::{RawMesh, check_triangles, check_vertices};
use crate::{Result, malformed};

const KIND: &str = "OBJ";

pub(crate) fn read(bytes: &[u8]) -> Result<RawMesh> {
    crate::guard(KIND, || read_inner(bytes))
}

fn read_inner(bytes: &[u8]) -> Result<RawMesh> {
    let text = String::from_utf8_lossy(bytes);
    // Material names come from `usemtl` (MTL files are never read): tobj only assigns a face's
    // material when a library defines it, so every library request gets a synthetic one.
    let mut names: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.split_whitespace().next() == Some("usemtl")
            && let Some((_, name)) = line.split_once(' ')
            && !name.trim().is_empty()
            && !names.contains(&name.trim())
        {
            names.push(name.trim());
        }
    }
    let library: String = names.iter().map(|n| format!("newmtl {n}\n")).collect();
    let source = format!("mtllib texturecraft-usemtl.mtl\n{text}");
    // Polygons stay whole so the wireframe can show them; `RawMesh::polygon` triangulates.
    let opts = tobj::LoadOptions { single_index: true, triangulate: false, ignore_points: true, ignore_lines: true };
    let (models, materials) =
        tobj::load_obj_buf(&mut source.as_bytes(), &opts, |_| tobj::load_mtl_buf(&mut library.as_bytes())).map_err(|e| malformed(KIND, e))?;
    let materials = materials.unwrap_or_default();

    let mut raw = RawMesh::default();
    let (mut normals, mut uvs, mut colors) = (Some(Vec::new()), Some(Vec::new()), Vec::new());
    let mut partial_uvs = false;
    for model in &models {
        let m = &model.mesh;
        let n = m.positions.len() / 3;
        check_vertices(raw.positions.len().saturating_add(n))?;
        // Corners per face; without arities every face is a triangle.
        let arities: Vec<usize> = if m.face_arities.is_empty() { vec![3; m.indices.len() / 3] } else { m.face_arities.iter().map(|a| *a as usize).collect() };
        check_triangles(raw.triangle_count(), arities.iter().map(|a| a.saturating_sub(2)).sum())?;
        let base = raw.positions.len() as u32;
        raw.positions.extend(m.positions.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])));
        if m.normals.len() == n * 3 {
            if let Some(v) = &mut normals {
                v.extend(m.normals.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])));
            }
        } else {
            normals = None;
        }
        if m.texcoords.len() == n * 2 {
            if let Some(v) = &mut uvs {
                // OBJ UVs have their origin bottom-left.
                v.extend(m.texcoords.chunks_exact(2).map(|t| Vec2::new(t[0], 1.0 - t[1])));
            }
        } else {
            partial_uvs |= n > 0 && !m.indices.is_empty();
            uvs = None;
        }
        // `v x y z r g b`: objects without colors are white.
        match m.vertex_color.len() == n * 3 {
            true => colors.extend(m.vertex_color.chunks_exact(3).map(|c| Vec4::new(c[0], c[1], c[2], 1.0))),
            false => colors.extend(std::iter::repeat_n(Vec4::ONE, n)),
        }
        let name = match m.material_id {
            Some(id) => materials.get(id).map(|mat| mat.name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Material_{id}")),
            None => "Default".to_string(),
        };
        if let Some(i) = m.indices.iter().find(|i| **i as usize >= n) {
            return Err(malformed(KIND, format!("face index {i} out of range")));
        }
        // tobj names models without `o`/`g` "unnamed_object"; an empty name takes the file stem.
        let part = if model.name == "unnamed_object" { "" } else { model.name.as_str() };
        let (mut rest, mut corners) = (m.indices.as_slice(), Vec::new());
        for arity in arities {
            let Some((face, tail)) = rest.split_at_checked(arity) else { break };
            rest = tail;
            corners.clear();
            corners.extend(face.iter().map(|i| base + i));
            if arity >= 3 {
                raw.polygon(&name, part, &corners);
            }
        }
    }
    if partial_uvs && models.iter().any(|m| !m.mesh.texcoords.is_empty()) {
        raw.warnings.push("some objects have no UVs; the whole mesh was unwrapped automatically".into());
    }
    raw.normals = normals;
    raw.uvs = uvs;
    raw.colors = models.iter().any(|m| !m.mesh.vertex_color.is_empty()).then_some(colors);
    Ok(raw)
}
