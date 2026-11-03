//! Tests on `.blend` files written here: a tiny SDNA with the structs the reader uses, in
//! every container layout (legacy 32- and 64-bit, big endian, Blender 5), every mesh storage
//! generation, compressed, packaged, and cut short.

use std::collections::HashMap;

use texturecraft_geom::glam::{Mat4, Vec2, Vec3, Vec4};

use super::*;

const PTR_TYPES: &[(&str, usize)] =
    &[("char", 1), ("uchar", 1), ("int8_t", 1), ("short", 2), ("ushort", 2), ("int", 4), ("uint", 4), ("int64_t", 8), ("float", 4), ("double", 8), ("void", 0)];

/// A struct of the test SDNA: name and (type, field) pairs in SDNA name syntax.
type StructSpec = (&'static str, Vec<(&'static str, &'static str)>);
/// Data of a mesh written after its ID block.
type Later = Box<dyn FnOnce(&mut Writer)>;
/// A Blender 5 attribute: name, data type, domain, storage (0 array, 1 single), bytes, count.
type AttrSpec<'a> = (&'a str, i64, i64, i64, Vec<u8>, usize);
/// A CustomData layer: type, name, bytes.
type LayerSpec<'a> = (i64, &'a str, Vec<u8>);

/// Which generation of mesh storage a file uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gen {
    /// Blender 5: `attribute_storage`, large header.
    Attributes5,
    /// Blender 3.4 to 4.4: CustomData attribute layers and `poly_offset_indices`.
    Attributes4,
    /// Blender 2.63 to 3.3: `MVert`, `MPoly`, `MLoop`, `MLoopUV`, `MEdge`.
    Structs2,
    /// Blender 2.62 and older: `MFace`, `MTFace`, `MCol`, scene base list.
    Faces1,
}

fn structs(generation: Gen) -> Vec<StructSpec> {
    let mut mesh = vec![
        ("ID", "id"),
        ("Material", "**mat"),
        ("int", "totvert"),
        ("int", "totedge"),
        ("int", "totpoly"),
        ("int", "totloop"),
        ("short", "totcol"),
        ("ushort", "flag"),
        ("float", "smoothresh"),
    ];
    match generation {
        Gen::Attributes5 => mesh.extend([
            ("AttributeStorage", "attribute_storage"),
            ("int", "*poly_offset_indices"),
            ("char", "*active_uv_map_attribute"),
            ("char", "*default_color_attribute"),
        ]),
        Gen::Attributes4 => mesh.extend([("int", "*poly_offset_indices"), ("char", "*active_uv_map_attribute"), ("char", "*default_color_attribute")]),
        _ => {}
    }
    mesh.extend([("CustomData", "vdata"), ("CustomData", "edata"), ("CustomData", "pdata"), ("CustomData", "ldata"), ("CustomData", "fdata")]);
    vec![
        ("ID", vec![("void", "*next"), ("void", "*prev"), ("Library", "*lib"), ("char", "name[66]")]),
        ("Library", vec![("ID", "id")]),
        ("ListBase", vec![("void", "*first"), ("void", "*last")]),
        ("FileGlobal", vec![("int", "subversion"), ("Scene", "*curscene"), ("ViewLayer", "*cur_view_layer")]),
        ("UnitSettings", vec![("float", "scale_length"), ("char", "system")]),
        ("RenderData", vec![("int", "xsch"), ("int", "ysch"), ("float", "xasp"), ("float", "yasp")]),
        (
            "Scene",
            vec![
                ("ID", "id"),
                ("ListBase", "base"),
                ("RenderData", "r"),
                ("UnitSettings", "unit"),
                ("ListBase", "view_layers"),
                ("Collection", "*master_collection"),
            ],
        ),
        ("Collection", vec![("ID", "id"), ("ListBase", "gobject"), ("ListBase", "children"), ("float", "dupli_ofs[3]"), ("short", "flag")]),
        ("CollectionObject", vec![("CollectionObject", "*next"), ("CollectionObject", "*prev"), ("Object", "*ob")]),
        ("CollectionChild", vec![("CollectionChild", "*next"), ("CollectionChild", "*prev"), ("Collection", "*collection")]),
        ("Base", vec![("Base", "*next"), ("Base", "*prev"), ("Object", "*object"), ("short", "flag")]),
        ("ViewLayer", vec![("ViewLayer", "*next"), ("ViewLayer", "*prev"), ("ListBase", "object_bases"), ("ListBase", "layer_collections")]),
        (
            "LayerCollection",
            vec![
                ("LayerCollection", "*next"),
                ("LayerCollection", "*prev"),
                ("Collection", "*collection"),
                ("short", "flag"),
                ("ListBase", "layer_collections"),
            ],
        ),
        (
            "Object",
            vec![
                ("ID", "id"),
                ("short", "type"),
                ("short", "partype"),
                ("Object", "*parent"),
                ("ID", "*data"),
                ("ListBase", "modifiers"),
                ("Material", "**mat"),
                ("char", "*matbits"),
                ("int", "totcol"),
                ("float", "loc[3]"),
                ("float", "dloc[3]"),
                ("float", "size[3]"),
                ("float", "dscale[3]"),
                ("float", "rot[3]"),
                ("float", "drot[3]"),
                ("float", "quat[4]"),
                ("float", "dquat[4]"),
                ("float", "rotAxis[3]"),
                ("float", "drotAxis[3]"),
                ("float", "rotAngle"),
                ("float", "drotAngle"),
                ("float", "parentinv[4][4]"),
                ("short", "transflag"),
                ("short", "rotmode"),
                ("short", "restrictflag"),
                ("Collection", "*dup_group"),
            ],
        ),
        ("ModifierData", vec![("ModifierData", "*next"), ("ModifierData", "*prev"), ("char", "name[64]")]),
        ("Mesh", mesh),
        ("CustomData", vec![("CustomDataLayer", "*layers"), ("int", "totlayer"), ("int", "maxlayer")]),
        ("CustomDataLayer", vec![("int", "type"), ("int", "offset"), ("int", "flag"), ("int", "active"), ("char", "name[64]"), ("void", "*data")]),
        ("AttributeStorage", vec![("Attribute", "*dna_attributes"), ("int", "dna_attributes_num")]),
        ("Attribute", vec![("char", "*name"), ("short", "data_type"), ("int8_t", "domain"), ("int8_t", "storage_type"), ("void", "*data")]),
        ("AttributeArray", vec![("void", "*data"), ("void", "*sharing_info"), ("int64_t", "size"), ("int8_t", "is_single")]),
        ("AttributeSingle", vec![("void", "*data"), ("void", "*sharing_info")]),
        ("MVert", vec![("float", "co[3]"), ("short", "no[3]"), ("char", "flag"), ("char", "bweight")]),
        ("MPoly", vec![("int", "loopstart"), ("int", "totloop"), ("short", "mat_nr"), ("char", "flag"), ("char", "_pad")]),
        ("MLoop", vec![("uint", "v"), ("uint", "e")]),
        ("MLoopUV", vec![("float", "uv[2]"), ("int", "flag")]),
        ("MEdge", vec![("uint", "v1"), ("uint", "v2"), ("char", "crease"), ("char", "bweight"), ("short", "flag")]),
        ("MFace", vec![("uint", "v1"), ("uint", "v2"), ("uint", "v3"), ("uint", "v4"), ("short", "mat_nr"), ("char", "edcode"), ("char", "flag")]),
        ("MTFace", vec![("float", "uv[4][2]")]),
        ("MCol", vec![("uchar", "a"), ("uchar", "r"), ("uchar", "g"), ("uchar", "b")]),
        ("Material", vec![("ID", "id"), ("float", "r"), ("float", "g"), ("float", "b"), ("char", "use_nodes"), ("bNodeTree", "*nodetree")]),
        ("bNodeTree", vec![("ID", "id"), ("ListBase", "nodes"), ("ListBase", "links")]),
        (
            "bNode",
            vec![
                ("bNode", "*next"),
                ("bNode", "*prev"),
                ("ListBase", "inputs"),
                ("ListBase", "outputs"),
                ("char", "name[64]"),
                ("char", "idname[64]"),
                ("short", "type"),
                ("ID", "*id"),
            ],
        ),
        ("bNodeSocket", vec![("bNodeSocket", "*next"), ("bNodeSocket", "*prev"), ("char", "identifier[64]"), ("char", "name[64]"), ("void", "*default_value")]),
        (
            "bNodeLink",
            vec![
                ("bNodeLink", "*next"),
                ("bNodeLink", "*prev"),
                ("bNode", "*fromnode"),
                ("bNode", "*tonode"),
                ("bNodeSocket", "*fromsock"),
                ("bNodeSocket", "*tosock"),
            ],
        ),
        ("bNodeSocketValueRGBA", vec![("float", "value[4]")]),
        ("Image", vec![("ID", "id"), ("char", "name[1024]"), ("short", "source"), ("PackedFile", "*packedfile"), ("ListBase", "packedfiles")]),
        ("PackedFile", vec![("int", "size"), ("int", "seek"), ("void", "*data")]),
        (
            "Camera",
            vec![
                ("ID", "id"),
                ("char", "type"),
                ("float", "lens"),
                ("float", "ortho_scale"),
                ("float", "sensor_x"),
                ("float", "sensor_y"),
                ("char", "sensor_fit"),
            ],
        ),
    ]
}

