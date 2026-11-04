//! Built-in materials and smart materials: fill layers (and folders of fill layers with
//! mesh-map-driven masks), all procedural (no bitmap assets), so they are original to
//! TextureCraft and work at any resolution.
//!
//! Values follow physically based conventions: metals are metallic 1 with base colors from
//! published measured reflectance (F0) tables; dielectrics are metallic 0 with linear albedo
//! between about 0.02 (charcoal, rubber) and 0.9 (fresh snow).
//!
//! A few materials show the extra lobes (clear coat, sheen, anisotropy, subsurface scattering);
//! applying one adds the lobe channels it uses to the texture set.

use texturecraft_core::{
    BlendMode, Channel, ChannelBlend, EffectKind, Fill, GeneratorKind, Layer, LayerId, LayerKind, MaskBackground, Procedural, ProceduralKind, Project,
    Projection, Source,
};

use crate::smart_masks::{build_mask, directional, generator, pattern};
use crate::{EngineError, Result, Session};

/// One material in the shelf: id, label, category, and a representative color/metal/roughness.
pub struct SmartMaterial {
    pub id: &'static str,
    pub label: &'static str,
    /// Shelf group ("Metal", "Wood", …).
    pub category: &'static str,
    pub base: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    /// True for multi-layer "smart" materials (worn, dirty…), false for simple materials.
    pub smart: bool,
}

const fn m(id: &'static str, label: &'static str, category: &'static str, base: [f32; 3], metallic: f32, roughness: f32) -> SmartMaterial {
    SmartMaterial { id, label, category, base, metallic, roughness, smart: false }
}

const fn sm(id: &'static str, label: &'static str, category: &'static str, base: [f32; 3], metallic: f32, roughness: f32) -> SmartMaterial {
    SmartMaterial { id, label, category, base, metallic, roughness, smart: true }
}

const METAL: &str = "Metal";
const WOOD: &str = "Wood";
const STONE: &str = "Stone & Masonry";
const PLASTIC: &str = "Plastic & Rubber";
const FABRIC: &str = "Fabric & Leather";
const GLASS: &str = "Ceramic & Glass";
const ORGANIC: &str = "Organic";

