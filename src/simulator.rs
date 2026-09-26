//! A pretend P-208II that answers commands the way the real scanner does,
//! producing interlaced receipt images. Used for development mode and tests.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::cdb::{self, WindowDescriptor, put_be};
use crate::channel::{Reply, Request, ScsiChannel};
use crate::error::ScanError;
use crate::image::{Interlace, Side, interlace_line};
use crate::params::HwMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperSupply {
    /// A fixed number of sheets, all loaded up front.
    Sheets(usize),
    /// A new sheet appears this long after the previous one was fed.
    Every(Duration),
}

const PAPER: [f32; 3] = [0.97, 0.95, 0.90];
const INK: [f32; 3] = [0.10, 0.10, 0.12];
const STAMP: [f32; 3] = [0.75, 0.15, 0.15];
const BACKING: [f32; 3] = [0.55, 0.55, 0.56];
const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    Dark,
    Light,
    Receipt { page: usize },
}

struct State {
    supply: PaperSupply,
    last_fed: Option<Instant>,
    in_path: bool,
    window: Option<WindowDescriptor>,
    duplex: bool,
    pending: Vec<u8>,
    cursor: usize,
    page_scan: bool,
    pages_fed: usize,
    calibration_scans: usize,
    cancelled: bool,
    jam_next_page: bool,
}

impl State {
    fn paper_available(&self) -> bool {
        match self.supply {
            PaperSupply::Sheets(count) => count > 0,
            PaperSupply::Every(interval) => self.last_fed.is_none_or(|t| t.elapsed() >= interval),
        }
    }
}

pub struct SimulatedScanner {
    state: RefCell<State>,
    read_delay: Duration,
}

impl SimulatedScanner {
    pub fn new(supply: PaperSupply, read_delay: Duration) -> Self {
        Self {
            state: RefCell::new(State {
                supply,
                last_fed: None,
                in_path: false,
                window: None,
                duplex: false,
                pending: Vec::new(),
                cursor: 0,
                page_scan: false,
                pages_fed: 0,
                calibration_scans: 0,
                cancelled: false,
                jam_next_page: false,
            }),
            read_delay,
        }
    }

    pub fn calibration_scans(&self) -> usize {
        self.state.borrow().calibration_scans
    }

    pub fn was_cancelled(&self) -> bool {
        self.state.borrow().cancelled
    }

    pub fn jam_next_page(&self) {
        self.state.borrow_mut().jam_next_page = true;
    }

    fn handle(&self, request: &Request) -> Result<Reply, ScanError> {
        let mut state = self.state.borrow_mut();
        let data = match request.opcode() {
            0x00 | 0x2a | 0xe1 => Vec::new(),
            0x03 => vec![0; cdb::SENSE_LEN],
            0x12 if request.cdb.get(1) == Some(&1) => vpd(),
            0x12 => inquiry(),
            0x1b => {
                start_scan(&mut state, request.data_out.as_deref().unwrap_or_default())?;
                Vec::new()
            }
            0x24 => {
                let window =
                    WindowDescriptor::parse(request.data_out.as_deref().unwrap_or_default())?;
                if window.window_id == cdb::WINDOW_FRONT {
                    state.window = Some(window);
                }
                Vec::new()
            }
            0x28 => return self.read(&mut state, request),
            0x31 if request.cdb.get(1).is_some_and(|b| b & 7 == 1) => {
                feed(&mut state)?;
                Vec::new()
            }
            0x31 => {
                state.in_path = false;
                Vec::new()
            }
            0xd6 => {
                if let Some(duplex) =
                    cdb::is_duplex_buffer_page(request.data_out.as_deref().unwrap_or_default())
                {
                    state.duplex = duplex;
                }
                Vec::new()
            }
            0xd8 => {
                state.cancelled = true;
                state.pending.clear();
                state.cursor = 0;
                Vec::new()
            }
            _ => return Err(ScanError::InvalidRequest("invalid command")),
        };
        Ok(reply_for(data, request.read_len))
    }

