//! `file.importMaterialX`: a MaterialX (`.mtlx`) material becomes one fill layer.
//!
//! The document is read by [`texturecraft_formats::materialx_read`]; here its image files are
//! loaded (only from the document's own folder and below: a document is untrusted, and its
//! `file` values must not reach other files), added to the project as resources and set as the
//! fill's sources, with constants as uniform sources. The whole import is one undo step. Channels
//! the texture set doesn't have, images that can't be read and anything the reader skipped are
//! reported in `warnings`; the import goes ahead with the rest.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use texturecraft_core::{Channel, Fill, Layer, LayerKind, Projection, Source};
use texturecraft_formats::materialx_read::{Material, Value as Mtlx};

use crate::commands::{CommandSpec, always, bad, strp};
use crate::{EngineError, Result, Session, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "file.importMaterialX",
        "Import MaterialX material...",
        ["File"],
        None,
        "{path: a .mtlx file, material?: the surfacematerial to read (default: the first), name?: layer name} → {layer, material, channels, resources, warnings}: one fill layer with the material's textures (read from the file's folder) and values; open_pbr_surface, standard_surface and gltf_pbr are read",
        always,
        import
    )]
}

/// Loads an image file named in the document (relative to it): its bytes, or why not.
pub type Loader<'a> = &'a dyn Fn(&str) -> std::result::Result<Vec<u8>, String>;

/// A material's fill, ready to insert: its sources, the image resources to add (encoded and
/// decoded) and what was skipped.
pub(crate) struct Prepared {
    pub fill: Fill,
    pub channels: Vec<Channel>,
    pub added: Vec<(String, Arc<Vec<u8>>)>,
    pub decoded: Vec<(String, Arc<texturecraft_compose::ImageRes>)>,
    pub warnings: Vec<String>,
    /// Image file → resource name, so each file is added once.
    names: BTreeMap<String, String>,
}

impl Prepared {
    /// Load image `file` with `load` as a new resource (once per file): its unique name.
    pub(crate) fn image(&mut self, s: &Session, file: &str, load: Loader) -> std::result::Result<String, String> {
        if let Some(n) = self.names.get(file) {
            return Ok(n.clone());
        }
        let bytes = load(file)?;
        let image = texturecraft_formats::decode_image(&bytes).map_err(|e| e.to_string())?;
        let leaf = file.rsplit(['/', '\\']).next().unwrap_or(file);
        let base = texturecraft_formats::sanitize_resource_name(leaf);
        let (mut unique, mut n) = (base.clone(), 2);
        while s.doc.resources.contains_key(&unique) || self.added.iter().any(|(a, _)| *a == unique) {
            unique = format!("{n}_{base}");
            n += 1;
        }
        self.added.push((unique.clone(), Arc::new(bytes)));
        self.decoded.push((unique.clone(), crate::resource_cmds::decoded(image)));
        self.names.insert(file.to_string(), unique.clone());
        Ok(unique)
    }
}

impl Session {
    /// `material`'s fill for the active texture set (UV projected), loading its images with
    /// `load`. Channels the set doesn't have and images that can't be read become warnings.
    pub(crate) fn prepare_material(&self, material: Material, load: Loader) -> Prepared {
        let ts = self.ts();
        let available = self.doc.project.texture_set(ts).map(|t| t.channel_list()).unwrap_or_default();
        let fill = Fill { projection: Projection::Uv { scale: material.uv_tiling, rotation: 0.0, offset: [0.0, 0.0] }, ..Fill::default() };
        let mut p =
            Prepared { fill, channels: Vec::new(), added: Vec::new(), decoded: Vec::new(), warnings: material.warnings.clone(), names: BTreeMap::new() };
        for (channel, value) in &material.channels {
            if !available.contains(channel) {
                p.warnings.push(format!("{} was skipped: this texture set has no such channel", channel.id()));
                continue;
            }
            let source = match value {
                Mtlx::Constant(c) => Source::Uniform(*c),
                Mtlx::Image(file) => match p.image(self, file, load) {
                    Ok(name) => Source::image(name),
                    Err(e) => {
                        p.warnings.push(format!("{} was skipped: {file}: {e}", channel.id()));
                        continue;
                    }
                },
            };
            p.fill.sources.insert(*channel, source);
            p.channels.push(*channel);
        }
        p
    }

