use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ImageFormat {
  Png,
  Jpg,
  Webp,
  Gif,
  Heic,
  Tiff,
  Bmp,
  Avif,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum VideoFormat {
  Mp4,
  Mov,
  Avi,
  Mkv,
  Webm,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum DocumentFormat {
  Pdf,
  Doc,
  Docx,
  Txt,
  Md,
  Rtf,
  Csv,
  Json,
  Xml,
  Html,
  Yaml,
  Toml,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum AudioFormat {
  Mp3,
  Wav,
  Flac,
  Ogg,
  Aac,
  M4a,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ArchiveFormat {
  Zip,
  Tar,
  Gz,
  Bz2,
  Xz,
  SevenZ,
  Rar,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub enum FileCategory {
  Image,
  Video,
  Audio,
  Document,
  Archive,
}

impl FileCategory {
  pub fn from_alias(s: &str) -> Option<Self> {
    match s.to_lowercase().as_str() {
      "image" | "images" | "photo" | "photos" | "picture"
      | "pictures" => Some(Self::Image),
      "video" | "videos" | "movie" | "movies" => Some(Self::Video),
      "audio" | "music" | "sound" | "sounds" => Some(Self::Audio),
      "document" | "documents" | "doc" | "docs" | "pdf" | "text" => {
        Some(Self::Document)
      }
      "archive" | "archives" | "zip" | "compressed" => {
        Some(Self::Archive)
      }
      _ => None,
    }
  }

  pub fn label(&self) -> &'static str {
    match self {
      Self::Image => "image",
      Self::Video => "video",
      Self::Audio => "audio",
      Self::Document => "document",
      Self::Archive => "archive",
    }
  }
}

impl std::fmt::Display for FileCategory {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.label())
  }
}

impl std::str::FromStr for FileCategory {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::from_alias(s).ok_or_else(|| {
      format!(
        "unknown file category '{}'. \
         Valid: image, video, audio, document, archive \
         (aliases: photo, movie, music, pdf, zip, etc.)",
        s
      )
    })
  }
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum FileType {
  Image(ImageFormat),
  Video(VideoFormat),
  Document(DocumentFormat),
  Audio(AudioFormat),
  Archive(ArchiveFormat),
  Other,
}

impl FileType {
  pub fn from_extension(ext: &str) -> Self {
    match ext.to_lowercase().as_str() {
      "png" => Self::Image(ImageFormat::Png),
      "jpg" | "jpeg" => Self::Image(ImageFormat::Jpg),
      "webp" => Self::Image(ImageFormat::Webp),
      "gif" => Self::Image(ImageFormat::Gif),
      "heic" | "heif" => Self::Image(ImageFormat::Heic),
      "tiff" | "tif" => Self::Image(ImageFormat::Tiff),
      "bmp" => Self::Image(ImageFormat::Bmp),
      "avif" => Self::Image(ImageFormat::Avif),

      "mp4" => Self::Video(VideoFormat::Mp4),
      "mov" => Self::Video(VideoFormat::Mov),
      "avi" => Self::Video(VideoFormat::Avi),
      "mkv" => Self::Video(VideoFormat::Mkv),
      "webm" => Self::Video(VideoFormat::Webm),

      "pdf" => Self::Document(DocumentFormat::Pdf),
      "doc" => Self::Document(DocumentFormat::Doc),
      "docx" => Self::Document(DocumentFormat::Docx),
      "txt" => Self::Document(DocumentFormat::Txt),
      "md" | "markdown" => Self::Document(DocumentFormat::Md),
      "rtf" => Self::Document(DocumentFormat::Rtf),
      "csv" => Self::Document(DocumentFormat::Csv),
      "json" => Self::Document(DocumentFormat::Json),
      "xml" => Self::Document(DocumentFormat::Xml),
      "html" | "htm" => Self::Document(DocumentFormat::Html),
      "yaml" | "yml" => Self::Document(DocumentFormat::Yaml),
      "toml" => Self::Document(DocumentFormat::Toml),

      "mp3" => Self::Audio(AudioFormat::Mp3),
      "wav" => Self::Audio(AudioFormat::Wav),
      "flac" => Self::Audio(AudioFormat::Flac),
      "ogg" => Self::Audio(AudioFormat::Ogg),
      "aac" => Self::Audio(AudioFormat::Aac),
      "m4a" => Self::Audio(AudioFormat::M4a),

      "zip" => Self::Archive(ArchiveFormat::Zip),
      "tar" => Self::Archive(ArchiveFormat::Tar),
      "gz" | "tgz" => Self::Archive(ArchiveFormat::Gz),
      "bz2" => Self::Archive(ArchiveFormat::Bz2),
      "xz" => Self::Archive(ArchiveFormat::Xz),
      "7z" => Self::Archive(ArchiveFormat::SevenZ),
      "rar" => Self::Archive(ArchiveFormat::Rar),

      _ => Self::Other,
    }
  }

  pub fn category(&self) -> Option<FileCategory> {
    match self {
      Self::Image(_) => Some(FileCategory::Image),
      Self::Video(_) => Some(FileCategory::Video),
      Self::Document(_) => Some(FileCategory::Document),
      Self::Audio(_) => Some(FileCategory::Audio),
      Self::Archive(_) => Some(FileCategory::Archive),
      Self::Other => None,
    }
  }

  pub fn from_magic_bytes(path: &Path) -> Option<Self> {
    let kind = infer::get_from_path(path).ok()??;
    Self::from_mime(kind.mime_type())
  }

  pub fn from_mime(mime: &str) -> Option<Self> {
    match mime {
      "image/png" => Some(Self::Image(ImageFormat::Png)),
      "image/jpeg" => Some(Self::Image(ImageFormat::Jpg)),
      "image/webp" => Some(Self::Image(ImageFormat::Webp)),
      "image/gif" => Some(Self::Image(ImageFormat::Gif)),
      "image/heic"
      | "image/heif"
      | "image/heic-sequence"
      | "image/heif-sequence" => Some(Self::Image(ImageFormat::Heic)),
      "image/tiff" => Some(Self::Image(ImageFormat::Tiff)),
      "image/bmp" | "image/x-bmp" => {
        Some(Self::Image(ImageFormat::Bmp))
      }
      "image/avif" => Some(Self::Image(ImageFormat::Avif)),

      "video/mp4" => Some(Self::Video(VideoFormat::Mp4)),
      "video/quicktime" => Some(Self::Video(VideoFormat::Mov)),
      "video/x-msvideo" => Some(Self::Video(VideoFormat::Avi)),
      "video/x-matroska" => Some(Self::Video(VideoFormat::Mkv)),
      "video/webm" => Some(Self::Video(VideoFormat::Webm)),

      "application/pdf" => Some(Self::Document(DocumentFormat::Pdf)),

      "audio/mpeg" => Some(Self::Audio(AudioFormat::Mp3)),
      "audio/x-wav" | "audio/wav" => {
        Some(Self::Audio(AudioFormat::Wav))
      }
      "audio/x-flac" | "audio/flac" => {
        Some(Self::Audio(AudioFormat::Flac))
      }
      "audio/ogg" => Some(Self::Audio(AudioFormat::Ogg)),
      "audio/aac" => Some(Self::Audio(AudioFormat::Aac)),
      "audio/x-m4a" | "audio/mp4" => {
        Some(Self::Audio(AudioFormat::M4a))
      }

      "application/zip" => Some(Self::Archive(ArchiveFormat::Zip)),
      "application/x-tar" => Some(Self::Archive(ArchiveFormat::Tar)),
      "application/gzip" => Some(Self::Archive(ArchiveFormat::Gz)),
      "application/x-bzip2" => {
        Some(Self::Archive(ArchiveFormat::Bz2))
      }
      "application/x-xz" => Some(Self::Archive(ArchiveFormat::Xz)),
      "application/x-7z-compressed" => {
        Some(Self::Archive(ArchiveFormat::SevenZ))
      }
      "application/vnd.rar" | "application/x-rar-compressed" => {
        Some(Self::Archive(ArchiveFormat::Rar))
      }

      _ => None,
    }
  }

  pub fn is_image(&self) -> bool {
    matches!(self, Self::Image(_))
  }

  pub fn is_video(&self) -> bool {
    matches!(self, Self::Video(_))
  }

  pub fn is_text(&self) -> bool {
    matches!(
      self,
      Self::Document(
        DocumentFormat::Txt
          | DocumentFormat::Md
          | DocumentFormat::Csv
          | DocumentFormat::Json
          | DocumentFormat::Xml
          | DocumentFormat::Html
          | DocumentFormat::Yaml
          | DocumentFormat::Toml
      )
    )
  }

  pub fn mime_type(&self) -> &'static str {
    match self {
      Self::Image(ImageFormat::Png) => "image/png",
      Self::Image(ImageFormat::Jpg) => "image/jpeg",
      Self::Image(ImageFormat::Webp) => "image/webp",
      Self::Image(ImageFormat::Gif) => "image/gif",
      Self::Image(ImageFormat::Heic) => "image/heic",
      Self::Image(ImageFormat::Tiff) => "image/tiff",
      Self::Image(ImageFormat::Bmp) => "image/bmp",
      Self::Image(ImageFormat::Avif) => "image/avif",
      Self::Video(VideoFormat::Mp4) => "video/mp4",
      Self::Video(VideoFormat::Mov) => "video/quicktime",
      Self::Video(VideoFormat::Avi) => "video/x-msvideo",
      Self::Video(VideoFormat::Mkv) => "video/x-matroska",
      Self::Video(VideoFormat::Webm) => "video/webm",
      Self::Document(DocumentFormat::Pdf) => "application/pdf",
      Self::Document(DocumentFormat::Doc) => "application/msword",
      Self::Document(
        DocumentFormat::Docx,
      ) => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      Self::Document(DocumentFormat::Txt) => "text/plain",
      Self::Document(DocumentFormat::Md) => "text/markdown",
      Self::Document(DocumentFormat::Rtf) => "application/rtf",
      Self::Document(DocumentFormat::Csv) => "text/csv",
      Self::Document(DocumentFormat::Json) => "application/json",
      Self::Document(DocumentFormat::Xml) => "application/xml",
      Self::Document(DocumentFormat::Html) => "text/html",
      Self::Document(DocumentFormat::Yaml) => "application/yaml",
      Self::Document(DocumentFormat::Toml) => "application/toml",
      Self::Audio(AudioFormat::Mp3) => "audio/mpeg",
      Self::Audio(AudioFormat::Wav) => "audio/wav",
      Self::Audio(AudioFormat::Flac) => "audio/flac",
      Self::Audio(AudioFormat::Ogg) => "audio/ogg",
      Self::Audio(AudioFormat::Aac) => "audio/aac",
      Self::Audio(AudioFormat::M4a) => "audio/mp4",
      Self::Archive(ArchiveFormat::Zip) => "application/zip",
      Self::Archive(ArchiveFormat::Tar) => "application/x-tar",
      Self::Archive(ArchiveFormat::Gz) => "application/gzip",
      Self::Archive(ArchiveFormat::Bz2) => "application/x-bzip2",
      Self::Archive(ArchiveFormat::Xz) => "application/x-xz",
      Self::Archive(ArchiveFormat::SevenZ) => {
        "application/x-7z-compressed"
      }
      Self::Archive(ArchiveFormat::Rar) => "application/vnd.rar",
      Self::Other => "application/octet-stream",
    }
  }
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
  pub path: PathBuf,
  pub scan_root: PathBuf,
  pub size: u64,
  pub modified: SystemTime,
  pub file_type: FileType,
}

impl ScannedFile {
  pub fn relative_path(&self) -> PathBuf {
    self
      .path
      .strip_prefix(&self.scan_root)
      .unwrap_or(&self.path)
      .to_path_buf()
  }
}

#[derive(Debug, Clone)]
pub struct FingerprintedFile {
  pub scanned: ScannedFile,
  pub blake3_hash: [u8; 32],
  pub perceptual_hash: Option<Vec<u8>>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_extension_recognizes_images() {
    assert_eq!(
      FileType::from_extension("png"),
      FileType::Image(ImageFormat::Png)
    );
    assert_eq!(
      FileType::from_extension("JPG"),
      FileType::Image(ImageFormat::Jpg)
    );
    assert_eq!(
      FileType::from_extension("jpeg"),
      FileType::Image(ImageFormat::Jpg)
    );
    assert_eq!(
      FileType::from_extension("webp"),
      FileType::Image(ImageFormat::Webp)
    );
    assert_eq!(
      FileType::from_extension("gif"),
      FileType::Image(ImageFormat::Gif)
    );
  }

  #[test]
  fn from_extension_recognizes_videos() {
    assert_eq!(
      FileType::from_extension("mp4"),
      FileType::Video(VideoFormat::Mp4)
    );
    assert_eq!(
      FileType::from_extension("mov"),
      FileType::Video(VideoFormat::Mov)
    );
    assert_eq!(
      FileType::from_extension("avi"),
      FileType::Video(VideoFormat::Avi)
    );
    assert_eq!(
      FileType::from_extension("mkv"),
      FileType::Video(VideoFormat::Mkv)
    );
    assert_eq!(
      FileType::from_extension("WEBM"),
      FileType::Video(VideoFormat::Webm)
    );
  }

  #[test]
  fn from_extension_recognizes_documents() {
    assert_eq!(
      FileType::from_extension("pdf"),
      FileType::Document(DocumentFormat::Pdf)
    );
    assert_eq!(
      FileType::from_extension("txt"),
      FileType::Document(DocumentFormat::Txt)
    );
    assert_eq!(
      FileType::from_extension("md"),
      FileType::Document(DocumentFormat::Md)
    );
    assert_eq!(
      FileType::from_extension("json"),
      FileType::Document(DocumentFormat::Json)
    );
  }

  #[test]
  fn from_extension_recognizes_audio() {
    assert_eq!(
      FileType::from_extension("mp3"),
      FileType::Audio(AudioFormat::Mp3)
    );
    assert_eq!(
      FileType::from_extension("flac"),
      FileType::Audio(AudioFormat::Flac)
    );
  }

  #[test]
  fn from_extension_recognizes_archives() {
    assert_eq!(
      FileType::from_extension("zip"),
      FileType::Archive(ArchiveFormat::Zip)
    );
    assert_eq!(
      FileType::from_extension("7z"),
      FileType::Archive(ArchiveFormat::SevenZ)
    );
  }

  #[test]
  fn from_extension_returns_other_for_unknown() {
    assert_eq!(FileType::from_extension("xyz"), FileType::Other);
    assert_eq!(FileType::from_extension(""), FileType::Other);
    assert_eq!(FileType::from_extension("blah"), FileType::Other);
  }

  #[test]
  fn is_image_and_is_video() {
    let img = FileType::Image(ImageFormat::Png);
    assert!(img.is_image());
    assert!(!img.is_video());

    let vid = FileType::Video(VideoFormat::Mp4);
    assert!(!vid.is_image());
    assert!(vid.is_video());

    let doc = FileType::Document(DocumentFormat::Pdf);
    assert!(!doc.is_image());
    assert!(!doc.is_video());

    assert!(!FileType::Other.is_image());
    assert!(!FileType::Other.is_video());
  }

  #[test]
  fn mime_type_covers_all_variants() {
    assert_eq!(
      FileType::Image(ImageFormat::Png).mime_type(),
      "image/png"
    );
    assert_eq!(
      FileType::Image(ImageFormat::Jpg).mime_type(),
      "image/jpeg"
    );
    assert_eq!(
      FileType::Image(ImageFormat::Webp).mime_type(),
      "image/webp"
    );
    assert_eq!(
      FileType::Image(ImageFormat::Gif).mime_type(),
      "image/gif"
    );
    assert_eq!(
      FileType::Video(VideoFormat::Mp4).mime_type(),
      "video/mp4"
    );
    assert_eq!(
      FileType::Video(VideoFormat::Mov).mime_type(),
      "video/quicktime"
    );
    assert_eq!(
      FileType::Video(VideoFormat::Avi).mime_type(),
      "video/x-msvideo"
    );
    assert_eq!(
      FileType::Video(VideoFormat::Mkv).mime_type(),
      "video/x-matroska"
    );
    assert_eq!(
      FileType::Video(VideoFormat::Webm).mime_type(),
      "video/webm"
    );
    assert_eq!(
      FileType::Document(DocumentFormat::Pdf).mime_type(),
      "application/pdf"
    );
    assert_eq!(
      FileType::Audio(AudioFormat::Mp3).mime_type(),
      "audio/mpeg"
    );
    assert_eq!(
      FileType::Archive(ArchiveFormat::Zip).mime_type(),
      "application/zip"
    );
    assert_eq!(
      FileType::Other.mime_type(),
      "application/octet-stream"
    );
  }

  #[test]
  fn relative_path_strips_scan_root() {
    let file = ScannedFile {
      path: PathBuf::from("/home/user/downloads/porn/image3.jpg"),
      scan_root: PathBuf::from("/home/user/downloads"),
      size: 100,
      modified: SystemTime::now(),
      file_type: FileType::Image(ImageFormat::Jpg),
    };

    assert_eq!(
      file.relative_path(),
      PathBuf::from("porn/image3.jpg")
    );
  }

  #[test]
  fn relative_path_returns_filename_for_root_file() {
    let file = ScannedFile {
      path: PathBuf::from("/downloads/cat.jpg"),
      scan_root: PathBuf::from("/downloads"),
      size: 100,
      modified: SystemTime::now(),
      file_type: FileType::Image(ImageFormat::Jpg),
    };

    assert_eq!(file.relative_path(), PathBuf::from("cat.jpg"));
  }

  #[test]
  fn relative_path_returns_full_when_no_prefix_match() {
    let file = ScannedFile {
      path: PathBuf::from("/other/dir/file.jpg"),
      scan_root: PathBuf::from("/downloads"),
      size: 100,
      modified: SystemTime::now(),
      file_type: FileType::Image(ImageFormat::Jpg),
    };

    assert_eq!(
      file.relative_path(),
      PathBuf::from("/other/dir/file.jpg")
    );
  }

  #[test]
  fn category_maps_variants() {
    assert_eq!(
      FileType::Image(ImageFormat::Png).category(),
      Some(FileCategory::Image)
    );
    assert_eq!(
      FileType::Video(VideoFormat::Mp4).category(),
      Some(FileCategory::Video)
    );
    assert_eq!(
      FileType::Audio(AudioFormat::Mp3).category(),
      Some(FileCategory::Audio)
    );
    assert_eq!(
      FileType::Document(DocumentFormat::Pdf).category(),
      Some(FileCategory::Document)
    );
    assert_eq!(
      FileType::Archive(ArchiveFormat::Zip).category(),
      Some(FileCategory::Archive)
    );
    assert_eq!(FileType::Other.category(), None);
  }

  #[test]
  fn file_category_from_alias() {
    assert_eq!(
      FileCategory::from_alias("image"),
      Some(FileCategory::Image)
    );
    assert_eq!(
      FileCategory::from_alias("photo"),
      Some(FileCategory::Image)
    );
    assert_eq!(
      FileCategory::from_alias("movie"),
      Some(FileCategory::Video)
    );
    assert_eq!(
      FileCategory::from_alias("music"),
      Some(FileCategory::Audio)
    );
    assert_eq!(
      FileCategory::from_alias("pdf"),
      Some(FileCategory::Document)
    );
    assert_eq!(
      FileCategory::from_alias("zip"),
      Some(FileCategory::Archive)
    );
    assert_eq!(
      FileCategory::from_alias("compressed"),
      Some(FileCategory::Archive)
    );
    assert_eq!(FileCategory::from_alias("foobar"), None);
  }

  #[test]
  fn file_category_from_str() {
    assert_eq!(
      "image".parse::<FileCategory>().unwrap(),
      FileCategory::Image
    );
    assert_eq!(
      "MOVIE".parse::<FileCategory>().unwrap(),
      FileCategory::Video
    );
    assert!("nonsense".parse::<FileCategory>().is_err());
  }

  #[test]
  fn from_mime_maps_known_types() {
    assert_eq!(
      FileType::from_mime("image/png"),
      Some(FileType::Image(ImageFormat::Png))
    );
    assert_eq!(
      FileType::from_mime("video/mp4"),
      Some(FileType::Video(VideoFormat::Mp4))
    );
    assert_eq!(
      FileType::from_mime("audio/mpeg"),
      Some(FileType::Audio(AudioFormat::Mp3))
    );
    assert_eq!(
      FileType::from_mime("application/pdf"),
      Some(FileType::Document(DocumentFormat::Pdf))
    );
    assert_eq!(
      FileType::from_mime("application/zip"),
      Some(FileType::Archive(ArchiveFormat::Zip))
    );
    assert_eq!(FileType::from_mime("text/plain"), None);
    assert_eq!(FileType::from_mime("totally/unknown"), None);
  }

  #[test]
  fn magic_bytes_detects_png() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("mystery.dat");
    let img: image::RgbaImage =
      image::ImageBuffer::from_raw(1, 1, vec![0, 0, 0, 255]).unwrap();
    img
      .save_with_format(&path, image::ImageFormat::Png)
      .unwrap();

    let detected = FileType::from_magic_bytes(&path);
    assert_eq!(detected, Some(FileType::Image(ImageFormat::Png)));
  }

  #[test]
  fn magic_bytes_returns_none_for_plain_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("readme.txt");
    std::fs::write(&path, "hello world").unwrap();

    let detected = FileType::from_magic_bytes(&path);
    assert_eq!(detected, None);
  }
}
