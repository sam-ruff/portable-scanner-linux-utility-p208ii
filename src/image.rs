//! Turns the byte stream from the scanner into images. The P-208 family sends
//! duplex data with front and back bytes alternating, colour lines as three
//! planes, and the back side mirrored.

use crate::params::{ColourMode, Geometry, HwMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Front,
    Back,
}

impl Side {
    pub fn index(self) -> usize {
        match self {
            Side::Front => 0,
            Side::Back => 1,
        }
    }
}

/// How one side's pixels are laid out within a raw line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interlace {
    Plain,
    Mirrored,
    Planes,
    MirroredPlanes,
}

impl Interlace {
    pub fn for_side(side: Side, mode: HwMode) -> Self {
        match (side, mode) {
            (Side::Front, HwMode::Grey) => Interlace::Plain,
            (Side::Back, HwMode::Grey) => Interlace::Mirrored,
            (Side::Front, HwMode::Colour) => Interlace::Planes,
            (Side::Back, HwMode::Colour) => Interlace::MirroredPlanes,
        }
    }
}

/// Reorders one raw line into left to right pixels (RGB triples for colour).
pub fn deinterlace_line(raw: &[u8], out: &mut [u8], width: usize, interlace: Interlace) {
    match interlace {
        Interlace::Plain => out.copy_from_slice(raw),
        Interlace::Mirrored => {
            for (dst, src) in out.iter_mut().zip(raw.iter().rev()) {
                *dst = *src;
            }
        }
        Interlace::Planes | Interlace::MirroredPlanes => {
            let mirrored = interlace == Interlace::MirroredPlanes;
            for x in 0..width {
                let src = if mirrored { width - 1 - x } else { x };
                for channel in 0..3 {
                    out[x * 3 + channel] = raw[channel * width + src];
                }
            }
        }
    }
}

/// The inverse of [`deinterlace_line`], producing what the scanner would send.
pub fn interlace_line(pixels: &[u8], out: &mut [u8], width: usize, interlace: Interlace) {
    match interlace {
        Interlace::Plain | Interlace::Mirrored => deinterlace_line(pixels, out, width, interlace),
        Interlace::Planes | Interlace::MirroredPlanes => {
            let mirrored = interlace == Interlace::MirroredPlanes;
            for x in 0..width {
                let dst = if mirrored { width - 1 - x } else { x };
                for channel in 0..3 {
                    out[channel * width + dst] = pixels[x * 3 + channel];
                }
            }
        }
    }
}

/// Per byte black level and white level measured during calibration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Correction {
    pub offset: Option<Vec<u8>>,
    pub gain: Option<Vec<u8>>,
}

impl Correction {
    pub fn apply(&self, line: &mut [u8]) {
        if let Some(offset) = &self.offset {
            for (value, black) in line.iter_mut().zip(offset) {
                *value = value.saturating_sub(*black);
            }
        }
        if let Some(gain) = &self.gain {
            for (value, white) in line.iter_mut().zip(gain) {
                let scaled = u32::from(*value) * 240 / u32::from((*white).max(1));
                *value = scaled.min(255) as u8;
            }
        }
    }
}

/// One side of a scan as delivered by the scanner, already deinterlaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImage {
    pub side: Side,
    pub width: usize,
    pub lines: usize,
    pub mode: HwMode,
    pub dpi: u16,
    pub data: Vec<u8>,
}

impl RawImage {
    pub fn bytes_per_line(&self) -> usize {
        self.width * self.mode.bytes_per_pixel()
    }

    pub fn line(&self, index: usize) -> Option<&[u8]> {
        let bpl = self.bytes_per_line();
        self.data.get(index * bpl..(index + 1) * bpl)
    }
}

struct SideBuffer {
    interlace: Interlace,
    correction: Correction,
    image: RawImage,
    max_lines: usize,
    scratch: Vec<u8>,
}

impl SideBuffer {
    fn push(&mut self, raw: &[u8]) {
        let bpl = self.scratch.len();
        for raw_line in raw.chunks_exact(bpl) {
            if self.image.lines >= self.max_lines {
                return;
            }
            deinterlace_line(
                raw_line,
                &mut self.scratch,
                self.image.width,
                self.interlace,
            );
            self.correction.apply(&mut self.scratch);
            self.image.data.extend_from_slice(&self.scratch);
            self.image.lines += 1;
        }
    }
}

