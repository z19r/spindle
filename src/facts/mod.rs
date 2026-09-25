//! Cheap, header-only facts about a file for the review detail pane:
//! dimensions and EXIF for photos (with GPS resolved to a place name
//! offline), ffprobe output for audio and video, page counts for PDFs,
//! core properties for office documents, entry counts for archives.
//! Nothing here reads a whole large file or touches the network.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::model::{DocumentFormat, FileType};

/// One row of the metadata table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
  pub key: &'static str,
  pub value: String,
}

fn fact(key: &'static str, value: impl Into<String>) -> Fact {
  Fact {
    key,
    value: value.into(),
  }
}

/// Files larger than this skip the extractors that must read the
/// whole file (PDF page count, tar listing).
const WHOLE_FILE_CAP: u64 = 25 * 1024 * 1024;
/// Text statistics read at most this much.
const TEXT_CAP: usize = 1024 * 1024;

/// Everything we can say about `path` without the model.
pub fn file_facts(path: &Path) -> Vec<Fact> {
  let mut out = Vec::new();
  let Ok(meta) = std::fs::metadata(path) else {
    return out;
  };
  let size = meta.len();
  let file_type = path
    .extension()
    .and_then(|e| e.to_str())
    .map(FileType::from_extension)
    .unwrap_or(FileType::Other);

  match &file_type {
    FileType::Image(_) => image_facts(path, &mut out),
    FileType::Video(_) | FileType::Audio(_) => {
      media_facts(path, &mut out)
    }
    FileType::Document(DocumentFormat::Pdf) => {
      pdf_facts(path, size, &mut out)
    }
    FileType::Document(f) if is_office(f) => {
      office_facts(path, &mut out)
    }
    FileType::Document(_) => text_facts(path, &mut out),
    FileType::Archive(_) => archive_facts(path, size, &mut out),
    _ => {}
  }

  out.push(fact("Size", format_size(size)));
  if let Ok(modified) = meta.modified() {
    let t = chrono::DateTime::<chrono::Local>::from(modified);
    out
      .push(fact("Modified", t.format("%Y-%m-%d %H:%M").to_string()));
  }
  out
}

fn is_office(f: &DocumentFormat) -> bool {
  matches!(
    f,
    DocumentFormat::Docx
      | DocumentFormat::Xlsx
      | DocumentFormat::Pptx
      | DocumentFormat::Odt
      | DocumentFormat::Ods
  )
}

// ---------------------------------------------------------------- images

fn image_facts(path: &Path, out: &mut Vec<Fact>) {
  if let Ok((w, h)) = image::image_dimensions(path) {
    out.push(fact("Dimensions", format!("{w} × {h}")));
  }
  let Ok(file) = std::fs::File::open(path) else {
    return;
  };
  let mut reader = std::io::BufReader::new(file);
  let Ok(exif) = exif::Reader::new().read_from_container(&mut reader)
  else {
    return;
  };
  exif_facts(&exif, out);
}

fn exif_facts(exif: &exif::Exif, out: &mut Vec<Fact>) {
  use exif::{In, Tag};
  let get = |tag: Tag| -> Option<String> {
    exif.get_field(tag, In::PRIMARY).map(|f| {
      f.display_value().to_string().trim_matches('"').to_string()
    })
  };

  if let Some(taken) = get(Tag::DateTimeOriginal) {
    match time_of_day(&taken) {
      Some(tod) => {
        out.push(fact("Taken", format!("{taken} ({tod})")))
      }
      None => out.push(fact("Taken", taken)),
    }
  }
  if let Some(place) = gps_place(exif) {
    out.push(fact("Place", place));
  }
  let camera = match (get(Tag::Make), get(Tag::Model)) {
    (Some(make), Some(model)) if model.starts_with(&make) => {
      Some(model)
    }
    (Some(make), Some(model)) => Some(format!("{make} {model}")),
    (None, Some(model)) => Some(model),
    (Some(make), None) => Some(make),
    (None, None) => None,
  };
  if let Some(camera) = camera {
    out.push(fact("Camera", camera));
  }
  if let Some(lens) = get(Tag::LensModel) {
    out.push(fact("Lens", lens));
  }
  let with_unit = |tag: Tag| -> Option<String> {
    exif
      .get_field(tag, In::PRIMARY)
      .map(|f| f.display_value().with_unit(exif).to_string())
  };
  let exposure: Vec<String> = [
    with_unit(Tag::FocalLength),
    with_unit(Tag::FNumber),
    with_unit(Tag::ExposureTime),
    get(Tag::PhotographicSensitivity).map(|iso| format!("ISO {iso}")),
  ]
  .into_iter()
  .flatten()
  .collect();
  if !exposure.is_empty() {
    out.push(fact("Exposure", exposure.join(", ")));
  }
}

