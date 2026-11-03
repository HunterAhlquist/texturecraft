//! The `.blend` container: the file header (the legacy 12-byte one and Blender 5's 17-byte one),
//! block headers (20, 24 or 32 bytes), the SDNA catalog the file describes its own struct layouts
//! with, typed field access by name, and pointer resolution. Pointers to ID blocks (objects,
//! meshes, materials, …) are file-wide; pointers to data blocks resolve among the data blocks
//! written right after their ID, exactly as Blender reads them (Blender 5 writes hashed
//! "stable" addresses that repeat across IDs). Compressed saves (gzip up to 2.93, zstd since
//! 3.0) are inflated first.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read as _;

use crate::{FormatError, Result, malformed};

pub(super) const KIND: &str = "Blender file";
const MAGIC: &[u8] = b"BLENDER";
const GZIP_MAGIC: &[u8] = &[0x1f, 0x8b];
const ZSTD_MAGIC: u32 = 0xFD2F_B528;
const ZSTD_SKIPPABLE: u32 = 0x184D_2A50;
/// Block header sizes: legacy 32-bit, legacy 64-bit, Blender 5.
const BHEAD4: usize = 20;
const BHEAD8: usize = 24;
const BHEAD_LARGE: usize = 32;
/// Longest linked list followed (bounds lists that loop back on themselves).
const MAX_LIST: usize = 1 << 20;
/// Most struct fields read for one struct definition and most SDNA entries of one kind.
const MAX_SDNA_ENTRIES: usize = 1 << 20;

pub(super) const CODE_DATA: [u8; 4] = *b"DATA";
const CODE_DNA: [u8; 4] = *b"DNA1";
const CODE_END: [u8; 4] = *b"ENDB";
/// Blocks that are neither IDs nor the data of one.
const NON_ID_CODES: [[u8; 4]; 6] = [CODE_DATA, CODE_DNA, CODE_END, *b"GLOB", *b"REND", *b"TEST"];

/// One block of the file.
pub(super) struct Block {
    pub code: [u8; 4],
    /// Index into [`Dna::structs`] of the struct the block holds (0 for raw data).
    pub sdna: usize,
    /// The address the block's data had when it was written: what pointers refer to.
    pub old: u64,
    /// How many structs the block holds.
    pub count: usize,
    pub start: usize,
    pub end: usize,
    /// Index of the ID block this data block belongs to (an ID block owns itself).
    pub owner: usize,
}

impl Block {
    pub fn is_id(&self) -> bool {
        !NON_ID_CODES.contains(&self.code)
    }
}

/// A field of an SDNA struct.
#[derive(Clone)]
pub(super) struct Field {
    /// Name without pointer stars and array dimensions.
    pub name: String,
    /// Index into [`Dna::types`].
    pub ty: usize,
    pub offset: usize,
    /// Elements of an array field (1 for scalars); pointer arrays count pointers.
    pub count: usize,
    pub is_pointer: bool,
    /// Size of the whole field in bytes.
    pub size: usize,
}

pub(super) struct Struct {
    pub fields: Vec<Field>,
    pub size: usize,
}

/// The file's struct catalog.
pub(super) struct Dna {
    pub types: Vec<String>,
    pub structs: Vec<Struct>,
    by_name: HashMap<String, usize>,
}

impl Dna {
    pub fn struct_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// Field `name` of struct `st` and its offset: a direct field, or one of the struct a
    /// first field embeds (Blender subtypes `ModifierData` and `ID` this way, so `next` and
    /// `name` of a `SubsurfModifierData` are those of its `modifier`).
    pub fn field(&self, st: usize, name: &str) -> Option<(&Field, usize)> {
        self.field_at(st, name, 0)
    }

    fn field_at(&self, st: usize, name: &str, depth: usize) -> Option<(&Field, usize)> {
        let s = self.structs.get(st)?;
        if let Some(f) = s.fields.iter().find(|f| f.name == name) {
            return Some((f, f.offset));
        }
        let first = s.fields.first().filter(|f| !f.is_pointer && f.count == 1)?;
        if depth >= 4 {
            return None;
        }
        let (f, off) = self.field_at(self.struct_index(self.type_name(first.ty))?, name, depth + 1)?;
        Some((f, first.offset.checked_add(off)?))
    }

    fn type_name(&self, ty: usize) -> &str {
        self.types.get(ty).map_or("", String::as_str)
    }
}

