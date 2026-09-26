use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::encode::{FileFormat, write_sheet};
use crate::error::ScanError;
use crate::image::{Image, Side};

/// A folder of numbered scans. Files are written under a hidden temporary name
/// and renamed when complete, so tools watching the folder never see half an image.
pub struct ScanFolder {
    dir: PathBuf,
    format: FileFormat,
    next: u32,
}

impl ScanFolder {
    pub fn open(dir: impl Into<PathBuf>, format: FileFormat) -> Result<Self, ScanError> {
        let dir = dir.into();
        let folder_error = |err: std::io::Error| ScanError::Folder(folder_problem(&dir, &err));
        fs::create_dir_all(&dir).map_err(folder_error)?;
        let highest = fs::read_dir(&dir)
            .map_err(folder_error)?
            .filter_map(Result::ok)
            .filter_map(|entry| parse_number(&entry.file_name().to_string_lossy()))
            .max()
            .unwrap_or(0);
        Ok(Self {
            dir,
            format,
            next: highest + 1,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Saves every side of one sheet under the next free number. A PDF holds
    /// both sides as pages; other formats get one file per side.
    pub fn save(&mut self, images: &[Image]) -> Result<Vec<PathBuf>, ScanError> {
        let number = self.next;
        self.next += 1;
        let extension = self.format.extension();
        if self.format == FileFormat::Pdf {
            let path = self.dir.join(file_name(number, Side::Front, extension));
            write_atomically(&path, self.format, images)?;
            return Ok(vec![path]);
        }
        images
            .iter()
            .map(|image| {
                let path = self.dir.join(file_name(number, image.side, extension));
                write_atomically(&path, self.format, std::slice::from_ref(image))?;
                Ok(path)
            })
            .collect()
    }
}

fn folder_problem(dir: &Path, err: &std::io::Error) -> String {
    let reason = match err.kind() {
        std::io::ErrorKind::PermissionDenied => "you don't have permission to write there",
        std::io::ErrorKind::NotFound => "part of the path doesn't exist",
        std::io::ErrorKind::ReadOnlyFilesystem => "the drive is read-only",
        std::io::ErrorKind::NotADirectory => "part of the path is a file, not a folder",
        _ => return format!("Can't save to {}: {err}", dir.display()),
    };
    format!("Can't save to {} because {reason}.", dir.display())
}

/// The user's Pictures folder with a Scans subfolder.
pub fn default_output_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Pictures")))
        .unwrap_or_default()
        .join("Scans")
}

pub fn file_name(number: u32, side: Side, extension: &str) -> String {
    match side {
        Side::Front => format!("scan-{number:04}.{extension}"),
        Side::Back => format!("scan-{number:04}-back.{extension}"),
    }
}

/// Reads the number from any scan this app writes, in any format.
fn parse_number(name: &str) -> Option<u32> {
    let (stem, extension) = name.strip_prefix("scan-")?.rsplit_once('.')?;
    if !FileFormat::ALL.iter().any(|f| f.extension() == extension) {
        return None;
    }
    let digits = stem.strip_suffix("-back").unwrap_or(stem);
    digits.parse().ok()
}

fn write_atomically(path: &Path, format: FileFormat, images: &[Image]) -> Result<(), ScanError> {
    let name = path
        .file_name()
        .ok_or_else(|| ScanError::Io(format!("invalid output path {}", path.display())))?;
    let temp = path.with_file_name(format!(".{}.part", name.to_string_lossy()));
    let result = fs::File::create(&temp)
        .map_err(ScanError::from)
        .and_then(|file| {
            let mut out = BufWriter::new(file);
            write_sheet(format, images, &mut out)?;
            out.flush()?;
            Ok(())
        })
        .and_then(|()| fs::rename(&temp, path).map_err(ScanError::from));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::PixelFormat;

    fn image(side: Side, format: PixelFormat) -> Image {
        let channels = if format == PixelFormat::Rgb8 { 3 } else { 1 };
        Image {
            side,
            width: 10,
            height: 2,
            dpi: 300,
            format,
            data: (0..10 * 2 * channels)
                .map(|i| if i % 3 == 0 { 0 } else { 255 })
                .collect(),
        }
    }

    fn decode(path: &Path) -> (png::OutputInfo, Vec<u8>) {
        let file = fs::File::open(path).expect("file exists");
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .expect("valid png");
        let mut buf = vec![0; reader.output_buffer_size().expect("sized output")];
        let info = reader.next_frame(&mut buf).expect("frame decodes");
        buf.truncate(info.buffer_size());
        (info, buf)
    }

    #[test]
    fn numbers_continue_after_existing_scans() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("scan-0007.png"), b"").expect("seed file");
        fs::write(dir.path().join("scan-0009-back.png"), b"").expect("seed file");
        fs::write(dir.path().join("notes.txt"), b"").expect("seed file");

        let mut folder = ScanFolder::open(dir.path(), FileFormat::Png).expect("folder opens");
        let paths = folder
            .save(&[image(Side::Front, PixelFormat::Grey8)])
            .expect("save works");
        assert_eq!(paths, vec![dir.path().join("scan-0010.png")]);
    }