/// "2023:07:14 06:12:00" → "early morning". EXIF dates use colons.
pub fn time_of_day(exif_datetime: &str) -> Option<&'static str> {
  let time = exif_datetime.split_whitespace().nth(1)?;
  let hour: u32 = time.split(':').next()?.parse().ok()?;
  Some(match hour {
    0..=4 => "night",
    5..=7 => "early morning",
    8..=11 => "morning",
    12..=13 => "midday",
    14..=17 => "afternoon",
    18..=20 => "evening",
    _ => "night",
  })
}

/// GPS coordinates as decimal degrees, if the photo carries them.
pub fn gps_coordinates(exif: &exif::Exif) -> Option<(f64, f64)> {
  use exif::{In, Tag, Value};
  let dms = |tag: Tag| -> Option<f64> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    let Value::Rational(parts) = &field.value else {
      return None;
    };
    let mut degrees = 0.0;
    for (i, r) in parts.iter().take(3).enumerate() {
      if r.denom == 0 {
        return None;
      }
      degrees += r.to_f64() / 60f64.powi(i as i32);
    }
    Some(degrees)
  };
  let sign = |tag: Tag, negative: &str| -> f64 {
    let s = exif
      .get_field(tag, In::PRIMARY)
      .map(|f| f.display_value().to_string())
      .unwrap_or_default();
    if s.trim_matches('"').starts_with(negative) {
      -1.0
    } else {
      1.0
    }
  };
  let lat = dms(Tag::GPSLatitude)? * sign(Tag::GPSLatitudeRef, "S");
  let lon = dms(Tag::GPSLongitude)? * sign(Tag::GPSLongitudeRef, "W");
  ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
    .then_some((lat, lon))
}

fn gps_place(exif: &exif::Exif) -> Option<String> {
  let (lat, lon) = gps_coordinates(exif)?;
  Some(format!("{} ({lat:.4}, {lon:.4})", place_name(lat, lon)))
}

fn geocoder() -> &'static reverse_geocoder::ReverseGeocoder {
  static GEOCODER: OnceLock<reverse_geocoder::ReverseGeocoder> =
    OnceLock::new();
  GEOCODER.get_or_init(reverse_geocoder::ReverseGeocoder::new)
}

/// Nearest populated place, offline: "Portland, Oregon, US".
pub fn place_name(lat: f64, lon: f64) -> String {
  let record = geocoder().search((lat, lon)).record;
  let mut parts = vec![record.name.clone()];
  if !record.admin1.is_empty() && record.admin1 != record.name {
    parts.push(record.admin1.clone());
  }
  parts.push(record.cc.clone());
  parts.join(", ")
}

// ------------------------------------------------------------ audio/video

fn ffprobe_available() -> bool {
  static AVAILABLE: OnceLock<bool> = OnceLock::new();
  *AVAILABLE.get_or_init(|| {
    Command::new("ffprobe")
      .arg("-version")
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status()
      .map(|s| s.success())
      .unwrap_or(false)
  })
}

fn media_facts(path: &Path, out: &mut Vec<Fact>) {
  if !ffprobe_available() {
    out.push(fact("Media", "install ffmpeg for duration and codec"));
    return;
  }
  let Ok(output) = Command::new("ffprobe")
    .args([
      "-v",
      "quiet",
      "-print_format",
      "json",
      "-show_format",
      "-show_streams",
    ])
    .arg(path)
    .output()
  else {
    return;
  };
  let Ok(json) =
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
  else {
    return;
  };
  ffprobe_facts(&json, out);
}