/// A parsed `.blend` file.
pub(super) struct BlendFile<'a> {
    bytes: Cow<'a, [u8]>,
    pub ptr_size: usize,
    pub little: bool,
    /// Blender version × 100 (`305` is 3.5).
    pub version: u32,
    pub dna: Dna,
    pub blocks: Vec<Block>,
    ids: HashMap<u64, usize>,
    data: HashMap<(usize, u64), usize>,
}

/// Inflate a compressed save (gzip or zstd), refusing results over `cap` bytes; uncompressed
/// files come back as they are.
pub(super) fn decompress(bytes: &[u8], cap: u64) -> Result<Cow<'_, [u8]>> {
    if bytes.starts_with(MAGIC) {
        return Ok(Cow::Borrowed(bytes));
    }
    let too_large = || FormatError::TooLarge(format!("the Blender file inflates to more than {cap} bytes"));
    if bytes.starts_with(GZIP_MAGIC) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes).take(cap.saturating_add(1)).read_to_end(&mut out).map_err(|e| malformed(KIND, format!("gzip: {e}")))?;
        if out.len() as u64 > cap {
            return Err(too_large());
        }
        return Ok(Cow::Owned(out));
    }
    let magic = bytes.get(..4).map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
    if magic != Some(ZSTD_MAGIC) {
        return Err(malformed(KIND, "not a Blender file (no BLENDER, gzip or zstd signature)"));
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(head) = bytes.get(pos..pos.saturating_add(8)) {
        let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        if magic & 0xFFFF_FFF0 == ZSTD_SKIPPABLE {
            // The seek table Blender appends.
            let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            pos = pos.saturating_add(8).saturating_add(len);
            continue;
        }
        if magic != ZSTD_MAGIC {
            return Err(malformed(KIND, "unexpected data between zstd frames"));
        }
        let mut cursor = std::io::Cursor::new(bytes.get(pos..).unwrap_or_default());
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut cursor).map_err(|e| malformed(KIND, format!("zstd: {e}")))?;
        let room = cap.saturating_add(1).saturating_sub(out.len() as u64);
        (&mut decoder).take(room).read_to_end(&mut out).map_err(|e| malformed(KIND, format!("zstd: {e}")))?;
        if out.len() as u64 > cap {
            return Err(too_large());
        }
        drop(decoder);
        let consumed = usize::try_from(cursor.position()).unwrap_or(usize::MAX);
        if consumed == 0 {
            return Err(malformed(KIND, "an empty zstd frame"));
        }
        pos = pos.saturating_add(consumed);
    }
    if !out.starts_with(MAGIC) {
        return Err(malformed(KIND, "the compressed data is not a Blender file"));
    }
    Ok(Cow::Owned(out))
}

/// Parsed file header.
struct Header {
    ptr_size: usize,
    little: bool,
    version: u32,
    large: bool,
    size: usize,
}

fn digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

fn header(b: &[u8]) -> Result<Header> {
    if !b.starts_with(MAGIC) {
        return Err(malformed(KIND, "not a Blender file (missing BLENDER signature)"));
    }
    let bad = || malformed(KIND, "unreadable file header");
    match b.get(7).copied().ok_or_else(bad)? {
        c @ (b'_' | b'-') => {
            let little = match b.get(8).copied().ok_or_else(bad)? {
                b'v' => true,
                b'V' => false,
                _ => return Err(bad()),
            };
            let version = b.get(9..12).and_then(digits).ok_or_else(bad)?;
            Ok(Header { ptr_size: if c == b'_' { 4 } else { 8 }, little, version, large: false, size: 12 })
        }
        _ => {
            // Blender 5: "BLENDER" + header size + '-' + file format version + 'v' + 4-digit version.
            let size = b.get(7..9).and_then(digits).ok_or_else(bad)?;
            let format = b.get(10..12).and_then(digits).ok_or_else(bad)?;
            if size != 17 || format != 1 {
                return Err(FormatError::Unsupported(format!("Blender file format {format} (header size {size}); this version reads format 1")));
            }
            if b.get(9) != Some(&b'-') || b.get(12) != Some(&b'v') {
                return Err(bad());
            }
            let version = b.get(13..17).and_then(digits).ok_or_else(bad)?;
            Ok(Header { ptr_size: 8, little: true, version, large: true, size: 17 })
        }
    }
}

