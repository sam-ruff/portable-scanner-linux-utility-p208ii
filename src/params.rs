use std::fmt;

use crate::cdb::{Vpd, WindowDescriptor};
use crate::error::ScanError;

/// Positions sent to the scanner are in 1/1200 inch.
pub const UNITS_PER_INCH: u32 = 1200;
pub const CALIBRATION_LINES: usize = 8;

pub fn mm_to_units(mm: u32) -> u32 {
    mm * UNITS_PER_INCH * 10 / 254
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ColourMode {
    #[default]
    Colour,
    Grey,
    BlackWhite,
}

impl ColourMode {
    pub const ALL: [ColourMode; 3] = [ColourMode::Colour, ColourMode::Grey, ColourMode::BlackWhite];
}

impl fmt::Display for ColourMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ColourMode::Colour => "Colour",
            ColourMode::Grey => "Greyscale",
            ColourMode::BlackWhite => "Black and white",
        })
    }
}

/// Image format produced by the scanner itself. Black and white output is
/// thresholded from grey in software so calibration always works per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HwMode {
    Grey,
    Colour,
}

impl HwMode {
    pub fn composition(self) -> u8 {
        match self {
            HwMode::Grey => 2,
            HwMode::Colour => 5,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            HwMode::Grey => 1,
            HwMode::Colour => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSettings {
    pub mode: ColourMode,
    pub dpi: u16,
    pub duplex: bool,
    /// Defaults to the full width of the feeder.
    pub page_width_mm: Option<u32>,
    /// Defaults to the longest document the scanner accepts. Scans stop at the
    /// end of the paper either way.
    pub page_length_mm: Option<u32>,
    pub brightness: i8,
    pub contrast: i8,
    /// Grey level below which a pixel becomes black in black and white mode.
    pub threshold: u8,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            mode: ColourMode::Colour,
            dpi: 300,
            duplex: false,
            page_width_mm: None,
            page_length_mm: None,
            brightness: 0,
            contrast: 0,
            threshold: 128,
        }
    }
}

/// Brightness, contrast and threshold as the scanner encodes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone {
    pub brightness: u8,
    pub contrast: u8,
    pub threshold: u8,
}

impl From<&ScanSettings> for Tone {
    fn from(settings: &ScanSettings) -> Self {
        let shift = |v: i8| (i16::from(v) + 128) as u8;
        Self {
            brightness: shift(settings.brightness),
            contrast: shift(settings.contrast),
            threshold: settings.threshold,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub max_width: u32,
    pub max_length: u32,
    pub resolutions: Vec<u16>,
    stepped: Option<(u16, u16)>,
    pub can_grey: bool,
}

impl Capabilities {
    pub fn from_vpd(vpd: &Vpd) -> Self {
        let in_range = |dpi: &u16| *dpi >= vpd.min_y_res && *dpi <= vpd.max_y_res;
        Self {
            max_width: vpd.max_width,
            max_length: vpd.max_length,
            resolutions: vpd
                .standard_resolutions
                .iter()
                .copied()
                .filter(in_range)
                .collect(),
            stepped: vpd.step_y_res.then_some((vpd.min_y_res, vpd.max_y_res)),
            can_grey: vpd.can_grey,
        }
    }

    pub fn supports(&self, dpi: u16) -> bool {
        self.resolutions.contains(&dpi)
            || self
                .stepped
                .is_some_and(|(min, max)| (min..=max).contains(&dpi))
    }

    pub fn hw_mode(&self, mode: ColourMode) -> HwMode {
        if mode == ColourMode::Colour || !self.can_grey {
            return HwMode::Colour;
        }
        HwMode::Grey
    }

    pub fn check_resolution(&self, dpi: u16) -> Result<(), ScanError> {
        if self.supports(dpi) {
            return Ok(());
        }
        let list: Vec<String> = self.resolutions.iter().map(u16::to_string).collect();
        Err(ScanError::Unsupported(format!(
            "{dpi} dpi is not supported, choose one of {}",
            list.join(", ")
        )))
    }
}

/// Everything about the shape of one scan, in the scanner's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub dpi: u16,
    pub mode: HwMode,
    pub duplex: bool,
    pub width: usize,
    pub bytes_per_line: usize,
    pub lines: usize,
    window_x: u32,
    window_width: u32,
    window_length: u32,
}

impl Geometry {
    pub fn new(
        caps: &Capabilities,
        dpi: u16,
        mode: HwMode,
        duplex: bool,
        page_width: u32,
        page_length: u32,
    ) -> Result<Self, ScanError> {
        let page_width = page_width.min(caps.max_width);
        let page_length = page_length.min(caps.max_length);
        let dpi32 = u32::from(dpi);

        let mut width = (page_width * dpi32 / UNITS_PER_INCH) as usize;
        // The P-208 family needs whole groups of 8 pixels per line
        width -= width % 8;
        if width == 0 {
            return Err(ScanError::Unsupported("page is too narrow to scan".into()));
        }
        let mut lines = (page_length * dpi32 / UNITS_PER_INCH) as usize;
        lines += lines % 2;
        if lines == 0 {
            return Err(ScanError::Unsupported("page is too short to scan".into()));
        }

        Ok(Self {
            dpi,
            mode,
            duplex,
            width,
            bytes_per_line: width * mode.bytes_per_pixel(),
            lines,
            window_x: (caps.max_width - page_width) / 2,
            window_width: width as u32 * UNITS_PER_INCH / dpi32,
            window_length: lines as u32 * UNITS_PER_INCH / dpi32,
        })
    }