/// A field value when writing a struct.
#[derive(Clone)]
enum Val {
    I(i64),
    F(f64),
    Fs(Vec<f32>),
    Ptr(u64),
    Str(&'static str),
    Sub(Vec<(&'static str, Val)>),
}

use Val::*;

/// Writes `.blend` files with the test SDNA.
struct Writer {
    ptr: usize,
    little: bool,
    large: bool,
    version: u32,
    types: Vec<(String, usize)>,
    names: Vec<String>,
    /// (type index, fields (type index, name index)).
    structs: Vec<(usize, Vec<(usize, usize)>)>,
    struct_by_name: HashMap<String, usize>,
    out: Vec<u8>,
    next_addr: u64,
    /// ID blocks get addresses of their own (Blender hashes their names).
    next_id_addr: u64,
}

impl Writer {
    fn new(generation: Gen, ptr: usize, little: bool) -> Writer {
        let (large, version) = match generation {
            Gen::Attributes5 => (true, 501),
            Gen::Attributes4 => (false, 402),
            Gen::Structs2 => (false, 283),
            Gen::Faces1 => (false, 249),
        };
        let mut w = Writer {
            ptr,
            little,
            large,
            version,
            types: PTR_TYPES.iter().map(|(n, s)| (n.to_string(), *s)).collect(),
            names: vec!["*first".into()],
            structs: Vec::new(),
            struct_by_name: HashMap::new(),
            out: Vec::new(),
            next_addr: 0x1000,
            next_id_addr: 0x8000_0000,
        };
        // A dummy struct 0 ("Link"), so struct indices of real structs are non-zero like Blender's.
        w.types.push(("Link".into(), 2 * ptr));
        w.structs.push((w.types.len() - 1, vec![]));
        let specs = structs(generation);
        for (name, _) in &specs {
            w.types.push((name.to_string(), 0));
        }
        for (name, fields) in &specs {
            let ty = w.type_index(name);
            let mut fs = Vec::new();
            for (fty, fname) in fields {
                let fti = w.type_index(fty);
                let ni = match w.names.iter().position(|n| n == fname) {
                    Some(i) => i,
                    None => {
                        w.names.push(fname.to_string());
                        w.names.len() - 1
                    }
                };
                fs.push((fti, ni));
            }
            w.struct_by_name.insert(name.to_string(), w.structs.len());
            w.structs.push((ty, fs));
        }
        // Sizes settle once every embedded struct (defined in any order) has its size.
        for _ in 0..8 {
            for si in 1..w.structs.len() {
                let (ty, fields) = w.structs[si].clone();
                w.types[ty].1 = fields.iter().map(|(fti, ni)| w.field_size(*fti, &w.names[*ni])).sum();
            }
        }
        w
    }

    fn type_index(&self, name: &str) -> usize {
        self.types.iter().position(|(n, _)| n == name).unwrap_or_else(|| panic!("type {name}"))
    }

    fn field_size(&self, ty: usize, name: &str) -> usize {
        let count: usize = name.split('[').skip(1).map(|d| d.trim_end_matches(']').parse::<usize>().unwrap()).product();
        if name.starts_with('*') { self.ptr * count } else { self.types[ty].1 * count }
    }

    fn alloc(&mut self) -> u64 {
        self.next_addr += 0x10;
        self.next_addr
    }

    fn put_u16(&self, out: &mut Vec<u8>, v: u16) {
        out.extend(if self.little { v.to_le_bytes() } else { v.to_be_bytes() });
    }

    fn put_u32(&self, out: &mut Vec<u8>, v: u32) {
        out.extend(if self.little { v.to_le_bytes() } else { v.to_be_bytes() });
    }

    fn put_u64(&self, out: &mut Vec<u8>, v: u64) {
        out.extend(if self.little { v.to_le_bytes() } else { v.to_be_bytes() });
    }

    fn put_ptr(&self, out: &mut Vec<u8>, v: u64) {
        if self.ptr == 8 { self.put_u64(out, v) } else { self.put_u32(out, v as u32) }
    }

    fn put_int(&self, out: &mut Vec<u8>, size: usize, v: i64) {
        match size {
            1 => out.push(v as u8),
            2 => self.put_u16(out, v as u16),
            4 => self.put_u32(out, v as u32),
            _ => self.put_u64(out, v as u64),
        }
    }

    /// The bytes of struct `name` with `vals` set (other fields zero).
    fn struct_bytes(&self, name: &str, vals: &[(&str, Val)]) -> Vec<u8> {
        let si = self.struct_by_name[name];
        let (ty, fields) = &self.structs[si];
        let mut out = vec![0u8; self.types[*ty].1];
        let mut off = 0;
        for (fti, ni) in fields {
            let fname = &self.names[*ni];
            let size = self.field_size(*fti, fname);
            let base = fname.trim_start_matches('*').split('[').next().unwrap();
            if let Some((_, v)) = vals.iter().find(|(n, _)| *n == base) {
                let tname = &self.types[*fti].0;
                let mut b = Vec::new();
                match v {
                    I(x) => self.put_int(&mut b, if fname.starts_with('*') { self.ptr } else { self.types[*fti].1 }, *x),
                    F(x) => {
                        if tname == "double" {
                            self.put_u64(&mut b, x.to_bits())
                        } else {
                            self.put_u32(&mut b, (*x as f32).to_bits())
                        }
                    }
                    Fs(xs) => xs.iter().for_each(|x| self.put_u32(&mut b, x.to_bits())),
                    Ptr(p) => self.put_ptr(&mut b, *p),
                    Str(s) => b.extend(s.as_bytes()),
                    Sub(sub) => b = self.struct_bytes(tname, sub),
                }
                assert!(b.len() <= size, "field {name}.{fname}: {} > {size}", b.len());
                out[off..off + b.len()].copy_from_slice(&b);
            }
            off += size;
        }
        out
    }

    fn block(&mut self, code: &[u8; 4], sdna: usize, old: u64, count: usize, data: &[u8]) {
        let mut h = Vec::new();
        h.extend(code);
        if self.large {
            self.put_u32(&mut h, sdna as u32);
            self.put_u64(&mut h, old);
            self.put_u64(&mut h, data.len() as u64);
            self.put_u64(&mut h, count as u64);
        } else {
            self.put_u32(&mut h, data.len() as u32);
            self.put_ptr(&mut h, old);
            self.put_u32(&mut h, sdna as u32);
            self.put_u32(&mut h, count as u32);
        }
        self.out.extend(h);
        self.out.extend(data);
    }

    /// An ID block of struct `st` named `name` (with its two-letter prefix), at a new address.
    fn id(&mut self, code: &[u8; 2], st: &str, name: &'static str, mut vals: Vec<(&'static str, Val)>) -> u64 {
        self.next_id_addr += 0x10;
        let addr = self.next_id_addr;
        vals.push(("id", Sub(vec![("name", Str(name))])));
        let bytes = self.struct_bytes(st, &vals);
        let si = self.struct_by_name[st];
        self.block(&[code[0], code[1], 0, 0], si, addr, 1, &bytes);
        addr
    }

    /// A DATA block of `count` structs `st` at `addr`.
    fn data(&mut self, addr: u64, st: &str, items: &[Vec<(&str, Val)>]) {
        let bytes: Vec<u8> = items.iter().flat_map(|v| self.struct_bytes(st, v)).collect();
        let si = self.struct_by_name[st];
        self.block(b"DATA", si, addr, items.len(), &bytes);
    }

    /// A raw DATA block at `addr`.
    fn raw(&mut self, addr: u64, bytes: &[u8]) {
        self.block(b"DATA", 0, addr, 1, bytes);
    }

    fn raw_i32(&mut self, addr: u64, values: &[i32]) {
        let mut b = Vec::new();
        values.iter().for_each(|v| self.put_u32(&mut b, *v as u32));
        self.raw(addr, &b);
    }

    fn raw_ptrs(&mut self, addr: u64, values: &[u64]) {
        let mut b = Vec::new();
        values.iter().for_each(|v| self.put_ptr(&mut b, *v));
        self.raw(addr, &b);
    }

    fn string(&mut self, addr: u64, s: &str) {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        self.raw(addr, &b);
    }

    /// A linked list of structs `st` written now (so owned by the last ID block written):
    /// each item gets an address; returns the first's.
    fn list(&mut self, st: &str, items: Vec<Vec<(&'static str, Val)>>) -> u64 {
        if items.is_empty() {
            return 0;
        }
        let first = self.alloc();
        self.list_at(first, st, items);
        first
    }

    /// A linked list whose first item sits at the pre-allocated `first` (so an ID block written
    /// in between can point at it).
    fn list_at(&mut self, first: u64, st: &str, items: Vec<Vec<(&'static str, Val)>>) {
        let addrs: Vec<u64> = items.iter().enumerate().map(|(i, _)| if i == 0 { first } else { self.alloc() }).collect();
        for (i, mut item) in items.into_iter().enumerate() {
            item.push(("next", Ptr(addrs.get(i + 1).copied().unwrap_or(0))));
            item.push(("prev", Ptr(if i > 0 { addrs[i - 1] } else { 0 })));
            self.data(addrs[i], st, &[item]);
        }
    }

    fn listbase(first: u64) -> Val {
        Sub(vec![("first", Ptr(first)), ("last", Ptr(first))])
    }

    fn finish(mut self) -> Vec<u8> {
        // DNA1
        let mut d = Vec::new();
        d.extend(b"SDNA");
        d.extend(b"NAME");
        self.put_u32(&mut d, self.names.len() as u32);
        for n in &self.names {
            d.extend(n.as_bytes());
            d.push(0);
        }
        while d.len() % 4 != 0 {
            d.push(0);
        }
        d.extend(b"TYPE");
        self.put_u32(&mut d, self.types.len() as u32);
        for (t, _) in &self.types {
            d.extend(t.as_bytes());
            d.push(0);
        }
        while d.len() % 4 != 0 {
            d.push(0);
        }
        d.extend(b"TLEN");
        for (_, l) in &self.types {
            self.put_u16(&mut d, *l as u16);
        }
        while d.len() % 4 != 0 {
            d.push(0);
        }
        d.extend(b"STRC");
        self.put_u32(&mut d, self.structs.len() as u32);
        for (ty, fields) in &self.structs {
            self.put_u16(&mut d, *ty as u16);
            self.put_u16(&mut d, fields.len() as u16);
            for (fty, ni) in fields {
                self.put_u16(&mut d, *fty as u16);
                self.put_u16(&mut d, *ni as u16);
            }
        }
        self.block(b"DNA1", 0, 0x100, 1, &d);
        self.block(b"ENDB", 0, 0, 0, &[]);
        let mut file = Vec::new();
        if self.large {
            file.extend(format!("BLENDER17-01v{:04}", self.version).as_bytes());
        } else {
            file.extend(b"BLENDER");
            file.push(if self.ptr == 8 { b'-' } else { b'_' });
            file.push(if self.little { b'v' } else { b'V' });
            file.extend(format!("{:03}", self.version).as_bytes());
        }
        file.extend(self.out);
        file
    }
}

/// A unit cube's 8 vertices (Blender coordinates), 6 quads with UVs.
fn cube() -> (Vec<f32>, Vec<i32>, Vec<f32>) {
    let positions = vec![-1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0];
    // Counter-clockwise seen from outside.
    let corners = vec![0, 3, 2, 1, 4, 5, 6, 7, 0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7];
    let uvs: Vec<f32> = (0..6).flat_map(|f| [0.0, 0.0, 0.25, 0.0, 0.25, 0.25, 0.0, 0.25].map(|v| v + (f as f32) * 0.1)).collect();
    (positions, corners, uvs)
}

/// An octahedron: 6 vertices, 8 triangles, smooth, no UVs.
fn octahedron() -> (Vec<f32>, Vec<i32>) {
    let positions = vec![1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0];
    let corners = vec![0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5];
    (positions, corners)
}

struct MeshSpec {
    name: &'static str,
    positions: Vec<f32>,
    /// Corner vertices per face (`face_len` corners each).
    corners: Vec<i32>,
    face_len: usize,
    uvs: Option<Vec<f32>>,
    /// Per face.
    materials: Vec<i64>,
    smooth: bool,
    /// Sharp edges (vertex pairs).
    sharp_edges: Vec<(i32, i32)>,
    /// Legacy auto smooth angle (degrees).
    autosmooth: Option<f32>,
    /// Per corner RGBA bytes.
    colors: Option<Vec<[u8; 4]>>,
    material_ptrs: Vec<u64>,
}

impl MeshSpec {
    fn cube(name: &'static str, materials: Vec<u64>) -> MeshSpec {
        let (positions, corners, uvs) = cube();
        MeshSpec {
            name,
            positions,
            corners,
            face_len: 4,
            uvs: Some(uvs),
            materials: (0..6).map(|f| i64::from(f >= 3)).collect(),
            smooth: false,
            sharp_edges: vec![],
            autosmooth: None,
            colors: None,
            material_ptrs: materials,
        }
    }

    fn octahedron(name: &'static str) -> MeshSpec {
        let (positions, corners) = octahedron();
        MeshSpec {
            name,
            positions,
            corners,
            face_len: 3,
            uvs: None,
            materials: vec![0; 8],
            smooth: true,
            sharp_edges: vec![],
            autosmooth: None,
            colors: None,
            material_ptrs: vec![],
        }
    }

    fn faces(&self) -> usize {
        self.corners.len() / self.face_len
    }

    /// Unique edges as vertex pairs (smaller first).
    fn edges(&self) -> Vec<(i32, i32)> {
        let mut edges = Vec::new();
        for face in self.corners.chunks(self.face_len) {
            for i in 0..face.len() {
                let (a, b) = (face[i], face[(i + 1) % face.len()]);
                let e = (a.min(b), a.max(b));
                if !edges.contains(&e) {
                    edges.push(e);
                }
            }
        }
        edges
    }
}

/// Write mesh `m` as an ME block in the writer's generation; returns its address.
fn write_mesh(w: &mut Writer, generation: Gen, m: &MeshSpec) -> u64 {
    let verts = m.positions.len() / 3;
    let edges = m.edges();
    let mat_addr = if m.material_ptrs.is_empty() { 0 } else { w.alloc() };
    let mut vals: Vec<(&str, Val)> = vec![
        ("totvert", I(verts as i64)),
        ("totedge", I(edges.len() as i64)),
        ("totpoly", I(m.faces() as i64)),
        ("totloop", I(m.corners.len() as i64)),
        ("totcol", I(m.material_ptrs.len() as i64)),
        ("mat", Ptr(mat_addr)),
    ];
    if let Some(angle) = m.autosmooth {
        vals.push(("flag", I(ME_AUTOSMOOTH)));
        vals.push(("smoothresh", F(f64::from(angle.to_radians()))));
    }
    let mut later: Vec<Later> = Vec::new();
    match generation {
        Gen::Attributes5 => {
            // name, data_type, domain, storage (0 array, 1 single), bytes, count
            let mut attrs: Vec<AttrSpec> = Vec::new();
            let f32s = |w: &Writer, v: &[f32]| {
                let mut b = Vec::new();
                v.iter().for_each(|x| w.put_u32(&mut b, x.to_bits()));
                b
            };
            let i32s = |w: &Writer, v: &[i32]| {
                let mut b = Vec::new();
                v.iter().for_each(|x| w.put_u32(&mut b, *x as u32));
                b
            };
            attrs.push(("position", 7, 0, 0, f32s(w, &m.positions), verts));
            attrs.push((".corner_vert", 3, 3, 0, i32s(w, &m.corners), m.corners.len()));
            let ev: Vec<i32> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
            attrs.push((".edge_verts", 4, 1, 0, i32s(w, &ev), edges.len()));
            attrs.push(("material_index", 3, 2, 0, i32s(w, &m.materials.iter().map(|x| *x as i32).collect::<Vec<_>>()), m.faces()));
            attrs.push(("sharp_face", 0, 2, 1, vec![u8::from(!m.smooth)], 1));
            if !m.sharp_edges.is_empty() {
                attrs.push(("sharp_edge", 0, 1, 0, edges.iter().map(|e| u8::from(m.sharp_edges.contains(e))).collect(), edges.len()));
            }
            if let Some(uv) = &m.uvs {
                attrs.push(("UVMap", 6, 3, 0, f32s(w, uv), m.corners.len()));
            }
            if let Some(c) = &m.colors {
                attrs.push(("Col", 9, 3, 0, c.iter().flatten().copied().collect(), m.corners.len()));
            }
            let arr_addr = w.alloc();
            let offsets_addr = w.alloc();
            let uv_name_addr = w.alloc();
            vals.push(("attribute_storage", Sub(vec![("dna_attributes", Ptr(arr_addr)), ("dna_attributes_num", I(attrs.len() as i64))])));
            vals.push(("poly_offset_indices", Ptr(offsets_addr)));
            vals.push(("active_uv_map_attribute", Ptr(uv_name_addr)));
            let items: Vec<(u64, u64, u64, AttrSpec)> = attrs.into_iter().map(|a| (w.alloc(), w.alloc(), w.alloc(), a)).collect();
            let offsets: Vec<i32> = (0..=m.faces()).map(|f| (f * m.face_len) as i32).collect();
            let face_len = m.face_len;
            later.push(Box::new(move |w: &mut Writer| {
                let _ = face_len;
                w.raw_i32(offsets_addr, &offsets);
                w.string(uv_name_addr, "UVMap");
                let structs: Vec<Vec<(&str, Val)>> = items
                    .iter()
                    .map(|(name_a, holder_a, _, (_, ty, dom, st, _, _))| {
                        vec![("name", Ptr(*name_a)), ("data_type", I(*ty)), ("domain", I(*dom)), ("storage_type", I(*st)), ("data", Ptr(*holder_a))]
                    })
                    .collect();
                w.data(arr_addr, "Attribute", &structs);
                for (name_a, holder_a, data_a, (name, _, _, st, bytes, count)) in items {
                    w.string(name_a, name);
                    if st == 1 {
                        w.data(holder_a, "AttributeSingle", &[vec![("data", Ptr(data_a))]]);
                    } else {
                        w.data(holder_a, "AttributeArray", &[vec![("data", Ptr(data_a)), ("size", I(count as i64))]]);
                    }
                    w.raw(data_a, &bytes);
                }
            }));
        }
        Gen::Attributes4 => {
            let offsets_addr = w.alloc();
            vals.push(("poly_offset_indices", Ptr(offsets_addr)));
            let offsets: Vec<i32> = (0..=m.faces()).map(|f| (f * m.face_len) as i32).collect();
            // CustomData layers per domain: (field, layers (type, name, bytes)).
            let f32s = |w: &Writer, v: &[f32]| {
                let mut b = Vec::new();
                v.iter().for_each(|x| w.put_u32(&mut b, x.to_bits()));
                b
            };
            let i32s = |w: &Writer, v: &[i32]| {
                let mut b = Vec::new();
                v.iter().for_each(|x| w.put_u32(&mut b, *x as u32));
                b
            };
            let ev: Vec<i32> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
            let mut vdata: Vec<LayerSpec> = vec![(CD_PROP_FLOAT3, "position", f32s(w, &m.positions))];
            let mut edata: Vec<LayerSpec> = vec![(CD_PROP_INT32_2D, ".edge_verts", i32s(w, &ev))];
            let mut pdata: Vec<LayerSpec> = vec![
                (CD_PROP_INT32, "material_index", i32s(w, &m.materials.iter().map(|x| *x as i32).collect::<Vec<_>>())),
                (CD_PROP_BOOL, "sharp_face", vec![u8::from(!m.smooth); m.faces()]),
            ];
            let mut ldata: Vec<LayerSpec> = vec![(CD_PROP_INT32, ".corner_vert", i32s(w, &m.corners))];
            if !m.sharp_edges.is_empty() {
                edata.push((CD_PROP_BOOL, "sharp_edge", edges.iter().map(|e| u8::from(m.sharp_edges.contains(e))).collect()));
            }
            if let Some(uv) = &m.uvs {
                // A decoy map first; the active index picks the second.
                ldata.push((CD_PROP_FLOAT2, "Decoy", vec![0; uv.len() * 4]));
                ldata.push((CD_PROP_FLOAT2, "UVMap", f32s(w, uv)));
            }
            if let Some(c) = &m.colors {
                ldata.push((CD_PROP_BYTE_COLOR, "Col", c.iter().flatten().copied().collect()));
            }
            vdata.sort_by_key(|l| l.0);
            pdata.sort_by_key(|l| l.0);
            let mut cds = Vec::new();
            for (field, layers) in [("vdata", vdata), ("edata", edata), ("pdata", pdata), ("ldata", ldata)] {
                let layers_addr = w.alloc();
                let with_addrs: Vec<(u64, LayerSpec)> = layers.into_iter().map(|l| (w.alloc(), l)).collect();
                vals.push((field, Sub(vec![("layers", Ptr(layers_addr)), ("totlayer", I(with_addrs.len() as i64))])));
                cds.push((layers_addr, with_addrs));
            }
            later.push(Box::new(move |w: &mut Writer| {
                w.raw_i32(offsets_addr, &offsets);
                for (layers_addr, layers) in cds {
                    let structs: Vec<Vec<(&str, Val)>> = layers
                        .iter()
                        .map(|(a, (ty, name, _))| {
                            vec![("type", I(*ty)), ("name", Str(name)), ("data", Ptr(*a)), ("active", I(i64::from(*ty == CD_PROP_FLOAT2)))]
                        })
                        .collect();
                    w.data(layers_addr, "CustomDataLayer", &structs);
                    for (a, (_, _, bytes)) in layers {
                        w.raw(a, &bytes);
                    }
                }
            }));
        }
        Gen::Structs2 => {
            let (vl, el, pl, ll) = (w.alloc(), w.alloc(), w.alloc(), w.alloc());
            let (va, ea, pa, la, uva) = (w.alloc(), w.alloc(), w.alloc(), w.alloc(), w.alloc());
            vals.push(("vdata", Sub(vec![("layers", Ptr(vl)), ("totlayer", I(1))])));
            vals.push(("edata", Sub(vec![("layers", Ptr(el)), ("totlayer", I(1))])));
            vals.push(("pdata", Sub(vec![("layers", Ptr(pl)), ("totlayer", I(1))])));
            vals.push(("ldata", Sub(vec![("layers", Ptr(ll)), ("totlayer", I(if m.uvs.is_some() { 2 } else { 1 }))])));
            let positions = m.positions.clone();
            let corners = m.corners.clone();
            let (face_len, materials, smooth, sharp, uvs) = (m.face_len, m.materials.clone(), m.smooth, m.sharp_edges.clone(), m.uvs.clone());
            let edges = edges.clone();
            later.push(Box::new(move |w: &mut Writer| {
                w.data(vl, "CustomDataLayer", &[vec![("type", I(CD_MVERT)), ("data", Ptr(va))]]);
                w.data(el, "CustomDataLayer", &[vec![("type", I(CD_MEDGE)), ("data", Ptr(ea))]]);
                w.data(pl, "CustomDataLayer", &[vec![("type", I(CD_MPOLY)), ("data", Ptr(pa))]]);
                let mut ll_items = vec![vec![("type", I(CD_MLOOP)), ("data", Ptr(la))]];
                if uvs.is_some() {
                    ll_items.push(vec![("type", I(CD_MLOOPUV)), ("name", Str("UVMap")), ("data", Ptr(uva))]);
                }
                w.data(ll, "CustomDataLayer", &ll_items);
                let mverts: Vec<Vec<(&str, Val)>> = positions.chunks(3).map(|p| vec![("co", Fs(p.to_vec()))]).collect();
                w.data(va, "MVert", &mverts);
                let medges: Vec<Vec<(&str, Val)>> = edges
                    .iter()
                    .map(|(a, b)| vec![("v1", I(*a as i64)), ("v2", I(*b as i64)), ("flag", I(if sharp.contains(&(*a, *b)) { ME_SHARP } else { 0 }))])
                    .collect();
                w.data(ea, "MEdge", &medges);
                let mpolys: Vec<Vec<(&str, Val)>> = (0..corners.len() / face_len)
                    .map(|f| {
                        vec![
                            ("loopstart", I((f * face_len) as i64)),
                            ("totloop", I(face_len as i64)),
                            ("mat_nr", I(materials[f])),
                            ("flag", I(if smooth { ME_SMOOTH } else { 0 })),
                        ]
                    })
                    .collect();
                w.data(pa, "MPoly", &mpolys);
                let mloops: Vec<Vec<(&str, Val)>> = corners.iter().map(|v| vec![("v", I(*v as i64))]).collect();
                w.data(la, "MLoop", &mloops);
                if let Some(uv) = uvs {
                    let items: Vec<Vec<(&str, Val)>> = uv.chunks(2).map(|u| vec![("uv", Fs(u.to_vec()))]).collect();
                    w.data(uva, "MLoopUV", &items);
                }
            }));
        }
        Gen::Faces1 => {
            let (vl, fl) = (w.alloc(), w.alloc());
            let (va, fa, ta, ca) = (w.alloc(), w.alloc(), w.alloc(), w.alloc());
            vals.push(("vdata", Sub(vec![("layers", Ptr(vl)), ("totlayer", I(1))])));
            let nf = 1 + usize::from(m.uvs.is_some()) + usize::from(m.colors.is_some());
            vals.push(("fdata", Sub(vec![("layers", Ptr(fl)), ("totlayer", I(nf as i64))])));
            let positions = m.positions.clone();
            let corners = m.corners.clone();
            let (face_len, materials, smooth, uvs, colors) = (m.face_len, m.materials.clone(), m.smooth, m.uvs.clone(), m.colors.clone());
            later.push(Box::new(move |w: &mut Writer| {
                w.data(vl, "CustomDataLayer", &[vec![("type", I(CD_MVERT)), ("data", Ptr(va))]]);
                let mut fl_items = vec![vec![("type", I(CD_MFACE)), ("data", Ptr(fa))]];
                if uvs.is_some() {
                    fl_items.push(vec![("type", I(CD_MTFACE)), ("data", Ptr(ta))]);
                }
                if colors.is_some() {
                    fl_items.push(vec![("type", I(CD_MCOL)), ("data", Ptr(ca))]);
                }
                w.data(fl, "CustomDataLayer", &fl_items);
                let mverts: Vec<Vec<(&str, Val)>> = positions.chunks(3).map(|p| vec![("co", Fs(p.to_vec()))]).collect();
                w.data(va, "MVert", &mverts);
                let mfaces: Vec<Vec<(&str, Val)>> = corners
                    .chunks(face_len)
                    .enumerate()
                    .map(|(f, c)| {
                        vec![
                            ("v1", I(c[0] as i64)),
                            ("v2", I(c[1] as i64)),
                            ("v3", I(c[2] as i64)),
                            ("v4", I(c.get(3).copied().unwrap_or(0) as i64)),
                            ("mat_nr", I(materials[f])),
                            ("flag", I(if smooth { ME_SMOOTH } else { 0 })),
                        ]
                    })
                    .collect();
                w.data(fa, "MFace", &mfaces);
                if let Some(uv) = uvs {
                    let items: Vec<Vec<(&str, Val)>> = uv.chunks(face_len * 2).map(|u| vec![("uv", Fs(u.to_vec()))]).collect();
                    w.data(ta, "MTFace", &items);
                }
                if let Some(c) = colors {
                    let mut b = Vec::new();
                    for face in c.chunks(face_len) {
                        for k in 0..4 {
                            let px = face.get(k).copied().unwrap_or([0, 0, 0, 255]);
                            b.extend([px[3], px[0], px[1], px[2]]);
                        }
                    }
                    w.raw(ca, &b);
                }
            }));
        }
    }
    let addr = w.id(b"ME", "Mesh", m.name, vals);
    if mat_addr != 0 {
        w.raw_ptrs(mat_addr, &m.material_ptrs);
    }
    for f in later {
        f(w);
    }
    addr
}

struct ObjectSpec {
    name: &'static str,
    ty: i64,
    data: u64,
    loc: [f32; 3],
    rot: [f32; 3],
    scale: [f32; 3],
    parent: u64,
    hidden: bool,
    modifiers: Vec<&'static str>,
    /// Instanced collection (an empty).
    instance: u64,
    /// Object-level material override for slot 0.
    material_override: u64,
}

impl ObjectSpec {
    fn new(name: &'static str, ty: i64, data: u64) -> ObjectSpec {
        ObjectSpec {
            name,
            ty,
            data,
            loc: [0.0; 3],
            rot: [0.0; 3],
            scale: [1.0; 3],
            parent: 0,
            hidden: false,
            modifiers: vec![],
            instance: 0,
            material_override: 0,
        }
    }
}

fn write_object(w: &mut Writer, o: &ObjectSpec) -> u64 {
    let mods = if o.modifiers.is_empty() { 0 } else { w.alloc() };
    let (mat_addr, bits_addr) = if o.material_override != 0 { (w.alloc(), w.alloc()) } else { (0, 0) };
    let mut vals = vec![
        ("type", I(o.ty)),
        ("data", Ptr(o.data)),
        ("parent", Ptr(o.parent)),
        ("loc", Fs(o.loc.to_vec())),
        ("rot", Fs(o.rot.to_vec())),
        ("size", Fs(o.scale.to_vec())),
        ("dscale", Fs(vec![1.0, 1.0, 1.0])),
        ("quat", Fs(vec![1.0, 0.0, 0.0, 0.0])),
        ("dquat", Fs(vec![1.0, 0.0, 0.0, 0.0])),
        ("rotmode", I(1)),
        ("parentinv", Fs(Mat4::IDENTITY.to_cols_array().to_vec())),
        ("restrictflag", I(if o.hidden { OB_HIDE_VIEWPORT } else { 0 })),
        ("modifiers", Writer::listbase(mods)),
        ("mat", Ptr(mat_addr)),
        ("matbits", Ptr(bits_addr)),
        ("totcol", I(if o.material_override != 0 { 1 } else { 0 })),
    ];
    if o.instance != 0 {
        vals.push(("transflag", I(OB_DUPLICOLLECTION)));
        vals.push(("dup_group", Ptr(o.instance)));
    }
    let addr = w.id(b"OB", "Object", o.name, vals);
    if mods != 0 {
        w.list_at(mods, "ModifierData", o.modifiers.iter().map(|m| vec![("name", Str(m))]).collect());
    }
    if o.material_override != 0 {
        w.raw_ptrs(mat_addr, &[o.material_override]);
        w.raw(bits_addr, &[1]);
    }
    addr
}

/// A material with a Principled BSDF base color, optionally fed by an image.
fn write_material(w: &mut Writer, name: &'static str, color: [f32; 4], image: u64) -> u64 {
    let tree_addr = w.alloc();
    let addr = w.id(b"MA", "Material", name, vec![("r", F(0.5)), ("g", F(0.5)), ("b", F(0.5)), ("use_nodes", I(1)), ("nodetree", Ptr(tree_addr))]);
    let rgba = w.alloc();
    let (out_node, bsdf, tex) = (w.alloc(), w.alloc(), w.alloc());
    let surface = w.list("bNodeSocket", vec![vec![("name", Str("Surface"))]]);
    let inputs = w.list("bNodeSocket", vec![vec![("name", Str("Roughness"))], vec![("name", Str("Base Color")), ("default_value", Ptr(rgba))]]);
    let base_color_sock = inputs + 0x10;
    let bsdf_out = w.list("bNodeSocket", vec![vec![("name", Str("BSDF"))]]);
    let tex_out = w.list("bNodeSocket", vec![vec![("name", Str("Color"))]]);
    w.data(rgba, "bNodeSocketValueRGBA", &[vec![("value", Fs(color.to_vec()))]]);
    let mut nodes = vec![
        (out_node, vec![("name", Str("Material Output")), ("idname", Str("ShaderNodeOutputMaterial")), ("inputs", Writer::listbase(surface))]),
        (
            bsdf,
            vec![
                ("name", Str("Principled BSDF")),
                ("idname", Str("ShaderNodeBsdfPrincipled")),
                ("inputs", Writer::listbase(inputs)),
                ("outputs", Writer::listbase(bsdf_out)),
            ],
        ),
    ];
    let mut links = vec![vec![("fromnode", Ptr(bsdf)), ("fromsock", Ptr(bsdf_out)), ("tonode", Ptr(out_node)), ("tosock", Ptr(surface))]];
    if image != 0 {
        nodes.push((
            tex,
            vec![("name", Str("Image Texture")), ("idname", Str("ShaderNodeTexImage")), ("id", Ptr(image)), ("outputs", Writer::listbase(tex_out))],
        ));
        links.push(vec![("fromnode", Ptr(tex)), ("fromsock", Ptr(tex_out)), ("tonode", Ptr(bsdf)), ("tosock", Ptr(base_color_sock))]);
    }
    for (i, (a, mut vals)) in nodes.clone().into_iter().enumerate() {
        vals.push(("next", Ptr(nodes.get(i + 1).map_or(0, |n| n.0))));
        w.data(a, "bNode", &[vals]);
    }
    let links_first = w.list("bNodeLink", links);
    w.data(tree_addr, "bNodeTree", &[vec![("nodes", Writer::listbase(out_node)), ("links", Writer::listbase(links_first))]]);
    addr
}

/// A 2×2 PNG.
fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(2, 2, |x, y| image::Rgba(if (x + y) % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// The reference scene: a cube with two materials at (1, 2, 3), an octahedron named "Octa"
/// under a rotated, scaled empty, a camera, a hidden cone, an object in a child collection that
/// the view layer excludes, one with modifiers, and a collection instance of the octahedron.
fn scene(generation: Gen, ptr: usize, little: bool) -> Vec<u8> {
    let mut w = Writer::new(generation, ptr, little);
    let png_addr = w.alloc();
    let packed = w.alloc();
    let image = w.id(b"IM", "Image", "IMChecker", vec![("source", I(1)), ("packedfile", Ptr(packed))]);
    w.data(packed, "PackedFile", &[vec![("size", I(png().len() as i64)), ("data", Ptr(png_addr))]]);
    w.raw(png_addr, &png());
    let file_image = w.id(b"IM", "Image", "IMWood", vec![("source", I(1)), ("name", Str("//textures/wood.png"))]);
    let red = write_material(&mut w, "MARed", [1.0, 0.0, 0.0, 1.0], 0);
    let blue = write_material(&mut w, "MABlue", [0.0, 0.0, 1.0, 1.0], 0);
    let textured = write_material(&mut w, "MATextured", [0.5, 0.5, 0.5, 1.0], image);
    let wood = write_material(&mut w, "MAWood", [1.0, 1.0, 1.0, 1.0], file_image);
    let cube_me = write_mesh(&mut w, generation, &MeshSpec::cube("MECube", vec![red, blue]));
    let octa_me = write_mesh(&mut w, generation, &MeshSpec { material_ptrs: vec![blue], ..MeshSpec::octahedron("MEOcta") });
    let plank_me = write_mesh(&mut w, generation, &MeshSpec::cube("MEPlank", vec![wood]));
    let camera_data = w.id(b"CA", "Camera", "CACam", vec![("lens", F(50.0)), ("sensor_x", F(36.0)), ("sensor_y", F(24.0))]);
    let cube = write_object(&mut w, &ObjectSpec { loc: [1.0, 2.0, 3.0], ..ObjectSpec::new("OBCube", OB_MESH, cube_me) });
    let rig = write_object(
        &mut w,
        &ObjectSpec { loc: [0.0, 0.0, 1.0], rot: [0.0, 0.0, std::f32::consts::FRAC_PI_2], scale: [2.0; 3], ..ObjectSpec::new("OBRig", OB_EMPTY, 0) },
    );
    let octa = write_object(
        &mut w,
        &ObjectSpec {
            loc: [3.0, 0.0, 0.0],
            parent: rig,
            modifiers: vec!["Subdivision"],
            material_override: textured,
            ..ObjectSpec::new("OBOcta", OB_MESH, octa_me)
        },
    );
    debug_assert!(octa > 0);
    let hidden = write_object(&mut w, &ObjectSpec { hidden: true, ..ObjectSpec::new("OBHidden", OB_MESH, cube_me) });
    let excluded = write_object(&mut w, &ObjectSpec::new("OBExcluded", OB_MESH, cube_me));
    let plank = write_object(&mut w, &ObjectSpec::new("OBPlank", OB_MESH, plank_me));
    let camera = write_object(&mut w, &ObjectSpec { loc: [0.0, -10.0, 5.0], rot: [1.1, 0.0, 0.0], ..ObjectSpec::new("OBCam", OB_CAMERA, camera_data) });
    // Collections: master > Props (octa, plank) and master > Old (excluded); "Lib" holds the
    // octahedron again, instanced by an empty at x = 10.
    let lib = {
        let objs = w.alloc();
        let a = w.id(b"GR", "Collection", "GRLib", vec![("gobject", Writer::listbase(objs))]);
        w.data(objs, "CollectionObject", &[vec![("ob", Ptr(octa))]]);
        a
    };
    let instance = write_object(&mut w, &ObjectSpec { loc: [10.0, 0.0, 0.0], instance: lib, ..ObjectSpec::new("OBInstance", OB_EMPTY, 0) });
    let props = {
        let objs = w.alloc();
        let a = w.id(b"GR", "Collection", "GRProps", vec![("gobject", Writer::listbase(objs))]);
        w.list_at(objs, "CollectionObject", vec![vec![("ob", Ptr(octa))], vec![("ob", Ptr(plank))]]);
        a
    };
    let old = {
        let objs = w.alloc();
        let a = w.id(b"GR", "Collection", "GROld", vec![("gobject", Writer::listbase(objs))]);
        w.list_at(objs, "CollectionObject", vec![vec![("ob", Ptr(excluded))]]);
        a
    };
    // The master collection is embedded in the scene: written as the scene's data, after it.
    let (master, master_objs, master_children) = (w.alloc(), w.alloc(), w.alloc());
    let layer_addr = w.alloc();
    let scene_vals = if generation == Gen::Faces1 {
        let bases = w.alloc();
        let objs = [cube, rig, octa, hidden, plank, camera];
        let v = vec![
            ("base", Writer::listbase(bases)),
            ("r", Sub(vec![("xsch", I(1920)), ("ysch", I(1080)), ("xasp", F(1.0)), ("yasp", F(1.0))])),
            ("unit", Sub(vec![("scale_length", F(1.0))])),
        ];
        let items: Vec<Vec<(&str, Val)>> = objs.iter().map(|o| vec![("object", Ptr(*o))]).collect();
        let sc = w.id(b"SC", "Scene", "SCScene", v);
        // The base list lives in the scene's data.
        w.list_at(bases, "Base", items);
        let _ = layer_addr;
        (sc, 0)
    } else {
        let v = vec![
            ("master_collection", Ptr(master)),
            ("view_layers", Writer::listbase(layer_addr)),
            ("r", Sub(vec![("xsch", I(1920)), ("ysch", I(1080)), ("xasp", F(1.0)), ("yasp", F(1.0))])),
            ("unit", Sub(vec![("scale_length", F(1.0))])),
        ];
        let sc = w.id(b"SC", "Scene", "SCScene", v);
        w.data(
            master,
            "Collection",
            &[vec![("gobject", Writer::listbase(master_objs)), ("children", Writer::listbase(master_children)), ("id", Sub(vec![("name", Str("GRMaster"))]))]],
        );
        w.list_at(
            master_objs,
            "CollectionObject",
            vec![vec![("ob", Ptr(cube))], vec![("ob", Ptr(hidden))], vec![("ob", Ptr(camera))], vec![("ob", Ptr(instance))]],
        );
        w.list_at(master_children, "CollectionChild", vec![vec![("collection", Ptr(props))], vec![("collection", Ptr(old))]]);
        let lc_old = w.list("LayerCollection", vec![vec![("collection", Ptr(old)), ("flag", I(LAYER_COLLECTION_EXCLUDE))]]);
        let lc_master = w.list("LayerCollection", vec![vec![("collection", Ptr(master)), ("layer_collections", Writer::listbase(lc_old))]]);
        w.data(layer_addr, "ViewLayer", &[vec![("layer_collections", Writer::listbase(lc_master))]]);
        (sc, layer_addr)
    };
    let glob = w.struct_bytes("FileGlobal", &[("curscene", Ptr(scene_vals.0)), ("cur_view_layer", Ptr(scene_vals.1))]);
    let si = w.struct_by_name["FileGlobal"];
    w.block(b"GLOB", si, 0x10, 1, &glob);
    w.finish()
}

fn import(bytes: &[u8]) -> super::super::ImportedMesh {
    super::super::import_mesh_bytes("scene.blend", bytes).unwrap()
}

fn part<'a>(m: &'a super::super::ImportedMesh, name: &str) -> Vec<&'a texturecraft_geom::SubMesh> {
    m.mesh.submeshes.iter().filter(|s| s.name == name).collect()
}

fn positions_of(m: &super::super::ImportedMesh, sub: &texturecraft_geom::SubMesh) -> Vec<Vec3> {
    m.mesh.indices[sub.start as usize..(sub.start + sub.count) as usize].iter().map(|i| m.mesh.positions[*i as usize]).collect()
}

fn check_scene(bytes: &[u8], generation: Gen) {
    let m = import(bytes);
    let names: Vec<&str> = m.mesh.submeshes.iter().map(|s| s.name.as_str()).collect();
    // Hidden and excluded objects are gone; the instance brings a second Octa as its own part.
    assert!(!names.contains(&"Hidden") && !names.contains(&"Excluded"), "{names:?}");
    assert_eq!(part(&m, "Octa").len(), 1, "{names:?}");
    assert_eq!(part(&m, "Instance/Octa").len(), 1, "{names:?}");
    // The cube: 12 triangles over materials Red and Blue, at Blender (1, 2, 3) → Y up (1, 3, -2).
    let cube: Vec<_> = part(&m, "Cube");
    assert_eq!(cube.iter().map(|s| s.count / 3).sum::<u32>(), 12);
    let cube_mats: Vec<&str> = cube.iter().map(|s| m.mesh.materials[s.material].as_str()).collect();
    assert_eq!(cube_mats, ["Red", "Blue"], "{names:?}");
    let ps = positions_of(&m, cube[0]);
    assert!(ps.iter().all(|p| (p.x - 1.0).abs() <= 1.001 && (p.y - 3.0).abs() <= 1.001 && (p.z + 2.0).abs() <= 1.001), "{ps:?}");
    // Flat shading: every cube normal is an axis.
    for t in 0..cube[0].count as usize / 3 {
        let i = m.mesh.indices[cube[0].start as usize + t * 3] as usize;
        let n = m.mesh.normals[i];
        assert!(n.abs().max_element() > 0.999, "cube normal {n}");
    }
    assert!(m.report.uvs_generated, "{generation:?}: the octahedron has no UVs");
    // The octahedron under the rig: local (3, 0, 0) → rig scales by 2, rotates 90° about Z,
    // lifts by 1: Blender (0, 6, 1) → Y up (0, 1, -6). Smooth: normals point away from the center.
    let (first_octa, instanced) = (part(&m, "Octa")[0], part(&m, "Instance/Octa")[0]);
    let ps = positions_of(&m, first_octa);
    let center = ps.iter().sum::<Vec3>() / ps.len() as f32;
    assert!(center.abs_diff_eq(Vec3::new(0.0, 1.0, -6.0), 1e-3), "octa center {center}");
    for &i in &m.mesh.indices[first_octa.start as usize..(first_octa.start + first_octa.count) as usize] {
        let (p, n) = (m.mesh.positions[i as usize], m.mesh.normals[i as usize]);
        assert!(n.abs_diff_eq((p - center).normalize(), 1e-3), "smooth normal {n} at {p}");
    }
    // Its material comes from the object override.
    assert_eq!(m.mesh.materials[first_octa.material], "Textured");
    // The instance of Lib at x = 10 re-places the octahedron: Blender (10, 6, 1) → (10, 1, -6).
    let ps = positions_of(&m, instanced);
    let center = ps.iter().sum::<Vec3>() / ps.len() as f32;
    assert!(center.abs_diff_eq(Vec3::new(10.0, 1.0, -6.0), 1e-3), "instanced center {center}");
    // The camera at Blender (0, -10, 5) → (0, 5, 10), looking at the scene, horizontal fit.
    assert_eq!(m.cameras.len(), 1);
    let cam = &m.cameras[0];
    assert_eq!(cam.name, "Cam");
    let expected_fov = 2.0 * ((18.0f32 / 50.0).atan().tan() / (16.0 / 9.0)).atan().to_degrees();
    assert!((cam.fov - expected_fov).abs() < 0.1, "fov {} vs {expected_fov}", cam.fov);
    assert!(!cam.ortho);
    assert!(m.report.warnings.iter().any(|w| w.contains("Octa (Subdivision)")), "{:?}", m.report.warnings);
}

#[test]
fn blender_5_scene_imports() {
    check_scene(&scene(Gen::Attributes5, 8, true), Gen::Attributes5);
}

#[test]
fn blender_4_attribute_layers_import() {
    check_scene(&scene(Gen::Attributes4, 8, true), Gen::Attributes4);
}

#[test]
fn blender_2_8_structs_import_in_every_container_layout() {
    check_scene(&scene(Gen::Structs2, 8, true), Gen::Structs2);
    check_scene(&scene(Gen::Structs2, 4, true), Gen::Structs2);
    check_scene(&scene(Gen::Structs2, 8, false), Gen::Structs2);
    check_scene(&scene(Gen::Structs2, 4, false), Gen::Structs2);
}

#[test]
fn legacy_mface_files_import_through_the_scene_base_list() {
    let m = import(&scene(Gen::Faces1, 4, true));
    let names: Vec<&str> = m.mesh.submeshes.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Cube") && names.contains(&"Octa") && names.contains(&"Plank") && !names.contains(&"Hidden"), "{names:?}");
    let cube = part(&m, "Cube");
    assert_eq!(cube.iter().map(|s| s.count / 3).sum::<u32>(), 12);
    // The octahedron has no UVs, so the whole mesh is unwrapped (as for every scene format).
    assert!(m.report.uvs_generated);
    assert!(m.report.warnings.iter().any(|w| w.contains("some objects have no UVs")), "{:?}", m.report.warnings);
}

#[test]
fn the_same_geometry_comes_out_of_every_generation() {
    let reference = import(&scene(Gen::Attributes5, 8, true));
    for generation in [Gen::Attributes4, Gen::Structs2] {
        let m = import(&scene(generation, 8, true));
        assert_eq!(m.mesh.positions.len(), reference.mesh.positions.len());
        assert_eq!(m.mesh.indices.len(), reference.mesh.indices.len());
        let sum = |m: &super::super::ImportedMesh| m.mesh.positions.iter().sum::<Vec3>();
        assert!(sum(&m).abs_diff_eq(sum(&reference), 1e-3));
        let uv_sum = |m: &super::super::ImportedMesh| m.mesh.uvs.iter().sum::<Vec2>();
        assert!(uv_sum(&m).abs_diff_eq(uv_sum(&reference), 1e-3), "{} vs {}", uv_sum(&m), uv_sum(&reference));
    }
}

#[test]
fn sharp_edges_and_auto_smooth_split_normals() {
    // A smooth cube with every edge sharp shades flat.
    let mut w = Writer::new(Gen::Attributes5, 8, true);
    let mut spec = MeshSpec::cube("MECube", vec![]);
    spec.smooth = true;
    spec.sharp_edges = spec.edges();
    let me = write_mesh(&mut w, Gen::Attributes5, &spec);
    let ob = write_object(&mut w, &ObjectSpec::new("OBCube", OB_MESH, me));
    let m = import(&finish_with_objects(w, &[ob]));
    assert!(m.mesh.normals.iter().all(|n| n.abs().max_element() > 0.999), "{:?}", m.mesh.normals);
    assert_eq!(m.mesh.positions.len(), 24);

    // Smooth with no sharp edges: one normal per corner position (8 vertices).
    let mut w = Writer::new(Gen::Structs2, 8, true);
    let mut spec = MeshSpec::cube("MECube", vec![]);
    spec.smooth = true;
    let me = write_mesh(&mut w, Gen::Structs2, &spec);
    let ob = write_object(&mut w, &ObjectSpec::new("OBCube", OB_MESH, me));
    let m = import(&finish_with_objects(w, &[ob]));
    assert!(m.mesh.normals.iter().all(|n| (n.abs() - Vec3::splat(1.0 / 3f32.sqrt())).abs().max_element() < 1e-3), "{:?}", m.mesh.normals);

    // Legacy auto smooth at 30°: the cube's 90° edges become sharp again.
    let mut w = Writer::new(Gen::Structs2, 8, true);
    let mut spec = MeshSpec::cube("MECube", vec![]);
    spec.smooth = true;
    spec.autosmooth = Some(30.0);
    let me = write_mesh(&mut w, Gen::Structs2, &spec);
    let ob = write_object(&mut w, &ObjectSpec::new("OBCube", OB_MESH, me));
    let m = import(&finish_with_objects(w, &[ob]));
    assert!(m.mesh.normals.iter().all(|n| n.abs().max_element() > 0.999), "{:?}", m.mesh.normals);
}

/// Finish a file whose scene shows `objects` in its (embedded) master collection.
fn finish_with_objects(w: Writer, objects: &[u64]) -> Vec<u8> {
    finish_with_objects_scaled(w, objects, 1.0)
}

fn finish_with_objects_scaled(mut w: Writer, objects: &[u64], unit_scale: f32) -> Vec<u8> {
    let (master, objs) = (w.alloc(), if objects.is_empty() { 0 } else { w.alloc() });
    let sc = w.id(b"SC", "Scene", "SCScene", vec![("master_collection", Ptr(master)), ("unit", Sub(vec![("scale_length", F(f64::from(unit_scale)))]))]);
    w.data(master, "Collection", &[vec![("gobject", Writer::listbase(objs))]]);
    if objs != 0 {
        w.list_at(objs, "CollectionObject", objects.iter().map(|o| vec![("ob", Ptr(*o))]).collect());
    }
    let glob = w.struct_bytes("FileGlobal", &[("curscene", Ptr(sc))]);
    let si = w.struct_by_name["FileGlobal"];
    w.block(b"GLOB", si, 0x10, 1, &glob);
    w.finish()
}

#[test]
fn vertex_colors_come_along_linearized() {
    for generation in [Gen::Attributes5, Gen::Attributes4, Gen::Faces1] {
        let mut w = Writer::new(generation, 8, true);
        let mut spec = MeshSpec::cube("MECube", vec![]);
        spec.colors = Some((0..24).map(|c| if c < 12 { [255, 0, 0, 255] } else { [128, 128, 128, 255] }).collect());
        let me = write_mesh(&mut w, generation, &spec);
        let ob = write_object(&mut w, &ObjectSpec::new("OBCube", OB_MESH, me));
        let m = import(&finish_with_objects(w, &[ob]));
        assert_eq!(m.mesh.colors.len(), m.mesh.positions.len(), "{generation:?}");
        let grey = texturecraft_texel::srgb_to_linear(128.0 / 255.0);
        assert!(
            m.mesh.colors.iter().all(|c| c.abs_diff_eq(Vec4::new(1.0, 0.0, 0.0, 1.0), 1e-5) || c.abs_diff_eq(Vec4::new(grey, grey, grey, 1.0), 1e-5)),
            "{:?}",
            m.mesh.colors
        );
    }
}

impl std::fmt::Debug for Gen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Gen::Attributes5 => "5",
                Gen::Attributes4 => "4",
                Gen::Structs2 => "2.8",
                Gen::Faces1 => "2.4",
            }
        )
    }
}