    fn read(&self, state: &mut State, request: &Request) -> Result<Reply, ScanError> {
        let data = match request.cdb.get(2).copied() {
            Some(cdb::datatype::IMAGE) => {
                if state.page_scan && state.jam_next_page {
                    state.jam_next_page = false;
                    state.pending.clear();
                    return Err(ScanError::Jammed("paper jam"));
                }
                std::thread::sleep(self.read_delay);
                let end = (state.cursor + request.read_len).min(state.pending.len());
                let chunk = state.pending[state.cursor..end].to_vec();
                state.cursor = end;
                chunk
            }
            Some(cdb::datatype::SENSORS) => vec![u8::from(state.paper_available())],
            Some(cdb::datatype::COUNTERS) => {
                let mut data = vec![0u8; cdb::COUNTERS_LEN];
                put_be(&mut data[4..8], state.pages_fed as u32);
                data
            }
            _ => vec![0; request.read_len],
        };
        Ok(reply_for(data, request.read_len))
    }
}

impl ScsiChannel for SimulatedScanner {
    fn execute(&self, request: Request) -> Result<Reply, ScanError> {
        self.handle(&request)
    }
}

fn reply_for(data: Vec<u8>, read_len: usize) -> Reply {
    let short = data.len() < read_len;
    Reply { data, short }
}

fn inquiry() -> Vec<u8> {
    let mut data = vec![0u8; cdb::INQUIRY_LEN];
    data[0] = 0x06;
    data[8..16].copy_from_slice(b"CANON   ");
    data[16..32].copy_from_slice(b"P-208II         ");
    data[32..36].copy_from_slice(b"SIM ");
    data
}

fn vpd() -> Vec<u8> {
    let mut data = vec![0u8; cdb::VPD_LEN];
    put_be(&mut data[0x05..0x07], 600);
    put_be(&mut data[0x07..0x09], 600);
    put_be(&mut data[0x0a..0x0c], 600);
    put_be(&mut data[0x0c..0x0e], 600);
    put_be(&mut data[0x0e..0x10], 100);
    put_be(&mut data[0x10..0x12], 100);
    // 150 and 200 dpi, then 300 and 600 dpi
    data[0x12] = 0b0000_1001;
    data[0x13] = 0b0100_0100;
    // 8.5 inches wide, 1000 mm long, in 1/600 inch
    put_be(&mut data[0x14..0x18], 5100);
    put_be(&mut data[0x18..0x1c], 23622);
    data[0x1c] = 0b0000_1110;
    data
}

fn feed(state: &mut State) -> Result<(), ScanError> {
    if !state.paper_available() {
        return Err(ScanError::NoDocuments);
    }
    if let PaperSupply::Sheets(count) = &mut state.supply {
        *count -= 1;
    }
    state.last_fed = Some(Instant::now());
    state.in_path = true;
    state.pages_fed += 1;
    Ok(())
}

fn start_scan(state: &mut State, windows: &[u8]) -> Result<(), ScanError> {
    let window = state
        .window
        .ok_or(ScanError::InvalidRequest("command sequence error"))?;
    let content = match windows.first() {
        Some(&cdb::CAL_SCAN_DARK) => Content::Dark,
        Some(&cdb::CAL_SCAN_LIGHT) => Content::Light,
        _ if state.in_path => Content::Receipt {
            page: state.pages_fed,
        },
        _ => return Err(ScanError::NoDocuments),
    };

    let dpi = usize::from(window.dpi);
    let width = window.width as usize * dpi / 1200;
    let mut lines = window.length as usize * dpi / 1200;
    if let Content::Receipt { page } = content {
        let receipt_mm = 90 + page * 37 % 120;
        lines = lines.min(receipt_mm * dpi * 10 / 254);
        state.in_path = false;
        state.page_scan = true;
    } else {
        state.calibration_scans += 1;
        state.page_scan = false;
    }
    let mode = if window.composition == HwMode::Colour.composition() {
        HwMode::Colour
    } else {
        HwMode::Grey
    };

    state.pending = render_stream(content, mode, state.duplex, width, lines, dpi);
    state.cursor = 0;
    Ok(())
}

