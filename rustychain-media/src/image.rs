use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageFormat};

use rustychain_core::{Error, MediaKind, MediaPayload, MediaPreprocessor, MimeType, Result};

/// What an [`ImagePreprocessor`] re-encodes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageOutput {
    Png,
    Jpeg,
}

impl ImageOutput {
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            ImageOutput::Png => "image/png",
            ImageOutput::Jpeg => "image/jpeg",
        }
    }
}

/// A host-configured image preprocessor.
///
/// It shrinks any image whose longest side exceeds `max_dimension` and keeps
/// re-encoding it until it fits within `max_bytes`. Images already within both
/// bounds are returned untouched, which makes the step idempotent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePreprocessor {
    pub max_dimension: u32,
    pub max_bytes: usize,
    pub output: ImageOutput,
    pub quality: u8,
}

impl Default for ImagePreprocessor {
    fn default() -> Self {
        Self {
            max_dimension: 1568,
            max_bytes: 4_000_000,
            output: ImageOutput::Jpeg,
            quality: 85,
        }
    }
}

impl ImagePreprocessor {
    #[must_use]
    pub fn new(max_dimension: u32, max_bytes: usize, output: ImageOutput, quality: u8) -> Self {
        Self {
            max_dimension,
            max_bytes,
            output,
            quality,
        }
    }

    fn encode(&self, image: &DynamicImage) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        match self.output {
            ImageOutput::Png => image
                .write_to(&mut Cursor::new(&mut buffer), ImageFormat::Png)
                .map_err(|error| Error::InvalidInput(format!("image encode: {error}")))?,
            ImageOutput::Jpeg => {
                let rgb = image.to_rgb8();
                let (width, height) = (rgb.width(), rgb.height());
                let mut encoder = JpegEncoder::new_with_quality(&mut buffer, self.quality);
                encoder
                    .encode(&rgb, width, height, ExtendedColorType::Rgb8)
                    .map_err(|error| Error::InvalidInput(format!("image encode: {error}")))?;
            }
        }
        Ok(buffer)
    }

    fn scale_to(image: &DynamicImage, longest: u32, target: u32) -> DynamicImage {
        let width = scaled(image.width(), longest, target);
        let height = scaled(image.height(), longest, target);
        image.resize_exact(width, height, FilterType::Lanczos3)
    }
}

fn scaled(dimension: u32, longest: u32, target: u32) -> u32 {
    let numerator = u64::from(dimension) * u64::from(target) + u64::from(longest) / 2;
    let value = numerator / u64::from(longest);
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

#[async_trait::async_trait]
impl MediaPreprocessor for ImagePreprocessor {
    async fn preprocess(&self, payload: MediaPayload) -> Result<MediaPayload> {
        if payload.kind != MediaKind::Image {
            return Ok(payload);
        }

        let decoded = image::load_from_memory(&payload.bytes)
            .map_err(|error| Error::InvalidInput(format!("image decode: {error}")))?;
        let longest = decoded.width().max(decoded.height());
        let within_bytes = payload.bytes.len() <= self.max_bytes;
        if within_bytes && longest <= self.max_dimension {
            return Ok(payload);
        }

        let mut current = if longest > self.max_dimension {
            Self::scale_to(&decoded, longest, self.max_dimension)
        } else {
            decoded
        };
        let mut encoded = self.encode(&current)?;

        let mut attempts = 0;
        while encoded.len() > self.max_bytes && attempts < 8 {
            let (width, height) = (current.width(), current.height());
            let next_width = (width - width / 4).max(1);
            let next_height = (height - height / 4).max(1);
            if next_width == width && next_height == height {
                break;
            }
            current = current.resize_exact(next_width, next_height, FilterType::Lanczos3);
            encoded = self.encode(&current)?;
            attempts += 1;
        }

        let mime = MimeType::new(self.output.mime())?;
        Ok(MediaPayload::new(MediaKind::Image, mime, encoded))
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageFormat, Rgb, RgbImage};

    use super::*;

    fn encoded_png(width: u32, height: u32) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for pixel in image.pixels_mut() {
            *pixel = Rgb([12, 34, 56]);
        }
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode png");
        bytes
    }

    fn image_payload(bytes: Vec<u8>) -> MediaPayload {
        MediaPayload::new(
            MediaKind::Image,
            MimeType::new("image/png").expect("mime"),
            bytes,
        )
    }

    #[tokio::test]
    async fn oversized_images_are_resized_and_reencoded() {
        let preprocessor = ImagePreprocessor::new(256, 200_000, ImageOutput::Jpeg, 80);
        let payload = image_payload(encoded_png(2000, 1000));

        let processed = preprocessor.preprocess(payload).await.expect("preprocess");

        assert_eq!(processed.mime.as_str(), "image/jpeg");
        let decoded = image::load_from_memory(&processed.bytes).expect("decode");
        assert!(decoded.width().max(decoded.height()) <= 256);
    }

    #[tokio::test]
    async fn images_within_bounds_pass_through_unchanged() {
        let preprocessor = ImagePreprocessor::default();
        let payload = image_payload(encoded_png(16, 16));

        let processed = preprocessor
            .preprocess(payload.clone())
            .await
            .expect("preprocess");

        assert_eq!(processed, payload);
    }

    #[tokio::test]
    async fn preprocessing_is_idempotent() {
        let preprocessor = ImagePreprocessor::new(256, 200_000, ImageOutput::Jpeg, 80);
        let once = preprocessor
            .preprocess(image_payload(encoded_png(2000, 1000)))
            .await
            .expect("first pass");

        let twice = preprocessor
            .preprocess(once.clone())
            .await
            .expect("second pass");

        assert_eq!(twice, once, "a second pass must not shrink again");
    }

    #[tokio::test]
    async fn non_image_payloads_are_ignored() {
        let preprocessor = ImagePreprocessor::default();
        let payload = MediaPayload::new(
            MediaKind::Audio,
            MimeType::new("audio/wav").expect("mime"),
            vec![0, 1, 2, 3],
        );

        let processed = preprocessor
            .preprocess(payload.clone())
            .await
            .expect("preprocess");

        assert_eq!(processed, payload);
    }
}
