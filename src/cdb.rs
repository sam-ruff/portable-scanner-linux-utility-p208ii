//! SCSI command blocks and payloads understood by the Canon P-208II, plus the
//! USB framing Canon wraps them in. Layouts follow the SANE `canon_dr` backend.

use crate::error::ScanError;

pub const USB_HEADER_LEN: usize = 12;
const USB_COMMAND_LEN: usize = 12;
pub const USB_STATUS_LEN: usize = 4;

pub const SENSE_LEN: usize = 0x0e;
pub const INQUIRY_LEN: usize = 0x30;
pub const VPD_LEN: usize = 0x1e;
pub const SENSORS_LEN: usize = 1;
pub const COUNTERS_LEN: usize = 0x80;
const PANEL_LEN: usize = 8;
const WINDOW_HEADER_LEN: usize = 8;
const WINDOW_DESC_LEN: usize = 0x2c;
const SCAN_MODE_PAYLOAD_LEN: usize = 0x14;
const AFE_PAYLOAD_LEN: usize = 0x28;

mod opcode {
    pub const TEST_UNIT_READY: u8 = 0x00;
    pub const REQUEST_SENSE: u8 = 0x03;
    pub const INQUIRY: u8 = 0x12;
    pub const SCAN: u8 = 0x1b;
    pub const SET_WINDOW: u8 = 0x24;
    pub const READ: u8 = 0x28;
    pub const SEND: u8 = 0x2a;
    pub const OBJECT_POSITION: u8 = 0x31;
    pub const SET_SCAN_MODE: u8 = 0xd6;
    pub const CANCEL: u8 = 0xd8;
    pub const COARSE_CAL: u8 = 0xe1;
}

pub mod datatype {
    pub const IMAGE: u8 = 0x00;
    pub const PANEL: u8 = 0x84;
    pub const SENSORS: u8 = 0x8b;
    pub const COUNTERS: u8 = 0x8c;
}

mod page {
    pub const DOUBLE_FEED: u8 = 0x30;
    pub const BUFFER: u8 = 0x32;
    pub const DROPOUT: u8 = 0x36;
}

/// Window identifiers, also used as the SCAN payload.
pub const WINDOW_FRONT: u8 = 0x00;
pub const WINDOW_BACK: u8 = 0x01;
/// Calibration scans with the lamp off and on.
pub const CAL_SCAN_DARK: u8 = 0xff;
pub const CAL_SCAN_LIGHT: u8 = 0xfe;

pub fn put_be(buf: &mut [u8], value: u32) {
    let n = buf.len();
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = (value >> (8 * (n - 1 - i))) as u8;
    }
}

pub fn get_be(buf: &[u8]) -> u32 {
    buf.iter().fold(0, |acc, &b| (acc << 8) | u32::from(b))
}

/// Wraps a command descriptor block in Canon's 12 byte USB header.
pub fn frame_command(cdb: &[u8]) -> Vec<u8> {
    let total = USB_HEADER_LEN + USB_COMMAND_LEN;
    let mut packet = vec![0u8; total];
    let len = cdb.len().min(USB_COMMAND_LEN);
    put_be(&mut packet[1..4], (total - 4) as u32);
    packet[5] = 1;
    packet[6] = 0x90;
    packet[USB_HEADER_LEN..USB_HEADER_LEN + len].copy_from_slice(&cdb[..len]);
    packet
}

/// Wraps an outgoing data phase in Canon's 12 byte USB header.
pub fn frame_data_out(payload: &[u8]) -> Vec<u8> {
    let total = USB_HEADER_LEN + payload.len();
    let mut packet = vec![0u8; total];
    put_be(&mut packet[1..4], (total - 4) as u32);
    packet[5] = 2;
    packet[6] = 0xb0;
    packet[USB_HEADER_LEN..].copy_from_slice(payload);
    packet
}

pub fn test_unit_ready() -> Vec<u8> {
    vec![opcode::TEST_UNIT_READY, 0, 0, 0, 0, 0]
}

