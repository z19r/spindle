use std::path::Path;

use anyhow::{Context, Result};

pub struct ExtractedFrame {
  pub png_data: Vec<u8>,
  pub timestamp_secs: f64,
}

/// Is `ffmpeg` on PATH? Probed once per process.
pub fn ffmpeg_available() -> bool {
  static AVAILABLE: std::sync::OnceLock<bool> =
    std::sync::OnceLock::new();
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
        "ffmpeg not found — video keyframes will not be analyzed"
      );
    }
    ok
  })
}

pub async fn check_ffmpeg() -> bool {
  tokio::process::Command::new("ffmpeg")
    .arg("-version")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .await
    .map(|s| s.success())
    .unwrap_or(false)
}

/// How many decode threads to hand one ffmpeg child when `concurrency`
/// of them run at once.
///
/// ffmpeg defaults to one thread per core, so N children on an N-core
/// box between them ask for N² threads. Measured on 16 cores against a
/// 1080x1920 10-bit HEVC clip, 16 concurrent extractions: uncapped
/// burns 9.4s of CPU in 0.75s of wall clock, capped at one thread each
/// it finishes *sooner* (0.49s) for 5.9s of CPU. Oversubscription
/// costs throughput as well as cores, so aim for roughly one thread
/// per core across all the children.
pub fn thread_budget(concurrency: usize) -> usize {
  let cores = std::thread::available_parallelism()
    .map(|n| n.get())
    .unwrap_or(1);
  (cores / concurrency.max(1)).max(1)
}

pub async fn get_duration(path: &Path) -> Result<f64> {
  let output = tokio::process::Command::new("ffprobe")
    .args([
      "-v",
      "error",
      "-show_entries",
      "format=duration",
      "-of",
      "csv=p=0",
    ])
    .arg(path)
    .output()
    .await
    .with_context(|| {
      format!("Failed to run ffprobe on {}", path.display())
    })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
      "ffprobe failed for {}: {}",
      path.display(),
      stderr.trim()
    );
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  stdout.trim().parse::<f64>().with_context(|| {
    format!("Failed to parse duration from ffprobe: {stdout:?}")
  })
}

pub async fn extract_frame_at(
  path: &Path,
  timestamp_secs: f64,
  threads: usize,
) -> Result<Vec<u8>> {
  // Downscale inside ffmpeg, to the same edge the still-image path
  // uploads. Left alone, a 4K 10-bit source encodes to a 16-bit PNG of
  // several megabytes that is piped back and base64'd for nothing: the
  // model never sees more than `MAX_EDGE` either way.
  let max = crate::analyze::image_prep::MAX_EDGE;
  let scale = format!(
    "scale='min({max},iw)':'min({max},ih)'\
     :force_original_aspect_ratio=decrease"
  );
  let output = tokio::process::Command::new("ffmpeg")
    .args(["-threads", &threads.max(1).to_string()])
    .args(["-ss", &format!("{timestamp_secs:.3}"), "-i"])
    .arg(path)
    .args([
      "-frames:v",
      "1",
      "-vf",
      scale.as_str(),
      "-pix_fmt",
      "rgb24",
      "-f",
      "image2pipe",
      "-vcodec",
      "png",
      "-",
    ])
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()
    .await
    .with_context(|| {
      format!(
        "Failed to run ffmpeg on {} at {timestamp_secs}s",
        path.display()
      )
    })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
      "ffmpeg frame extraction failed for {} at {timestamp_secs}s: {}",
      path.display(),
      stderr.trim()
    );
  }

  if output.stdout.is_empty() {
    anyhow::bail!(
      "ffmpeg produced no output for {} at {timestamp_secs}s",
      path.display()
    );
  }

  Ok(output.stdout)
}

pub async fn extract_keyframes(
  path: &Path,
  max_frames: usize,
  threads: usize,
) -> Result<Vec<ExtractedFrame>> {
  if max_frames == 0 {
    return Ok(vec![]);
  }

  let duration = get_duration(path).await?;
  if duration <= 0.0 {
    anyhow::bail!(
      "Video has zero or negative duration: {}",
      path.display()
    );
  }

  let timestamps = distribute_timestamps(duration, max_frames);
  let mut frames = Vec::with_capacity(timestamps.len());

  for ts in timestamps {
    match extract_frame_at(path, ts, threads).await {
      Ok(png_data) => frames.push(ExtractedFrame {
        png_data,
        timestamp_secs: ts,
      }),
      Err(e) => {
        tracing::warn!(
          path = %path.display(),
          timestamp = ts,
          error = %e,
          "Skipping frame extraction"
        );
      }
    }
  }

  Ok(frames)
}