pub const MATERIALS: &[SmartMaterial] = &[
    // Metals (base color = measured F0, linear).
    m("steel", "Steel", METAL, [0.56, 0.57, 0.58], 1.0, 0.34),
    m("steel_stainless", "Steel Stainless", METAL, [0.65, 0.64, 0.62], 1.0, 0.24),
    m("steel_dark", "Steel Dark", METAL, [0.48, 0.48, 0.5], 1.0, 0.3),
    m("iron", "Iron Raw", METAL, [0.53, 0.53, 0.54], 1.0, 0.5),
    m("cast_iron", "Cast Iron", METAL, [0.48, 0.48, 0.48], 1.0, 0.65),
    m("copper", "Copper", METAL, [0.955, 0.638, 0.538], 1.0, 0.26),
    m("copper_hammered", "Copper Hammered", METAL, [0.92, 0.6, 0.5], 1.0, 0.3),
    m("brass", "Brass", METAL, [0.91, 0.778, 0.423], 1.0, 0.27),
    m("bronze", "Bronze", METAL, [0.8, 0.56, 0.32], 1.0, 0.36),
    m("gold_polished", "Gold Polished", METAL, [1.0, 0.766, 0.336], 1.0, 0.17),
    m("silver", "Silver", METAL, [0.972, 0.96, 0.915], 1.0, 0.22),
    m("aluminium", "Aluminium Raw", METAL, [0.913, 0.922, 0.924], 1.0, 0.42),
    m("aluminium_brushed", "Aluminium Brushed", METAL, [0.913, 0.922, 0.924], 1.0, 0.32),
    m("steel_brushed_aniso", "Steel Brushed Anisotropic", METAL, [0.56, 0.57, 0.58], 1.0, 0.3),
    m("chrome", "Chrome", METAL, [0.55, 0.556, 0.554], 1.0, 0.05),
    m("titanium", "Titanium", METAL, [0.542, 0.497, 0.449], 1.0, 0.32),
    m("nickel", "Nickel", METAL, [0.66, 0.609, 0.526], 1.0, 0.25),
    m("platinum", "Platinum", METAL, [0.673, 0.637, 0.585], 1.0, 0.2),
    m("zinc", "Zinc", METAL, [0.664, 0.824, 0.85], 1.0, 0.38),
    m("cobalt", "Cobalt", METAL, [0.662, 0.655, 0.634], 1.0, 0.3),
    m("galvanized", "Steel Galvanized", METAL, [0.69, 0.71, 0.72], 1.0, 0.4),
    m("rust", "Rust", METAL, [0.3, 0.1, 0.03], 0.0, 0.85),
    // Woods.
    m("wood", "Wood Varnished", WOOD, [0.29, 0.15, 0.06], 0.0, 0.26),
    m("wood_oak", "Oak", WOOD, [0.33, 0.2, 0.09], 0.0, 0.62),
    m("wood_walnut", "Walnut", WOOD, [0.12, 0.06, 0.03], 0.0, 0.5),
    m("wood_pine", "Pine", WOOD, [0.52, 0.33, 0.16], 0.0, 0.65),
    m("plywood", "Plywood", WOOD, [0.55, 0.38, 0.21], 0.0, 0.75),
    m("bark", "Bark", WOOD, [0.14, 0.1, 0.07], 0.0, 0.9),
    m("wood_maple", "Maple", WOOD, [0.62, 0.45, 0.27], 0.0, 0.55),
    m("wood_cherry", "Cherry", WOOD, [0.35, 0.13, 0.06], 0.0, 0.45),
    m("bamboo", "Bamboo", WOOD, [0.6, 0.5, 0.28], 0.0, 0.5),
    m("wood_painted", "Wood Painted", WOOD, [0.61, 0.59, 0.54], 0.0, 0.48),
    // Stone, masonry and earth.
    m("concrete", "Concrete", STONE, [0.27, 0.26, 0.25], 0.0, 0.9),
    m("brick", "Brick Red", STONE, [0.3, 0.075, 0.04], 0.0, 0.85),
    m("tiles_ceramic", "Tiles Ceramic", STONE, [0.8, 0.8, 0.78], 0.0, 0.1),
    m("marble", "Marble", STONE, [0.78, 0.78, 0.76], 0.0, 0.14),
    m("marble_black", "Marble Black", STONE, [0.03, 0.03, 0.032], 0.0, 0.1),
    m("granite", "Granite", STONE, [0.3, 0.27, 0.25], 0.0, 0.3),
    m("asphalt", "Asphalt", STONE, [0.06, 0.06, 0.058], 0.0, 0.9),
    m("clay", "Clay Earthenware", STONE, [0.45, 0.18, 0.09], 0.0, 0.8),
    m("slate", "Slate", STONE, [0.11, 0.115, 0.13], 0.0, 0.72),
    m("sandstone", "Sandstone", STONE, [0.52, 0.38, 0.23], 0.0, 0.9),
    m("terrazzo", "Terrazzo", STONE, [0.7, 0.69, 0.66], 0.0, 0.3),
    m("sand", "Sand", STONE, [0.52, 0.41, 0.26], 0.0, 0.95),
    m("mud", "Mud", STONE, [0.12, 0.08, 0.05], 0.0, 0.6),
    m("cobblestone", "Cobblestone", STONE, [0.3, 0.29, 0.27], 0.0, 0.8),
    m("gravel", "Gravel", STONE, [0.35, 0.33, 0.3], 0.0, 0.85),
    m("plaster", "Plaster", STONE, [0.78, 0.76, 0.72], 0.0, 0.88),
    // Plastics, rubber, composites.
    m("plastic_glossy", "Plastic Glossy", PLASTIC, [0.8, 0.12, 0.08], 0.0, 0.16),
    m("plastic_matte", "Plastic Matte", PLASTIC, [0.18, 0.18, 0.19], 0.0, 0.6),
    m("plastic_textured", "Plastic Textured", PLASTIC, [0.035, 0.035, 0.04], 0.0, 0.55),
    m("rubber", "Rubber", PLASTIC, [0.03, 0.03, 0.03], 0.0, 0.88),
    m("carbon_fiber", "Carbon Fiber", PLASTIC, [0.035, 0.035, 0.04], 0.0, 0.15),
    // Fabric and leather.
    m("fabric", "Fabric Cotton", FABRIC, [0.45, 0.47, 0.54], 0.0, 0.95),
    m("denim", "Denim", FABRIC, [0.06, 0.09, 0.19], 0.0, 0.92),
    m("leather", "Leather", FABRIC, [0.17, 0.08, 0.04], 0.0, 0.52),
    m("leather_black", "Leather Black", FABRIC, [0.025, 0.025, 0.028], 0.0, 0.42),
    m("velvet", "Velvet", FABRIC, [0.12, 0.015, 0.09], 0.0, 0.9),
    m("linen", "Linen", FABRIC, [0.72, 0.66, 0.55], 0.0, 0.9),
    m("wool", "Wool", FABRIC, [0.55, 0.52, 0.48], 0.0, 0.97),
    // Ceramic and glass.
    m("ceramic", "Ceramic White", GLASS, [0.8, 0.8, 0.78], 0.0, 0.08),
    m("glass", "Glass Clear", GLASS, [0.88, 0.9, 0.9], 0.0, 0.03),
    m("glass_frosted", "Glass Frosted", GLASS, [0.84, 0.88, 0.88], 0.0, 0.45),
    m("ice", "Ice", GLASS, [0.72, 0.82, 0.88], 0.0, 0.12),
    m("car_paint", "Car Paint", PLASTIC, [0.45, 0.025, 0.03], 0.0, 0.4),
    // Organic.
    m("skin", "Skin", ORGANIC, [0.56, 0.35, 0.27], 0.0, 0.5),
    m("wax", "Candle Wax", ORGANIC, [0.8, 0.72, 0.55], 0.0, 0.35),
    m("cork", "Cork", ORGANIC, [0.36, 0.21, 0.1], 0.0, 0.85),
    m("paper", "Paper", ORGANIC, [0.82, 0.81, 0.77], 0.0, 0.92),
    m("snow", "Snow", ORGANIC, [0.85, 0.87, 0.9], 0.0, 0.6),
    m("bone", "Bone", ORGANIC, [0.7, 0.63, 0.5], 0.0, 0.55),
    m("moss", "Moss", ORGANIC, [0.09, 0.14, 0.04], 0.0, 0.85),
    // Smart materials.
    sm("steel_painted_worn", "Steel Painted Worn", METAL, [0.42, 0.06, 0.04], 0.2, 0.45),
    sm("steel_painted_rust", "Steel Painted Rust Leaks", METAL, [0.62, 0.62, 0.6], 0.1, 0.5),
    sm("military_paint", "Military Paint", METAL, [0.07, 0.08, 0.035], 0.1, 0.6),
    sm("iron_rusted", "Iron Rusted", METAL, [0.38, 0.14, 0.05], 0.3, 0.8),
    sm("aluminium_scratched", "Aluminium Scratched", METAL, [0.913, 0.922, 0.924], 1.0, 0.3),
    sm("copper_patina", "Copper Oxidized", METAL, [0.6, 0.45, 0.35], 0.6, 0.5),
    sm("bronze_statue", "Bronze Statue", METAL, [0.35, 0.4, 0.3], 0.5, 0.6),
    sm("brass_tarnished", "Brass Tarnished", METAL, [0.7, 0.58, 0.3], 0.8, 0.4),
    sm("gold_dirty", "Gold Dirty", METAL, [0.9, 0.68, 0.3], 0.9, 0.3),
    sm("chrome_smudged", "Chrome Smudged", METAL, [0.55, 0.556, 0.554], 1.0, 0.12),
    sm("wood_weathered", "Wood Weathered", WOOD, [0.33, 0.3, 0.27], 0.0, 0.8),
    sm("wood_chipped_paint", "Wood Chipped Paint", WOOD, [0.04, 0.22, 0.22], 0.0, 0.55),
    sm("concrete_dirty", "Concrete Dirty", STONE, [0.28, 0.26, 0.23], 0.0, 0.92),
    sm("brick_mossy", "Brick Mossy", STONE, [0.22, 0.12, 0.05], 0.0, 0.9),
    sm("plastic_dusty", "Plastic Dusty", PLASTIC, [0.05, 0.2, 0.45], 0.0, 0.4),
    sm("plastic_scuffed", "Plastic Scuffed", PLASTIC, [0.06, 0.06, 0.065], 0.0, 0.5),
    sm("leather_worn", "Leather Worn", FABRIC, [0.22, 0.12, 0.07], 0.0, 0.5),
    sm("stone_mossy", "Stone Mossy", STONE, [0.36, 0.35, 0.33], 0.0, 0.85),
    sm("paint_sun_faded", "Paint Sun Faded", PLASTIC, [0.55, 0.06, 0.05], 0.0, 0.45),
    sm("steel_water_stained", "Steel Water Stained", METAL, [0.56, 0.57, 0.58], 1.0, 0.35),
    sm("plastic_grimy", "Plastic Grimy", PLASTIC, [0.75, 0.74, 0.7], 0.0, 0.45),
];

fn norm(x: &str) -> String {
    x.to_ascii_lowercase().replace([' ', '_', '-'], "")
}

pub fn find(id: &str) -> Option<&'static SmartMaterial> {
    MATERIALS.iter().find(|m| norm(m.id) == norm(id) || norm(m.label) == norm(id))
}

fn u(v: f32) -> Source {
    Source::Uniform([v; 3])
}

fn rgb(c: [f32; 3]) -> Source {
    Source::Uniform(c)
}

fn pat(kind: ProceduralKind, scale: f32, seed: u32, contrast: f32, low: [f32; 3], high: [f32; 3]) -> Source {
    Source::Procedural { pattern: Procedural { kind, scale, seed, contrast, balance: 0.0, invert: false }, low, high }
}

/// A scalar pattern between `lo` and `hi`.
fn gray(kind: ProceduralKind, scale: f32, seed: u32, lo: f32, hi: f32) -> Source {
    pat(kind, scale, seed, 1.0, [lo; 3], [hi; 3])
}

type Sources = Vec<(Channel, Source)>;

fn surface(base: Source, metallic: f32, rough: Source) -> Sources {
    vec![(Channel::BaseColor, base), (Channel::Metallic, u(metallic)), (Channel::Roughness, rough)]
}