pub fn request_sense() -> Vec<u8> {
    vec![opcode::REQUEST_SENSE, 0, 0, 0, SENSE_LEN as u8, 0]
}

pub fn inquiry() -> Vec<u8> {
    vec![opcode::INQUIRY, 0, 0, 0, INQUIRY_LEN as u8, 0]
}

pub fn inquiry_vpd() -> Vec<u8> {
    vec![opcode::INQUIRY, 1, 0xf0, 0, VPD_LEN as u8, 0]
}

pub fn read(datatype: u8, len: usize) -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = opcode::READ;
    cdb[2] = datatype;
    put_be(&mut cdb[6..9], len as u32);
    cdb
}

pub fn send(datatype: u8, len: usize) -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = opcode::SEND;
    cdb[2] = datatype;
    put_be(&mut cdb[6..9], len as u32);
    cdb
}

pub fn object_position(feed: bool) -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = opcode::OBJECT_POSITION;
    cdb[1] = u8::from(feed);
    cdb
}

pub fn scan(payload_len: usize) -> Vec<u8> {
    vec![opcode::SCAN, 0, 0, 0, payload_len as u8, 0]
}

pub fn cancel() -> Vec<u8> {
    vec![opcode::CANCEL, 0, 0, 0, 0, 0]
}

pub fn set_window() -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = opcode::SET_WINDOW;
    put_be(&mut cdb[6..9], (WINDOW_HEADER_LEN + WINDOW_DESC_LEN) as u32);
    cdb
}

pub fn set_scan_mode() -> Vec<u8> {
    vec![
        opcode::SET_SCAN_MODE,
        0x10,
        0,
        0,
        SCAN_MODE_PAYLOAD_LEN as u8,
        0,
    ]
}

pub fn coarse_calibration() -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = opcode::COARSE_CAL;
    cdb[5] = 3;
    put_be(&mut cdb[6..9], AFE_PAYLOAD_LEN as u32);
    cdb
}

/// Scan area and image format for one side. Positions are in 1/1200 inch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowDescriptor {
    pub window_id: u8,
    pub dpi: u16,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub length: u32,
    pub brightness: u8,
    pub threshold: u8,
    pub contrast: u8,
    pub composition: u8,
    pub bits_per_pixel: u8,
}

impl WindowDescriptor {
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = vec![0u8; WINDOW_HEADER_LEN + WINDOW_DESC_LEN];
        put_be(&mut out[6..8], WINDOW_DESC_LEN as u32);
        let desc = &mut out[WINDOW_HEADER_LEN..];
        desc[0] = self.window_id;
        put_be(&mut desc[0x02..0x04], u32::from(self.dpi));
        put_be(&mut desc[0x04..0x06], u32::from(self.dpi));
        put_be(&mut desc[0x06..0x0a], self.x);
        put_be(&mut desc[0x0a..0x0e], self.y);
        put_be(&mut desc[0x0e..0x12], self.width);
        put_be(&mut desc[0x12..0x16], self.length);
        desc[0x16] = self.brightness;
        desc[0x17] = self.threshold;
        desc[0x18] = self.contrast;
        desc[0x19] = self.composition;
        desc[0x1a] = self.bits_per_pixel;
        // RGB format 1, no padding
        desc[0x1d] = 0x10;
        // Undocumented byte the P-208 family requires
        desc[0x2a] = 0x88;
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ScanError> {
        if payload.len() < WINDOW_HEADER_LEN + WINDOW_DESC_LEN {
            return Err(ScanError::Protocol("window payload too short".into()));
        }
        let desc = &payload[WINDOW_HEADER_LEN..];
        Ok(Self {
            window_id: desc[0],
            dpi: get_be(&desc[0x02..0x04]) as u16,
            x: get_be(&desc[0x06..0x0a]),
            y: get_be(&desc[0x0a..0x0e]),
            width: get_be(&desc[0x0e..0x12]),
            length: get_be(&desc[0x12..0x16]),
            brightness: desc[0x16],
            threshold: desc[0x17],
            contrast: desc[0x18],
            composition: desc[0x19],
            bits_per_pixel: desc[0x1a],
        })
    }
}

