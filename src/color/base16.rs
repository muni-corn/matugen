use crate::{
    color::{
        backend::{celebi::CelebiBackend, wal::WalBackend},
        color::{get_source_color_from_color, ColorFormat, Source},
        format::{argb_from_rgb, rgb_from_argb},
        math::{luminance, saturation},
    },
    scheme::Schemes,
};
use color_eyre::{eyre::WrapErr, Report};
use colorsys::{Hsl, Rgb};
use image::{ImageReader, RgbImage};
use indexmap::IndexMap;
use material_colors::{
    color::Argb,
    hct::Hct,
    palette::TonalPalette,
    theme::Theme,
    utils::math::{difference_degrees, rotate_direction, sanitize_degrees_double},
};
use serde::{Deserialize, Serialize};

/// Strength of hue blending applied to extracted accent colors toward the
/// Material You source color. Higher harmonization reduces jarring color
/// clashes at the cost of rainbow diversity.
#[derive(Debug, Clone, Default, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harmonization {
    /// No blending; accent hues are used exactly as extracted or synthesized.
    None,
    /// Gentle blend of up to 15° toward the source hue (default).
    #[default]
    Light,
    /// Stronger blend of up to 30° toward the source hue.
    Moderate,
    /// Strongest blend of up to 45° toward the source hue.
    Strong,
}

const GRAY_NAMES: [&str; 8] = [
    "base00", "base01", "base02", "base03", "base04", "base05", "base06", "base07",
];

const ACCENT_NAMES: [&str; 8] = [
    "base08", "base09", "base0a", "base0b", "base0c", "base0d", "base0e", "base0f",
];

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum Backend {
    /// K-means clustering in RGB space (original wal-inspired extractor).
    Wal,
    /// Material You's own QuantizerCelebi (Wu + WSMeans) extractor.
    Celebi,
}

impl Backend {
    pub fn create(&self) -> Box<dyn PaletteBackend> {
        match self {
            Backend::Wal => Box::new(WalBackend::default()),
            Backend::Celebi => Box::new(CelebiBackend::default()),
        }
    }
}

pub trait PaletteBackend {
    fn extract(&self, image: &RgbImage) -> Vec<Rgb>;
}

fn drag_hue(source_hue: f64, target_hue: f64, amount: f64) -> f64 {
    let rot_deg = difference_degrees(source_hue, target_hue);
    let rot_dir = rotate_direction(source_hue, target_hue) * amount;
    sanitize_degrees_double(rot_deg.mul_add(rot_dir, source_hue))
}

pub fn generate_base16_scheme_from_palette(
    palette: &[Rgb],
    neutral: Option<&TonalPalette>,
    source_color: Option<Argb>,
    dark: bool,
) -> Result<IndexMap<String, Argb>, Report> {
    let mut scheme = IndexMap::new();

    // gray ramp: prefer the Material You neutral tonal palette when available
    // so that the steps are perceptually uniform and source-color-tinted
    let gray_ramp = match neutral {
        Some(pal) => grays_from_tonal_palette(pal, dark),
        None => {
            let mut sorted = palette.to_vec();
            sorted.sort_by(|a, b| luminance(b).partial_cmp(&luminance(a)).unwrap());
            let base00 = sorted.first().unwrap().clone();
            let base05 = sorted.last().unwrap().clone();
            interpolate_grays(&base00, &base05, dark)
        }
    };

    for (i, &name) in GRAY_NAMES.iter().enumerate() {
        scheme.insert(name.to_string(), gray_ramp[i]);
    }

    let accents = assign_accents(palette, source_color, dark);
    for (i, &name) in ACCENT_NAMES.iter().enumerate() {
        scheme.insert(name.to_string(), accents[i]);
    }

    Ok(scheme)
}