/// Collects arbitrarily sized chunks from the scanner into whole lines per side.
pub struct ImageAssembler {
    unit: usize,
    carry: Vec<u8>,
    sides: Vec<SideBuffer>,
}

impl ImageAssembler {
    pub fn new(geometry: &Geometry, corrections: [Correction; 2]) -> Self {
        let sides = if geometry.duplex {
            vec![Side::Front, Side::Back]
        } else {
            vec![Side::Front]
        };
        let [front, back] = corrections;
        let sides = sides
            .into_iter()
            .zip([front, back])
            .map(|(side, correction)| SideBuffer {
                interlace: Interlace::for_side(side, geometry.mode),
                correction,
                image: RawImage {
                    side,
                    width: geometry.width,
                    lines: 0,
                    mode: geometry.mode,
                    dpi: geometry.dpi,
                    data: Vec::new(),
                },
                max_lines: geometry.lines,
                scratch: vec![0; geometry.bytes_per_line],
            })
            .collect::<Vec<_>>();
        Self {
            unit: geometry.bytes_per_line * sides.len(),
            carry: Vec::new(),
            sides,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.carry.extend_from_slice(chunk);
        let whole = self.carry.len() / self.unit * self.unit;
        if whole == 0 {
            return;
        }
        let block: Vec<u8> = self.carry.drain(..whole).collect();
        match self.sides.as_mut_slice() {
            [single] => single.push(&block),
            [front, back] => {
                let (front_bytes, back_bytes) = split_duplex(&block);
                front.push(&front_bytes);
                back.push(&back_bytes);
            }
            _ => {}
        }
    }

    pub fn finish(self) -> Vec<RawImage> {
        self.sides.into_iter().map(|side| side.image).collect()
    }
}

/// Splits alternating front and back bytes.
pub fn split_duplex(block: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let front = block.iter().step_by(2).copied().collect();
    let back = block.iter().skip(1).step_by(2).copied().collect();
    (front, back)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb8,
    Grey8,
    /// One byte per pixel, either 0 (black) or 255 (white).
    BlackWhite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub side: Side,
    pub width: usize,
    pub height: usize,
    pub dpi: u16,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

fn to_grey(raw: &RawImage) -> Vec<u8> {
    match raw.mode {
        HwMode::Grey => raw.data.clone(),
        HwMode::Colour => raw
            .data
            .chunks_exact(3)
            .map(|p| ((u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])) / 3) as u8)
            .collect(),
    }
}

