use std::path::Path;

use anyhow::{Context, Result};

pub struct ExtractedFrame {
  pub png_data: Vec<u8>,
  pub timestamp_secs: f64,
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
) -> Result<Vec<u8>> {
  let output = tokio::process::Command::new("ffmpeg")
    .args(["-ss", &format!("{timestamp_secs:.3}"), "-i"])
    .arg(path)
    .args([
      "-frames:v",
      "1",
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
    match extract_frame_at(path, ts).await {
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