#[test]
fn unit_scale_and_mirrored_objects() {
    let mut w = Writer::new(Gen::Attributes5, 8, true);
    let me = write_mesh(&mut w, Gen::Attributes5, &MeshSpec::cube("MECube", vec![]));
    let ob = write_object(&mut w, &ObjectSpec { scale: [-1.0, 1.0, 1.0], ..ObjectSpec::new("OBCube", OB_MESH, me) });
    let m = import(&finish_with_objects_scaled(w, &[ob], 0.01));
    // UVs keep their layout with v flipped: Blender's v in 0..0.75 becomes 0.25..1.
    assert!(!m.report.uvs_generated);
    let (lo, hi) = m.mesh.uvs.iter().fold((Vec2::splat(9.0), Vec2::splat(-9.0)), |(lo, hi), uv| (lo.min(*uv), hi.max(*uv)));
    assert!(lo.abs_diff_eq(Vec2::new(0.0, 0.25), 1e-5) && hi.abs_diff_eq(Vec2::new(0.75, 1.0), 1e-5), "{lo} {hi}");
    // Centimeters: the cube is 2 cm wide.
    let (lo, hi) = m.mesh.bounds();
    assert!((hi - lo).abs_diff_eq(Vec3::splat(0.02), 1e-5), "{lo} {hi}");
    // Mirrored in X: the winding was flipped, so normals still point outward.
    for t in 0..m.mesh.triangle_count() {
        let [a, b, c] = m.mesh.tri(t).unwrap();
        let p = [m.mesh.positions[a], m.mesh.positions[b], m.mesh.positions[c]];
        let geometric = (p[1] - p[0]).cross(p[2] - p[0]);
        let centroid = (p[0] + p[1] + p[2]) / 3.0;
        assert!(geometric.dot(centroid) > 0.0, "triangle {t} faces inward");
        assert!(m.mesh.normals[a].dot(centroid) > 0.0);
    }
}