fn render_stream(
    content: Content,
    mode: HwMode,
    duplex: bool,
    width: usize,
    lines: usize,
    dpi: usize,
) -> Vec<u8> {
    let bpl = width * mode.bytes_per_pixel();
    let sides: &[Side] = if duplex {
        &[Side::Front, Side::Back]
    } else {
        &[Side::Front]
    };
    let mut stream = Vec::with_capacity(bpl * lines * sides.len());
    let mut pixels = vec![0u8; bpl];
    let mut raw_lines = vec![vec![0u8; bpl]; sides.len()];

    for y in 0..lines {
        for (side, raw) in sides.iter().zip(raw_lines.iter_mut()) {
            for x in 0..width {
                let reflect = reflectance(content, *side, x, y, width, dpi);
                match mode {
                    HwMode::Colour => {
                        for channel in 0..3 {
                            pixels[x * 3 + channel] = sensor_value(reflect[channel], x);
                        }
                    }
                    HwMode::Grey => {
                        let grey = reflect.iter().sum::<f32>() / 3.0;
                        pixels[x] = sensor_value(grey, x);
                    }
                }
            }
            interlace_line(&pixels, raw, width, Interlace::for_side(*side, mode));
        }
        match raw_lines.as_slice() {
            [front, back] => {
                for (f, b) in front.iter().zip(back) {
                    stream.push(*f);
                    stream.push(*b);
                }
            }
            [single] => stream.extend_from_slice(single),
            _ => {}
        }
    }
    stream
}

/// Uneven sensor response and black level that calibration should remove.
fn sensor_value(reflect: f32, x: usize) -> u8 {
    let response = 0.8 + 0.2 * (x % 37) as f32 / 36.0;
    let dark = 6.0 + (x % 5) as f32;
    (dark + reflect * response * 220.0)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn pseudo_random(a: usize, b: usize) -> usize {
    a.wrapping_mul(2_654_435_761) ^ b.wrapping_mul(40_503).rotate_left(7)
}

fn reflectance(
    content: Content,
    side: Side,
    x: usize,
    y: usize,
    width: usize,
    dpi: usize,
) -> [f32; 3] {
    let page = match content {
        Content::Dark => return BLACK,
        Content::Light => return WHITE,
        Content::Receipt { page } => page,
    };
    // Receipts are narrower than the feeder and never quite centred
    let receipt_mm = if page % 3 == 0 { 58 } else { 80 };
    let receipt_width = (receipt_mm * dpi * 10 / 254).min(width);
    let drift = (page * 7 % 17) * dpi * 10 / 254;
    let left = ((width - receipt_width) / 2 + drift).min(width - receipt_width);
    if x < left || x >= left + receipt_width {
        return BACKING;
    }
    let (x, width) = (x - left, receipt_width);

    let margin = width / 10;
    if side == Side::Back || x < margin || x >= width - margin {
        return PAPER;
    }
    let row_height = (dpi / 8).max(4);
    let (row, within) = (y / row_height, y % row_height);
    let usable = width - 2 * margin;
    if row < 3 {
        let in_stamp = x.abs_diff(width / 2) < usable / 4 && within < row_height * 3 / 4;
        return if in_stamp { STAMP } else { PAPER };
    }
    if within >= row_height / 2 {
        return PAPER;
    }
    let text_len = pseudo_random(page, row) % usable.max(1);
    let glyph = (row_height / 2).max(2);
    let in_glyph = (x - margin) < text_len && (x / glyph) % 4 != 3;
    if in_glyph { INK } else { PAPER }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(cdb: Vec<u8>) -> Request {
        Request::new(cdb)
    }

    #[test]
    fn sensors_follow_sheet_count() {
        let sim = SimulatedScanner::new(PaperSupply::Sheets(1), Duration::ZERO);
        let sensors = || {
            sim.execute(request(cdb::read(cdb::datatype::SENSORS, 1)).reading(1))
                .expect("sensor read")
                .data
        };
        assert_eq!(sensors(), vec![1]);
        sim.execute(request(cdb::object_position(true)))
            .expect("feed");
        assert_eq!(sensors(), vec![0]);
        assert_eq!(
            sim.execute(request(cdb::object_position(true))),
            Err(ScanError::NoDocuments)
        );
    }

    #[test]
    fn timed_supply_refills() {
        let sim = SimulatedScanner::new(PaperSupply::Every(Duration::ZERO), Duration::ZERO);
        for _ in 0..3 {
            sim.execute(request(cdb::object_position(true)))
                .expect("feed");
        }
    }

    #[test]
    fn scan_without_window_is_rejected() {
        let sim = SimulatedScanner::new(PaperSupply::Sheets(1), Duration::ZERO);
        let result = sim.execute(request(cdb::scan(1)).with_data(vec![0]));
        assert!(matches!(result, Err(ScanError::InvalidRequest(_))));
    }

    #[test]
    fn duplex_stream_alternates_sides() {
        let stream = render_stream(Content::Receipt { page: 1 }, HwMode::Grey, true, 16, 2, 150);
        assert_eq!(stream.len(), 16 * 2 * 2);
    }
}