fn metal(base: Source, rough: Source) -> Sources {
    surface(base, 1.0, rough)
}

fn dielectric(base: Source, rough: Source) -> Sources {
    surface(base, 0.0, rough)
}

/// `sources` plus `extra` channels (height, opacity…).
fn plus(mut sources: Sources, extra: impl IntoIterator<Item = (Channel, Source)>) -> Sources {
    sources.extend(extra);
    sources
}

fn height(s: Source) -> [(Channel, Source); 1] {
    [(Channel::Height, s)]
}

pub(crate) struct Builder<'a> {
    pub p: &'a mut Project,
    /// Channels of the target texture set; layers only enable these.
    pub channels: Vec<Channel>,
}

impl Builder<'_> {
    fn fill(&mut self, name: &str, sources: Sources) -> Layer {
        let id = self.p.alloc_id();
        let enabled: Vec<Channel> = sources.iter().map(|(c, _)| *c).filter(|c| self.channels.contains(c)).collect();
        let projection = Projection::Triplanar { scale: 1.0, sharpness: 8.0, offset: [0.0; 3] };
        Layer::new(id, name, LayerKind::Fill(Fill { sources: sources.into_iter().collect(), projection, link: None, cutout: None }), &enabled)
    }

    /// A fill masked by smart mask preset `mask` (inverted: present where the preset is not).
    fn preset(&mut self, name: &str, sources: Sources, mask: &str, invert: bool) -> Result<Layer> {
        let mut l = self.fill(name, sources);
        l.mask = Some(crate::smart_masks::mask(self.p, mask, invert)?);
        Ok(l)
    }

    /// A fill with a custom black-background mask.
    fn masked(&mut self, name: &str, sources: Sources, effects: Vec<(EffectKind, BlendMode)>) -> Layer {
        let mut l = self.fill(name, sources);
        l.mask = Some(build_mask(self.p, MaskBackground::Black, effects));
        l
    }

    /// The common grime layer: dark dirt settled in cavities.
    fn dirt(&mut self, color: [f32; 3]) -> Result<Layer> {
        self.preset("Dirt", dielectric(rgb(color), u(0.9)), "dirt_cavities", false)
    }
}