#[test]
fn materials_bring_base_colors_and_packed_textures() {
    let high = super::super::import_high_poly_bytes("scene.blend", &scene(Gen::Attributes5, 8, true)).unwrap();
    let find = |name: &str| high.mesh.materials.iter().position(|m| m == name).and_then(|i| high.base_color[i].as_ref());
    let red = find("Red").expect("Red");
    assert!(red.factor.abs_diff_eq(Vec4::new(1.0, 0.0, 0.0, 1.0), 1e-6));
    assert_eq!(red.width, 0);
    let textured = find("Textured").expect("Textured");
    assert_eq!((textured.width, textured.height), (2, 2));
    assert!(textured.factor.abs_diff_eq(Vec4::ONE, 1e-6));
    assert_eq!(textured.texels[0], [255, 0, 0, 255]);
    // The wood texture is a file beside the .blend: not packaged here, so white.
    assert!(find("Wood").is_none());
}

#[test]
fn self_contained_mesh_packages_texture_files_beside_the_blend() {
    let bytes = scene(Gen::Attributes5, 8, true);
    let mut asked = Vec::new();
    let file = super::super::self_contained_mesh("scene.blend", bytes.clone(), &mut |p| {
        asked.push(p.to_string());
        (p == "textures/wood.png").then(png)
    })
    .unwrap();
    assert_eq!(asked, ["textures/wood.png"]);
    assert_eq!(file.name, "scene.blend");
    assert!(file.bytes.starts_with(ZIP_MAGIC));
    assert!(file.warnings.is_empty(), "{:?}", file.warnings);
    let high = super::super::import_high_poly_bytes(&file.name, &file.bytes).unwrap();
    let wood = high.mesh.materials.iter().position(|m| m == "Wood").and_then(|i| high.base_color[i].as_ref()).expect("Wood");
    assert_eq!((wood.width, wood.height), (2, 2));
    // The package still imports as a regular mesh.
    let m = super::super::import_mesh_bytes(&file.name, &file.bytes).unwrap();
    assert_eq!(m.mesh.triangle_count(), import(&bytes).mesh.triangle_count());
    // Missing files are reported, and a file with none to fold stays as it is.
    let file = super::super::self_contained_mesh("scene.blend", bytes.clone(), &mut |_| None).unwrap();
    assert_eq!(file.bytes, bytes);
    assert_eq!(file.warnings, ["scene.blend: textures/wood.png was not found next to the mesh"]);
}