fn distribute_timestamps(duration: f64, count: usize) -> Vec<f64> {
  if count == 0 {
    return vec![];
  }
  if count == 1 {
    return vec![duration / 2.0];
  }

  let step = duration / (count as f64 + 1.0);
  (1..=count).map(|i| step * i as f64).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn thread_budget_divides_cores_between_children() {
    let cores = std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(1);
    // One child gets everything; as many children as cores get one each.
    assert_eq!(thread_budget(1), cores);
    assert_eq!(thread_budget(cores), 1);
    // Never zero, however many children there are.
    assert_eq!(thread_budget(cores * 4), 1);
    assert_eq!(thread_budget(0), cores);
    // Between them, children never ask for more than the machine has.
    for n in 1..=(cores * 2) {
      assert!(
        thread_budget(n) * n <= cores.max(n),
        "{n} children x {} threads exceeds {cores} cores",
        thread_budget(n)
      );
    }
  }

  /// A frame comes back scaled to `MAX_EDGE` and 8 bits per channel,
  /// not as the multi-megabyte 16-bit PNG ffmpeg produces by default.
  #[tokio::test]
  async fn extracted_frames_are_capped_at_max_edge_and_8_bit() {
    if !ffmpeg_available() {
      eprintln!("ffmpeg not on PATH; skipping");
      return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("tall.mp4");
    let status = std::process::Command::new("ffmpeg")
      .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
      .arg("testsrc2=s=1080x1920:r=10:d=2")
      .args(["-pix_fmt", "yuv420p"])
      .arg(&path)
      .status()
      .unwrap();
    assert!(status.success());

    let png = extract_frame_at(&path, 1.0, 1).await.unwrap();
    let img = image::load_from_memory(&png).unwrap();
    let max_edge = crate::analyze::image_prep::MAX_EDGE;
    assert_eq!(
      img.height(),
      max_edge,
      "long edge should be MAX_EDGE"
    );
    assert_eq!(img.width(), 882, "aspect ratio should be kept");
    assert!(
      matches!(img.color(), image::ColorType::Rgb8),
      "expected 8-bit RGB, got {:?}",
      img.color()
    );
  }

  /// A source already inside the cap is passed through at its own size
  /// rather than upscaled.
  #[tokio::test]
  async fn small_frames_are_not_upscaled() {
    if !ffmpeg_available() {
      eprintln!("ffmpeg not on PATH; skipping");
      return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("small.mp4");
    let status = std::process::Command::new("ffmpeg")
      .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
      .arg("testsrc2=s=320x240:r=10:d=2")
      .args(["-pix_fmt", "yuv420p"])
      .arg(&path)
      .status()
      .unwrap();
    assert!(status.success());

    let png = extract_frame_at(&path, 1.0, 1).await.unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (320, 240));
  }

  #[test]
  fn distribute_timestamps_single_frame() {
    let ts = distribute_timestamps(10.0, 1);

    assert_eq!(ts.len(), 1);
    assert!((ts[0] - 5.0).abs() < f64::EPSILON);
  }

  #[test]
  fn distribute_timestamps_multiple_frames() {
    let ts = distribute_timestamps(12.0, 3);

    assert_eq!(ts.len(), 3);
    assert!((ts[0] - 3.0).abs() < f64::EPSILON);
    assert!((ts[1] - 6.0).abs() < f64::EPSILON);
    assert!((ts[2] - 9.0).abs() < f64::EPSILON);
  }

  #[test]
  fn distribute_timestamps_zero_count() {
    let ts = distribute_timestamps(10.0, 0);

    assert!(ts.is_empty());
  }

  #[test]
  fn distribute_timestamps_avoids_boundaries() {
    let ts = distribute_timestamps(100.0, 4);

    for t in &ts {
      assert!(*t > 0.0);
      assert!(*t < 100.0);
    }
  }

  #[test]
  fn distribute_timestamps_evenly_spaced() {
    let ts = distribute_timestamps(20.0, 4);

    for i in 1..ts.len() {
      let gap = ts[i] - ts[i - 1];
      assert!((gap - 4.0).abs() < f64::EPSILON);
    }
  }

  #[test]
  fn distribute_timestamps_two_frames() {
    let ts = distribute_timestamps(9.0, 2);

    assert_eq!(ts.len(), 2);
    assert!((ts[0] - 3.0).abs() < f64::EPSILON);
    assert!((ts[1] - 6.0).abs() < f64::EPSILON);
  }
}