/// Layers (bottom → top) making up material `id`.
pub(crate) fn build(b: &mut Builder, id: &str) -> Result<Vec<Layer>> {
    use BlendMode::{Lighten, Multiply, Normal};
    use Channel::*;
    use GeneratorKind as G;
    use ProceduralKind as P;
    let m = find(id).ok_or_else(|| EngineError::Other(format!("unknown material `{id}`")))?;
    let one = |b: &mut Builder, s: Sources| -> Result<Vec<Layer>> { Ok(vec![b.fill(m.label, s)]) };
    // A simple metal: subtle cloudy tone and roughness variation around the table values.
    let plain_metal = |b: &mut Builder, rough: Source| {
        let lo = m.base.map(|v| v * 0.94);
        one(b, metal(pat(P::Clouds, 3.0, 5, 1.0, lo, m.base), rough))
    };
    let r = m.roughness;
    let clouds_rough = |seed: u32| gray(P::Clouds, 6.0, seed, r * 0.85, (r * 1.2).min(1.0));
    match m.id {
        // ---- Metals -------------------------------------------------------------------------
        "steel" | "steel_dark" | "iron" | "copper" | "brass" | "bronze" | "gold_polished" | "silver" | "aluminium" | "chrome" | "nickel" | "platinum"
        | "cobalt" => plain_metal(b, clouds_rough(7)),
        "steel_stainless" | "aluminium_brushed" | "titanium" => plain_metal(b, gray(P::Brushed, 3.0, 3, r * 0.75, r * 1.25)),
        // Zinc plate: large crystalline spangles.
        "zinc" => one(b, metal(pat(P::Voronoi, 6.0, 21, 1.0, m.base.map(|v| v * 0.8), m.base), gray(P::Voronoi, 6.0, 21, r * 1.5, r * 0.6))),
        // Brushing lines run along X; the highlights stretch across them (a quarter turn).
        "steel_brushed_aniso" => one(
            b,
            plus(
                metal(pat(P::Brushed, 3.0, 5, 1.0, m.base.map(|v| v * 0.92), m.base), gray(P::Brushed, 3.0, 5, r * 0.8, r * 1.2)),
                [(AnisotropyLevel, u(0.8)), (AnisotropyAngle, u(0.5))],
            ),
        ),
        "cast_iron" => one(
            b,
            plus(metal(pat(P::Noise, 1.0, 4, 1.0, [0.44; 3], [0.52; 3]), gray(P::Grunge, 3.0, 9, 0.55, 0.75)), height(gray(P::Pores, 14.0, 3, -0.02, 0.0))),
        ),
        "copper_hammered" => one(
            b,
            plus(
                metal(pat(P::Hammered, 6.0, 1, 1.0, [0.86, 0.55, 0.45], [0.955, 0.638, 0.538]), gray(P::Hammered, 6.0, 1, 0.38, 0.24)),
                height(gray(P::Hammered, 6.0, 1, -0.04, 0.0)),
            ),
        ),
        // Iron oxide is a dielectric: rough, pitted, blotchy orange-brown.
        "rust" => one(
            b,
            plus(
                dielectric(pat(P::Grunge, 3.0, 11, 1.0, [0.12, 0.035, 0.012], [0.36, 0.13, 0.04]), gray(P::Clouds, 5.0, 11, 0.95, 0.75)),
                height(gray(P::Pores, 10.0, 11, -0.015, 0.0)),
            ),
        ),
        "slate" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 3.0, 5, 1.2, [0.07, 0.075, 0.085], [0.16, 0.17, 0.19]), gray(P::Noise, 1.0, 5, 0.64, 0.8)),
                height(gray(P::Cells, 4.0, 5, -0.01, 0.0)),
            ),
        ),
        "sandstone" => {
            one(b, plus(dielectric(pat(P::Noise, 4.0, 13, 1.0, [0.42, 0.3, 0.18], [0.6, 0.45, 0.28]), u(0.9)), height(gray(P::Perlin, 6.0, 13, -0.01, 0.0))))
        }
        // Stone chips in a pale binder, polished flat.
        "terrazzo" => one(b, dielectric(pat(P::Voronoi, 18.0, 9, 1.0, [0.55, 0.55, 0.53], [0.85, 0.84, 0.8]), gray(P::Voronoi, 18.0, 9, 0.34, 0.26))),
        // Fine grains in soft wind ripples.
        "sand" => one(
            b,
            plus(dielectric(pat(P::Noise, 12.0, 27, 1.0, [0.42, 0.32, 0.19], [0.6, 0.48, 0.31]), u(0.95)), height(gray(P::Perlin, 4.0, 27, -0.012, 0.0))),
        ),
        // Glossy wet patches over rougher, drier mud.
        "mud" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 3.0, 29, 1.0, [0.05, 0.035, 0.02], [0.17, 0.12, 0.075]), gray(P::Grunge, 3.0, 29, 0.85, 0.2)),
                height(gray(P::Clouds, 3.0, 29, -0.03, 0.0)),
            ),
        ),
        // Rounded stones with deep joints between them.
        "cobblestone" => one(
            b,
            plus(
                dielectric(pat(P::Cells, 6.0, 37, 1.0, m.base.map(|v| v * 1.4), [0.16, 0.15, 0.14]), gray(P::Cells, 6.0, 37, 0.7, 0.95)),
                height(gray(P::Cells, 6.0, 37, 0.0, -0.05)),
            ),
        ),
        // Small loose stones of mixed tones.
        "gravel" => one(
            b,
            plus(
                dielectric(pat(P::Voronoi, 30.0, 39, 1.2, [0.18, 0.17, 0.15], [0.52, 0.5, 0.46]), gray(P::Voronoi, 30.0, 39, 0.92, 0.78)),
                height(gray(P::Cells, 30.0, 39, -0.03, 0.0)),
            ),
        ),
        // Troweled wall: soft patches and a fine sandy tooth.
        "plaster" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 2.0, 41, 0.6, [0.7, 0.68, 0.64], [0.84, 0.82, 0.78]), gray(P::Noise, 1.0, 41, 0.82, 0.92)),
                height(gray(P::Clouds, 2.0, 41, -0.008, 0.0)),
            ),
        ),
        // Low cushions of tiny leaves.
        "moss" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 5.0, 43, 1.3, [0.04, 0.07, 0.015], [0.18, 0.26, 0.07]), gray(P::Noise, 1.0, 43, 0.75, 0.95)),
                [(SheenWeight, u(0.4)), (SheenColor, rgb([0.45, 0.6, 0.25])), (SheenRoughness, u(0.5)), (Height, gray(P::Clouds, 5.0, 43, -0.03, 0.0))],
            ),
        ),
        "cork" => {
            one(b, plus(dielectric(pat(P::Noise, 6.0, 17, 1.0, [0.25, 0.14, 0.06], [0.45, 0.28, 0.13]), u(0.85)), height(gray(P::Pores, 20.0, 17, -0.01, 0.0))))
        }
        "paper" => one(
            b,
            plus(dielectric(pat(P::Clouds, 8.0, 19, 1.0, [0.78, 0.77, 0.73], [0.86, 0.85, 0.81]), u(0.92)), height(gray(P::Noise, 1.0, 19, -0.002, 0.0))),
        ),
        "galvanized" => one(b, metal(pat(P::Voronoi, 10.0, 4, 1.0, [0.6, 0.62, 0.63], [0.78, 0.8, 0.81]), gray(P::Voronoi, 10.0, 4, 0.5, 0.28))),
        // ---- Woods (grain along X, rings across Y) ---------------------------------------------
        "wood" => one(
            b,
            plus(dielectric(pat(P::Wood, 2.0, 7, 1.0, [0.12, 0.05, 0.018], [0.42, 0.22, 0.09]), clouds_rough(3)), height(gray(P::Wood, 2.0, 7, -0.004, 0.0))),
        ),
        "wood_oak" => one(
            b,
            plus(
                dielectric(pat(P::Wood, 2.0, 7, 1.0, [0.2, 0.11, 0.045], [0.46, 0.29, 0.14]), gray(P::Wood, 2.0, 7, 0.74, 0.56)),
                height(gray(P::Wood, 2.0, 7, -0.012, 0.0)),
            ),
        ),
        "wood_walnut" => one(
            b,
            plus(
                dielectric(pat(P::Wood, 2.5, 21, 1.0, [0.05, 0.025, 0.012], [0.2, 0.1, 0.05]), gray(P::Wood, 2.5, 21, 0.56, 0.46)),
                height(gray(P::Wood, 2.5, 21, -0.008, 0.0)),
            ),
        ),
        "wood_pine" => one(
            b,
            plus(
                dielectric(pat(P::Wood, 1.5, 4, 1.0, [0.33, 0.17, 0.065], [0.7, 0.48, 0.25]), gray(P::Wood, 1.5, 4, 0.72, 0.62)),
                height(gray(P::Wood, 1.5, 4, -0.012, 0.0)),
            ),
        ),
        "plywood" => one(b, dielectric(pat(P::Wood, 1.2, 15, 0.6, [0.42, 0.27, 0.13], [0.66, 0.47, 0.27]), gray(P::Wood, 1.2, 15, 0.8, 0.7))),
        // Dark, deep furrows between gray-brown ridges.
        "bark" => one(
            b,
            plus(dielectric(pat(P::Streaks, 8.0, 25, 1.4, [0.13, 0.105, 0.08], [0.03, 0.022, 0.015]), u(0.9)), height(gray(P::Streaks, 8.0, 25, 0.0, -0.06))),
        ),
        // Pale, fine and even grain.
        "wood_maple" => one(
            b,
            plus(
                dielectric(pat(P::Wood, 3.0, 31, 0.5, [0.52, 0.36, 0.2], [0.7, 0.53, 0.33]), gray(P::Wood, 3.0, 31, 0.6, 0.5)),
                height(gray(P::Wood, 3.0, 31, -0.006, 0.0)),
            ),
        ),
        // Reddish heartwood with a satin finish.
        "wood_cherry" => one(
            b,
            plus(
                dielectric(pat(P::Wood, 2.2, 33, 0.9, [0.22, 0.07, 0.03], [0.45, 0.19, 0.09]), gray(P::Wood, 2.2, 33, 0.5, 0.4)),
                height(gray(P::Wood, 2.2, 33, -0.008, 0.0)),
            ),
        ),
        // Long straight fibres with a few raised ridges.
        "bamboo" => one(
            b,
            plus(
                dielectric(pat(P::Streaks, 2.0, 35, 0.6, [0.5, 0.4, 0.2], [0.68, 0.58, 0.34]), gray(P::Streaks, 2.0, 35, 0.55, 0.45)),
                height(gray(P::Stripes, 1.5, 35, 0.0, -0.01)),
            ),
        ),
        "wood_painted" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 3.0, 4, 1.0, [0.57, 0.55, 0.5], [0.64, 0.62, 0.57]), gray(P::Wood, 2.0, 7, 0.56, 0.42)),
                height(gray(P::Wood, 2.0, 7, -0.015, 0.0)),
            ),
        ),
        // ---- Stone, masonry, earth ----------------------------------------------------------
        "concrete" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 6.0, 4, 1.3, [0.2, 0.19, 0.18], [0.35, 0.34, 0.32]), gray(P::Noise, 1.0, 6, 0.85, 0.95)),
                height(gray(P::Pores, 14.0, 6, -0.03, 0.0)),
            ),
        ),
        "brick" => one(
            b,
            plus(
                dielectric(pat(P::Bricks, 3.0, 4, 1.0, [0.27, 0.26, 0.24], [0.3, 0.075, 0.04]), gray(P::Bricks, 3.0, 4, 0.95, 0.82)),
                height(gray(P::Bricks, 3.0, 4, -0.04, 0.0)),
            ),
        ),
        "tiles_ceramic" => one(
            b,
            plus(
                dielectric(pat(P::Tiles, 4.0, 2, 1.0, [0.33, 0.32, 0.3], [0.82, 0.82, 0.8]), gray(P::Tiles, 4.0, 2, 0.85, 0.08)),
                height(gray(P::Tiles, 4.0, 2, -0.03, 0.0)),
            ),
        ),
        "marble" => one(b, dielectric(pat(P::Marble, 2.0, 12, 1.0, [0.38, 0.38, 0.37], [0.86, 0.86, 0.84]), clouds_rough(12))),
        "marble_black" => one(b, dielectric(pat(P::Marble, 2.0, 5, 1.0, [0.55, 0.55, 0.53], [0.015, 0.015, 0.017]), clouds_rough(5))),
        "granite" => one(b, dielectric(pat(P::Voronoi, 70.0, 8, 1.0, [0.1, 0.095, 0.09], [0.5, 0.47, 0.45]), gray(P::Voronoi, 70.0, 8, 0.25, 0.35))),
        "asphalt" => one(
            b,
            plus(
                dielectric(pat(P::Voronoi, 90.0, 3, 1.0, [0.025; 3], [0.11, 0.105, 0.1]), gray(P::Voronoi, 90.0, 3, 0.95, 0.75)),
                height(gray(P::Pores, 10.0, 5, -0.02, 0.0)),
            ),
        ),
        "clay" => one(
            b,
            plus(dielectric(pat(P::Clouds, 5.0, 2, 1.0, [0.4, 0.15, 0.07], [0.5, 0.21, 0.11]), clouds_rough(2)), height(gray(P::Noise, 0.8, 3, -0.005, 0.005))),
        ),
        // ---- Plastics, rubber, composites ---------------------------------------------------
        "plastic_glossy" | "plastic_matte" | "ceramic" => one(b, dielectric(rgb(m.base), clouds_rough(9))),
        "plastic_textured" => one(b, plus(dielectric(rgb(m.base), gray(P::Leather, 40.0, 5, 0.66, 0.5)), height(gray(P::Leather, 40.0, 5, -0.01, 0.0)))),
        "rubber" => one(b, dielectric(pat(P::Noise, 0.5, 3, 1.0, [0.025; 3], [0.035; 3]), gray(P::Noise, 0.5, 3, 0.84, 0.94))),
        // Twill-woven tows under a clear coat: the warp catches the light, the weft stays dark.
        "carbon_fiber" => one(
            b,
            plus(
                dielectric(pat(P::Twill, 12.0, 1, 1.0, [0.004, 0.004, 0.005], [0.08, 0.08, 0.085]), gray(P::Twill, 12.0, 1, 0.5, 0.28)),
                [(CoatWeight, u(1.0)), (CoatRoughness, u(r * 0.2)), (Height, gray(P::Twill, 12.0, 1, 0.0, 0.002))],
            ),
        ),
        // ---- Fabric and leather -------------------------------------------------------------
        "fabric" => {
            one(b, plus(dielectric(pat(P::Weave, 18.0, 2, 1.0, [0.27, 0.29, 0.34], [0.6, 0.62, 0.7]), u(r)), height(gray(P::Weave, 18.0, 2, -0.01, 0.0))))
        }
        // Indigo twill: diagonal ribs.
        "denim" => {
            one(b, plus(dielectric(pat(P::Twill, 22.0, 5, 1.0, [0.015, 0.025, 0.07], [0.15, 0.2, 0.38]), u(r)), height(gray(P::Twill, 22.0, 5, -0.01, 0.0))))
        }
        // Coarse, slubby plain weave.
        "linen" => {
            one(b, plus(dielectric(pat(P::Weave, 30.0, 45, 1.0, [0.58, 0.52, 0.42], [0.8, 0.74, 0.62]), u(r)), height(gray(P::Weave, 30.0, 45, -0.006, 0.0))))
        }
        // Fuzzy knit: no visible threads, a soft sheen at grazing angles.
        "wool" => one(
            b,
            plus(
                dielectric(pat(P::Noise, 2.0, 47, 1.0, [0.45, 0.42, 0.38], [0.6, 0.57, 0.53]), u(r)),
                [(SheenWeight, u(0.6)), (SheenColor, rgb([0.85, 0.83, 0.8])), (SheenRoughness, u(0.6)), (Height, gray(P::Noise, 2.0, 47, -0.004, 0.0))],
            ),
        ),
        "leather" => one(
            b,
            plus(
                dielectric(pat(P::Leather, 42.0, 2, 1.0, [0.07, 0.03, 0.013], [0.25, 0.12, 0.06]), gray(P::Leather, 42.0, 2, 0.62, 0.46)),
                height(gray(P::Leather, 42.0, 2, -0.02, 0.0)),
            ),
        ),
        "leather_black" => one(
            b,
            plus(
                dielectric(pat(P::Leather, 42.0, 6, 1.0, [0.01; 3], [0.04, 0.04, 0.045]), gray(P::Leather, 42.0, 6, 0.52, 0.36)),
                height(gray(P::Leather, 42.0, 6, -0.02, 0.0)),
            ),
        ),
        "velvet" => one(
            b,
            plus(
                // A pile, not a weave: soft crushed patches under a bright sheen.
                dielectric(pat(P::Clouds, 3.0, 3, 1.0, [0.06, 0.006, 0.045], m.base), u(r)),
                [(SheenWeight, u(1.0)), (SheenColor, rgb([0.95, 0.55, 0.85])), (SheenRoughness, u(0.35)), (Height, gray(P::Noise, 1.0, 3, -0.002, 0.0))],
            ),
        ),
        // ---- Coated and translucent ---------------------------------------------------------
        "car_paint" => one(
            b,
            plus(
                dielectric(pat(P::Noise, 0.3, 8, 1.0, [0.38, 0.02, 0.025], m.base), gray(P::Noise, 0.3, 8, 0.3, 0.45)),
                [(CoatWeight, u(1.0)), (CoatRoughness, u(0.04))],
            ),
        ),
        "skin" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 4.0, 6, 1.0, [0.5, 0.3, 0.23], m.base), gray(P::Pores, 30.0, 2, 0.42, 0.58)),
                [(ScatterWeight, u(1.0)), (SheenWeight, u(0.15)), (SheenRoughness, u(0.6)), (Height, gray(P::Pores, 30.0, 2, -0.004, 0.0))],
            ),
        ),
        "wax" => one(b, plus(dielectric(pat(P::Clouds, 2.0, 11, 1.0, [0.74, 0.66, 0.5], m.base), clouds_rough(11)), [(ScatterWeight, u(1.0))])),
        // ---- Glass (the Transmission channel; the base color tints the light going through) ----
        "glass" => one(b, plus(dielectric(rgb(m.base), u(r)), [(Transmission, u(1.0))])),
        // Clear with cloudy patches of trapped air.
        "ice" => one(
            b,
            plus(dielectric(pat(P::Clouds, 4.0, 31, 1.0, m.base, [0.84, 0.88, 0.9]), clouds_rough(31)), [(Transmission, gray(P::Clouds, 4.0, 31, 0.9, 0.5))]),
        ),
        // Bright, rough and slightly translucent.
        "snow" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 4.0, 33, 1.0, [0.8, 0.82, 0.86], [0.88, 0.89, 0.9]), gray(P::Noise, 1.0, 33, 0.5, 0.7)),
                [(ScatterWeight, u(0.5)), (Height, gray(P::Perlin, 3.0, 33, -0.02, 0.0))],
            ),
        ),
        // Porous, ivory to tan, with a little light under the surface.
        "bone" => one(
            b,
            plus(
                dielectric(pat(P::Clouds, 5.0, 35, 1.0, [0.58, 0.5, 0.37], m.base), clouds_rough(35)),
                [(ScatterWeight, u(0.3)), (Height, gray(P::Pores, 16.0, 35, -0.006, 0.0))],
            ),
        ),
        "glass_frosted" => one(b, plus(dielectric(rgb(m.base), gray(P::Noise, 1.0, 2, 0.4, 0.5)), [(Transmission, u(1.0))])),
        // ---- Smart materials ----------------------------------------------------------------
        "steel_painted_worn" => {
            let mut v = build(b, "steel")?;
            v.push(b.preset("Paint", plus(dielectric(rgb(m.base), gray(P::Clouds, 3.0, 9, 0.38, 0.55)), height(u(0.004))), "edge_chipped", true)?);
            v.push(b.dirt([0.12, 0.09, 0.06])?);
            Ok(v)
        }
        "steel_painted_rust" => {
            let mut v = build(b, "steel")?;
            v.push(b.preset(
                "Paint",
                plus(dielectric(pat(P::Clouds, 4.0, 3, 1.0, [0.58, 0.58, 0.56], [0.66, 0.66, 0.64]), u(0.45)), height(u(0.004))),
                "edge_wear",
                true,
            )?);
            v.push(b.preset("Rust leaks", dielectric(pat(P::Grunge, 4.0, 2, 1.0, [0.1, 0.03, 0.01], [0.24, 0.08, 0.025]), u(0.85)), "rust_leaks", false)?);
            Ok(v)
        }
        "military_paint" => {
            let mut v = build(b, "steel_dark")?;
            v.push(b.preset(
                "Olive drab",
                plus(dielectric(pat(P::Clouds, 3.0, 6, 1.0, [0.06, 0.07, 0.03], [0.085, 0.095, 0.042]), u(0.62)), height(u(0.004))),
                "edge_chipped",
                true,
            )?);
            v.push(b.dirt([0.06, 0.05, 0.035])?);
            v.push(b.preset("Dust", dielectric(rgb([0.36, 0.31, 0.23]), u(0.95)), "dust_top", false)?);
            Ok(v)
        }
        "iron_rusted" => {
            let mut v = build(b, "cast_iron")?;
            let rust =
                plus(dielectric(pat(P::Grunge, 5.0, 3, 0.8, [0.045, 0.018, 0.008], [0.14, 0.05, 0.018]), u(0.88)), height(gray(P::Noise, 1.0, 1, 0.0, 0.04)));
            v.push(b.masked("Rust", rust, vec![(pattern(P::Grunge, 2.0, 8, 1.2), Normal), (generator(G::Dirt, 0.5, 0.25, 0.3), Lighten)]));
            v.push(b.preset("Rust leaks", dielectric(rgb([0.11, 0.04, 0.014]), u(0.8)), "rust_leaks", false)?);
            Ok(v)
        }
        "aluminium_scratched" => {
            let mut v = build(b, "aluminium_brushed")?;
            v.push(b.preset("Polished edges", metal(rgb([0.95, 0.95, 0.95]), u(0.14)), "edge_wear", false)?);
            v.push(b.preset("Scratches", plus(metal(rgb([0.95, 0.95, 0.95]), u(0.12)), height(u(-0.006))), "scratches", false)?);
            Ok(v)
        }
        "copper_patina" => {
            let mut v = build(b, "copper")?;
            v.push(b.preset("Tarnish", dielectric(pat(P::Clouds, 4.0, 2, 1.0, [0.18, 0.07, 0.04], [0.3, 0.13, 0.07]), u(0.6)), "grunge", false)?);
            let patina = plus(dielectric(pat(P::Clouds, 4.0, 11, 1.4, [0.15, 0.38, 0.32], [0.33, 0.6, 0.5]), u(0.85)), height(u(0.02)));
            v.push(b.masked("Patina", patina, vec![(generator(G::Dirt, 0.65, 0.25, 0.6), Normal), (pattern(P::Streaks, 3.0, 9, 1.0), Lighten)]));
            Ok(v)
        }
        "bronze_statue" => {
            let mut v = build(b, "bronze")?;
            v.push(b.preset("Dark oxide", dielectric(pat(P::Clouds, 4.0, 5, 1.0, [0.05, 0.035, 0.02], [0.12, 0.08, 0.04]), u(0.6)), "grunge", false)?);
            v.push(b.preset(
                "Verdigris",
                plus(dielectric(pat(P::Clouds, 5.0, 8, 1.3, [0.1, 0.3, 0.24], [0.25, 0.5, 0.42]), u(0.88)), height(u(0.015))),
                "rust_leaks",
                false,
            )?);
            Ok(v)
        }
        "brass_tarnished" => {
            let mut v = build(b, "brass")?;
            v.push(b.masked(
                "Tarnish",
                dielectric(pat(P::Clouds, 4.0, 4, 1.0, [0.08, 0.06, 0.025], [0.16, 0.12, 0.05]), u(0.7)),
                vec![(generator(G::Dirt, 0.6, 0.25, 0.5), Normal), (pattern(P::Grunge, 2.5, 3, 0.9), Lighten)],
            ));
            v.push(b.preset("Fingerprints", metal(rgb([0.8, 0.68, 0.37]), u(0.45)), "fingerprints", false)?);
            Ok(v)
        }
        "gold_dirty" => {
            let mut v = build(b, "gold_polished")?;
            v.push(b.preset("Smudges", metal(rgb([0.85, 0.64, 0.28]), u(0.4)), "fingerprints", false)?);
            v.push(b.dirt([0.06, 0.045, 0.03])?);
            Ok(v)
        }
        "chrome_smudged" => {
            let mut v = build(b, "chrome")?;
            v.push(b.preset("Smudges", metal(rgb([0.5, 0.505, 0.5]), u(0.3)), "fingerprints", false)?);
            v.push(b.preset("Dust", dielectric(rgb([0.5, 0.48, 0.45]), u(0.9)), "dust_top", false)?);
            Ok(v)
        }
        "wood_weathered" => {
            let mut v = build(b, "wood_pine")?;
            let gray_wood =
                plus(dielectric(pat(P::Wood, 1.5, 4, 1.0, [0.17, 0.16, 0.14], [0.42, 0.4, 0.37]), u(0.85)), height(gray(P::Wood, 1.5, 4, -0.03, 0.0)));
            v.push(b.masked(
                "Weathered",
                gray_wood,
                vec![(pattern(P::Grunge, 2.0, 15, 0.8), Normal), (directional(G::Direction, [0.0, 1.0, 0.0], 0.45, 0.2, 0.4, false), Lighten)],
            ));
            v.push(b.dirt([0.05, 0.04, 0.03])?);
            Ok(v)
        }
        "wood_chipped_paint" => {
            let mut v = build(b, "wood_oak")?;
            v.push(b.preset(
                "Paint",
                plus(dielectric(pat(P::Clouds, 3.0, 2, 1.0, [0.035, 0.19, 0.19], [0.05, 0.25, 0.25]), u(0.55)), height(u(0.01))),
                "paint_chips",
                true,
            )?);
            v.push(b.dirt([0.05, 0.04, 0.03])?);
            Ok(v)
        }
        "concrete_dirty" => {
            let mut v = build(b, "concrete")?;
            v.push(b.preset("Water stains", dielectric(rgb([0.13, 0.13, 0.12]), u(0.9)), "rust_leaks", false)?);
            v.push(b.dirt([0.08, 0.065, 0.05])?);
            v.push(b.preset("Ground mud", dielectric(rgb([0.12, 0.09, 0.06]), u(0.95)), "ground_dirt", false)?);
            Ok(v)
        }
        "brick_mossy" => {
            let mut v = build(b, "brick")?;
            v.push(b.preset("Grime", dielectric(rgb([0.07, 0.06, 0.05]), u(0.95)), "ground_dirt", false)?);
            let moss = plus(dielectric(pat(P::Noise, 1.0, 7, 1.0, [0.02, 0.035, 0.008], [0.05, 0.075, 0.015]), u(0.95)), height(u(0.02)));
            v.push(b.masked(
                "Moss",
                moss,
                vec![
                    (directional(G::Direction, [0.0, 1.0, 0.0], 0.45, 0.15, 0.3, false), Normal),
                    (pattern(P::Grunge, 2.0, 5, 1.0), Multiply),
                    (generator(G::Dirt, 0.4, 0.2, 0.5), Lighten),
                ],
            ));
            Ok(v)
        }
        "stone_mossy" => {
            let mut v = build(b, "granite")?;
            v.push(b.preset("Dust", dielectric(rgb([0.42, 0.4, 0.36]), u(0.95)), "dust_cavities", false)?);
            let moss = plus(dielectric(pat(P::Noise, 1.0, 11, 1.0, [0.03, 0.05, 0.01], [0.07, 0.1, 0.02]), u(0.95)), height(u(0.015)));
            v.push(b.preset("Moss", moss, "moss_top", false)?);
            Ok(v)
        }
        "paint_sun_faded" => {
            let mut v = one(b, dielectric(rgb(m.base), u(0.4)))?;
            v.push(b.preset("Faded", dielectric(pat(P::Clouds, 3.0, 4, 1.0, [0.62, 0.36, 0.33], [0.72, 0.48, 0.45]), u(0.7)), "sun_faded", false)?);
            v.push(b.preset("Chipped edges", dielectric(rgb([0.75, 0.73, 0.7]), u(0.6)), "edges_sharp", false)?);
            Ok(v)
        }
        "steel_water_stained" => {
            let mut v = build(b, "steel")?;
            v.push(b.preset("Stains", dielectric(pat(P::Grunge, 4.0, 8, 1.0, [0.2, 0.18, 0.15], [0.32, 0.29, 0.25]), u(0.75)), "water_stains", false)?);
            Ok(v)
        }
        "plastic_grimy" => {
            let mut v = one(b, dielectric(rgb(m.base), u(0.45)))?;
            v.push(b.preset("Worn edges", dielectric(rgb([0.86, 0.85, 0.82]), u(0.3)), "edges_soft", false)?);
            v.push(b.preset("Grime", dielectric(rgb([0.16, 0.14, 0.11]), u(0.9)), "dust_cavities", false)?);
            Ok(v)
        }
        "plastic_dusty" => {
            let mut v = one(b, dielectric(rgb(m.base), u(0.35)))?;
            v.push(b.preset("Dust", dielectric(rgb([0.5, 0.46, 0.4]), u(0.95)), "dust_top", false)?);
            v.push(b.dirt([0.1, 0.09, 0.08])?);
            Ok(v)
        }
        "plastic_scuffed" => {
            let mut v = one(b, dielectric(rgb(m.base), gray(P::Leather, 40.0, 5, 0.45, 0.35)))?;
            v.push(b.preset("Scuffed edges", dielectric(rgb([0.2, 0.2, 0.21]), u(0.7)), "edge_wear", false)?);
            v.push(b.preset("Scratches", dielectric(rgb([0.14, 0.14, 0.15]), u(0.6)), "scratches", false)?);
            Ok(v)
        }
        "leather_worn" => {
            let mut v = build(b, "leather")?;
            v.push(b.preset("Worn", dielectric(rgb([0.3, 0.18, 0.1]), u(0.36)), "edge_wear", false)?);
            v.push(b.dirt([0.035, 0.022, 0.014])?);
            Ok(v)
        }
        other => Err(EngineError::Other(format!("material `{other}` has no definition"))),
    }
}