#[test]
fn data_block_addresses_are_resolved_per_id() {
    // Blender 5 gives the data of different IDs the same addresses: two cubes whose position
    // blocks share an address must each get their own.
    let mut w = Writer::new(Gen::Structs2, 8, true);
    let a = write_mesh(&mut w, Gen::Structs2, &MeshSpec::cube("MEA", vec![]));
    let saved = w.next_addr;
    w.next_addr = 0x1000;
    let mut shifted = MeshSpec::cube("MEB", vec![]);
    shifted.positions.iter_mut().for_each(|p| *p += 10.0);
    let b = write_mesh(&mut w, Gen::Structs2, &shifted);
    assert!(w.next_addr <= saved);
    w.next_addr = saved;
    let oa = write_object(&mut w, &ObjectSpec::new("OBA", OB_MESH, a));
    let ob = write_object(&mut w, &ObjectSpec::new("OBB", OB_MESH, b));
    let m = import(&finish_with_objects(w, &[oa, ob]));
    let (pa, pb) = (part(&m, "A"), part(&m, "B"));
    let mean = |ps: Vec<Vec3>| ps.iter().sum::<Vec3>() / ps.len() as f32;
    let (ca, cb) = (mean(positions_of(&m, pa[0])), mean(positions_of(&m, pb[0])));
    assert!(ca.abs_diff_eq(Vec3::ZERO, 1e-4), "{ca}");
    assert!(cb.abs_diff_eq(Vec3::new(10.0, 10.0, -10.0), 1e-4), "{cb}");
}

