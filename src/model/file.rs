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
  Psd,
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
  Xlsx,
  Pptx,
  Odt,
  Ods,
  Epub,
  /// SVG is XML text; described from its markup, not rasterised.
  Svg,
  /// Source code of any language; treated as plain text.
  Code,
}

#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum InstallerFormat {
  Dmg,
  Pkg,
  Exe,
  Msi,
  Apk,
  Iso,
  Deb,
  Rpm,
  AppImage,
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
  Installer,
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
      "installer" | "installers" | "app" | "apps" | "software" => {
        Some(Self::Installer)
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
      Self::Installer => "installer",
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
         Valid: image, video, audio, document, archive, installer \
         (aliases: photo, movie, music, pdf, zip, app, etc.)",
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
  Installer(InstallerFormat),
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
      "psd" => Self::Image(ImageFormat::Psd),

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
      "xlsx" | "xlsm" => Self::Document(DocumentFormat::Xlsx),
      "pptx" => Self::Document(DocumentFormat::Pptx),
      "odt" => Self::Document(DocumentFormat::Odt),
      "ods" => Self::Document(DocumentFormat::Ods),
      "epub" => Self::Document(DocumentFormat::Epub),
      "svg" => Self::Document(DocumentFormat::Svg),
      "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "rb"
      | "sh" | "bash" | "zsh" | "fish" | "java" | "kt" | "swift"
      | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "php" | "sql"
      | "lua" | "pl" | "r" | "scala" | "css" | "scss" | "dart"
      | "ex" | "exs" | "hs" | "ml" | "zig" | "vue" | "svelte"
      | "ipynb" => Self::Document(DocumentFormat::Code),

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

      "dmg" => Self::Installer(InstallerFormat::Dmg),
      "pkg" => Self::Installer(InstallerFormat::Pkg),
      "exe" => Self::Installer(InstallerFormat::Exe),
      "msi" => Self::Installer(InstallerFormat::Msi),
      "apk" => Self::Installer(InstallerFormat::Apk),
      "iso" => Self::Installer(InstallerFormat::Iso),
      "deb" => Self::Installer(InstallerFormat::Deb),
      "rpm" => Self::Installer(InstallerFormat::Rpm),
      "appimage" => Self::Installer(InstallerFormat::AppImage),

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
      Self::Installer(_) => Some(FileCategory::Installer),
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
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
        Some(Self::Document(DocumentFormat::Docx))
      }
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
        Some(Self::Document(DocumentFormat::Xlsx))
      }
      "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
        Some(Self::Document(DocumentFormat::Pptx))
      }
      "application/vnd.oasis.opendocument.text" => {
        Some(Self::Document(DocumentFormat::Odt))
      }
      "application/vnd.oasis.opendocument.spreadsheet" => {
        Some(Self::Document(DocumentFormat::Ods))
      }
      "application/epub+zip" => Some(Self::Document(DocumentFormat::Epub)),
      "image/vnd.adobe.photoshop" => Some(Self::Image(ImageFormat::Psd)),
      "application/vnd.microsoft.portable-executable"
      | "application/x-msdownload" => {
        Some(Self::Installer(InstallerFormat::Exe))
      }
      "application/x-msi" => Some(Self::Installer(InstallerFormat::Msi)),
      "application/vnd.android.package-archive" => {
        Some(Self::Installer(InstallerFormat::Apk))
      }
      "application/x-iso9660-image" => {
        Some(Self::Installer(InstallerFormat::Iso))
      }
      "application/vnd.debian.binary-package" => {
        Some(Self::Installer(InstallerFormat::Deb))
      }
      "application/x-rpm" => Some(Self::Installer(InstallerFormat::Rpm)),
      "application/x-apple-diskimage" => {
        Some(Self::Installer(InstallerFormat::Dmg))
      }

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
          | DocumentFormat::Svg
          | DocumentFormat::Code
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
      Self::Image(ImageFormat::Psd) => "image/vnd.adobe.photoshop",
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
      Self::Document(DocumentFormat::Xlsx) => {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
      }
      Self::Document(DocumentFormat::Pptx) => {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
      }
      Self::Document(DocumentFormat::Odt) => {
        "application/vnd.oasis.opendocument.text"
      }
      Self::Document(DocumentFormat::Ods) => {
        "application/vnd.oasis.opendocument.spreadsheet"
      }
      Self::Document(DocumentFormat::Epub) => "application/epub+zip",
      Self::Document(DocumentFormat::Svg) => "image/svg+xml",
      Self::Document(DocumentFormat::Code) => "text/plain",
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
      Self::Installer(InstallerFormat::Dmg) => "application/x-apple-diskimage",
      Self::Installer(InstallerFormat::Pkg) => "application/octet-stream",
      Self::Installer(InstallerFormat::Exe) => {
        "application/vnd.microsoft.portable-executable"
      }
      Self::Installer(InstallerFormat::Msi) => "application/x-msi",
      Self::Installer(InstallerFormat::Apk) => {
        "application/vnd.android.package-archive"
      }
      Self::Installer(InstallerFormat::Iso) => "application/x-iso9660-image",
      Self::Installer(InstallerFormat::Deb) => {
        "application/vnd.debian.binary-package"
      }
      Self::Installer(InstallerFormat::Rpm) => "application/x-rpm",
      Self::Installer(InstallerFormat::AppImage) => "application/x-executable",
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
  fn office_ebook_vector_and_code_extensions_are_documents() {
    use DocumentFormat as D;
    for (ext, want) in [
      ("xlsx", D::Xlsx),
      ("pptx", D::Pptx),
      ("odt", D::Odt),
      ("ods", D::Ods),
      ("epub", D::Epub),
      ("svg", D::Svg),
      ("rs", D::Code),
      ("py", D::Code),
      ("ts", D::Code),
      ("sh", D::Code),
    ] {
      assert_eq!(
        FileType::from_extension(ext),
        FileType::Document(want),
        "{ext}"
      );
    }
    assert!(FileType::from_extension("svg").is_text());
    assert!(FileType::from_extension("py").is_text());
    assert!(!FileType::from_extension("xlsx").is_text());
  }

  #[test]
  fn psd_is_an_image() {
    assert_eq!(
      FileType::from_extension("psd"),
      FileType::Image(ImageFormat::Psd)
    );
  }

  #[test]
  fn installer_extensions_get_their_own_category() {
    use InstallerFormat as I;
    for (ext, want) in [
      ("dmg", I::Dmg),
      ("pkg", I::Pkg),
      ("exe", I::Exe),
      ("msi", I::Msi),
      ("apk", I::Apk),
      ("iso", I::Iso),
      ("deb", I::Deb),
      ("rpm", I::Rpm),
      ("appimage", I::AppImage),
    ] {
      let ft = FileType::from_extension(ext);
      assert_eq!(ft, FileType::Installer(want), "{ext}");
      assert_eq!(
        ft.category(),
        Some(FileCategory::Installer),
        "{ext}"
      );
    }
    for alias in
      ["installer", "installers", "app", "apps", "software"]
    {
      assert_eq!(
        FileCategory::from_alias(alias),
        Some(FileCategory::Installer)
      );
    }
    assert_eq!(FileCategory::Installer.label(), "installer");
  }

  #[test]
  fn new_mime_types_map_to_new_formats() {
    assert_eq!(
      FileType::from_mime(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
      ),
      Some(FileType::Document(DocumentFormat::Xlsx))
    );
    assert_eq!(
      FileType::from_mime("application/epub+zip"),
      Some(FileType::Document(DocumentFormat::Epub))
    );
    assert_eq!(
      FileType::from_mime("application/vnd.android.package-archive"),
      Some(FileType::Installer(InstallerFormat::Apk))
    );
    assert_eq!(
      FileType::from_mime(
        "application/vnd.microsoft.portable-executable"
      ),
      Some(FileType::Installer(InstallerFormat::Exe))
    );
  }

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