/// Where [`apply_at`] puts a material.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// Above the selected layer (inside it when it is a folder).
    Selection,
    /// Above a layer, or at the top of a folder when `inside` (shelf drag and drop).
    Above(LayerId, bool),
    /// At this index of the top-level stack (0 = bottom).
    Index(usize),
}

/// Add material `id` above the selection: a single fill layer, or a folder for multi-layer
/// materials. Returns the new top-level layer id.
pub fn apply(s: &mut Session, id: &str) -> Result<LayerId> {
    apply_at(s, id, Place::Selection)
}

/// [`apply`] at `place` (one undo step).
pub fn apply_at(s: &mut Session, id: &str, place: Place) -> Result<LayerId> {
    let m = find(id).ok_or_else(|| EngineError::Other(format!("unknown material `{id}`")))?;
    let label = format!("Add {}", m.label);
    s.edit(&label, |s| {
        let ts = s.ts();
        let mut channels = s.doc.project.texture_set(ts)?.channel_list();
        // Lobe channels the material uses join the texture set (other channels it lacks are skipped).
        let missing: Vec<Channel> = Channel::LOBES.into_iter().filter(|c| !channels.contains(c)).collect();
        let layers = build(&mut Builder { p: &mut s.doc.project, channels: [channels.clone(), missing.clone()].concat() }, m.id)?;
        for c in missing.into_iter().filter(|c| layers.iter().any(|l| l.channels.contains_key(c))) {
            crate::set_cmds::add_channel(s, ts, c)?;
            channels.push(c);
        }
        let top = if layers.len() == 1 {
            let l = layers.into_iter().next().ok_or_else(|| EngineError::Other("empty material".into()))?;
            crate::layer_cmds::insert_selected(s, l)?
        } else {
            let gid = s.doc.project.alloc_id();
            let mut group = Layer::new(gid, m.label, LayerKind::Group { children: Vec::new(), collapsed: false }, &channels);
            for c in group.channels.values_mut() {
                *c = ChannelBlend { mode: BlendMode::Passthrough, opacity: 1.0 };
            }
            let gid = crate::layer_cmds::insert_selected(s, group)?;
            for l in layers {
                s.doc.project.insert_layer(ts, l, Some(gid), true)?;
            }
            s.selected.insert(ts, gid);
            gid
        };
        place.apply(s, top)?;
        Ok(top)
    })
}

