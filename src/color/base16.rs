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
    scheme::Scheme,
    theme::Theme,
    utils::math::{difference_degrees, rotate_direction, sanitize_degrees_double},
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

/// Blends `accent` hue toward `source` hue by at most `max_degrees`.
///
/// Mirrors the formula used by `material_colors::blend::harmonize` but with
/// a configurable ceiling instead of the fixed 15° cap.
fn harmonize_hue(accent: Argb, source: Argb, max_degrees: f64) -> Argb {
    let accent_hct: Hct = accent.into();
    let source_hct: Hct = source.into();

    let diff = difference_degrees(accent_hct.get_hue(), source_hct.get_hue());
    let rotation = (diff * 0.5).min(max_degrees);
    let direction = rotate_direction(accent_hct.get_hue(), source_hct.get_hue());
    let new_hue = sanitize_degrees_double(rotation.mul_add(direction, accent_hct.get_hue()));

    Hct::from(new_hue, accent_hct.get_chroma(), accent_hct.get_tone()).into()
}

/// Applies the requested harmonization level to a single accent color.
fn apply_harmonization(accent: Argb, source: Argb, harmonization: &Harmonization) -> Argb {
    match harmonization {
        Harmonization::None => accent,
        Harmonization::Light => harmonize_hue(accent, source, 15.0),
        Harmonization::Moderate => harmonize_hue(accent, source, 30.0),
        Harmonization::Strong => harmonize_hue(accent, source, 45.0),
    }
}