fn scan_mode_page(page_code: u8) -> Vec<u8> {
    let mut out = vec![0u8; SCAN_MODE_PAYLOAD_LEN];
    out[1] = 0x13;
    out[4] = page_code;
    out[5] = 0x0e;
    out
}

pub fn buffer_page(duplex: bool) -> Vec<u8> {
    let mut out = scan_mode_page(page::BUFFER);
    if duplex {
        out[6] |= 0x02;
    }
    out
}

pub fn is_duplex_buffer_page(payload: &[u8]) -> Option<bool> {
    (payload.len() > 6 && payload[4] == page::BUFFER).then(|| payload[6] & 0x02 != 0)
}

/// Dropout page with no colour dropped, sent before non-colour scans.
pub fn dropout_page() -> Vec<u8> {
    let mut out = scan_mode_page(page::DROPOUT);
    out[7] = 0x03;
    out
}

/// Double feed detection page with every detector turned off.
pub fn double_feed_page() -> Vec<u8> {
    scan_mode_page(page::DOUBLE_FEED)
}

pub fn panel_payload(enable_led: bool, counter: u32) -> Vec<u8> {
    let mut out = vec![0u8; PANEL_LEN];
    out[2] = u8::from(enable_led);
    put_be(&mut out[4..8], counter);
    out
}

/// Analogue front end settings, indexed by side then colour channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AfeSettings {
    pub gain: [u8; 2],
    pub offset: [u8; 2],
    pub exposure: [[u16; 3]; 2],
}

impl AfeSettings {
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = vec![0u8; AFE_PAYLOAD_LEN];
        for (side, base) in [0usize, 0x14].into_iter().enumerate() {
            out[base..base + 3].fill(self.gain[side]);
            out[base + 4..base + 7].fill(self.offset[side]);
            for (channel, exposure) in self.exposure[side].iter().enumerate() {
                let at = base + 8 + channel * 2;
                put_be(&mut out[at..at + 2], u32::from(*exposure));
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inquiry {
    pub vendor: String,
    pub product: String,
    pub version: String,
}

impl Inquiry {
    pub fn parse(data: &[u8]) -> Result<Self, ScanError> {
        if data.len() < 0x24 {
            return Err(ScanError::Protocol("inquiry reply too short".into()));
        }
        if data[0] & 0x1f != 0x06 {
            return Err(ScanError::Unsupported("device is not a scanner".into()));
        }
        let text = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&data[range]).trim_end().to_string()
        };
        Ok(Self {
            vendor: text(0x08..0x10),
            product: text(0x10..0x20),
            version: text(0x20..0x24),
        })
    }
}

const STANDARD_RESOLUTIONS: [(usize, u8, u16); 16] = [
    (0x12, 7, 60),
    (0x12, 6, 75),
    (0x12, 5, 100),
    (0x12, 4, 120),
    (0x12, 3, 150),
    (0x12, 2, 160),
    (0x12, 1, 180),
    (0x12, 0, 200),
    (0x13, 7, 240),
    (0x13, 6, 300),
    (0x13, 5, 320),
    (0x13, 4, 400),
    (0x13, 3, 480),
    (0x13, 2, 600),
    (0x13, 1, 800),
    (0x13, 0, 1200),
];

/// Vital product data: what the scanner says it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vpd {
    pub basic_x_res: u16,
    pub basic_y_res: u16,
    pub step_y_res: bool,
    pub max_y_res: u16,
    pub min_y_res: u16,
    pub standard_resolutions: Vec<u16>,
    /// Maximum scan width in 1/1200 inch.
    pub max_width: u32,
    /// Maximum scan length in 1/1200 inch.
    pub max_length: u32,
    pub can_grey: bool,
}