    #[test]
    fn unusable_folder_is_explained() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let err = ScanFolder::open(file.path().join("Scans"), FileFormat::Png)
            .err()
            .expect("a file cannot hold a folder");
        let message = err.to_string();
        assert!(message.starts_with("Can't save to"), "{message}");
        assert!(message.contains("Scans"), "{message}");
    }

    #[test]
    fn creates_missing_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("Pictures/Scans");
        let folder = ScanFolder::open(&nested, FileFormat::Png).expect("folder opens");
        assert!(folder.dir().is_dir());
    }

    #[test]
    fn duplex_sides_share_a_number() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut folder = ScanFolder::open(dir.path(), FileFormat::Png).expect("folder opens");
        let paths = folder
            .save(&[
                image(Side::Front, PixelFormat::Rgb8),
                image(Side::Back, PixelFormat::Rgb8),
            ])
            .expect("save works");
        assert_eq!(
            paths,
            vec![
                dir.path().join("scan-0001.png"),
                dir.path().join("scan-0001-back.png")
            ]
        );
        let leftovers = fs::read_dir(dir.path())
            .expect("readable")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn colour_png_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut folder = ScanFolder::open(dir.path(), FileFormat::Png).expect("folder opens");
        let original = image(Side::Front, PixelFormat::Rgb8);
        let paths = folder
            .save(std::slice::from_ref(&original))
            .expect("save works");
        let (info, data) = decode(&paths[0]);
        assert_eq!((info.width, info.height), (10, 2));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(data, original.data);
    }

    #[test]
    fn black_and_white_is_one_bit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut folder = ScanFolder::open(dir.path(), FileFormat::Png).expect("folder opens");
        let original = image(Side::Front, PixelFormat::BlackWhite);
        let paths = folder
            .save(std::slice::from_ref(&original))
            .expect("save works");
        let (info, data) = decode(&paths[0]);
        assert_eq!(info.bit_depth, png::BitDepth::One);
        // Each 10 pixel row packs into two bytes
        assert_eq!(data.len(), 4);
        assert_eq!(data[0], 0b0110_1101);
    }

    #[test]
    fn parses_scan_numbers() {
        assert_eq!(parse_number("scan-0012.png"), Some(12));
        assert_eq!(parse_number("scan-0012-back.png"), Some(12));
        assert_eq!(parse_number("scan-12345.png"), Some(12345));
        assert_eq!(parse_number("scan-0013.jpg"), Some(13));
        assert_eq!(parse_number("scan-0014.pdf"), Some(14));
        assert_eq!(parse_number("scan-0015.docx"), None);
        assert_eq!(parse_number(".scan-0003.png.part"), None);
        assert_eq!(parse_number("photo.png"), None);
    }

    #[test]
    fn numbering_continues_across_formats() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("scan-0004.jpg"), b"").expect("seed file");
        let mut folder = ScanFolder::open(dir.path(), FileFormat::Tiff).expect("folder opens");
        let paths = folder
            .save(&[image(Side::Front, PixelFormat::Grey8)])
            .expect("save works");
        assert_eq!(paths, vec![dir.path().join("scan-0005.tif")]);
    }

    #[test]
    fn pdf_keeps_both_sides_in_one_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut folder = ScanFolder::open(dir.path(), FileFormat::Pdf).expect("folder opens");
        let paths = folder
            .save(&[
                image(Side::Front, PixelFormat::Rgb8),
                image(Side::Back, PixelFormat::Rgb8),
            ])
            .expect("save works");
        assert_eq!(paths, vec![dir.path().join("scan-0001.pdf")]);
        let data = fs::read(&paths[0]).expect("pdf written");
        assert!(data.starts_with(b"%PDF"));
    }
}