pub fn generate_base16_scheme_from_palette(
    palette: &[Rgb],
    material_scheme: Option<&Scheme>,
    source_color: Option<Argb>,
    harmonization: &Harmonization,
    dark: bool,
) -> Result<IndexMap<String, Argb>, Report> {
    let mut scheme = IndexMap::new();

    // gray ramp: prefer the Material You primary/secondary swatch colors so
    // the ramp matches the palette the user sees in Material You output.
    // Falls back to linear RGB interpolation when no theme is available.
    let (base00, base05) = if let Some(s) = material_scheme {
        let base00 = s.surface_container_lowest;
        let base05 = s.primary;

        (rgb_from_argb(base00), rgb_from_argb(base05))
    } else {
        let mut sorted = palette.to_vec();
        sorted.sort_by(|a, b| luminance(b).partial_cmp(&luminance(a)).unwrap());
        let base00 = sorted.first().unwrap().clone();
        let base05 = sorted.last().unwrap().clone();
        (base00, base05)
    };

    let gray_ramp = interpolate_grays(&base00, &base05);

    for (name, color) in GRAY_NAMES.iter().zip(gray_ramp) {
        scheme.insert(name.to_string(), color);
    }

    let accents = assign_accents(palette, source_color, harmonization, dark);
    for (name, color) in ACCENT_NAMES.iter().zip(accents) {
        scheme.insert(name.to_string(), color);
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

    let dim: Rgb = Hsl::new(source_hue, source_sat * 0.3, source_lit * 1.5, None).into();
    let bright: Rgb = Hsl::new(source_hue, source_sat * 0.7, source_lit * 0.2, None).into();

    let (base00, base05) = if dark { (dim, bright) } else { (bright, dim) };

    let gray_ramp = interpolate_grays(&base00, &base05);
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
/// hue-targeted bucketing, then optionally harmonizes them toward the Material
/// You source color.
///
/// For each base16 accent slot the palette is searched for the chromatic color
/// whose HCT hue falls within the slot's range; among candidates the one with
/// the highest chroma wins. When no candidate exists a color is synthesized
/// from the source color's chroma and the slot's target hue. All accents are
/// then tone-normalized for readability before harmonization is applied.
fn assign_accents(
    palette: &[Rgb],
    source_color: Option<Argb>,
    harmonization: &Harmonization,
    dark: bool,
) -> [Argb; 8] {
    // pre-convert palette to HCT once
    let hct_palette: Vec<Hct> = palette
        .iter()
        .map(|c| argb_from_rgb(c).into())
        .filter(|c: &Hct| c.get_chroma() >= 20.)
        .collect();

    // a map from a base16 name to the given palette, colors sorted by the difference in hue from that of target accent slot.
    let mut leaderboards = ACCENT_NAMES
        .iter()
        .zip(ACCENT_SLOTS.iter())
        .map(|(name, slot)| {
            // pair each color with its score: the difference in degrees from the target hue
            let mut palette_scores = hct_palette
                .iter()
                .map(|hct| {
                    // multiplying by 10000 lets us cast this float to an integer with a precision of 4 decimal points
                    let score =
                        (difference_degrees(hct.get_hue(), slot.hue_center) * 10000.) as i64;

                    // modify the chroma of this color if the slot has a chroma_hint
                    let final_chroma = if let Some(c) = slot.chroma_factor {
                        hct.get_chroma() * c
                    } else {
                        hct.get_chroma()
                    };

                    // modify the tone if it is too dark or too bright
                    let final_tone = if dark {
                        hct.get_tone().max(50.)
                    } else {
                        hct.get_tone().min(50.)
                    };

                    let final_hct = Hct::from(hct.get_hue(), final_chroma, final_tone);

                    (final_hct, score)
                })
                .collect::<Vec<_>>();

            palette_scores.sort_by_key(|(_, score)| *score);

            (*name, palette_scores)
        })
        .collect::<Vec<_>>();

    // now, sort so that we address the scores that have the smallest differences first
    leaderboards.sort_by_key(|(_, palette_scores)| {
        *palette_scores
            .first()
            .map(|(_, score)| score)
            .unwrap_or(&i64::MAX)
    });

    // finally, we assign colors to names
    let mut assignments = HashMap::new();
    let mut colors_used = HashSet::new(); // keeps track of colors that already have an assignement
    for (name, palette_scores) in leaderboards.into_iter() {
        if let Some((color_to_assign, _)) = palette_scores
            .into_iter()
            .find(|(hct, _)| !colors_used.contains(&hct.to_string()))
        {
            let mut argb = Argb::from(color_to_assign);
            if let Some(src) = source_color {
                argb = apply_harmonization(argb, src, harmonization);
            }
            assignments.insert(name, argb);
            colors_used.insert(color_to_assign.to_string());
        }
    }

    ACCENT_NAMES.map(|name| assignments.remove(name).unwrap_or_default())
}

fn interpolate_grays(base00: &Rgb, base05: &Rgb) -> Vec<Argb> {
    let mut grays = Vec::new();
    let n = GRAY_NAMES.len();

    for i in 0..n {
        // we want to interpolate for 8 colors, but we're only given base00 and base05. therefore, to interpolate for base06 and base07, our denominator here needs to be 6 to correctly interpolate from base00 to base05 to base07.
        let t = i as f64 / 6.;

        let r = base00.red() + t * (base05.red() - base00.red());
        let g = base00.green() + t * (base05.green() - base00.green());
        let b = base00.blue() + t * (base05.blue() - base00.blue());

        grays.push(Argb::new(
            255,
            r.round().min(255.) as u8,
            g.round().min(255.) as u8,
            b.round().min(255.) as u8,
        ));
    }

    grays
}

pub fn generate_base16_schemes(
    source: &Source,
    backend: Backend,
    theme: Option<&Theme>,
    harmonization: &Harmonization,
) -> Result<Schemes, Report> {
    let schemes = match source {
        Source::Json { path: _ } => unreachable!(),
        Source::Image { path } => {
            let image = ImageReader::open(path)?
                .with_guessed_format()?
                .decode()?
                .to_rgb8();
            generate_base16_schemes_from_image(&image, backend, theme, harmonization).wrap_err(
                format!("Could not generate base16 scheme from image: {}", path),
            )?
        }
        Source::Color(color) => generate_base16_schemes_from_color(color).wrap_err(format!(
            "Could not generate base16 scheme from color: {}",
            color.get_string()
        ))?,
        #[cfg(feature = "web-image")]
        Source::WebImage { url } => {
            let bytes = reqwest::blocking::get(url)?.bytes()?;
            let image = image::load_from_memory(&bytes)?.to_rgb8();
            generate_base16_schemes_from_image(&image, backend, theme, harmonization).wrap_err(
                format!("Could not generate base16 scheme from image: {}", url),
            )?
        }
    };
    Ok(schemes)
}
pub fn generate_base16_schemes_from_image(
    image: &RgbImage,
    backend: Backend,
    theme: Option<&Theme>,
    harmonization: &Harmonization,
) -> Result<Schemes, Report> {
    let palette = backend.create().extract(image);
    let source_color = theme.map(|t| t.source);

    let dark_swatch = theme.map(|t| &t.schemes.dark);
    let dark_scheme = generate_base16_scheme_from_palette(
        &palette,
        dark_swatch,
        source_color,
        harmonization,
        true,
    )?;

    let light_swatch = theme.map(|t| &t.schemes.light);
    let light_scheme = generate_base16_scheme_from_palette(
        &palette,
        light_swatch,
        source_color,
        harmonization,
        false,
    )?;

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
