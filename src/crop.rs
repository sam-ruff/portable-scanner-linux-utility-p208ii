//! Trims the scanner's backing plate from around a receipt. The backing is
//! measured at the left and right edges of the scan, and the receipt is the
//! block of rows and columns where most pixels differ clearly from it.

use crate::image::{Image, PixelFormat, rgb_to_grey};

/// How far from the backing level a pixel must be to count as receipt.
const BACKING_TOLERANCE: u8 = 20;
/// Share of a row or column that must differ from the backing.
const RECEIPT_SHARE: f32 = 0.5;
/// Receipts narrower or shorter than this are treated as a failed detection.
const MIN_SIZE_MM: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropBox {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

fn luminance(image: &Image) -> Vec<u8> {
    match image.format {
        PixelFormat::Rgb8 => rgb_to_grey(&image.data),
        PixelFormat::Grey8 | PixelFormat::BlackWhite => image.data.clone(),
    }
}

/// Median brightness of thin strips down the left and right edges.
fn backing_level(grey: &[u8], width: usize, height: usize) -> u8 {
    let strip = (width / 50).max(2).min(width / 2);
    let mut samples: Vec<u8> = (0..height)
        .flat_map(|y| {
            let row = &grey[y * width..(y + 1) * width];
            row[..strip].iter().chain(&row[width - strip..]).copied()
        })
        .collect();
    let middle = samples.len() / 2;
    *samples.select_nth_unstable(middle).1
}

fn span(is_paper: impl Iterator<Item = bool>) -> Option<(usize, usize)> {
    let flags: Vec<bool> = is_paper.collect();
    let first = flags.iter().position(|p| *p)?;
    let last = flags.iter().rposition(|p| *p)?;
    Some((first, last + 1))
}

/// Finds the receipt, or `None` when the paper can't be told apart from the backing.
pub fn find_receipt(image: &Image) -> Option<CropBox> {
    let (width, height) = (image.width, image.height);
    if width < 8 || height < 8 || image.format == PixelFormat::BlackWhite {
        return None;
    }
    let grey = luminance(image);
    let backing = backing_level(&grey, width, height);
    let receipt = |v: &u8| v.abs_diff(backing) > BACKING_TOLERANCE;

    let (left, right) = span((0..width).map(|x| {
        let count = (0..height)
            .filter(|y| receipt(&grey[y * width + x]))
            .count();
        count as f32 / height as f32 > RECEIPT_SHARE
    }))?;
    let (top, bottom) = span((0..height).map(|y| {
        let row = &grey[y * width + left..y * width + right];
        row.iter().filter(|v| receipt(v)).count() as f32 / row.len() as f32 > RECEIPT_SHARE
    }))?;

    let min_px = MIN_SIZE_MM * usize::from(image.dpi) * 10 / 254;
    let found = CropBox {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    };
    let whole = found.width == width && found.height == height;
    if found.width < min_px || found.height < min_px || whole {
        return None;
    }
    Some(found)
}

pub fn crop(image: &Image, area: CropBox) -> Image {
    let channels = if image.format == PixelFormat::Rgb8 {
        3
    } else {
        1
    };
    let stride = image.width * channels;
    let data = (area.y..area.y + area.height)
        .flat_map(|y| {
            let start = y * stride + area.x * channels;
            image.data[start..start + area.width * channels]
                .iter()
                .copied()
        })
        .collect();
    Image {
        width: area.width,
        height: area.height,
        data,
        ..image.clone()
    }
}

/// Crops to the receipt when one is found, otherwise returns the scan unchanged.
pub fn smart_crop(image: Image) -> Image {
    match find_receipt(&image) {
        Some(area) => crop(&image, area),
        None => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Side;

    const BACKING: u8 = 140;
    const PAPER: u8 = 240;
    const INK: u8 = 20;

    /// A 300 dpi grey scan with paper at the given box and a line of text.
    fn scan(width: usize, height: usize, paper: CropBox) -> Image {
        let mut data = vec![BACKING; width * height];
        for y in paper.y..paper.y + paper.height {
            for x in paper.x..paper.x + paper.width {
                let text = y % 20 < 6 && x % 7 != 0;
                data[y * width + x] = if text { INK } else { PAPER };
            }
        }
        Image {
            side: Side::Front,
            width,
            height,
            dpi: 300,
            format: PixelFormat::Grey8,
            data,
        }
    }

    #[test]
    fn finds_receipt_on_backing() {
        let paper = CropBox {
            x: 300,
            y: 40,
            width: 900,
            height: 1500,
        };
        assert_eq!(find_receipt(&scan(2500, 1600, paper)), Some(paper));
    }

    #[test]
    fn crop_keeps_only_the_receipt() {
        let paper = CropBox {
            x: 100,
            y: 0,
            width: 400,
            height: 600,
        };
        let cropped = smart_crop(scan(1000, 600, paper));
        assert_eq!((cropped.width, cropped.height), (400, 600));
        assert!(cropped.data.iter().all(|v| *v == PAPER || *v == INK));
    }

    #[test]
    fn colour_scans_crop_every_channel() {
        let paper = CropBox {
            x: 50,
            y: 10,
            width: 300,
            height: 300,
        };
        let grey = scan(600, 330, paper);
        let colour = Image {
            format: PixelFormat::Rgb8,
            data: grey.data.iter().flat_map(|v| [*v, *v, *v]).collect(),
            ..grey
        };
        let cropped = smart_crop(colour);
        assert_eq!((cropped.width, cropped.height), (300, 300));
        assert_eq!(cropped.data.len(), 300 * 300 * 3);
    }

    #[test]
    fn full_width_paper_is_left_alone() {
        let paper = CropBox {
            x: 0,
            y: 0,
            width: 800,
            height: 800,
        };
        assert_eq!(find_receipt(&scan(800, 800, paper)), None);
    }

    #[test]
    fn blank_backing_is_left_alone() {
        let image = scan(
            800,
            800,
            CropBox {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
        );
        assert_eq!(find_receipt(&image), None);
        assert_eq!(smart_crop(image.clone()), image);
    }

    #[test]
    fn tiny_detections_are_ignored() {
        let speck = CropBox {
            x: 400,
            y: 400,
            width: 30,
            height: 30,
        };
        assert_eq!(find_receipt(&scan(1000, 1000, speck)), None);
    }
}
