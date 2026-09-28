use std::time::Duration;

use log::{debug, info, trace, warn};

use crate::calibration::{
    START_EXPOSURE, afe_exposure, afe_gain, afe_offset, channel_max, column_average,
};
use crate::cdb::{self, AfeSettings, Counters, Inquiry, Vpd, datatype};
use crate::channel::{Reply, Request, ScsiChannel};
use crate::error::ScanError;
use crate::image::{Correction, Image, ImageAssembler, RawImage, render};
use crate::params::{
    CALIBRATION_LINES, Capabilities, ColourMode, Geometry, HwMode, ScanSettings, Tone, mm_to_units,
};

pub const DEFAULT_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const MAX_TRANSFER: usize = 0xff_ffff;
const READY_TIMEOUT: Duration = Duration::from_millis(500);
const READY_ATTEMPTS: usize = 5;
const MAX_BUSY_RETRIES: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub vendor: String,
    pub model: String,
    pub firmware: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AfeKey {
    dpi: u16,
    page_width: u32,
    colour: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FineKey {
    dpi: u16,
    page_width: u32,
    mode: HwMode,
}

/// Retries TEST UNIT READY, since the scanner can be slow to respond after
/// being opened or after moving paper.
fn wait_ready(channel: &impl ScsiChannel) -> Result<(), ScanError> {
    let mut last = Ok(());
    for attempt in 1..=READY_ATTEMPTS {
        let mut request = Request::new(cdb::test_unit_ready()).with_timeout(READY_TIMEOUT);
        // Reading sense on one attempt wakes models that are asleep
        if attempt != 4 {
            request = request.without_sense();
        }
        match channel.execute(request) {
            Ok(_) => return Ok(()),
            Err(err) => {
                debug!("scanner not ready on attempt {attempt}: {err}");
                last = Err(err);
            }
        }
    }
    last
}

fn per_side<T>(images: &[RawImage], f: impl Fn(&RawImage) -> T) -> Result<[T; 2], ScanError> {
    match images {
        [front, back] if front.lines > 0 && back.lines > 0 => Ok([f(front), f(back)]),
        _ => Err(ScanError::Protocol(
            "calibration scan did not return data for both sides".into(),
        )),
    }
}

fn first_line(image: &RawImage) -> &[u8] {
    image.line(0).unwrap_or_default()
}

pub struct Scanner<C: ScsiChannel> {
    channel: C,
    info: DeviceInfo,
    buffer_size: usize,
    afe: Option<AfeKey>,
    fine: Option<(FineKey, [Correction; 2])>,
}

impl<C: ScsiChannel> Scanner<C> {
    pub fn open(channel: C) -> Result<Self, ScanError> {
        Self::with_buffer_size(channel, DEFAULT_BUFFER_SIZE)
    }

    pub fn with_buffer_size(channel: C, buffer_size: usize) -> Result<Self, ScanError> {
        wait_ready(&channel)?;

        let reply = channel.execute(Request::new(cdb::inquiry()).reading(cdb::INQUIRY_LEN))?;
        let inquiry = Inquiry::parse(&reply.data)?;
        if inquiry.vendor != "CANON" {
            return Err(ScanError::Unsupported(format!(
                "expected a Canon scanner but found {}",
                inquiry.vendor
            )));
        }
        if !inquiry.product.contains("P208") && !inquiry.product.contains("P-208") {
            warn!("{} has not been tested with this driver", inquiry.product);
        }

        let reply = channel.execute(Request::new(cdb::inquiry_vpd()).reading(cdb::VPD_LEN))?;
        let capabilities = Capabilities::from_vpd(&Vpd::parse(&reply.data)?);
        info!(
            "Connected to {} {} (firmware {}), resolutions {:?}",
            inquiry.vendor, inquiry.product, inquiry.version, capabilities.resolutions
        );

        let scanner = Self {
            channel,
            info: DeviceInfo {
                vendor: inquiry.vendor,
                model: inquiry.product,
                firmware: inquiry.version,
                capabilities,
            },
            buffer_size: buffer_size.clamp(1, MAX_TRANSFER),
            afe: None,
            fine: None,
        };
        if let Err(err) = scanner.reset_panel() {
            debug!("panel is unavailable: {err}");
        }
        Ok(scanner)
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn paper_loaded(&self) -> Result<bool, ScanError> {
        let reply = self.exec(
            Request::new(cdb::read(datatype::SENSORS, cdb::SENSORS_LEN)).reading(cdb::SENSORS_LEN),
        )?;
        Ok(cdb::paper_loaded(&reply.data))
    }

    pub fn counters(&self) -> Result<Counters, ScanError> {
        let reply = self.exec(
            Request::new(cdb::read(datatype::COUNTERS, cdb::COUNTERS_LEN))
                .reading(cdb::COUNTERS_LEN),
        )?;
        Counters::parse(&reply.data)
    }

    /// Calibrates if needed and configures the scanner for a run of pages.
    pub fn start_batch(&mut self, settings: &ScanSettings) -> Result<Batch<'_, C>, ScanError> {
        let caps = self.info.capabilities.clone();
        caps.check_resolution(settings.dpi)?;
        let mode = caps.hw_mode(settings.mode);
        let page_width = settings.page_width_mm.map_or(caps.max_width, mm_to_units);
        let page_length = settings.page_length_mm.map_or(caps.max_length, mm_to_units);
        let geometry = Geometry::new(
            &caps,
            settings.dpi,
            mode,
            settings.duplex,
            page_width,
            page_length,
        )?;
        let tone = Tone::from(settings);

        if let Err(err) = self.eject() {
            debug!("could not clear the paper path: {err}");
        }
        wait_ready(&self.channel)?;

        let afe_key = AfeKey {
            dpi: settings.dpi,
            page_width,
            colour: settings.mode == ColourMode::Colour,
        };
        self.calibrate_afe(afe_key, tone)?;
        let fine_key = FineKey {
            dpi: settings.dpi,
            page_width,
            mode,
        };
        let corrections = self.calibrate_fine(fine_key, tone)?;

        if let Err(err) = self.reset_panel() {
            debug!("could not reset the page counter: {err}");
        }
        self.set_windows(&geometry, tone)?;
        self.set_buffer_mode(settings.duplex)?;
        if mode != HwMode::Colour {
            self.set_scan_mode(cdb::dropout_page())?;
        }
        self.set_scan_mode(cdb::double_feed_page())?;

        Ok(Batch {
            scanner: self,
            geometry,
            corrections,
            mode: settings.mode,
            threshold: settings.threshold,
            pages: 0,
            active: true,
        })
    }

    fn exec(&self, request: Request) -> Result<Reply, ScanError> {
        self.channel.execute(request)
    }

    fn reset_panel(&self) -> Result<(), ScanError> {
        let payload = cdb::panel_payload(true, 0);
        self.exec(Request::new(cdb::send(datatype::PANEL, payload.len())).with_data(payload))
            .map(drop)
    }

    /// Takes hold of a sheet, then moves it out without scanning.
    pub fn feed_paper(&self) -> Result<(), ScanError> {
        self.feed()?;
        self.eject()
    }

    fn feed(&self) -> Result<(), ScanError> {
        self.exec(Request::new(cdb::object_position(true)))
            .map(drop)
    }

    /// Runs the paper discharge operation without scanning or checking the feeder sensor.
    pub fn eject(&self) -> Result<(), ScanError> {
        self.exec(Request::new(cdb::object_position(false)))
            .map(drop)
    }

    fn cancel(&self) -> Result<(), ScanError> {
        self.exec(Request::new(cdb::cancel())).map(drop)
    }

    fn set_scan_mode(&self, page: Vec<u8>) -> Result<(), ScanError> {
        self.exec(Request::new(cdb::set_scan_mode()).with_data(page))
            .map(drop)
    }

    fn set_buffer_mode(&self, duplex: bool) -> Result<(), ScanError> {
        self.set_scan_mode(cdb::buffer_page(duplex))
    }

    fn set_windows(&self, geometry: &Geometry, tone: Tone) -> Result<(), ScanError> {
        let mut windows = vec![cdb::WINDOW_FRONT];
        if geometry.duplex {
            windows.push(cdb::WINDOW_BACK);
        }
        for id in windows {
            let payload = geometry.window(id, tone).to_payload();
            self.exec(Request::new(cdb::set_window()).with_data(payload))?;
        }
        Ok(())
    }

    fn start_scan(&self, windows: Vec<u8>) -> Result<(), ScanError> {
        self.exec(Request::new(cdb::scan(windows.len())).with_data(windows))
            .map(drop)
    }

    fn write_afe(&self, afe: &AfeSettings) -> Result<(), ScanError> {
        trace!("AFE {afe:?}");
        self.exec(Request::new(cdb::coarse_calibration()).with_data(afe.to_payload()))
            .map(drop)
    }

    /// Reads image data until the scanner signals the end of the page. An
    /// `exact` read asks for no more than the page holds, as calibration needs.
    fn read_image(
        &self,
        geometry: &Geometry,
        corrections: [Correction; 2],
        exact: bool,
    ) -> Result<Vec<RawImage>, ScanError> {
        let mut assembler = ImageAssembler::new(geometry, corrections);
        let unit = geometry.bytes_per_line * geometry.sides();
        let chunk = (self.buffer_size / unit).max(1) * unit;
        let total = geometry.total_bytes();
        let mut received = 0;
        let mut busy = 0;
        let mut ended_early = false;

        while received < total {
            let len = if exact {
                chunk.min(total - received)
            } else {
                chunk
            };
            let reply = match self.exec(Request::new(cdb::read(datatype::IMAGE, len)).reading(len))
            {
                Err(ScanError::Busy) if busy < MAX_BUSY_RETRIES => {
                    busy += 1;
                    continue;
                }
                reply => reply?,
            };
            busy = 0;
            let take = reply.data.len().min(total - received);
            assembler.push(&reply.data[..take]);
            received += take;
            if reply.short {
                ended_early = true;
                break;
            }
        }
        trace!("read {received} of {total} image bytes");

        if exact && !ended_early {
            self.eject()?;
        }
        Ok(assembler.finish())
    }

    fn calibration_scan(
        &self,
        geometry: &Geometry,
        kind: u8,
        corrections: [Correction; 2],
    ) -> Result<Vec<RawImage>, ScanError> {
        self.start_scan(vec![kind, kind])?;
        self.read_image(geometry, corrections, true)
    }

    /// Sets the analogue black level, exposure and gain for both sides.
    fn calibrate_afe(&mut self, key: AfeKey, tone: Tone) -> Result<(), ScanError> {
        if self.afe == Some(key) {
            return Ok(());
        }
        info!("Calibrating at {} dpi", key.dpi);
        self.afe = None;
        let geometry = Geometry::calibration(
            &self.info.capabilities,
            key.dpi,
            HwMode::Colour,
            key.page_width,
        )?;
        self.set_buffer_mode(true)?;
        self.set_windows(&geometry, tone)?;

        let mut afe = AfeSettings {
            gain: [1; 2],
            offset: [1; 2],
            exposure: [[0; 3]; 2],
        };
        self.write_afe(&afe)?;
        let dark = self.calibration_scan(&geometry, cdb::CAL_SCAN_DARK, Default::default())?;
        afe.offset = per_side(&dark, |image| afe_offset(first_line(image)))?;

        afe.exposure = [[START_EXPOSURE; 3]; 2];
        self.write_afe(&afe)?;
        let light = self.calibration_scan(&geometry, cdb::CAL_SCAN_LIGHT, Default::default())?;
        afe.exposure = per_side(&light, |image| {
            let line = first_line(image);
            [0, 1, 2]
                .map(|channel| afe_exposure(START_EXPOSURE, channel_max(line, channel), key.colour))
        })?;

        self.write_afe(&afe)?;
        let light = self.calibration_scan(&geometry, cdb::CAL_SCAN_LIGHT, Default::default())?;
        afe.gain = per_side(&light, |image| {
            let brightest = first_line(image).iter().copied().max().unwrap_or(0);
            afe_gain(brightest, key.colour)
        })?;

        self.write_afe(&afe)?;
        debug!("analogue calibration {afe:?}");
        self.afe = Some(key);
        Ok(())
    }

    /// Measures per pixel black and white levels, applied in software.
    fn calibrate_fine(&mut self, key: FineKey, tone: Tone) -> Result<[Correction; 2], ScanError> {
        if let Some((cached, corrections)) = &self.fine
            && *cached == key
        {
            return Ok(corrections.clone());
        }
        self.fine = None;
        let geometry =
            Geometry::calibration(&self.info.capabilities, key.dpi, key.mode, key.page_width)?;
        let bpl = geometry.bytes_per_line;
        self.set_buffer_mode(true)?;
        self.set_windows(&geometry, tone)?;

        let dark = self.calibration_scan(&geometry, cdb::CAL_SCAN_DARK, Default::default())?;
        let offsets = per_side(&dark, |image| {
            column_average(&image.data, bpl, CALIBRATION_LINES)
        })?;

        let offset_only = offsets.clone().map(|offset| Correction {
            offset: Some(offset),
            gain: None,
        });
        let light = self.calibration_scan(&geometry, cdb::CAL_SCAN_LIGHT, offset_only)?;
        let gains = per_side(&light, |image| {
            column_average(&image.data, bpl, CALIBRATION_LINES)
                .into_iter()
                .map(|v| v.max(1))
                .collect::<Vec<_>>()
        })?;

        let [front_offset, back_offset] = offsets;
        let [front_gain, back_gain] = gains;
        let corrections = [
            Correction {
                offset: Some(front_offset),
                gain: Some(front_gain),
            },
            Correction {
                offset: Some(back_offset),
                gain: Some(back_gain),
            },
        ];
        self.fine = Some((key, corrections.clone()));
        Ok(corrections)
    }
}

/// A run of pages scanned with the same settings. Dropping it mid-run cancels
/// the scan and ejects any paper.
pub struct Batch<'a, C: ScsiChannel> {
    scanner: &'a Scanner<C>,
    geometry: Geometry,
    corrections: [Correction; 2],
    mode: ColourMode,
    threshold: u8,
    pages: usize,
    active: bool,
}

