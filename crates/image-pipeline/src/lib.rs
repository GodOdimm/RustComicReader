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

    fn decode_dynamic(&self, bytes: &[u8]) -> Result<image::DynamicImage> {
        image::load_from_memory(bytes)
            .map_err(|error| ReaderError::Decode(format!("image decode failed: {error}")))
    }
}

impl ImageDecoder for ImageCrateDecoder {
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage> {
        let image = self.decode_dynamic(bytes)?.to_rgba8();
        let (width, height) = image.dimensions();

        Ok(DecodedImage {
            width,
            height,
            rgba: image.into_raw(),
        })
    }

    fn thumbnail(&self, bytes: &[u8], max_edge: u32) -> Result<DecodedImage> {
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
}
