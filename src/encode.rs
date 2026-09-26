//! Image file encoders. Every format records the scan resolution where it can,
//! so OCR tools and printers know the real size of the receipt.

use std::fmt;
use std::io::{Cursor, Write};

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::error::ScanError;
use crate::image::{Image, PixelFormat};

const JPEG_QUALITY: u8 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum FileFormat {
    #[default]
    Png,
    Jpeg,
    Tiff,
    /// One document per sheet, with the back as a second page.
    Pdf,
}

impl FileFormat {
    pub const ALL: [FileFormat; 4] = [
        FileFormat::Png,
        FileFormat::Jpeg,
        FileFormat::Tiff,
        FileFormat::Pdf,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            FileFormat::Png => "png",
            FileFormat::Jpeg => "jpg",
            FileFormat::Tiff => "tif",
            FileFormat::Pdf => "pdf",
        }
    }
}

impl fmt::Display for FileFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileFormat::Png => "PNG",
            FileFormat::Jpeg => "JPEG",
            FileFormat::Tiff => "TIFF",
            FileFormat::Pdf => "PDF",
        })
    }
}

fn encode_error(format: FileFormat, err: impl fmt::Display) -> ScanError {
    ScanError::Io(format!("could not encode {format}: {err}"))
}

/// Packs 0/255 pixels into 1 bit per pixel rows, where a set bit is white.
fn pack_bits(image: &Image) -> Vec<u8> {
    image
        .data
        .chunks(image.width)
        .flat_map(|row| {
            row.chunks(8).map(|pixels| {
                pixels.iter().enumerate().fold(
                    0u8,
                    |byte, (i, v)| if *v > 0 { byte | 0x80 >> i } else { byte },
                )
            })
        })
        .collect()
}

fn colour_type(image: &Image) -> ExtendedColorType {
    match image.format {
        PixelFormat::Rgb8 => ExtendedColorType::Rgb8,
        PixelFormat::Grey8 | PixelFormat::BlackWhite => ExtendedColorType::L8,
    }
}

pub fn write_png(image: &Image, out: impl Write) -> Result<(), ScanError> {
    let fail = |err: png::EncodingError| encode_error(FileFormat::Png, err);
    let mut encoder = png::Encoder::new(out, image.width as u32, image.height as u32);
    let (colour, depth) = match image.format {
        PixelFormat::Rgb8 => (png::ColorType::Rgb, png::BitDepth::Eight),
        PixelFormat::Grey8 => (png::ColorType::Grayscale, png::BitDepth::Eight),
        PixelFormat::BlackWhite => (png::ColorType::Grayscale, png::BitDepth::One),
    };
    encoder.set_color(colour);
    encoder.set_depth(depth);
    let pixels_per_metre = u32::from(image.dpi) * 10_000 / 254;
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: pixels_per_metre,
        yppu: pixels_per_metre,
        unit: png::Unit::Meter,
    }));

    let mut writer = encoder.write_header().map_err(fail)?;
    match image.format {
        PixelFormat::BlackWhite => writer.write_image_data(&pack_bits(image)),
        _ => writer.write_image_data(&image.data),
    }
    .map_err(fail)?;
    writer.finish().map_err(fail)
}

fn jpeg_bytes(image: &Image) -> Result<Vec<u8>, ScanError> {
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    encoder.set_pixel_density(PixelDensity::dpi(image.dpi));
    encoder
        .encode(
            &image.data,
            image.width as u32,
            image.height as u32,
            colour_type(image),
        )
        .map_err(|err| encode_error(FileFormat::Jpeg, err))?;
    Ok(out)
}

fn tiff_bytes(image: &Image) -> Result<Vec<u8>, ScanError> {
    let mut out = Cursor::new(Vec::new());
    TiffEncoder::new(&mut out)
        .write_image(
            &image.data,
            image.width as u32,
            image.height as u32,
            colour_type(image),
        )
        .map_err(|err| encode_error(FileFormat::Tiff, err))?;
    Ok(out.into_inner())
}

/// Writes one sheet: a single image, or every side as pages of a PDF.
pub fn write_sheet(
    format: FileFormat,
    images: &[Image],
    mut out: impl Write,
) -> Result<(), ScanError> {
    let first = images
        .first()
        .ok_or_else(|| ScanError::Io("nothing to save".into()))?;
    match format {
        FileFormat::Png => write_png(first, out),
        FileFormat::Jpeg => Ok(out.write_all(&jpeg_bytes(first)?)?),
        FileFormat::Tiff => Ok(out.write_all(&tiff_bytes(first)?)?),
        FileFormat::Pdf => Ok(out.write_all(&pdf_bytes(images)?)?),
    }
}

