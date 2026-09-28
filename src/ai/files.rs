use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::encode::{FileFormat, write_png, write_sheet};
use crate::image::{Image, PixelFormat};

use super::{Analysis, Provider, ReceiptReader};

#[derive(Debug)]
struct Original {
    path: PathBuf,
    metadata: fs::Metadata,
}

pub struct Job {
    pub provider: Provider,
    pub images: Vec<Image>,
    pub format: FileFormat,
    originals: Vec<Original>,
}

impl Job {
    pub fn new(
        provider: Provider,
        images: Vec<Image>,
        format: FileFormat,
        paths: Vec<PathBuf>,
    ) -> Result<Self, String> {
        let expected = if format == FileFormat::Pdf {
            1
        } else {
            images.len()
        };
        if images.is_empty()
            || paths.len() != expected
            || paths.iter().any(|path| path.parent() != paths[0].parent())
        {
            return Err("scan images and files do not match".into());
        }
        let originals = paths
            .into_iter()
            .map(|path| {
                let metadata = fs::symlink_metadata(&path).map_err(|err| err.to_string())?;
                if !metadata.is_file() {
                    return Err("original scan is not a regular file".into());
                }
                Ok(Original { path, metadata })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            provider,
            images,
            format,
            originals,
        })
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.originals
            .iter()
            .map(|original| original.path.clone())
            .collect()
    }

    fn unchanged(&self) -> bool {
        self.originals.iter().all(|original| {
            fs::symlink_metadata(&original.path).is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.dev() == original.metadata.dev()
                    && metadata.ino() == original.metadata.ino()
                    && metadata.len() == original.metadata.len()
                    && metadata.mtime() == original.metadata.mtime()
                    && metadata.mtime_nsec() == original.metadata.mtime_nsec()
                    && metadata.ctime() == original.metadata.ctime()
                    && metadata.ctime_nsec() == original.metadata.ctime_nsec()
            })
        })
    }
}

/// Returns the replacement paths only after every output has been safely written.
pub fn process(job: &Job, reader: &impl ReceiptReader) -> Result<Vec<PathBuf>, String> {
    if job.images.is_empty() || job.originals.is_empty() || !job.unchanged() {
        return Err("original scans are missing or have changed".into());
    }
    let workspace = tempfile::tempdir().map_err(|err| err.to_string())?;
    let mut previews = Vec::new();
    for (index, image) in job.images.iter().enumerate() {
        let path = workspace.path().join(format!("side-{index}.png"));
        let file = fs::File::create(&path).map_err(|err| err.to_string())?;
        write_png(image, file).map_err(|err| err.to_string())?;
        previews.push(path);
    }
    let analysis = reader.analyse(previews)?;
    let stem = file_stem(&analysis, job.images.len())?;
    let keep: Vec<_> = job
        .images
        .iter()
        .enumerate()
        .filter_map(|(index, image)| {
            (!(analysis.blank_sides[index] && looks_blank(image))).then_some(index)
        })
        .collect();
    if keep.is_empty() {
        return Err("AI found no readable side; keeping the originals".into());
    }
    replace(job, &stem, &keep)
}

fn file_stem(analysis: &Analysis, sides: usize) -> Result<String, String> {
    if !analysis.confidence.is_finite()
        || !(0.9..=1.0).contains(&analysis.confidence)
        || analysis.blank_sides.len() != sides
    {
        return Err("AI result is uncertain or incomplete".into());
    }
    let merchant = sanitise(&analysis.merchant);
    if merchant.len() < 2 {
        return Err("AI could not identify the merchant".into());
    }
    let mut parts = Vec::new();
    if let Some(date) = &analysis.date {
        if date.len() != 10
            || !date.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            return Err("AI returned an invalid date".into());
        }
        parts.push(date.clone());
    }
    parts.push(merchant);
    if let Some(total) = &analysis.total {
        if total.len() > 20
            || total.is_empty()
            || !total.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            || total.parse::<f64>().is_err()
        {
            return Err("AI returned an invalid total".into());
        }
        parts.push(total.clone());
    }
    if let Some(currency) = &analysis.currency {
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err("AI returned an invalid currency".into());
        }
        parts.push(currency.to_uppercase());
    }
    Ok(parts.join("-"))
}