pub fn generate_base16_scheme_from_color(
    color: &Rgb,
    dark: bool,
) -> Result<IndexMap<String, Argb>, Report> {
    let mut scheme = IndexMap::new();

    let hsl: Hsl = color.into();
    let (source_hue, source_sat, source_lit) = (hsl.hue(), hsl.saturation(), hsl.lightness());
    let base00: Rgb = Hsl::new(source_hue, source_sat * 0.3, source_lit * 1.5, None).into();
    let base05: Rgb = Hsl::new(source_hue, source_sat * 0.7, source_lit * 0.2, None).into();

    let gray_ramp = interpolate_grays(&base00, &base05, dark);
    for (i, &name) in GRAY_NAMES.iter().enumerate() {
        scheme.insert(name.to_string(), gray_ramp[i]);
    }

    let hct: Hct = argb_from_rgb(color).into();
    let source_chroma = hct.get_chroma();
    let source_tone = hct.get_tone();
    let pri_hue = hct.get_hue();
    let acc_hue = pri_hue + 60.0;
    let red_hue = drag_hue(pri_hue, 25.0, 0.8);
    let grn_hue = drag_hue(pri_hue, 118.0, 0.8);
    let off_hue = 10.0_f64.mul_add(rotate_direction(red_hue, pri_hue), red_hue);
    let main_chroma = source_chroma.max(80.0);
    let mute_chroma = main_chroma / 2.0;
    let depr_chroma = (source_chroma / 6.0).min(10.0);
    let main_tone = source_tone.mul_add(0.3, 50.0);
    let depr_tone = source_tone.mul_add(0.5, 20.0);
    let accent_parameters = [
        (red_hue, main_chroma, main_tone), // Semantics: Variables, Diff Deleted
        (off_hue, mute_chroma, main_tone), // Semantics: Literals
        (pri_hue, mute_chroma, main_tone), // Semantics: Classes
        (grn_hue, main_chroma, main_tone), // Semantics: Strings, Diff Inserted
        (acc_hue, mute_chroma, main_tone), // Semantics: Escape Characters
        (pri_hue, main_chroma, main_tone), // Semantics: Functions
        (acc_hue, main_chroma, main_tone), // Semantics: Keywords, Diff Changed
        (pri_hue, depr_chroma, depr_tone), // Semantics: Deprecated
    ];

    for (i, &name) in ACCENT_NAMES.iter().enumerate() {
        let (hue, chroma, tone) = accent_parameters[i];
        scheme.insert(name.to_string(), Hct::from(hue, chroma, tone).into());
    }

    Ok(scheme)
}

/// One of the eight base16 accent slots with its target hue window.
///
/// `hue_min` and `hue_max` are in HCT degrees (0–360). For red the range
/// wraps through 0°, so `hue_min > hue_max` is the wrap-around sentinel.
struct AccentSlot {
    hue_center: f64,

    /// If `Some`, multiply the original color's chroma by this amount
    chroma_factor: Option<f64>,
}

/// The eight accent slot definitions for base08–base0F in HCT hue order.
///
/// Hue ranges are based on the Munsell/CAM16 color wheel as implemented in
/// HCT. base0F (brown) is a low-chroma warm color rather than a distinct hue.
const ACCENT_SLOTS: [AccentSlot; 8] = [
    // base08 – red (variables, diff deleted)
    AccentSlot {
        hue_center: 10.0,
        chroma_factor: None,
    },
    // base09 – orange (integers, constants)
    AccentSlot {
        hue_center: 40.0,
        chroma_factor: None,
    },
    // base0A – yellow (classes, search highlight)
    AccentSlot {
        hue_center: 65.0,
        chroma_factor: None,
    },
    // base0B – green (strings, diff inserted)
    AccentSlot {
        hue_center: 115.0,
        chroma_factor: None,
    },
    // base0C – cyan (regex, escape characters)
    AccentSlot {
        hue_center: 185.0,
        chroma_factor: None,
    },
    // base0D – blue (functions, methods)
    AccentSlot {
        hue_center: 245.0,
        chroma_factor: None,
    },
    // base0E – purple/magenta (keywords, diff changed)
    AccentSlot {
        hue_center: 295.0,
        chroma_factor: None,
    },
    // base0F – brown/deprecated (low chroma, warm hue)
    AccentSlot {
        hue_center: 25.0,
        chroma_factor: Some(0.5), // intentionally muted
    },
];