    /// Short duplex scan used for calibration, matching the page width.
    pub fn calibration(
        caps: &Capabilities,
        dpi: u16,
        mode: HwMode,
        page_width: u32,
    ) -> Result<Self, ScanError> {
        let length = CALIBRATION_LINES as u32 * UNITS_PER_INCH / u32::from(dpi);
        Self::new(caps, dpi, mode, true, page_width, length)
    }

    pub fn sides(&self) -> usize {
        if self.duplex { 2 } else { 1 }
    }

    pub fn total_bytes(&self) -> usize {
        self.bytes_per_line * self.lines * self.sides()
    }

    pub fn window(&self, window_id: u8, tone: Tone) -> WindowDescriptor {
        WindowDescriptor {
            window_id,
            dpi: self.dpi,
            x: self.window_x,
            // The P-208 family wants the top edge inverted
            y: !0,
            width: self.window_width,
            length: self.window_length,
            brightness: tone.brightness,
            threshold: tone.threshold,
            contrast: tone.contrast,
            composition: self.mode.composition(),
            bits_per_pixel: 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            max_width: 10200,
            max_length: 43200,
            resolutions: vec![150, 200, 300, 600],
            stepped: None,
            can_grey: true,
        }
    }

    #[test]
    fn millimetres_convert_to_scanner_units() {
        assert_eq!(mm_to_units(254), 12000);
        assert_eq!(mm_to_units(80), 3779);
    }

    #[test]
    fn full_width_colour_geometry() {
        let g = Geometry::new(&caps(), 300, HwMode::Colour, false, 10200, 13200)
            .expect("valid geometry");
        assert_eq!(g.width, 2544);
        assert_eq!(g.bytes_per_line, 2544 * 3);
        assert_eq!(g.lines, 3300);
        assert_eq!(g.total_bytes(), 2544 * 3 * 3300);
    }

    #[test]
    fn narrow_page_is_centred() {
        let g =
            Geometry::new(&caps(), 200, HwMode::Grey, true, 3780, 12000).expect("valid geometry");
        assert_eq!(g.width, 624);
        let window = g.window(
            1,
            Tone {
                brightness: 128,
                contrast: 128,
                threshold: 128,
            },
        );
        assert_eq!(window.x, (10200 - 3780) / 2);
        assert_eq!(window.width, 624 * 1200 / 200);
        assert_eq!(window.y, u32::MAX);
        assert_eq!(window.composition, 2);
        assert_eq!(g.total_bytes(), 624 * 2000 * 2);
    }

    #[test]
    fn page_is_clamped_to_scanner_limits() {
        let g = Geometry::new(&caps(), 150, HwMode::Grey, false, 99_999, 99_999)
            .expect("valid geometry");
        assert_eq!(g.width, 1272);
        assert_eq!(g.lines, 5400);
    }

    #[test]
    fn odd_line_count_rounds_up() {
        let g = Geometry::new(&caps(), 150, HwMode::Grey, false, 10200, 8).expect("valid geometry");
        assert_eq!(g.lines, 2);
    }

    #[test]
    fn calibration_is_eight_duplex_lines() {
        for dpi in [150, 200, 300, 600] {
            let g =
                Geometry::calibration(&caps(), dpi, HwMode::Colour, 10200).expect("valid geometry");
            assert_eq!(g.lines, CALIBRATION_LINES, "at {dpi} dpi");
            assert!(g.duplex);
        }
    }

    #[test]
    fn too_narrow_page_is_rejected() {
        let result = Geometry::new(&caps(), 150, HwMode::Grey, false, 10, 1000);
        assert!(matches!(result, Err(ScanError::Unsupported(_))));
    }

    #[test]
    fn unsupported_resolution_lists_alternatives() {
        let err = caps()
            .check_resolution(250)
            .expect_err("250 dpi is unsupported");
        assert!(err.to_string().contains("150, 200, 300, 600"));
    }

    #[test]
    fn grey_falls_back_to_colour_without_grey_support() {
        let mut caps = caps();
        assert_eq!(caps.hw_mode(ColourMode::BlackWhite), HwMode::Grey);
        caps.can_grey = false;
        assert_eq!(caps.hw_mode(ColourMode::Grey), HwMode::Colour);
    }

    #[test]
    fn tone_is_centred_on_128() {
        let settings = ScanSettings {
            brightness: -20,
            contrast: 10,
            ..ScanSettings::default()
        };
        let tone = Tone::from(&settings);
        assert_eq!(tone.brightness, 108);
        assert_eq!(tone.contrast, 138);
    }
}