impl<C: ScsiChannel> Batch<'_, C> {
    pub fn pages(&self) -> usize {
        self.pages
    }

    /// Feeds and scans the next sheet. Returns `None` once the feeder is empty.
    pub fn next_page(&mut self) -> Result<Option<Vec<Image>>, ScanError> {
        if !self.active {
            return Ok(None);
        }
        let result = self.scan_page();
        if !matches!(result, Ok(Some(_))) {
            self.active = false;
        }
        result
    }

    fn scan_page(&mut self) -> Result<Option<Vec<Image>>, ScanError> {
        match self.scanner.feed() {
            Err(ScanError::NoDocuments) => return Ok(None),
            result => result?,
        }
        if self.pages == 0 {
            wait_ready(&self.scanner.channel)?;
        }
        let mut windows = vec![cdb::WINDOW_FRONT];
        if self.geometry.duplex {
            windows.push(cdb::WINDOW_BACK);
        }
        self.scanner.start_scan(windows)?;
        let raw = self
            .scanner
            .read_image(&self.geometry, self.corrections.clone(), false)?;
        self.pages += 1;
        info!(
            "Scanned a sheet, {} lines at {} dpi",
            raw.first().map_or(0, |image| image.lines),
            self.geometry.dpi
        );
        Ok(Some(
            raw.iter()
                .filter(|image| image.lines > 0)
                .map(|image| render(image, self.mode, self.threshold))
                .collect(),
        ))
    }

    /// Stops the scanner mid-run. Does nothing once the feeder has run dry.
    pub fn cancel(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        if let Err(err) = self.scanner.cancel() {
            debug!("cancel was rejected: {err}");
        }
        if let Err(err) = self.scanner.eject() {
            debug!("eject was rejected: {err}");
        }
    }
}