/// A zstd stream of raw (stored) blocks: one frame per chunk, then a skippable frame like
/// Blender's seek table.
fn zstd_stored(data: &[u8], chunk: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for frame in data.chunks(chunk.max(1)) {
        out.extend(0xFD2F_B528u32.to_le_bytes());
        out.push(0xE0); // single segment, 8-byte content size
        out.extend((frame.len() as u64).to_le_bytes());
        let header = 1 | ((frame.len() as u32) << 3); // last block, raw
        out.extend(&header.to_le_bytes()[..3]);
        out.extend(frame);
    }
    out.extend(0x184D_2A5Eu32.to_le_bytes());
    out.extend(4u32.to_le_bytes());
    out.extend([1, 2, 3, 4]);
    out
}

#[test]
fn compressed_saves_are_inflated() {
    let plain = scene(Gen::Attributes5, 8, true);
    let reference = import(&plain);
    let zstd = zstd_stored(&plain, 4000);
    assert!(!zstd.starts_with(b"BLENDER"));
    let m = import(&zstd);
    assert_eq!(m.mesh.positions, reference.mesh.positions);
    assert_eq!(m.mesh.indices, reference.mesh.indices);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &plain).unwrap();
    let m = import(&gz.finish().unwrap());
    assert_eq!(m.mesh.positions, reference.mesh.positions);
    // Junk is refused, not unpacked.
    let err = super::super::import_mesh_bytes("x.blend", b"not a blend file at all").unwrap_err().to_string();
    assert!(err.contains("Blender"), "{err}");
}

