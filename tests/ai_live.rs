use std::path::{Path, PathBuf};

use p208ii::ai::{self, Analysis, CliReader, Job, ReceiptReader};
use p208ii::encode::FileFormat;
use p208ii::image::{Image, PixelFormat, Side};
use p208ii::output::ScanFolder;

fn load(path: &Path, side: Side) -> Image {
    let file = std::fs::File::open(path).expect("receipt fixture");
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("PNG");
    let mut data = vec![0; reader.output_buffer_size().expect("image size")];
    let info = reader.next_frame(&mut data).expect("image data");
    data.truncate(info.buffer_size());
    let format = match info.color_type {
        png::ColorType::Rgb => PixelFormat::Rgb8,
        png::ColorType::Grayscale => PixelFormat::Grey8,
        other => panic!("fixture must be RGB or greyscale, got {other:?}"),
    };
    Image {
        side,
        width: info.width as usize,
        height: info.height as usize,
        dpi: 300,
        format,
        data,
    }
}

struct ReportingReader(CliReader);

impl ReceiptReader for ReportingReader {
    fn analyse(&self, images: Vec<PathBuf>) -> Result<Analysis, String> {
        let result = self.0.analyse(images)?;
        println!(
            "AI confidence: {}, blank sides: {:?}",
            result.confidence, result.blank_sides
        );
        Ok(result)
    }
}

#[test]
#[ignore = "uses a logged-in AI CLI and receipt fixtures supplied through environment variables"]
fn real_cli_names_receipt_copies() {
    let name = std::env::var("P208II_AI_PROVIDER").expect("provider");
    let provider = ai::discover()
        .into_iter()
        .find(|provider| provider.kind.to_string().eq_ignore_ascii_case(&name))
        .expect("installed provider");
    let front = PathBuf::from(std::env::var("P208II_AI_TEST_FRONT").expect("front fixture"));
    let back = PathBuf::from(std::env::var("P208II_AI_TEST_BACK").expect("back fixture"));
    let images = vec![load(&front, Side::Front), load(&back, Side::Back)];
    let dir = tempfile::tempdir().expect("test directory");
    let originals = ScanFolder::open(dir.path(), FileFormat::Png)
        .expect("folder")
        .save(&images)
        .expect("test copies");
    let job = Job::new(provider.clone(), images, FileFormat::Png, originals.clone()).expect("job");
    let paths = ai::process(&job, &ReportingReader(CliReader { provider })).expect("AI result");
    assert!(paths.iter().all(|path| path.is_file()));
    assert!(originals.iter().all(|path| !path.exists()));
    println!(
        "Named receipt: {:?}",
        paths
            .iter()
            .filter_map(|p| p.file_name())
            .collect::<Vec<_>>()
    );
}