impl Place {
    /// From `above` (+ `inside`) or `index` params; the selection by default.
    pub(crate) fn from_params(p: &serde_json::Value) -> Place {
        match (crate::commands::u64p(p, "above"), crate::commands::u64p(p, "index")) {
            (Some(above), _) => Place::Above(LayerId(above), crate::commands::boolp(p, "inside").unwrap_or(false)),
            (None, Some(i)) => Place::Index(usize::try_from(i).unwrap_or(usize::MAX)),
            (None, None) => Place::Selection,
        }
    }

    /// Move `top` (just added above the selection) to this place.
    pub(crate) fn apply(self, s: &mut Session, top: LayerId) -> Result<()> {
        match self {
            Place::Selection => {}
            Place::Above(target, inside) => s.doc.project.move_above(top, Some(target), inside)?,
            Place::Index(i) => {
                s.doc.project.move_above(top, None, false)?;
                s.doc.project.move_layer(top, i)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_labels_are_unique_and_values_physical() {
        let (mut ids, mut labels) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
        for m in MATERIALS {
            assert!(ids.insert(norm(m.id)), "duplicate id {}", m.id);
            assert!(labels.insert(norm(m.label)), "duplicate label {}", m.label);
            assert_eq!(find(m.label).map(|f| f.id), Some(m.id));
            if !m.smart {
                // Metals: measured F0 is at least ~0.45 in some channel; dielectrics: 0.01..0.9.
                let hi = m.base.iter().copied().fold(0.0, f32::max);
                if m.metallic == 1.0 {
                    assert!(hi >= 0.45, "{} too dark for a metal", m.id);
                } else {
                    assert_eq!(m.metallic, 0.0, "{} must be metal or dielectric", m.id);
                    assert!(m.base.iter().all(|v| (0.01..=0.9).contains(v)), "{} albedo out of range", m.id);
                }
            }
        }
        assert!(MATERIALS.iter().filter(|m| !m.smart).count() >= 40);
        assert!(MATERIALS.iter().filter(|m| m.smart).count() >= 15);
        assert!(find("Gold Polished").is_some());
        assert!(find("nope").is_none());
    }

    #[test]
    fn materials_land_where_they_are_dropped() {
        let mut s = Session::new_project("t", texturecraft_geom::primitives::sphere(1.0, 16, 8), "builtin:sphere", None, 128).unwrap();
        let run = |s: &mut Session, id: &str, p: serde_json::Value| LayerId(crate::execute(s, id, &p).unwrap()["layer"].as_u64().unwrap());
        let stack = |s: &Session| s.doc.project.texture_sets[0].stack.clone();
        let base = stack(&s);
        let a = run(&mut s, "layer.add.paint", serde_json::json!({}));
        let b = run(&mut s, "layer.add.paint", serde_json::json!({}));
        let m = run(&mut s, "material.apply", serde_json::json!({"material": "gold_polished", "above": a.0}));
        assert_eq!(stack(&s), [base.clone(), vec![a, m, b]].concat());
        // A multi-layer smart material (a folder) at the bottom of the stack.
        let n = run(&mut s, "material.apply", serde_json::json!({"material": "steel_painted_worn", "index": 0}));
        assert_eq!(stack(&s), [vec![n], base.clone(), vec![a, m, b]].concat());
        // Into a folder.
        let x = run(&mut s, "material.apply", serde_json::json!({"material": "copper", "above": n.0, "inside": true}));
        assert_eq!(s.doc.project.layers[&n].children().last(), Some(&x));
        s.undo().unwrap();
        assert!(!s.doc.project.layers[&n].children().contains(&x), "one undo step");
        // Default: above the selection.
        crate::execute(&mut s, "layer.select", &serde_json::json!({"layer": a.0})).unwrap();
        let y = run(&mut s, "material.apply", serde_json::json!({"material": "copper"}));
        assert_eq!(stack(&s), [vec![n], base, vec![a, y, m, b]].concat());
    }

    #[test]
    fn lobe_materials_add_their_channels_and_round_trip() {
        use Channel::*;
        let mut s = Session::new_project("t", texturecraft_geom::primitives::sphere(1.0, 16, 8), "builtin:sphere", None, 128).unwrap();
        let chans = |s: &Session| s.doc.project.texture_sets[0].channel_list();
        let before = chans(&s);
        let cases: [(&str, &[Channel]); 6] = [
            ("glass", &[Transmission]),
            ("car_paint", &[CoatWeight, CoatRoughness]),
            ("velvet", &[SheenWeight, SheenColor, SheenRoughness]),
            ("steel_brushed_aniso", &[AnisotropyLevel, AnisotropyAngle]),
            ("skin", &[SheenWeight, SheenRoughness, ScatterWeight]),
            ("wax", &[ScatterWeight]),
        ];
        for (id, lobes) in cases {
            apply(&mut s, id).unwrap();
            let added: Vec<Channel> = chans(&s).into_iter().filter(|c| !before.contains(c)).collect();
            assert_eq!(added, lobes, "{id}");
            s.undo().unwrap();
            assert_eq!(chans(&s), before, "{id}: one undo step");
        }
        apply(&mut s, "car_paint").unwrap();
        s.update();
        let coat = |s: &Session| s.caches[0].composite.value(CoatWeight, 64, 64)[0];
        assert!((coat(&s) - 1.0).abs() < 1e-3);
        let mut back = Session::from_project_bytes(&s.to_project_bytes().unwrap()).unwrap();
        back.update();
        assert_eq!(chans(&back), chans(&s));
        assert_eq!(coat(&back), coat(&s));
        crate::execute(&mut s, "textureSet.channel.remove", &serde_json::json!({"channel": "coatWeight"})).unwrap();
        assert!(!chans(&s).contains(&CoatWeight));
    }

    /// Base color, roughness and metallic at a grid of texels of texture set `ts`.
    fn snapshot(s: &Session, ts: usize) -> Vec<[f32; 3]> {
        let c = &s.caches[ts].composite;
        let n = c.resolution;
        (0..n)
            .step_by(3)
            .flat_map(|y| (0..n).step_by(3).map(move |x| (x, y)))
            .flat_map(|(x, y)| [Channel::BaseColor, Channel::Roughness, Channel::Metallic].map(|ch| c.value(ch, x, y)))
            .collect()
    }

    #[test]
    fn every_material_and_smart_mask_applies_on_the_sample() {
        let mut s = crate::sample_session(256, 256).unwrap();
        // A baseline no material matches, so every material must visibly change the composite.
        crate::execute(&mut s, "layer.add.fill", &serde_json::json!({"color": [1.0, 0.0, 1.0], "roughness": 0.123})).unwrap();
        s.update();
        let before = snapshot(&s, 0);
        for m in MATERIALS {
            apply(&mut s, m.id).unwrap_or_else(|e| panic!("{}: {e}", m.id));
            s.doc.project.validate().unwrap();
            s.update();
            let after = snapshot(&s, 0);
            assert!(after.iter().flatten().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)), "{} produced invalid values", m.id);
            assert_ne!(after, before, "{} changed nothing", m.id);
            s.undo().unwrap();
        }
        assert!(apply(&mut s, "nope").is_err());
        // Smart masks on the pedestal: it has edges and cavities (the ball is a plain sphere).
        s.active_set = 1;
        apply(&mut s, "gold_polished").unwrap();
        let layer = s.selected_layer().unwrap();
        for sm in crate::smart_masks::SMART_MASKS {
            crate::smart_masks::apply(&mut s, layer, sm.id).unwrap();
            s.update();
            let view = s.mask_view_rgba(layer, 64).unwrap();
            assert!(view.chunks(4).any(|px| px[0] > 0), "{} is empty", sm.id);
            assert!(snapshot(&s, 1).iter().flatten().all(|v| v.is_finite()), "{}", sm.id);
        }
        assert!(crate::smart_masks::apply(&mut s, layer, "nope").is_err());
    }
}