#[test]
fn newer_formats_and_empty_scenes_are_refused_clearly() {
    let mut bytes = scene(Gen::Attributes5, 8, true);
    bytes[10..12].copy_from_slice(b"02");
    let err = super::super::import_mesh_bytes("x.blend", &bytes).unwrap_err().to_string();
    assert!(err.contains("format 2"), "{err}");
    let w = Writer::new(Gen::Attributes5, 8, true);
    let err = super::super::import_mesh_bytes("x.blend", &finish_with_objects(w, &[])).unwrap_err().to_string();
    assert!(err.contains("no visible mesh objects"), "{err}");
}

#[test]
fn a_file_without_a_scene_imports_its_meshes() {
    let mut w = Writer::new(Gen::Attributes4, 8, true);
    write_mesh(&mut w, Gen::Attributes4, &MeshSpec::cube("MELoose", vec![]));
    let m = import(&w.finish());
    assert_eq!(m.mesh.submeshes[0].name, "Loose");
    assert_eq!(m.mesh.triangle_count(), 12);
}

#[test]
fn cut_short_files_fail_without_panicking() {
    for (generation, ptr, little) in [(Gen::Attributes5, 8, true), (Gen::Structs2, 4, false)] {
        let bytes = scene(generation, ptr, little);
        let mut cut = 0;
        while cut < bytes.len() {
            let _ = super::super::import_mesh_bytes("x.blend", &bytes[..cut]);
            cut += 37;
        }
        // Flipped bytes everywhere a block header lives.
        let mut corrupt = bytes.clone();
        for i in (17..corrupt.len()).step_by(101) {
            corrupt[i] ^= 0xff;
        }
        let _ = super::super::import_mesh_bytes("x.blend", &corrupt);
    }
}

