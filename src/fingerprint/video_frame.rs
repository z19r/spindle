//! Keyframe perceptual hashing for videos via ffmpeg (when installed).
//! The resulting phash joins the same near-duplicate pool as images,
//! so re-encodes of the same clip — and video↔image matches — surface
//! with zero extra machinery.

use std::path::Path;
use std::sync::OnceLock;

fn ffmpeg_available() -> bool {
  static AVAILABLE: OnceLock<bool> = OnceLock::new();
  *AVAILABLE.get_or_init(|| {
    let ok = std::process::Command::new("ffmpeg")
      .arg("-version")
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status()
      .map(|s| s.success())
      .unwrap_or(false);
    if !ok {
      tracing::info!(
        "ffmpeg not found — video similarity detection disabled, \
         exact-hash matching still applies"
      );
    }
    ok
  })
}

/// Perceptual hash of an early keyframe, or `None` when ffmpeg is
/// missing or the file can't be decoded.
pub fn keyframe_phash(path: &Path) -> Option<Vec<u8>> {
  if !ffmpeg_available() {
    return None;
  }

  let tmp = std::env::temp_dir().join(format!(
    "spindle-frame-{}-{}.png",
    std::process::id(),
    // Distinguish concurrent extractions within one process.
    path
      .file_name()
      .map(|n| blake3::hash(n.to_string_lossy().as_bytes())
        .to_hex()
        .to_string())
      .unwrap_or_default()
  ));

  let status = std::process::Command::new("ffmpeg")
    .args(["-y", "-ss", "1", "-i"])
    .arg(path)
    .args(["-frames:v", "1", "-vf", "scale=256:-1"])
    .arg(&tmp)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .ok()?;
  if !status.success() || !tmp.exists() {
    let _ = std::fs::remove_file(&tmp);
    return None;
  }

  let img = image::open(&tmp).ok();
  let _ = std::fs::remove_file(&tmp);
  let img = img?;

  let hasher = image_hasher::HasherConfig::new().to_hasher();
  Some(hasher.hash_image(&img).as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unreadable_video_yields_none() {
    // Regardless of whether ffmpeg is installed, garbage bytes must
    // never produce a hash.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("fake.mp4");
    std::fs::write(&path, b"not a real video").unwrap();

    assert!(keyframe_phash(&path).is_none());
  }
}