/// Assigns the eight accent colors (base08–base0F) from a raw palette using
/// hue-targeted bucketing.
///
/// For each base16 accent slot the palette is searched for the chromatic color
/// whose HCT hue falls within the slot's range; among candidates the one with
/// the highest chroma wins. When no candidate exists a color is synthesized
/// from the source color's chroma and the slot's target hue. All accents are
/// then tone-normalized for readability.
fn assign_accents(palette: &[Rgb], source_color: Option<Argb>, dark: bool) -> [Argb; 8] {
    // minimum chroma to be considered a chromatic (non-neutral) color
    const MIN_CHROMA: f64 = 12.0;
    // chroma to use when synthesising a completely new accent
    const SYNTH_CHROMA: f64 = 48.0;
    // clamp range for extracted chroma so accents are not too washed out/vivid
    const CHROMA_MIN: f64 = 20.0;
    const CHROMA_MAX: f64 = 80.0;

    // derive a fallback source HCT so synthesised colors inherit source style
    let source_hct: Hct = source_color.unwrap_or(Argb::new(255, 128, 128, 128)).into();
    let source_chroma = source_hct.get_chroma().clamp(SYNTH_CHROMA, CHROMA_MAX);

    // pre-convert palette to HCT once
    let hct_palette: Vec<Hct> = palette.iter().map(|c| argb_from_rgb(c).into()).collect();

    let slots = ACCENT_SLOTS;
    let mut out = [Argb::new(255, 0, 0, 0); 8];

    for (idx, slot) in slots.iter().enumerate() {
        let target_tone = if dark {
            slot.tone_dark
        } else {
            slot.tone_light
        };
        let target_chroma = slot.chroma_hint.unwrap_or(source_chroma);

        // find the most chromatic candidate in this hue bucket
        let best = hct_palette
            .iter()
            .filter(|h| h.get_chroma() >= MIN_CHROMA)
            .filter(|h| hue_in_range(h.get_hue(), slot.hue_min, slot.hue_max))
            .max_by(|a, b| {
                a.get_chroma()
                    .partial_cmp(&b.get_chroma())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        let (hue, chroma) = match best {
            Some(h) => {
                // keep the extracted hue; clamp chroma to a sane range
                let c = if slot.chroma_hint.is_some() {
                    slot.chroma_hint.unwrap()
                } else {
                    h.get_chroma().clamp(CHROMA_MIN, CHROMA_MAX)
                };
                (h.get_hue(), c)
            }
            // no image color in this hue range — synthesize from slot center
            None => (slot.hue_center, target_chroma),
        };

        out[idx] = Hct::from(hue, chroma, target_tone).into();
    }

    ACCENT_NAMES.map(|name| assignments.remove(name).unwrap_or_default())
}

fn interpolate_grays(base00: &Rgb, base05: &Rgb, dark: bool) -> Vec<Argb> {
    let mut grays = Vec::new();
    let n = GRAY_NAMES.len();

    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let r = base00.red() as f32 + t * (base05.red() as f32 - base00.red() as f32);
        let g = base00.green() as f32 + t * (base05.green() as f32 - base00.green() as f32);
        let b = base00.blue() as f32 + t * (base05.blue() as f32 - base00.blue() as f32);
        grays.push(Argb::new(
            255,
            r.round() as u8,
            g.round() as u8,
            b.round() as u8,
        ));
    }

    if dark {
        grays.reverse();
    }

    grays
}

/// Builds the 8-step gray ramp (base00–base07) from a Material You neutral
/// tonal palette using perceptually-uniform HCT tones.
///
/// Dark theme tones run from near-black (6) to near-white (96), covering the
/// full range editors need for backgrounds, comments, and foregrounds.
/// Light theme tones are the mirror image.
fn grays_from_tonal_palette(neutral: &TonalPalette, dark: bool) -> Vec<Argb> {
    // tone values chosen to give clearly-distinct steps in a typical editor
    let tones: [i32; 8] = if dark {
        [6, 10, 16, 28, 55, 80, 90, 96]
    } else {
        [98, 93, 88, 73, 45, 20, 10, 4]
    };

    tones.iter().map(|&t| neutral.tone(t)).collect()
}

pub fn generate_base16_schemes(
    source: &Source,
    backend: Backend,
    theme: Option<&Theme>,
) -> Result<Schemes, Report> {
    let schemes = match source {
        Source::Json { path: _ } => unreachable!(),
        Source::Image { path } => {
            let image = ImageReader::open(path)?
                .with_guessed_format()?
                .decode()?
                .to_rgb8();
            generate_base16_schemes_from_image(&image, backend, theme).wrap_err(format!(
                "Could not generate base16 scheme from image: {}",
                path
            ))?
        }
        Source::Color(color) => generate_base16_schemes_from_color(color).wrap_err(format!(
            "Could not generate base16 scheme from color: {}",
            color.get_string()
        ))?,
        #[cfg(feature = "web-image")]
        Source::WebImage { url } => {
            let bytes = reqwest::blocking::get(url)?.bytes()?;
            let image = image::load_from_memory(&bytes)?.to_rgb8();
            generate_base16_schemes_from_image(&image, backend, theme).wrap_err(format!(
                "Could not generate base16 scheme from image: {}",
                url
            ))?
        }
    };
    Ok(schemes)
}

pub fn generate_base16_schemes_from_image(
    image: &RgbImage,
    backend: Backend,
    theme: Option<&Theme>,
) -> Result<Schemes, Report> {
    let palette = backend.create().extract(image);
    let neutral = theme.map(|t| &t.palettes.neutral);
    let source_color = theme.map(|t| t.source);

    let dark_scheme = generate_base16_scheme_from_palette(&palette, neutral, source_color, true)?;
    let light_scheme = generate_base16_scheme_from_palette(&palette, neutral, source_color, false)?;

    Ok(Schemes {
        dark: dark_scheme,
        light: light_scheme,
    })
}

pub fn generate_base16_schemes_from_color(color: &ColorFormat) -> Result<Schemes, Report> {
    let source_color = rgb_from_argb(get_source_color_from_color(color)?);

    let dark_scheme = generate_base16_scheme_from_color(&source_color, true)?;
    let light_scheme = generate_base16_scheme_from_color(&source_color, false)?;

    Ok(Schemes {
        dark: dark_scheme,
        light: light_scheme,
    })
}