impl<'a> BlendFile<'a> {
    /// Parse `bytes` (a plain or compressed save), inflating at most `cap` bytes.
    pub fn parse(bytes: &'a [u8], cap: u64) -> Result<BlendFile<'a>> {
        let bytes = decompress(bytes, cap)?;
        let h = header(&bytes)?;
        let mut file = BlendFile {
            bytes,
            ptr_size: h.ptr_size,
            little: h.little,
            version: h.version,
            dna: Dna { types: Vec::new(), structs: Vec::new(), by_name: HashMap::new() },
            blocks: Vec::new(),
            ids: HashMap::new(),
            data: HashMap::new(),
        };
        file.read_blocks(&h)?;
        file.read_dna()?;
        file.index();
        Ok(file)
    }

    fn read_blocks(&mut self, h: &Header) -> Result<()> {
        let b: &[u8] = &self.bytes;
        let head_size = if h.large {
            BHEAD_LARGE
        } else if h.ptr_size == 8 {
            BHEAD8
        } else {
            BHEAD4
        };
        let mut pos = h.size;
        let mut owner = 0usize;
        while pos < b.len() {
            let head = b.get(pos..pos + head_size).ok_or_else(|| malformed(KIND, "a block header runs past the end of the file"))?;
            let code = [head[0], head[1], head[2], head[3]];
            let (sdna, old, len, count) = if h.large {
                (self.i32(head, 4)? as i64, self.u64(head, 8)?, self.i64(head, 16)?, self.i64(head, 24)?)
            } else if h.ptr_size == 8 {
                (self.i32(head, 16)? as i64, self.u64(head, 8)?, self.i32(head, 4)? as i64, self.i32(head, 20)? as i64)
            } else {
                (self.i32(head, 12)? as i64, u64::from(self.u32(head, 8)?), self.i32(head, 4)? as i64, self.i32(head, 16)? as i64)
            };
            let (sdna, len, count) = (usize::try_from(sdna), usize::try_from(len), usize::try_from(count));
            let (Ok(sdna), Ok(len), Ok(count)) = (sdna, len, count) else { return Err(malformed(KIND, "a block header has negative sizes")) };
            let start = pos + head_size;
            let end = start.checked_add(len).filter(|e| *e <= b.len()).ok_or_else(|| malformed(KIND, "a block runs past the end of the file"))?;
            let index = self.blocks.len();
            if !NON_ID_CODES.contains(&code) {
                owner = index;
            }
            self.blocks.push(Block { code, sdna, old, count, start, end, owner: if code == CODE_DATA { owner } else { index } });
            pos = end;
            if code == CODE_END {
                break;
            }
        }
        Ok(())
    }

    fn read_dna(&mut self) -> Result<()> {
        let dna = self.blocks.iter().find(|b| b.code == CODE_DNA).ok_or_else(|| malformed(KIND, "no DNA1 block (struct catalog)"))?;
        let d = self.bytes.get(dna.start..dna.end).unwrap_or_default();
        let bad = |what: &str| malformed(KIND, format!("broken struct catalog: {what}"));
        if !d.starts_with(b"SDNA") {
            return Err(bad("missing SDNA"));
        }
        let mut pos = 4;
        let strings = |pos: &mut usize, tag: &[u8]| -> Result<Vec<String>> {
            if d.get(*pos..*pos + 4) != Some(tag) {
                return Err(bad(&String::from_utf8_lossy(tag)));
            }
            let n = self.u32(d, *pos + 4)? as usize;
            if n > MAX_SDNA_ENTRIES {
                return Err(bad("too many entries"));
            }
            *pos += 8;
            let mut out = Vec::with_capacity(n.min(d.len()));
            for _ in 0..n {
                let rest = d.get(*pos..).ok_or_else(|| bad("names end early"))?;
                let len = rest.iter().position(|&c| c == 0).ok_or_else(|| bad("unterminated name"))?;
                out.push(String::from_utf8_lossy(&rest[..len]).into_owned());
                *pos += len + 1;
            }
            *pos = (*pos + 3) & !3;
            Ok(out)
        };
        let names = strings(&mut pos, b"NAME")?;
        let types = strings(&mut pos, b"TYPE")?;
        if d.get(pos..pos + 4) != Some(b"TLEN") {
            return Err(bad("TLEN"));
        }
        pos += 4;
        let mut tlen = Vec::with_capacity(types.len());
        for _ in 0..types.len() {
            tlen.push(self.u16(d, pos)? as usize);
            pos += 2;
        }
        pos = (pos + 3) & !3;
        if d.get(pos..pos + 4) != Some(b"STRC") {
            return Err(bad("STRC"));
        }
        let n = self.u32(d, pos + 4)? as usize;
        if n > MAX_SDNA_ENTRIES {
            return Err(bad("too many structs"));
        }
        pos += 8;
        let parsed: Vec<(String, usize, bool, bool)> = names.iter().map(|n| parse_name(n)).collect();
        let mut structs = Vec::with_capacity(n.min(d.len() / 4));
        let mut by_name = HashMap::new();
        for _ in 0..n {
            let ty = self.u16(d, pos)? as usize;
            let nfields = self.u16(d, pos + 2)? as usize;
            pos += 4;
            if nfields > MAX_SDNA_ENTRIES {
                return Err(bad("too many fields"));
            }
            let mut fields = Vec::with_capacity(nfields.min(d.len() / 4));
            let mut offset = 0usize;
            for _ in 0..nfields {
                let fty = self.u16(d, pos)? as usize;
                let fname = self.u16(d, pos + 2)? as usize;
                pos += 4;
                let (name, count, is_pointer, is_fn) = parsed.get(fname).cloned().ok_or_else(|| bad("field name index"))?;
                let base = if is_pointer || is_fn { self.ptr_size } else { *tlen.get(fty).ok_or_else(|| bad("field type index"))? };
                let size = base.saturating_mul(count);
                fields.push(Field { name, ty: fty, offset, count, is_pointer, size });
                offset = offset.saturating_add(size);
            }
            let size = *tlen.get(ty).ok_or_else(|| bad("struct type index"))?;
            let name = types.get(ty).cloned().unwrap_or_default();
            by_name.entry(name).or_insert(structs.len());
            structs.push(Struct { fields, size });
        }
        self.dna = Dna { types, structs, by_name };
        Ok(())
    }

    /// Index ID blocks by address and data blocks by (owner, address).
    fn index(&mut self) {
        for (i, b) in self.blocks.iter().enumerate() {
            if b.is_id() {
                self.ids.entry(b.old).or_insert(i);
            } else if b.code == CODE_DATA {
                self.data.entry((b.owner, b.old)).or_insert(i);
            }
        }
    }

    // Raw reads in the file's byte order.
    pub fn u16(&self, b: &[u8], off: usize) -> Result<u16> {
        let s = b.get(off..off + 2).ok_or_else(|| malformed(KIND, "data ends early"))?;
        Ok(if self.little { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    }

    pub fn u32(&self, b: &[u8], off: usize) -> Result<u32> {
        let s = b.get(off..off + 4).ok_or_else(|| malformed(KIND, "data ends early"))?;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if self.little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }

    pub fn i32(&self, b: &[u8], off: usize) -> Result<i32> {
        Ok(self.u32(b, off)? as i32)
    }

    pub fn u64(&self, b: &[u8], off: usize) -> Result<u64> {
        let s = b.get(off..off + 8).ok_or_else(|| malformed(KIND, "data ends early"))?;
        let a = [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]];
        Ok(if self.little { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) })
    }

    pub fn i64(&self, b: &[u8], off: usize) -> Result<i64> {
        Ok(self.u64(b, off)? as i64)
    }

    pub fn f32(&self, b: &[u8], off: usize) -> Result<f32> {
        Ok(f32::from_bits(self.u32(b, off)?))
    }

    pub fn ptr(&self, b: &[u8], off: usize) -> Result<u64> {
        if self.ptr_size == 8 { self.u64(b, off) } else { self.u32(b, off).map(u64::from) }
    }

    pub fn block(&self, index: usize) -> Option<BlockRef<'_>> {
        (index < self.blocks.len()).then_some(BlockRef { file: self, index })
    }