    /// Insert a prepared fill as layer `name` above the selection, with its resources (one undo
    /// step named `label`); returns the layer.
    pub(crate) fn insert_prepared(&mut self, p: &Prepared, name: String, label: &str) -> Result<texturecraft_core::LayerId> {
        let (fill, channels, added) = (p.fill.clone(), p.channels.clone(), p.added.clone());
        let id = self.edit(label, |s| {
            for (n, bytes) in &added {
                s.doc.resources.insert(n.clone(), bytes.clone());
            }
            let id = s.doc.project.alloc_id();
            crate::layer_cmds::insert_selected(s, Layer::new(id, name, LayerKind::Fill(fill), &channels))
        })?;
        self.images.extend(p.decoded.iter().cloned());
        Ok(id)
    }

    /// Add `material` as a fill layer on the active texture set, loading its images with `load`.
    /// Returns the reply of `file.importMaterialX`.
    pub fn import_material(&mut self, material: Material, name: Option<&str>, load: Loader) -> Result<Value> {
        let material_name = material.name.clone();
        let p = self.prepare_material(material, load);
        if p.channels.is_empty() {
            let why = if p.warnings.is_empty() { String::new() } else { format!(" ({})", p.warnings.join("; ")) };
            return Err(EngineError::Other(format!("file.importMaterialX: nothing to import from {material_name:?}{why}")));
        }
        let ts = self.ts();
        let layer_name = name.map(str::to_string).unwrap_or_else(|| self.doc.project.next_layer_name(ts, &material_name));
        let id = self.insert_prepared(&p, layer_name, "Import MaterialX material")?;
        Ok(json!({
            "layer": id.0,
            "material": material_name,
            "channels": p.channels.iter().map(|c| c.id()).collect::<Vec<_>>(),
            "resources": p.added.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            "warnings": p.warnings,
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    use std::path::{Component, Path};
    let cmd = "file.importMaterialX";
    let path = strp(p, "path").filter(|s| !s.trim().is_empty()).ok_or_else(|| bad(cmd, "`path` is required"))?;
    let meta = std::fs::metadata(path).map_err(|e| bad(cmd, format!("cannot read {path}: {e}")))?;
    if !meta.is_file() || meta.len() > texturecraft_formats::materialx_read::MAX_BYTES as u64 {
        return Err(bad(cmd, format!("{path} is not a MaterialX file of a readable size")));
    }
    let bytes = std::fs::read(path).map_err(|e| bad(cmd, format!("cannot read {path}: {e}")))?;
    let material = texturecraft_formats::materialx_read::read_materialx(&bytes, strp(p, "material")).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
    let folder = Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let dir = folder.canonicalize().map_err(|e| bad(cmd, format!("cannot read the folder of {path}: {e}")))?;
    let load = |file: &str| -> std::result::Result<Vec<u8>, String> {
        // `<UDIM>` stands for a tile number; the first tile is read.
        let file = file.replace("<UDIM>", "1001");
        let rel = Path::new(&file);
        let outside = "the file is outside the material's folder";
        if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) {
            return Err(outside.into());
        }
        let full = dir.join(rel).canonicalize().map_err(|e| e.to_string())?;
        if !full.starts_with(&dir) {
            return Err(outside.into());
        }
        let meta = std::fs::metadata(&full).map_err(|e| e.to_string())?;
        // A pipe or device in the folder would block the read (a pipe reports no length).
        if !meta.is_file() {
            return Err("it is not a regular file".into());
        }
        let len = meta.len();
        if len > crate::resource_cmds::MAX_BYTES as u64 {
            return Err(format!("the file is over {} MB", crate::resource_cmds::MAX_BYTES >> 20));
        }
        std::fs::read(&full).map_err(|e| e.to_string())
    };
    s.import_material(material, strp(p, "name"), &load)
}

#[cfg(target_arch = "wasm32")]
fn import(_: &mut Session, _: &Value) -> Result<Value> {
    Err(EngineError::Other("file paths are not available in the browser".into()))
}
