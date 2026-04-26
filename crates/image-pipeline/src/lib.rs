use image::imageops::FilterType;
use reader_core::{DecodedImage, ImageDecoder, ReaderError, Result};

#[derive(Debug, Clone)]
pub struct ImageCrateDecoder {
    thumbnail_filter: FilterType,
}

impl Default for ImageCrateDecoder {
    fn default() -> Self {
        Self {
            thumbnail_filter: FilterType::Triangle,
        }
    }
}

impl ImageCrateDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn decode_webp(&self, bytes: &[u8]) -> Result<Option<DecodedImage>> {
        if !is_webp(bytes) {
            return Ok(None);
        }

        let image = libwebp_image::webp_load_rgba_from_memory(bytes)
            .map_err(|error| ReaderError::Decode(format!("native WebP decode failed: {error}")))?;
        let (width, height) = image.dimensions();

        Ok(Some(DecodedImage {
            width,
            height,
            rgba: image.into_raw(),
        }))
    }

    fn decode_dynamic(&self, bytes: &[u8]) -> Result<image::DynamicImage> {
        image::load_from_memory(bytes)
            .map_err(|error| ReaderError::Decode(format!("image decode failed: {error}")))
    }
}

impl ImageDecoder for ImageCrateDecoder {
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        if let Some(image) = self.decode_webp(bytes)? {
            return Ok(image);
        }

        let image = self.decode_dynamic(bytes)?.to_rgba8();
        let (width, height) = image.dimensions();

        Ok(DecodedImage {
            width,
            height,
            rgba: image.into_raw(),
        })
    }

    fn thumbnail(&self, bytes: &[u8], max_edge: u32) -> Result<DecodedImage> {
        if let Some(image) = self.decode_webp(bytes)? {
            return self.thumbnail_from_decoded(&image, max_edge);
        }

        let image = self.decode_dynamic(bytes)?;
        let thumbnail = image
            .resize(max_edge, max_edge, self.thumbnail_filter)
            .to_rgba8();
        let (width, height) = thumbnail.dimensions();

        Ok(DecodedImage {
            width,
            height,
            rgba: thumbnail.into_raw(),
        })
    }

    fn thumbnail_from_decoded(&self, image: &DecodedImage, max_edge: u32) -> Result<DecodedImage> {
        let rgba = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
            .ok_or_else(|| ReaderError::Decode("invalid decoded RGBA buffer".to_string()))?;
        let thumbnail = image::DynamicImage::ImageRgba8(rgba)
            .resize(max_edge, max_edge, self.thumbnail_filter)
            .to_rgba8();
        let (width, height) = thumbnail.dimensions();

        Ok(DecodedImage {
            width,
            height,
            rgba: thumbnail.into_raw(),
        })
    }
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}