/// Rows from ffprobe's JSON. Public so it can be tested without ffprobe.
pub fn ffprobe_facts(json: &serde_json::Value, out: &mut Vec<Fact>) {
  let format = &json["format"];
  if let Some(secs) = format["duration"]
    .as_str()
    .and_then(|s| s.parse::<f64>().ok())
  {
    out.push(fact("Duration", format_duration(secs)));
  }
  let streams =
    json["streams"].as_array().cloned().unwrap_or_default();
  if let Some(v) = streams
    .iter()
    .find(|s| s["codec_type"].as_str() == Some("video"))
  {
    if let (Some(w), Some(h)) =
      (v["width"].as_u64(), v["height"].as_u64())
    {
      let mut dims = format!("{w} × {h}");
      if let Some(fps) =
        v["r_frame_rate"].as_str().and_then(parse_ratio)
      {
        dims.push_str(&format!(" @ {fps:.0} fps"));
      }
      out.push(fact("Video", dims));
    }
    if let Some(codec) = v["codec_name"].as_str() {
      out.push(fact("Video codec", codec.to_string()));
    }
  }
  if let Some(a) = streams
    .iter()
    .find(|s| s["codec_type"].as_str() == Some("audio"))
  {
    let mut parts = Vec::new();
    if let Some(codec) = a["codec_name"].as_str() {
      parts.push(codec.to_string());
    }
    if let Some(rate) = a["sample_rate"].as_str() {
      parts.push(format!("{rate} Hz"));
    }
    if let Some(ch) = a["channels"].as_u64() {
      parts.push(match ch {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} ch"),
      });
    }
    if !parts.is_empty() {
      out.push(fact("Audio", parts.join(", ")));
    }
  }
  let tags = &format["tags"];
  for (key, label) in [
    ("title", "Title"),
    ("artist", "Artist"),
    ("album", "Album"),
    ("creation_time", "Recorded"),
  ] {
    if let Some(v) = tags[key].as_str() {
      let v = if key == "creation_time" {
        v.replace('T', " ").trim_end_matches('Z').to_string()
      } else {
        v.to_string()
      };
      out.push(fact(label, v));
    }
  }
  if let Some((lat, lon)) = tags
    ["com.apple.quicktime.location.ISO6709"]
    .as_str()
    .and_then(parse_iso6709)
  {
    out.push(fact(
      "Place",
      format!("{} ({lat:.4}, {lon:.4})", place_name(lat, lon)),
    ));
  }
}

/// "+42.3029-088.8368+5052.395/" (QuickTime location) → (lat, lon).
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
  let s = s.trim().trim_end_matches('/');
  let mut cuts: Vec<usize> = s
    .char_indices()
    .filter(|&(i, c)| i > 0 && (c == '+' || c == '-'))
    .map(|(i, _)| i)
    .collect();
  cuts.push(s.len());
  let lat: f64 = s[..*cuts.first()?].parse().ok()?;
  let lon: f64 = s[cuts[0]..cuts[1]].parse().ok()?;
  ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
    .then_some((lat, lon))
}

fn parse_ratio(s: &str) -> Option<f64> {
  let (n, d) = s.split_once('/')?;
  let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
  (d != 0.0).then(|| n / d)
}

pub fn format_duration(secs: f64) -> String {
  let total = secs.round() as u64;
  let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
  if h > 0 {
    format!("{h}:{m:02}:{s:02}")
  } else {
    format!("{m}:{s:02}")
  }
}

// ------------------------------------------------------------- documents

fn pdf_facts(path: &Path, size: u64, out: &mut Vec<Fact>) {
  if size > WHOLE_FILE_CAP {
    return;
  }
  let Ok(doc) = lopdf::Document::load(path) else {
    return;
  };
  out.push(fact("Pages", doc.get_pages().len().to_string()));
  let info = doc
    .trailer
    .get(b"Info")
    .ok()
    .and_then(|o| doc.dereference(o).ok())
    .and_then(|(_, o)| o.as_dict().ok().cloned());
  if let Some(info) = info {
    for (key, label) in [
      (b"Title".as_slice(), "Title"),
      (b"Author".as_slice(), "Author"),
      (b"CreationDate".as_slice(), "Created"),
    ] {
      if let Some(text) = info
        .get(key)
        .ok()
        .and_then(|o| o.as_str().ok())
        .map(|b| String::from_utf8_lossy(b).trim().to_string())
        .filter(|s| !s.is_empty())
      {
        out.push(fact(label, pdf_date(&text)));
      }
    }
  }
}

/// "D:20240115093000Z" → "2024-01-15 09:30"; anything else unchanged.
fn pdf_date(s: &str) -> String {
  let digits = s.strip_prefix("D:").unwrap_or(s);
  if digits.len() >= 12
    && digits[..12].chars().all(|c| c.is_ascii_digit())
  {
    format!(
      "{}-{}-{} {}:{}",
      &digits[..4],
      &digits[4..6],
      &digits[6..8],
      &digits[8..10],
      &digits[10..12]
    )
  } else {
    s.to_string()
  }
}