    /// The ID block at `old` (an address written into a pointer field).
    pub fn id_block(&self, old: u64) -> Option<BlockRef<'_>> {
        self.block(*self.ids.get(&old)?)
    }

    /// The data block at `old` among those of the ID block `owner`.
    pub fn data_block(&self, owner: usize, old: u64) -> Option<BlockRef<'_>> {
        if old == 0 {
            return None;
        }
        self.block(*self.data.get(&(owner, old))?)
    }

    /// Every block whose code starts with `code` (`b"OB"` for objects).
    pub fn blocks_with_code<'f>(&'f self, code: &'f [u8; 2]) -> impl Iterator<Item = BlockRef<'f>> + 'f {
        self.blocks.iter().enumerate().filter(move |(_, b)| b.code[..2] == code[..] && b.code[2] == 0).map(move |(i, _)| BlockRef { file: self, index: i })
    }

    pub fn block_with_code(&self, code: &[u8; 4]) -> Option<BlockRef<'_>> {
        self.blocks.iter().position(|b| b.code == *code).and_then(|i| self.block(i))
    }
}

/// `name` of an SDNA field: (base name, element count, is pointer, is function pointer).
fn parse_name(name: &str) -> (String, usize, bool, bool) {
    let is_fn = name.starts_with("(*");
    let s = name.trim_start_matches('(');
    let is_pointer = s.starts_with('*');
    let s = s.trim_start_matches('*');
    let end = s.find(['[', ')']).unwrap_or(s.len());
    let base = s[..end].to_string();
    let mut count = 1usize;
    let mut rest = &s[end..];
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else { break };
        let n: usize = rest[open + 1..open + close].trim().parse().unwrap_or(1);
        count = count.saturating_mul(n);
        rest = &rest[open + close + 1..];
    }
    (base, count, is_pointer, is_fn)
}