/// Builds a PDF with one page per image, each page the physical size of the scan.
fn pdf_bytes(images: &[Image]) -> Result<Vec<u8>, ScanError> {
    let mut pdf = PdfWriter::default();
    pdf.object(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..images.len())
        .map(|i| format!("{} 0 R", 3 + i * 3))
        .collect();
    pdf.object(
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            images.len()
        )
        .as_bytes(),
    );

    for (i, image) in images.iter().enumerate() {
        let (page, contents, xobject) = (3 + i * 3, 4 + i * 3, 5 + i * 3);
        let points = |px: usize| px as f64 * 72.0 / f64::from(image.dpi.max(1));
        let (w, h) = (points(image.width), points(image.height));

        pdf.object(
            page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] \
                 /Resources << /XObject << /Im0 {xobject} 0 R >> >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        pdf.stream(
            contents,
            "",
            format!("q {w:.2} 0 0 {h:.2} 0 0 cm /Im0 Do Q").as_bytes(),
        );

        let (colour_space, bits, filter, data) = match image.format {
            PixelFormat::BlackWhite => (
                "/DeviceGray",
                1,
                "/FlateDecode",
                miniz_oxide::deflate::compress_to_vec_zlib(&pack_bits(image), 6),
            ),
            PixelFormat::Grey8 => ("/DeviceGray", 8, "/DCTDecode", jpeg_bytes(image)?),
            PixelFormat::Rgb8 => ("/DeviceRGB", 8, "/DCTDecode", jpeg_bytes(image)?),
        };
        pdf.stream(
            xobject,
            &format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {colour_space} \
                 /BitsPerComponent {bits} /Filter {filter}",
                image.width, image.height
            ),
            &data,
        );
    }
    Ok(pdf.finish())
}

/// Minimal PDF serialiser that tracks object offsets for the cross-reference table.
#[derive(Default)]
struct PdfWriter {
    body: Vec<u8>,
    offsets: Vec<(usize, usize)>,
}

impl PdfWriter {
    fn start(&mut self, id: usize) {
        if self.body.is_empty() {
            self.body
                .extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
        }
        self.offsets.push((id, self.body.len()));
        self.body
            .extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    }

    fn object(&mut self, id: usize, content: &[u8]) {
        self.start(id);
        self.body.extend_from_slice(content);
        self.body.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, id: usize, dictionary: &str, data: &[u8]) {
        self.start(id);
        self.body.extend_from_slice(
            format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).as_bytes(),
        );
        self.body.extend_from_slice(data);
        self.body.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn finish(mut self) -> Vec<u8> {
        self.offsets.sort_unstable();
        let xref = self.body.len();
        let size = self.offsets.len() + 1;
        let mut table = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for (_, offset) in &self.offsets {
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        table.push_str(&format!(
            "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        ));
        self.body.extend_from_slice(table.as_bytes());
        self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Side;

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

    fn sheet(format: FileFormat, images: &[Image]) -> Vec<u8> {
        let mut out = Vec::new();
        write_sheet(format, images, &mut out).expect("encodes");
        out
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn extensions_match_formats() {
        let extensions: Vec<_> = FileFormat::ALL.iter().map(|f| f.extension()).collect();
        assert_eq!(extensions, vec!["png", "jpg", "tif", "pdf"]);
    }

    #[test]
    fn jpeg_records_resolution() {
        let data = sheet(FileFormat::Jpeg, &[image(Side::Front, PixelFormat::Rgb8)]);
        assert_eq!(&data[..3], &[0xff, 0xd8, 0xff]);
        // JFIF density: units = 1 (dpi), then 300 x 300
        let jfif = find(&data, b"JFIF\0").expect("JFIF header");
        assert_eq!(&data[jfif + 7..jfif + 12], &[1, 1, 44, 1, 44]);
    }

    #[test]
    fn tiff_has_header() {
        let data = sheet(FileFormat::Tiff, &[image(Side::Front, PixelFormat::Grey8)]);
        assert!(data.starts_with(b"II*\0") || data.starts_with(b"MM\0*"));
    }

    #[test]
    fn black_and_white_encodes_in_every_format() {
        let bw = image(Side::Front, PixelFormat::BlackWhite);
        for format in FileFormat::ALL {
            assert!(
                !sheet(format, std::slice::from_ref(&bw)).is_empty(),
                "{format}"
            );
        }
    }

    #[test]
    fn pdf_puts_each_side_on_a_page() {
        let data = sheet(
            FileFormat::Pdf,
            &[
                image(Side::Front, PixelFormat::Rgb8),
                image(Side::Back, PixelFormat::Rgb8),
            ],
        );
        assert!(data.starts_with(b"%PDF-1.4"));
        assert!(find(&data, b"/Count 2").is_some());
        // 10 px at 300 dpi is 2.4 points
        assert!(find(&data, b"/MediaBox [0 0 2.40 0.48]").is_some());
    }

    #[test]
    fn pdf_cross_reference_points_at_objects() {
        let data = sheet(FileFormat::Pdf, &[image(Side::Front, PixelFormat::Grey8)]);
        // The trailer and table are ASCII, but the image data before them is not
        let marker = b"startxref\n";
        let trailer = data
            .windows(marker.len())
            .rposition(|w| w == marker)
            .expect("startxref present");
        let tail = String::from_utf8_lossy(&data[trailer + marker.len()..]);
        let start: usize = tail
            .lines()
            .next()
            .and_then(|n| n.parse().ok())
            .expect("startxref offset");
        assert!(data[start..].starts_with(b"xref"));

        let table = String::from_utf8_lossy(&data[start..]);
        let offsets: Vec<usize> = table
            .lines()
            .skip(3)
            .take_while(|line| line.ends_with(" n "))
            .filter_map(|line| line.split(' ').next()?.parse().ok())
            .collect();
        assert_eq!(offsets.len(), 5);
        for (index, offset) in offsets.iter().enumerate() {
            let expected = format!("{} 0 obj", index + 1);
            assert!(
                data[*offset..].starts_with(expected.as_bytes()),
                "object {}",
                index + 1
            );
        }
    }

    #[test]
    fn empty_sheet_is_an_error() {
        assert!(write_sheet(FileFormat::Png, &[], Vec::new()).is_err());
    }
}