fn office_facts(path: &Path, out: &mut Vec<Fact>) {
  use std::io::Read;
  let Ok(file) = std::fs::File::open(path) else {
    return;
  };
  let Ok(mut zip) = zip::ZipArchive::new(file) else {
    return;
  };
  let mut xml = String::new();
  for name in ["docProps/core.xml", "meta.xml"] {
    if let Ok(entry) = zip.by_name(name) {
      let _ = entry.take(256 * 1024).read_to_string(&mut xml);
      break;
    }
  }
  if xml.is_empty() {
    return;
  }
  for (tag, label) in [
    ("dc:title", "Title"),
    ("dc:creator", "Author"),
    ("dcterms:created", "Created"),
    ("dcterms:modified", "Edited"),
    ("cp:lastModifiedBy", "Last edited by"),
  ] {
    if let Some(v) = xml_text(&xml, tag) {
      out.push(fact(
        label,
        v.replace('T', " ").trim_end_matches('Z').to_string(),
      ));
    }
  }
}

/// Text of the first `<tag ...>…</tag>` element, or `None`.
fn xml_text(xml: &str, tag: &str) -> Option<String> {
  let open = xml.find(&format!("<{tag}"))?;
  let start = xml[open..].find('>')? + open + 1;
  let end = xml[start..].find(&format!("</{tag}>"))? + start;
  let text = xml[start..end].trim();
  (!text.is_empty()).then(|| text.to_string())
}

fn text_facts(path: &Path, out: &mut Vec<Fact>) {
  use std::io::Read;
  let Ok(file) = std::fs::File::open(path) else {
    return;
  };
  let mut buf = Vec::new();
  if file.take(TEXT_CAP as u64).read_to_end(&mut buf).is_err() {
    return;
  }
  let text = String::from_utf8_lossy(&buf);
  let lines = text.lines().count();
  let words = text.split_whitespace().count();
  let more = if buf.len() >= TEXT_CAP { "+" } else { "" };
  out.push(fact(
    "Text",
    format!("{lines}{more} lines, {words}{more} words"),
  ));
}

// -------------------------------------------------------------- archives

fn archive_facts(path: &Path, size: u64, out: &mut Vec<Fact>) {
  let Ok(file) = std::fs::File::open(path) else {
    return;
  };
  if let Ok(mut zip) = zip::ZipArchive::new(&file) {
    let mut total = 0u64;
    let mut files = 0usize;
    for i in 0..zip.len() {
      if let Ok(entry) = zip.by_index(i) {
        if entry.is_file() {
          files += 1;
          total += entry.size();
        }
      }
    }
    out.push(fact(
      "Contents",
      format!("{files} files, {} unpacked", format_size(total)),
    ));
    return;
  }
  if size > WHOLE_FILE_CAP {
    return;
  }
  use std::io::Seek;
  let mut file = file;
  let _ = file.rewind();
  let name = path.to_string_lossy().to_ascii_lowercase();
  let reader: Box<dyn std::io::Read> =
    if name.ends_with(".gz") || name.ends_with(".tgz") {
      Box::new(flate2::read::GzDecoder::new(file))
    } else {
      Box::new(file)
    };
  let mut archive = tar::Archive::new(reader);
  let Ok(entries) = archive.entries() else {
    return;
  };
  let mut files = 0usize;
  let mut total = 0u64;
  for entry in entries.flatten() {
    if entry.header().entry_type().is_file() {
      files += 1;
      total += entry.header().size().unwrap_or(0);
    }
  }
  out.push(fact(
    "Contents",
    format!("{files} files, {} unpacked", format_size(total)),
  ));
}

