//! Make an image safe to upload to the Messages API: the API accepts
//! only JPEG/PNG/GIF/WebP, rejects very large files and dimensions, and
//! anything beyond ~1.5k px on the long edge is wasted tokens. Small
//! images in an accepted format pass through untouched; everything else
//! is decoded (ImageMagick fallback for HEIC/AVIF), downscaled, and
//! re-encoded as JPEG.

use std::path::Path;

use anyhow::{Context, Result};

/// Long-edge cap for uploads. Matches the API's recommended maximum.
pub const MAX_EDGE: u32 = 1568;

/// Original bytes larger than this are re-encoded even if the format
/// is accepted. Comfortably under the API's per-image limit.
pub const MAX_PASSTHROUGH_BYTES: u64 = 3 * 1024 * 1024;

/// JPEG quality for re-encoded uploads.
const JPEG_QUALITY: u8 = 85;

/// MIME types the API accepts as-is.
pub const ACCEPTED_MIME: &[&str] =
  &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Bytes ready to upload plus the MIME type to declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
  pub data: Vec<u8>,
  pub mime_type: &'static str,
  /// True when the original bytes were sent unchanged.
  pub passthrough: bool,
}

/// Prepare `path` (declared as `mime_type`) for upload.
pub fn prepare_for_upload(
  path: &Path,
  mime_type: &str,
) -> Result<PreparedImage> {
  let size = std::fs::metadata(path)
    .with_context(|| format!("Cannot stat image {}", path.display()))?
    .len();

  if let Some(mime) = ACCEPTED_MIME.iter().find(|m| **m == mime_type)
  {
    if size <= MAX_PASSTHROUGH_BYTES {
      if let Ok((w, h)) = image::image_dimensions(path) {
        if w.max(h) <= MAX_EDGE {
          let data = std::fs::read(path).with_context(|| {
            format!("Cannot read image {}", path.display())
          })?;
          return Ok(PreparedImage {
            data,
            mime_type: mime,
            passthrough: true,
          });
        }
      }
    }
  }

  let img = image::open(path)
    .or_else(|crate_err| {
      crate::fingerprint::decode_via_magick(path, MAX_EDGE)
        .with_context(|| format!("image crate: {crate_err}"))
    })
    .with_context(|| {
      format!("Cannot decode image {}", path.display())
    })?;

  let img = if img.width().max(img.height()) > MAX_EDGE {
    img.resize(
      MAX_EDGE,
      MAX_EDGE,
      image::imageops::FilterType::Triangle,
    )
  } else {
    img
  };

  let mut data = Vec::new();
  let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
    &mut data,
    JPEG_QUALITY,
  );
  img.to_rgb8().write_with_encoder(encoder).with_context(|| {
    format!("Cannot encode {} as JPEG", path.display())
  })?;

  Ok(PreparedImage {
    data,
    mime_type: "image/jpeg",
    passthrough: false,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn write_image(
    dir: &TempDir,
    name: &str,
    w: u32,
    h: u32,
    format: image::ImageFormat,
  ) -> std::path::PathBuf {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
      image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    let path = dir.path().join(name);
    image::DynamicImage::ImageRgb8(img)
      .save_with_format(&path, format)
      .unwrap();
    path
  }

  fn dims(data: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(data).unwrap();
    (img.width(), img.height())
  }

  #[test]
  fn small_png_passes_through_unchanged() {
    let dir = TempDir::new().unwrap();
    let path =
      write_image(&dir, "a.png", 64, 48, image::ImageFormat::Png);
    let original = std::fs::read(&path).unwrap();

    let out = prepare_for_upload(&path, "image/png").unwrap();

    assert!(out.passthrough);
    assert_eq!(out.mime_type, "image/png");
    assert_eq!(out.data, original);
  }

  #[test]
  fn oversized_jpeg_is_downscaled_to_max_edge() {
    let dir = TempDir::new().unwrap();
    let path = write_image(
      &dir,
      "big.jpg",
      4000,
      1000,
      image::ImageFormat::Jpeg,
    );

    let out = prepare_for_upload(&path, "image/jpeg").unwrap();

    assert!(!out.passthrough);
    assert_eq!(out.mime_type, "image/jpeg");
    assert_eq!(dims(&out.data), (MAX_EDGE, 392));
  }

  #[test]
  fn tall_image_scales_by_long_edge() {
    let dir = TempDir::new().unwrap();
    let path = write_image(
      &dir,
      "tall.png",
      500,
      3136,
      image::ImageFormat::Png,
    );

    let out = prepare_for_upload(&path, "image/png").unwrap();

    assert_eq!(dims(&out.data), (250, MAX_EDGE));
  }

  #[test]
  fn unaccepted_format_is_transcoded_to_jpeg() {
    let dir = TempDir::new().unwrap();
    // BMP and TIFF decode natively; the API accepts neither.
    for (name, fmt, mime) in [
      ("x.bmp", image::ImageFormat::Bmp, "image/bmp"),
      ("x.tiff", image::ImageFormat::Tiff, "image/tiff"),
    ] {
      let path = write_image(&dir, name, 100, 80, fmt);
      let out = prepare_for_upload(&path, mime).unwrap();
      assert!(!out.passthrough, "{name}");
      assert_eq!(out.mime_type, "image/jpeg", "{name}");
      assert_eq!(dims(&out.data), (100, 80), "{name}");
      assert_eq!(&out.data[..2], &[0xFF, 0xD8], "{name} not JPEG");
    }
  }

  /// Real HEIC through the ImageMagick fallback. Needs `magick` with
  /// libheif and a sample path in `SPINDLE_HEIC_SAMPLE`.
  #[test]
  #[ignore = "needs magick+libheif and SPINDLE_HEIC_SAMPLE"]
  fn heic_sample_is_transcoded_to_jpeg() {
    let sample = std::env::var("SPINDLE_HEIC_SAMPLE")
      .expect("SPINDLE_HEIC_SAMPLE");
    let out =
      prepare_for_upload(Path::new(&sample), "image/heic").unwrap();
    assert!(!out.passthrough);
    assert_eq!(out.mime_type, "image/jpeg");
    let (w, h) = dims(&out.data);
    assert!(w > 0 && h > 0 && w.max(h) <= MAX_EDGE, "{w}x{h}");
  }

  #[test]
  fn undecodable_file_is_an_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("junk.heic");
    std::fs::write(&path, b"not an image at all").unwrap();

    let err = prepare_for_upload(&path, "image/heic").unwrap_err();

    assert!(err.to_string().contains("junk.heic"), "{err:#}");
  }
}