impl Vpd {
    pub fn parse(data: &[u8]) -> Result<Self, ScanError> {
        if data.len() < 0x1d {
            return Err(ScanError::Protocol("VPD reply too short".into()));
        }
        let basic_x_res = get_be(&data[0x05..0x07]) as u16;
        let basic_y_res = get_be(&data[0x07..0x09]) as u16;
        if basic_x_res == 0 || basic_y_res == 0 {
            return Err(ScanError::Protocol(
                "VPD reports zero base resolution".into(),
            ));
        }
        let standard_resolutions = STANDARD_RESOLUTIONS
            .iter()
            .filter(|(byte, bit, _)| data[*byte] >> bit & 1 == 1)
            .map(|(_, _, dpi)| *dpi)
            .collect();
        Ok(Self {
            basic_x_res,
            basic_y_res,
            step_y_res: data[0x09] >> 4 & 1 == 1,
            max_y_res: get_be(&data[0x0c..0x0e]) as u16,
            min_y_res: get_be(&data[0x10..0x12]) as u16,
            standard_resolutions,
            max_width: get_be(&data[0x14..0x18]) * 1200 / u32::from(basic_x_res),
            max_length: get_be(&data[0x18..0x1c]) * 1200 / u32::from(basic_y_res),
            can_grey: data[0x1c] >> 3 & 1 == 1,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub total: u32,
    pub since_roller_service: u32,
}

impl Counters {
    pub fn parse(data: &[u8]) -> Result<Self, ScanError> {
        if data.len() < 0x48 {
            return Err(ScanError::Protocol("counter reply too short".into()));
        }
        let total = get_be(&data[0x04..0x08]);
        let last_service = get_be(&data[0x44..0x48]);
        Ok(Self {
            total,
            since_roller_service: total.saturating_sub(last_service),
        })
    }
}

pub fn paper_loaded(sensors: &[u8]) -> bool {
    sensors.first().is_some_and(|b| b & 1 == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenseStatus {
    Good,
    /// The transfer ended early, `residual` bytes short of the request.
    ShortRead {
        residual: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
    pub ili: bool,
    pub info: u32,
}

impl Sense {
    pub fn parse(data: &[u8]) -> Result<Self, ScanError> {
        if data.len() < SENSE_LEN {
            return Err(ScanError::Protocol("sense reply too short".into()));
        }
        Ok(Self {
            key: data[2] & 0x0f,
            asc: data[0x0c],
            ascq: data[0x0d],
            ili: data[2] >> 5 & 1 == 1,
            info: get_be(&data[3..7]),
        })
    }

    pub fn outcome(&self) -> Result<SenseStatus, ScanError> {
        let code = (self.asc, self.ascq);
        match self.key {
            0 if self.ili => Ok(SenseStatus::ShortRead {
                residual: self.info,
            }),
            0 | 1 => Ok(SenseStatus::Good),
            2 => Err(ScanError::Busy),
            3 => Err(match code {
                (0x3a, 0x00) => ScanError::NoDocuments,
                (0x80, 0x00) => ScanError::Jammed("paper jam"),
                (0x80, 0x01) => ScanError::CoverOpen,
                (0x81, 0x01) => ScanError::Jammed("double feed"),
                (0x81, 0x02) => ScanError::Jammed("skew detected"),
                (0x81, 0x04) => ScanError::Jammed("staple detected"),
                _ => ScanError::Hardware("medium error"),
            }),
            4 => Err(ScanError::Hardware(match code {
                (0x60, 0x00) => "lamp error",
                (0x80, 0x01) => "CPU check error",
                (0x80, 0x02) => "RAM check error",
                (0x80, 0x03) => "ROM check error",
                (0x80, 0x04) => "hardware check error",
                _ => "unknown hardware error",
            })),
            5 => Err(match code {
                (0x3a, 0x00) => ScanError::NoDocuments,
                (0x55, 0x00) => ScanError::OutOfMemory,
                (0x25, 0x00) => ScanError::Unsupported("unsupported logical unit".into()),
                (0x1a, 0x00) => ScanError::InvalidRequest("parameter list error"),
                (0x20, 0x00) => ScanError::InvalidRequest("invalid command"),
                (0x24, 0x00) => ScanError::InvalidRequest("invalid CDB field"),
                (0x26, 0x00) => ScanError::InvalidRequest("invalid field in parameter list"),
                (0x2c, 0x00) => ScanError::InvalidRequest("command sequence error"),
                (0x2c, 0x01) => ScanError::InvalidRequest("too many windows"),
                _ => ScanError::InvalidRequest("illegal request"),
            }),
            6 if matches!(code, (0x29, 0x00) | (0x2a, 0x00)) => Ok(SenseStatus::Good),
            0x0b if code == (0x00, 0x00) => Err(ScanError::Cancelled),
            key => Err(ScanError::Protocol(format!(
                "sense key {key:#x}, asc {:#x}, ascq {:#x}",
                self.asc, self.ascq
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sense_bytes(key: u8, asc: u8, ascq: u8, ili: bool, info: u32) -> Vec<u8> {
        let mut data = vec![0u8; SENSE_LEN];
        data[0] = 0x70;
        data[2] = key | if ili { 0x20 } else { 0 };
        put_be(&mut data[3..7], info);
        data[0x0c] = asc;
        data[0x0d] = ascq;
        data
    }

    #[test]
    fn command_frame_has_canon_header() {
        let packet = frame_command(&test_unit_ready());
        assert_eq!(packet.len(), 24);
        assert_eq!(&packet[..7], &[0, 0, 0, 20, 0, 1, 0x90]);
        assert_eq!(&packet[12..], &[0u8; 12]);
    }

    #[test]
    fn data_frame_has_canon_header() {
        let packet = frame_data_out(&[0xaa, 0xbb]);
        assert_eq!(
            packet,
            vec![0, 0, 0, 10, 0, 2, 0xb0, 0, 0, 0, 0, 0, 0xaa, 0xbb]
        );
    }

    #[test]
    fn read_encodes_three_byte_length() {
        assert_eq!(
            read(datatype::IMAGE, 0x123456),
            vec![0x28, 0, 0, 0, 0, 0, 0x12, 0x34, 0x56, 0]
        );
    }

    #[test]
    fn window_descriptor_round_trips() {
        let window = WindowDescriptor {
            window_id: WINDOW_BACK,
            dpi: 300,
            x: 120,
            y: u32::MAX,
            width: 9600,
            length: 14000,
            brightness: 128,
            threshold: 90,
            contrast: 128,
            composition: 5,
            bits_per_pixel: 8,
        };
        let payload = window.to_payload();
        assert_eq!(payload.len(), 52);
        assert_eq!(payload[7], 0x2c);
        assert_eq!(payload[8 + 0x1d], 0x10);
        assert_eq!(payload[8 + 0x2a], 0x88);
        assert_eq!(WindowDescriptor::parse(&payload), Ok(window));
    }

    #[test]
    fn buffer_page_sets_duplex_bit() {
        let simplex = buffer_page(false);
        let duplex = buffer_page(true);
        assert_eq!(&simplex[..7], &[0, 0x13, 0, 0, 0x32, 0x0e, 0]);
        assert_eq!(duplex[6], 0x02);
        assert_eq!(is_duplex_buffer_page(&duplex), Some(true));
        assert_eq!(is_duplex_buffer_page(&simplex), Some(false));
        assert_eq!(is_duplex_buffer_page(&dropout_page()), None);
    }

    #[test]
    fn afe_payload_places_both_sides() {
        let afe = AfeSettings {
            gain: [10, 20],
            offset: [30, 40],
            exposure: [[0x0102, 0x0304, 0x0506], [0x0708, 0x090a, 0x0b0c]],
        };
        let out = afe.to_payload();
        assert_eq!(&out[0..4], &[10, 10, 10, 0]);
        assert_eq!(&out[4..8], &[30, 30, 30, 0]);
        assert_eq!(&out[8..14], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&out[0x14..0x17], &[20, 20, 20]);
        assert_eq!(&out[0x18..0x1b], &[40, 40, 40]);
        assert_eq!(&out[0x1c..0x22], &[7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn inquiry_parses_identity() {
        let mut data = vec![0u8; INQUIRY_LEN];
        data[0] = 0x06;
        data[8..16].copy_from_slice(b"CANON   ");
        data[16..32].copy_from_slice(b"P-208II         ");
        data[32..36].copy_from_slice(b"1.02");
        let inquiry = Inquiry::parse(&data).expect("valid inquiry");
        assert_eq!(inquiry.vendor, "CANON");
        assert_eq!(inquiry.product, "P-208II");
        assert_eq!(inquiry.version, "1.02");
    }

    #[test]
    fn inquiry_rejects_non_scanner() {
        let data = vec![0u8; INQUIRY_LEN];
        assert!(matches!(
            Inquiry::parse(&data),
            Err(ScanError::Unsupported(_))
        ));
    }

    #[test]
    fn vpd_parses_capabilities() {
        let mut data = vec![0u8; VPD_LEN];
        put_be(&mut data[0x05..0x07], 600);
        put_be(&mut data[0x07..0x09], 600);
        put_be(&mut data[0x0c..0x0e], 600);
        put_be(&mut data[0x10..0x12], 100);
        data[0x12] = 0b0000_1001;
        data[0x13] = 0b0100_0100;
        put_be(&mut data[0x14..0x18], 5100);
        put_be(&mut data[0x18..0x1c], 21600);
        data[0x1c] = 0b0000_1000;
        let vpd = Vpd::parse(&data).expect("valid vpd");
        assert_eq!(vpd.standard_resolutions, vec![150, 200, 300, 600]);
        assert_eq!(vpd.max_width, 10200);
        assert_eq!(vpd.max_length, 43200);
        assert!(vpd.can_grey);
        assert!(!vpd.step_y_res);
    }

    #[test]
    fn sense_short_read_reports_residual() {
        let sense = Sense::parse(&sense_bytes(0, 0, 0, true, 4096)).expect("valid sense");
        assert_eq!(
            sense.outcome(),
            Ok(SenseStatus::ShortRead { residual: 4096 })
        );
    }

    #[test]
    fn sense_maps_paper_errors() {
        let cases = [
            (3, 0x3a, 0, ScanError::NoDocuments),
            (5, 0x3a, 0, ScanError::NoDocuments),
            (3, 0x80, 0, ScanError::Jammed("paper jam")),
            (3, 0x80, 1, ScanError::CoverOpen),
            (3, 0x81, 1, ScanError::Jammed("double feed")),
            (2, 0x04, 1, ScanError::Busy),
            (0x0b, 0, 0, ScanError::Cancelled),
        ];
        for (key, asc, ascq, expected) in cases {
            let sense = Sense::parse(&sense_bytes(key, asc, ascq, false, 0)).expect("valid sense");
            assert_eq!(sense.outcome(), Err(expected));
        }
    }

    #[test]
    fn sense_treats_unit_attention_reset_as_good() {
        let sense = Sense::parse(&sense_bytes(6, 0x29, 0, false, 0)).expect("valid sense");
        assert_eq!(sense.outcome(), Ok(SenseStatus::Good));
    }

    #[test]
    fn counters_report_roller_usage() {
        let mut data = vec![0u8; COUNTERS_LEN];
        put_be(&mut data[4..8], 1500);
        put_be(&mut data[0x44..0x48], 1000);
        assert_eq!(
            Counters::parse(&data),
            Ok(Counters {
                total: 1500,
                since_roller_service: 500
            })
        );
    }
}