impl<C: ScsiChannel> Drop for Batch<'_, C> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::MockScsiChannel;
    use crate::image::{PixelFormat, Side};
    use crate::simulator::{PaperSupply, SimulatedScanner};

    fn simulated(sheets: usize) -> Scanner<SimulatedScanner> {
        let sim = SimulatedScanner::new(PaperSupply::Sheets(sheets), Duration::ZERO);
        Scanner::with_buffer_size(sim, 64 * 1024).expect("simulator opens")
    }

    fn receipt_settings() -> ScanSettings {
        ScanSettings {
            dpi: 150,
            page_width_mm: Some(80),
            ..ScanSettings::default()
        }
    }

    #[test]
    fn open_reads_identity_and_capabilities() {
        let scanner = simulated(0);
        let info = scanner.info();
        assert_eq!(info.vendor, "CANON");
        assert_eq!(info.model, "P-208II");
        assert_eq!(info.capabilities.resolutions, vec![150, 200, 300, 600]);
    }

    #[test]
    fn scans_every_sheet_then_stops() {
        let mut scanner = simulated(2);
        let mut batch = scanner
            .start_batch(&receipt_settings())
            .expect("batch starts");
        let first = batch
            .next_page()
            .expect("first page")
            .expect("paper present");
        let second = batch
            .next_page()
            .expect("second page")
            .expect("paper present");
        assert_eq!(batch.next_page(), Ok(None));
        assert_eq!(batch.pages(), 2);

        for page in [&first, &second] {
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].format, PixelFormat::Rgb8);
            assert_eq!(page[0].width, 472);
            assert!(page[0].height > 0);
            assert_eq!(page[0].data.len(), page[0].width * page[0].height * 3);
        }
    }

    #[test]
    fn duplex_returns_both_sides() {
        let mut scanner = simulated(1);
        let settings = ScanSettings {
            duplex: true,
            mode: ColourMode::Grey,
            ..receipt_settings()
        };
        let mut batch = scanner.start_batch(&settings).expect("batch starts");
        let page = batch
            .next_page()
            .expect("page scans")
            .expect("paper present");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].side, Side::Front);
        assert_eq!(page[1].side, Side::Back);
        assert_eq!(page[1].format, PixelFormat::Grey8);
        assert_eq!(page[0].height, page[1].height);
    }

    #[test]
    fn calibration_flattens_paper_to_near_white() {
        let mut scanner = simulated(1);
        let settings = ScanSettings {
            mode: ColourMode::Grey,
            ..receipt_settings()
        };
        let mut batch = scanner.start_batch(&settings).expect("batch starts");
        let page = batch
            .next_page()
            .expect("page scans")
            .expect("paper present");
        let image = &page[0];
        // The simulator's paper is uneven before correction
        let corner = image.data[image.width * (image.height - 1) + 2];
        assert!((220..=255).contains(&corner), "paper level {corner}");
    }

    #[test]
    fn black_and_white_is_thresholded() {
        let mut scanner = simulated(1);
        let settings = ScanSettings {
            mode: ColourMode::BlackWhite,
            ..receipt_settings()
        };
        let mut batch = scanner.start_batch(&settings).expect("batch starts");
        let page = batch
            .next_page()
            .expect("page scans")
            .expect("paper present");
        assert_eq!(page[0].format, PixelFormat::BlackWhite);
        assert!(page[0].data.iter().all(|v| *v == 0 || *v == 255));
        assert!(page[0].data.contains(&0));
    }

    #[test]
    fn calibration_is_reused_for_same_settings() {
        let mut scanner = simulated(2);
        drop(
            scanner
                .start_batch(&receipt_settings())
                .expect("batch starts"),
        );
        let scans_before = scanner.channel.calibration_scans();
        drop(
            scanner
                .start_batch(&receipt_settings())
                .expect("batch starts"),
        );
        assert_eq!(scanner.channel.calibration_scans(), scans_before);

        let other = ScanSettings {
            dpi: 300,
            ..receipt_settings()
        };
        drop(scanner.start_batch(&other).expect("batch starts"));
        assert!(scanner.channel.calibration_scans() > scans_before);
    }

    #[test]
    fn dropping_an_active_batch_cancels() {
        let mut scanner = simulated(3);
        {
            let mut batch = scanner
                .start_batch(&receipt_settings())
                .expect("batch starts");
            batch.next_page().expect("page scans");
        }
        assert!(scanner.channel.was_cancelled());
    }

    #[test]
    fn empty_feeder_does_not_cancel() {
        let mut scanner = simulated(0);
        {
            let mut batch = scanner
                .start_batch(&receipt_settings())
                .expect("batch starts");
            assert_eq!(batch.next_page(), Ok(None));
        }
        assert!(!scanner.channel.was_cancelled());
    }

    #[test]
    fn manual_feed_reports_firmware_paper_errors_without_claiming_success() {
        for loaded in [true, false] {
            let simulator = SimulatedScanner::new(PaperSupply::Sheets(0), Duration::ZERO);
            let mut channel = MockScsiChannel::new();
            channel
                .expect_execute()
                .returning(move |request| simulator.execute(request));
            let mut scanner = Scanner::open(channel).expect("scanner opens");
            scanner.channel.checkpoint();
            let mut sequence = mockall::Sequence::new();
            scanner
                .channel
                .expect_execute()
                .withf(|request| request.cdb == cdb::object_position(true))
                .times(1)
                .in_sequence(&mut sequence)
                .return_once(move |_| {
                    if loaded {
                        Ok(Reply::default())
                    } else {
                        Err(ScanError::NoDocuments)
                    }
                });
            if loaded {
                scanner
                    .channel
                    .expect_execute()
                    .withf(|request| request.cdb == cdb::object_position(false))
                    .times(1)
                    .in_sequence(&mut sequence)
                    .returning(|_| Ok(Reply::default()));
            }
            assert_eq!(
                scanner.feed_paper(),
                if loaded {
                    Ok(())
                } else {
                    Err(ScanError::NoDocuments)
                }
            );
        }
    }

    #[test]
    fn manual_eject_only_discharges_paper_and_preserves_errors() {
        for outcome in [
            Ok(Reply::default()),
            Err(ScanError::Jammed("paper jam")),
            Err(ScanError::NoDocuments),
            Err(ScanError::Transport(
                crate::error::TransportError::Disconnected,
            )),
        ] {
            let simulator = SimulatedScanner::new(PaperSupply::Sheets(0), Duration::ZERO);
            let mut channel = MockScsiChannel::new();
            channel
                .expect_execute()
                .returning(move |request| simulator.execute(request));
            let mut scanner = Scanner::open(channel).expect("scanner opens");
            scanner.channel.checkpoint();

            let expected = outcome.clone().map(drop);
            scanner
                .channel
                .expect_execute()
                .withf(|request| {
                    request.cdb == cdb::object_position(false)
                        && request.read_len == 0
                        && request.data_out.is_none()
                })
                .times(1)
                .return_once(move |_| outcome);

            assert_eq!(scanner.eject(), expected);
        }
    }

    #[test]
    fn unsupported_resolution_is_rejected_before_scanning() {
        let mut scanner = simulated(1);
        let settings = ScanSettings {
            dpi: 250,
            ..receipt_settings()
        };
        assert!(matches!(
            scanner.start_batch(&settings).err(),
            Some(ScanError::Unsupported(_))
        ));
    }

    #[test]
    fn open_rejects_other_vendors() {
        let mut mock = MockScsiChannel::new();
        mock.expect_execute().returning(|request| {
            let mut data = vec![0u8; request.read_len];
            if request.opcode() == 0x12 {
                data[0] = 0x06;
                data[8..16].copy_from_slice(b"ACME    ");
            }
            Ok(Reply { data, short: false })
        });
        assert!(matches!(
            Scanner::open(mock).err(),
            Some(ScanError::Unsupported(_))
        ));
    }

    #[test]
    fn open_fails_when_scanner_never_ready() {
        let mut mock = MockScsiChannel::new();
        mock.expect_execute()
            .times(READY_ATTEMPTS)
            .returning(|_| Err(ScanError::Io("no reply".into())));
        assert_eq!(
            Scanner::open(mock).err(),
            Some(ScanError::Io("no reply".into()))
        );
    }

    #[test]
    fn jam_mid_page_is_reported_without_cancelling() {
        let mut scanner = simulated(1);
        scanner.channel.jam_next_page();
        let mut batch = scanner
            .start_batch(&receipt_settings())
            .expect("batch starts");
        assert_eq!(batch.next_page(), Err(ScanError::Jammed("paper jam")));
        drop(batch);
        assert!(!scanner.channel.was_cancelled());
    }
}