/// Real files: every `.blend` in `TC_BLEND_FILES` must import; a `scene.json` beside them
/// (written by the fixture script) gives triangle counts to compare.
#[test]
#[ignore]
fn real_blender_files_import() {
    let dir = std::env::var("TC_BLEND_FILES").expect("TC_BLEND_FILES");
    let truth: Option<serde_json::Value> = std::fs::read(std::path::Path::new(&dir).join("scene.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("blend") {
            continue;
        }
        seen += 1;
        let m = super::super::import_mesh(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!(
            "{}: {} triangles, parts {:?}, materials {:?}, cameras {}, warnings {:?}",
            path.display(),
            m.mesh.triangle_count(),
            m.mesh.submeshes.iter().map(|s| &s.name).collect::<Vec<_>>(),
            m.mesh.materials,
            m.cameras.len(),
            m.report.warnings
        );
        if let Some(t) = &truth {
            let expected: u64 =
                t["objects"].as_object().unwrap().values().filter(|o| !o["hidden"].as_bool().unwrap_or(false)).map(|o| o["tris"].as_u64().unwrap()).sum();
            assert_eq!(m.mesh.triangle_count() as u64, expected, "{}", path.display());
            for (name, o) in t["objects"].as_object().unwrap() {
                let parts = part(&m, name);
                assert_eq!(!parts.is_empty(), !o["hidden"].as_bool().unwrap_or(false), "{name}");
                if let Some(p) = parts.first() {
                    let want: Vec<&str> = o["materials"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
                    let got = &m.mesh.materials[p.material];
                    assert!(want.contains(&got.as_str()) || want.is_empty() && got == "Default", "{name}: {got} not in {want:?}");
                    // The first face's first corner: Blender (x, y, z) → (x, z, -y).
                    let fw = &o["first_face_world"][0];
                    let expect = Vec3::new(fw[0].as_f64().unwrap() as f32, fw[2].as_f64().unwrap() as f32, -fw[1].as_f64().unwrap() as f32);
                    let hit = m.mesh.positions.iter().any(|p| p.abs_diff_eq(expect, 1e-3));
                    assert!(hit, "{name}: no vertex at {expect}");
                    for modifier in o["modifiers"].as_array().unwrap().iter().filter_map(|v| v.as_str()) {
                        assert!(
                            m.report.warnings.iter().any(|w| w.contains(&format!("{name} ({modifier}"))),
                            "{name}: {modifier} not reported in {:?}",
                            m.report.warnings
                        );
                    }
                }
            }
        }
    }
    assert!(seen > 0, "no .blend files in {dir}");
}