pub fn format_size(bytes: u64) -> String {
  const KB: f64 = 1024.0;
  let b = bytes as f64;
  if b >= KB * KB * KB {
    format!("{:.1} GB", b / (KB * KB * KB))
  } else if b >= KB * KB {
    format!("{:.1} MB", b / (KB * KB))
  } else if b >= KB {
    format!("{:.1} KB", b / KB)
  } else {
    format!("{bytes} B")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn row<'a>(facts: &'a [Fact], key: &str) -> Option<&'a str> {
    facts
      .iter()
      .find(|f| f.key == key)
      .map(|f| f.value.as_str())
  }

  #[test]
  fn time_of_day_buckets_the_exif_hour() {
    assert_eq!(
      time_of_day("2023:07:14 06:12:00"),
      Some("early morning")
    );
    assert_eq!(time_of_day("2023:07:14 12:30:00"), Some("midday"));
    assert_eq!(time_of_day("2023:07:14 19:00:00"), Some("evening"));
    assert_eq!(time_of_day("2023:07:14 23:59:00"), Some("night"));
    assert_eq!(time_of_day("garbage"), None);
  }

  #[test]
  fn place_name_resolves_offline() {
    let place = place_name(45.5152, -122.6784);
    assert!(place.starts_with("Portland, Oregon"), "{place}");
    assert!(place.ends_with("US"), "{place}");
  }

  #[test]
  fn png_dimensions_and_generic_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.png");
    image::RgbImage::new(12, 7).save(&path).unwrap();
    let facts = file_facts(&path);
    assert_eq!(row(&facts, "Dimensions"), Some("12 × 7"));
    assert!(row(&facts, "Size").is_some());
    assert!(row(&facts, "Modified").is_some());
  }

  #[test]
  fn fixture_photo_reports_when_it_was_taken() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tests/fixtures/organize/Desktop/IMG_4021.jpg");
    let facts = file_facts(&path);
    let taken = row(&facts, "Taken").unwrap_or("");
    assert!(taken.starts_with("2023"), "{taken}");
  }

  #[test]
  fn text_files_count_lines_and_words() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    std::fs::write(&path, "one two\nthree\n").unwrap();
    let facts = file_facts(&path);
    assert_eq!(row(&facts, "Text"), Some("2 lines, 3 words"));
  }

  #[test]
  fn zip_archives_count_their_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("x.txt", opts).unwrap();
    std::io::Write::write_all(&mut w, b"hello").unwrap();
    w.start_file("y.txt", opts).unwrap();
    std::io::Write::write_all(&mut w, b"world!").unwrap();
    w.finish().unwrap();
    let facts = file_facts(&path);
    assert_eq!(
      row(&facts, "Contents"),
      Some("2 files, 11 B unpacked")
    );
  }

  #[test]
  fn office_core_properties_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.docx");
    let file = std::fs::File::create(&path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("docProps/core.xml", opts).unwrap();
    std::io::Write::write_all(
      &mut w,
      br#"<cp:coreProperties><dc:title>Q1 Report</dc:title><dc:creator>Sam</dc:creator><dcterms:created xsi:type="dcterms:W3CDTF">2024-01-15T09:30:00Z</dcterms:created></cp:coreProperties>"#,
    )
    .unwrap();
    w.finish().unwrap();
    let facts = file_facts(&path);
    assert_eq!(row(&facts, "Title"), Some("Q1 Report"));
    assert_eq!(row(&facts, "Author"), Some("Sam"));
    assert_eq!(row(&facts, "Created"), Some("2024-01-15 09:30:00"));
  }

  #[test]
  fn ffprobe_json_becomes_rows() {
    let json = serde_json::json!({
      "format": {
        "duration": "125.4",
        "tags": {"title": "Ep 12", "creation_time": "2024-05-01T10:00:00.000000Z"}
      },
      "streams": [
        {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "r_frame_rate": "30000/1001"},
        {"codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2}
      ]
    });
    let mut facts = Vec::new();
    ffprobe_facts(&json, &mut facts);
    assert_eq!(row(&facts, "Duration"), Some("2:05"));
    assert_eq!(row(&facts, "Video"), Some("1920 × 1080 @ 30 fps"));
    assert_eq!(row(&facts, "Audio"), Some("aac, 48000 Hz, stereo"));
    assert_eq!(row(&facts, "Title"), Some("Ep 12"));
    assert_eq!(
      row(&facts, "Recorded"),
      Some("2024-05-01 10:00:00.000000")
    );
  }

  #[test]
  fn quicktime_location_strings_parse_and_resolve() {
    assert_eq!(
      parse_iso6709("+42.3029-088.8368+5052.395/"),
      Some((42.3029, -88.8368))
    );
    assert_eq!(
      parse_iso6709("+42.3029+088.8368/"),
      Some((42.3029, 88.8368))
    );
    assert_eq!(parse_iso6709("garbage"), None);
    let json = serde_json::json!({
      "format": {"tags": {"com.apple.quicktime.location.ISO6709": "+45.5152-122.6784/"}},
      "streams": []
    });
    let mut facts = Vec::new();
    ffprobe_facts(&json, &mut facts);
    let place = row(&facts, "Place").unwrap_or("");
    assert!(place.starts_with("Portland, Oregon, US ("), "{place}");
  }

  #[test]
  fn pdf_dates_are_humanised() {
    assert_eq!(pdf_date("D:20240115093000Z"), "2024-01-15 09:30");
    assert_eq!(pdf_date("yesterday"), "yesterday");
  }

  #[test]
  fn durations_format_like_a_player() {
    assert_eq!(format_duration(59.6), "1:00");
    assert_eq!(format_duration(3725.0), "1:02:05");
  }
}