fn sanitise(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(60)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

fn looks_blank(image: &Image) -> bool {
    let channels = if image.format == PixelFormat::Rgb8 {
        3
    } else {
        1
    };
    let pixels = image.data.len() / channels;
    if pixels == 0 {
        return false;
    }
    let dark = image
        .data
        .chunks_exact(channels)
        .filter(|pixel| pixel.iter().any(|value| *value < 180))
        .count();
    dark as f64 / (pixels as f64) < 0.001
}

fn target_paths(dir: &Path, stem: &str, extension: &str, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|index| {
            let suffix = if index == 0 {
                String::new()
            } else {
                "-back".into()
            };
            dir.join(format!("{stem}{suffix}.{extension}"))
        })
        .collect()
}

fn replace(job: &Job, stem: &str, keep: &[usize]) -> Result<Vec<PathBuf>, String> {
    let dir = job.originals[0]
        .path
        .parent()
        .ok_or("invalid scan directory")?;
    let count = if job.format == FileFormat::Pdf {
        1
    } else {
        keep.len()
    };
    let mut temporary = Vec::new();
    for index in 0..count {
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|err| err.to_string())?;
        if job.format == FileFormat::Pdf && keep.len() != job.images.len() {
            let images: Vec<_> = keep
                .iter()
                .map(|index| job.images[*index].clone())
                .collect();
            write_sheet(job.format, &images, &mut file).map_err(|err| err.to_string())?;
        } else {
            let original = if job.format == FileFormat::Pdf {
                0
            } else {
                keep[index]
            };
            let mut source =
                fs::File::open(&job.originals[original].path).map_err(|err| err.to_string())?;
            std::io::copy(&mut source, &mut file).map_err(|err| err.to_string())?;
        }
        file.as_file().sync_all().map_err(|err| err.to_string())?;
        temporary.push(file);
    }
    if !job.unchanged() {
        return Err("original scans changed during AI processing".into());
    }
    let paths = (0..10_000)
        .map(|suffix| {
            let name = if suffix == 0 {
                stem.into()
            } else {
                format!("{stem}-{suffix}")
            };
            target_paths(dir, &name, job.format.extension(), count)
        })
        .find(|paths| paths.iter().all(|path| !path.exists()))
        .ok_or("no free receipt filename")?;
    let mut written = Vec::new();
    for (file, path) in temporary.into_iter().zip(&paths) {
        if let Err(err) = file.persist_noclobber(path) {
            for path in &written {
                let _ = fs::remove_file(path);
            }
            return Err(format!("could not save AI result: {err}"));
        }
        written.push(path.clone());
    }
    fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|err| err.to_string())?;
    if !job.unchanged() {
        for path in &written {
            let _ = fs::remove_file(path);
        }
        return Err("original scans changed before replacement".into());
    }
    for original in &job.originals {
        if let Err(err) = fs::remove_file(&original.path) {
            log::warn!(
                "AI result saved, but could not remove {}: {err}",
                original.path.display()
            );
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{MockReceiptReader, ProviderKind};
    use crate::image::Side;
    use crate::output::ScanFolder;

    fn image(side: Side, blank: bool) -> Image {
        Image {
            side,
            width: 100,
            height: 100,
            dpi: 150,
            format: PixelFormat::Grey8,
            data: vec![if blank { 240 } else { 80 }; 10_000],
        }
    }

    fn job(dir: &Path, format: FileFormat, blank_front: bool, blank_back: bool) -> Job {
        let images = vec![
            image(Side::Front, blank_front),
            image(Side::Back, blank_back),
        ];
        let paths = ScanFolder::open(dir, format)
            .expect("folder")
            .save(&images)
            .expect("save");
        Job::new(
            Provider {
                kind: ProviderKind::Codex,
                executable: PathBuf::from("codex"),
            },
            images,
            format,
            paths,
        )
        .expect("job")
    }

    fn analysis(blank_sides: Vec<bool>) -> Analysis {
        Analysis {
            merchant: "A Test Shop".into(),
            date: Some("2026-09-28".into()),
            total: Some("12.30".into()),
            currency: Some("GBP".into()),
            confidence: 0.99,
            blank_sides,
        }
    }

    fn reader(analysis: Analysis) -> MockReceiptReader {
        let mut reader = MockReceiptReader::new();
        reader
            .expect_analyse()
            .times(1)
            .return_once(move |_| Ok(analysis));
        reader
    }

    #[test]
    fn blank_front_is_removed_only_after_named_back_is_saved() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Png, true, false);
        let back = fs::read(&job.originals[1].path).expect("back");
        let paths = process(&job, &reader(analysis(vec![true, false]))).expect("rename");
        assert_eq!(
            paths,
            vec![dir.path().join("2026-09-28-a-test-shop-12.30-GBP.png")]
        );
        assert_eq!(fs::read(&paths[0]).expect("renamed"), back);
        assert!(job.paths().iter().all(|path| !path.exists()));
    }

    #[test]
    fn uncertain_results_and_ai_errors_keep_both_originals() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Png, true, false);
        let mut uncertain = analysis(vec![true, false]);
        uncertain.confidence = 0.5;
        assert!(process(&job, &reader(uncertain)).is_err());
        let mut failed = MockReceiptReader::new();
        failed
            .expect_analyse()
            .returning(|_| Err("not logged in".into()));
        assert!(process(&job, &failed).is_err());
        assert!(job.paths().iter().all(|path| path.is_file()));
    }

    #[test]
    fn local_ink_check_preserves_content_even_if_ai_calls_it_blank() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Png, false, false);
        let paths = process(&job, &reader(analysis(vec![true, false]))).expect("rename");
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.is_file()));
    }

    #[test]
    fn all_blank_receipts_and_wrong_side_counts_are_kept() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Png, true, true);
        assert!(process(&job, &reader(analysis(vec![true, true]))).is_err());
        assert!(process(&job, &reader(analysis(vec![false]))).is_err());
        assert!(job.paths().iter().all(|path| path.is_file()));
    }

    #[test]
    fn existing_named_receipts_are_never_overwritten() {
        let dir = tempfile::tempdir().expect("temp directory");
        let existing = dir.path().join("2026-09-28-a-test-shop-12.30-GBP.png");
        fs::write(&existing, "existing receipt").expect("existing");
        let job = job(dir.path(), FileFormat::Png, false, true);
        let paths = process(&job, &reader(analysis(vec![false, true]))).expect("rename");
        assert_ne!(paths[0], existing);
        assert_eq!(
            fs::read_to_string(existing).expect("existing"),
            "existing receipt"
        );
    }

    #[test]
    fn changed_original_is_not_replaced() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Png, false, true);
        let path = job.originals[0].path.clone();
        let mut reader = MockReceiptReader::new();
        reader.expect_analyse().return_once(move |_| {
            fs::write(path, "edited by the user").expect("edit");
            Ok(analysis(vec![false, true]))
        });
        assert!(process(&job, &reader).is_err());
        assert_eq!(
            fs::read_to_string(&job.originals[0].path).expect("original"),
            "edited by the user"
        );
        assert!(job.originals[1].path.exists());
    }

    #[test]
    fn pdf_is_rebuilt_with_only_the_nonblank_page() {
        let dir = tempfile::tempdir().expect("temp directory");
        let job = job(dir.path(), FileFormat::Pdf, true, false);
        let paths = process(&job, &reader(analysis(vec![true, false]))).expect("rename");
        let data = fs::read(&paths[0]).expect("pdf");
        assert!(data.windows(8).any(|bytes| bytes == b"/Count 1"));
        assert!(!job.originals[0].path.exists());
    }

    #[test]
    fn filenames_cannot_escape_the_scan_folder() {
        let mut result = analysis(vec![false]);
        result.merchant = "../../Shop/../Other\nReceipt".into();
        let stem = file_stem(&result, 1).expect("safe name");
        assert!(!stem.contains('/') && !stem.contains("..") && !stem.contains('\n'));
        result.date = Some("../../other".into());
        assert!(file_stem(&result, 1).is_err());
    }
}