/// A block of the file.
#[derive(Clone, Copy)]
pub(super) struct BlockRef<'f> {
    pub file: &'f BlendFile<'f>,
    pub index: usize,
}

impl<'f> BlockRef<'f> {
    fn block(&self) -> &'f Block {
        // The index was checked when the reference was made; an empty fallback keeps this total.
        static EMPTY: Block = Block { code: [0; 4], sdna: 0, old: 0, count: 0, start: 0, end: 0, owner: 0 };
        self.file.blocks.get(self.index).unwrap_or(&EMPTY)
    }

    pub fn code(&self) -> [u8; 4] {
        self.block().code
    }

    pub fn count(&self) -> usize {
        self.block().count
    }

    pub fn bytes(&self) -> &'f [u8] {
        let b = self.block();
        self.file.bytes.get(b.start..b.end).unwrap_or_default()
    }

    /// The `i`-th struct of the block, of the struct the block header declares.
    pub fn view(&self, i: usize) -> Option<View<'f>> {
        self.view_as(self.block().sdna, i)
    }

    /// The `i`-th struct of the block read as struct `st`.
    pub fn view_as(&self, st: usize, i: usize) -> Option<View<'f>> {
        let size = self.file.dna.structs.get(st)?.size;
        let start = i.checked_mul(size)?;
        let data = self.bytes().get(start..start.checked_add(size)?)?;
        Some(View { file: self.file, owner: self.block().owner, st, data, addr: self.block().old.wrapping_add(start as u64) })
    }

    pub fn view_named(&self, name: &str, i: usize) -> Option<View<'f>> {
        self.view_as(self.file.dna.struct_index(name)?, i)
    }

    /// The pointers of a pointer-array block.
    pub fn ptrs(&self) -> Vec<u64> {
        self.bytes().chunks_exact(self.file.ptr_size).filter_map(|c| self.file.ptr(c, 0).ok()).collect()
    }

    /// The block as a NUL-terminated string.
    pub fn string(&self) -> String {
        c_string(self.bytes())
    }

    /// The ID name of an ID block without its two-letter type prefix (`OBCube` → `Cube`).
    pub fn id_name(&self) -> Option<String> {
        let name = self.view(0)?.sub("id")?.string("name")?;
        Some(name.get(2..).unwrap_or("").to_string())
    }

    /// The `Library` block of an ID that comes from another file (its pointer resolves to one
    /// only when the file holds that library's ID).
    pub fn is_linked(&self) -> bool {
        self.view(0).and_then(|v| v.sub("id")).and_then(|id| id.ptr("lib")).is_some_and(|p| p != 0)
    }
}

fn c_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// One struct instance in a block.
#[derive(Clone, Copy)]
pub(super) struct View<'f> {
    pub file: &'f BlendFile<'f>,
    pub owner: usize,
    pub st: usize,
    pub data: &'f [u8],
    /// The address this struct had when written (what pointers to it hold).
    pub addr: u64,
}

