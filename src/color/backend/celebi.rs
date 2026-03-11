use crate::color::base16::PaletteBackend;
use colorsys::Rgb;
use image::{imageops::FilterType, RgbImage};
use material_colors::{
    color::Argb,
    quantize::{Quantizer, QuantizerCelebi},
};

/// Palette backend that uses Material You's own QuantizerCelebi (Wu + WSMeans)
/// to extract colors, producing results that are stylistically consistent with
/// the Material You pipeline.
pub struct CelebiBackend {
    pub max_colors: usize,
    pub resize_width: u32,
}

impl Default for CelebiBackend {
    fn default() -> Self {
        Self {
            max_colors: 16,
            resize_width: 300,
        }
    }
}

impl PaletteBackend for CelebiBackend {
    fn extract(&self, image: &RgbImage) -> Vec<Rgb> {
        let resized = resize_image(image, self.resize_width);

        // convert pixels to Argb (fully opaque)
        let pixels: Vec<Argb> = resized
            .pixels()
            .map(|p| Argb::new(255, p[0], p[1], p[2]))
            .collect();

        if pixels.is_empty() {
            return Vec::new();
        }

        let result = QuantizerCelebi::quantize(&pixels, self.max_colors);

        // sort by population (most frequent first) for stable ordering
        let mut color_counts: Vec<(Argb, u32)> = result.color_to_count.into_iter().collect();
        color_counts.sort_by(|a, b| b.1.cmp(&a.1));

        color_counts
            .into_iter()
            .map(|(argb, _)| Rgb::new(argb.red.into(), argb.green.into(), argb.blue.into(), None))
            .collect()
    }
}

/// Resizes the image to a square for color extraction.
fn resize_image(image: &RgbImage, target_width: u32) -> RgbImage {
    image::imageops::resize(image, target_width, target_width, FilterType::Triangle)
}