/// Converts scanner output into the format the user asked for.
pub fn render(raw: &RawImage, mode: ColourMode, threshold: u8) -> Image {
    let (format, data) = match (mode, raw.mode) {
        (ColourMode::Colour, HwMode::Colour) => (PixelFormat::Rgb8, raw.data.clone()),
        (ColourMode::Colour, HwMode::Grey) | (ColourMode::Grey, _) => {
            (PixelFormat::Grey8, to_grey(raw))
        }
        (ColourMode::BlackWhite, _) => (
            PixelFormat::BlackWhite,
            to_grey(raw)
                .into_iter()
                .map(|v| if v < threshold { 0 } else { 255 })
                .collect(),
        ),
    };
    Image {
        side: raw.side,
        width: raw.width,
        height: raw.lines,
        dpi: raw.dpi,
        format,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdb::Vpd;
    use crate::params::Capabilities;

    fn caps() -> Capabilities {
        Capabilities::from_vpd(&Vpd {
            basic_x_res: 600,
            basic_y_res: 600,
            step_y_res: false,
            max_y_res: 600,
            min_y_res: 150,
            standard_resolutions: vec![150, 300, 600],
            max_width: 10200,
            max_length: 43200,
            can_grey: true,
        })
    }

    fn geometry(mode: HwMode, duplex: bool, lines_units: u32) -> Geometry {
        // 8 pixels wide at 150 dpi
        Geometry::new(&caps(), 150, mode, duplex, 64, lines_units).expect("valid geometry")
    }

    #[test]
    fn colour_planes_become_rgb_triples() {
        let raw = [1, 2, 10, 20, 100, 200];
        let mut out = [0; 6];
        deinterlace_line(&raw, &mut out, 2, Interlace::Planes);
        assert_eq!(out, [1, 10, 100, 2, 20, 200]);
    }

    #[test]
    fn mirrored_planes_reverse_pixel_order() {
        let raw = [1, 2, 10, 20, 100, 200];
        let mut out = [0; 6];
        deinterlace_line(&raw, &mut out, 2, Interlace::MirroredPlanes);
        assert_eq!(out, [2, 20, 200, 1, 10, 100]);
    }

    #[test]
    fn mirrored_grey_reverses_line() {
        let mut out = [0; 4];
        deinterlace_line(&[1, 2, 3, 4], &mut out, 4, Interlace::Mirrored);
        assert_eq!(out, [4, 3, 2, 1]);
    }

    #[test]
    fn interlace_is_inverse_of_deinterlace() {
        let pixels: Vec<u8> = (0..24).collect();
        for interlace in [
            Interlace::Plain,
            Interlace::Mirrored,
            Interlace::Planes,
            Interlace::MirroredPlanes,
        ] {
            let width = if matches!(interlace, Interlace::Plain | Interlace::Mirrored) {
                24
            } else {
                8
            };
            let mut raw = vec![0; 24];
            let mut back = vec![0; 24];
            interlace_line(&pixels, &mut raw, width, interlace);
            deinterlace_line(&raw, &mut back, width, interlace);
            assert_eq!(back, pixels, "{interlace:?}");
        }
    }

    #[test]
    fn correction_subtracts_offset_then_scales_gain() {
        let correction = Correction {
            offset: Some(vec![10, 10, 50]),
            gain: Some(vec![120, 240, 0]),
        };
        let mut line = [70, 130, 40];
        correction.apply(&mut line);
        assert_eq!(line, [120, 120, 0]);
    }

    #[test]
    fn correction_clamps_bright_values() {
        let correction = Correction {
            offset: None,
            gain: Some(vec![100]),
        };
        let mut line = [200];
        correction.apply(&mut line);
        assert_eq!(line, [255]);
    }

    #[test]
    fn duplex_stream_splits_into_sides() {
        let g = geometry(HwMode::Grey, true, 16);
        assert_eq!((g.width, g.lines), (8, 2));
        let mut assembler = ImageAssembler::new(&g, Default::default());
        let front_line: Vec<u8> = (0..8).collect();
        let back_line: Vec<u8> = (100..108).collect();
        let stream: Vec<u8> = front_line
            .iter()
            .zip(back_line.iter().rev())
            .flat_map(|(f, b)| [*f, *b])
            .collect();
        // Deliver in awkward chunk sizes to exercise the carry buffer
        for chunk in stream.repeat(2).chunks(5) {
            assembler.push(chunk);
        }
        let images = assembler.finish();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].lines, 2);
        assert_eq!(images[0].line(0), Some(front_line.as_slice()));
        assert_eq!(images[1].side, Side::Back);
        assert_eq!(images[1].line(1), Some(back_line.as_slice()));
    }

    #[test]
    fn lines_beyond_page_are_dropped() {
        let g = geometry(HwMode::Grey, false, 16);
        let mut assembler = ImageAssembler::new(&g, Default::default());
        assembler.push(&[7; 8 * 5]);
        let images = assembler.finish();
        assert_eq!(images[0].lines, 2);
        assert_eq!(images[0].data.len(), 16);
    }

    #[test]
    fn partial_line_is_held_back() {
        let g = geometry(HwMode::Colour, false, 16);
        let mut assembler = ImageAssembler::new(&g, Default::default());
        assembler.push(&[1; 30]);
        let images = assembler.finish();
        assert_eq!(images[0].lines, 1);
    }

    #[test]
    fn render_thresholds_black_and_white() {
        let raw = RawImage {
            side: Side::Front,
            width: 2,
            lines: 1,
            mode: HwMode::Colour,
            dpi: 300,
            data: vec![10, 20, 30, 200, 210, 220],
        };
        let image = render(&raw, ColourMode::BlackWhite, 128);
        assert_eq!(image.format, PixelFormat::BlackWhite);
        assert_eq!(image.data, vec![0, 255]);
        let grey = render(&raw, ColourMode::Grey, 128);
        assert_eq!(grey.data, vec![20, 210]);
    }
}