impl<'f> View<'f> {
    /// Field `name` with its bytes (see [`Dna::field`]).
    pub fn field(&self, name: &str) -> Option<(&'f Field, &'f [u8])> {
        let (f, offset) = self.file.dna.field(self.st, name)?;
        Some((f, self.data.get(offset..offset.checked_add(f.size)?)?))
    }

    /// Element `i` of numeric field `name` as f64 (any integer or float type).
    pub fn num_at(&self, name: &str, i: usize) -> Option<f64> {
        let (f, b) = self.field(name)?;
        if f.is_pointer || i >= f.count {
            return None;
        }
        let file = self.file;
        let elem = f.size.checked_div(f.count)?;
        let off = i.checked_mul(elem)?;
        Some(match file.dna.type_name(f.ty) {
            "char" | "int8_t" => *b.get(off)? as i8 as f64,
            "uchar" | "uint8_t" => f64::from(*b.get(off)?),
            "short" | "int16_t" => f64::from(file.u16(b, off).ok()? as i16),
            "ushort" | "uint16_t" => f64::from(file.u16(b, off).ok()?),
            "int" | "int32_t" => f64::from(file.i32(b, off).ok()?),
            "uint" | "uint32_t" => f64::from(file.u32(b, off).ok()?),
            "float" => f64::from(file.f32(b, off).ok()?),
            "double" => f64::from_bits(file.u64(b, off).ok()?),
            "int64_t" => file.i64(b, off).ok()? as f64,
            "uint64_t" | "ulong" => file.u64(b, off).ok()? as f64,
            _ => return None,
        })
    }

    pub fn num(&self, name: &str) -> Option<f64> {
        self.num_at(name, 0)
    }

    pub fn f32(&self, name: &str) -> Option<f32> {
        self.num(name).map(|v| v as f32)
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        self.num(name).map(|v| v as i64)
    }

    /// The first `n` elements of numeric array field `name`.
    pub fn f32s(&self, name: &str, n: usize) -> Option<Vec<f32>> {
        (0..n).map(|i| self.num_at(name, i).map(|v| v as f32)).collect()
    }

    /// Pointer field `name` (0 when null).
    pub fn ptr(&self, name: &str) -> Option<u64> {
        let (f, b) = self.field(name)?;
        if !f.is_pointer {
            return None;
        }
        self.file.ptr(b, 0).ok()
    }

    /// A NUL-terminated char array field.
    pub fn string(&self, name: &str) -> Option<String> {
        let (f, b) = self.field(name)?;
        if f.is_pointer {
            return None;
        }
        Some(c_string(b))
    }

    /// An embedded struct field.
    pub fn sub(&self, name: &str) -> Option<View<'f>> {
        let (f, offset) = self.file.dna.field(self.st, name)?;
        if f.is_pointer {
            return None;
        }
        let st = self.file.dna.struct_index(self.file.dna.type_name(f.ty))?;
        let data = self.data.get(offset..offset.checked_add(f.size)?)?;
        Some(View { file: self.file, owner: self.owner, st, data, addr: self.addr.wrapping_add(offset as u64) })
    }

    /// The data block (of this struct's ID) that pointer field `name` refers to.
    pub fn deref(&self, name: &str) -> Option<BlockRef<'f>> {
        self.file.data_block(self.owner, self.ptr(name)?)
    }

    /// The ID block that pointer field `name` refers to.
    pub fn id(&self, name: &str) -> Option<BlockRef<'f>> {
        self.file.id_block(self.ptr(name)?)
    }

    /// The structs of the `ListBase` field `name`, following `next` pointers.
    pub fn list(&self, name: &str) -> ListIter<'f> {
        let first = self.sub(name).and_then(|lb| lb.ptr("first")).unwrap_or(0);
        ListIter { file: self.file, owner: self.owner, next: first, left: MAX_LIST }
    }
}

pub(super) struct ListIter<'f> {
    file: &'f BlendFile<'f>,
    owner: usize,
    next: u64,
    left: usize,
}

impl<'f> Iterator for ListIter<'f> {
    type Item = View<'f>;

    fn next(&mut self) -> Option<View<'f>> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let v = self.file.data_block(self.owner, self.next)?.view(0)?;
        self.next = v.ptr("next").unwrap_or(0);
        Some(v)
    }
}
