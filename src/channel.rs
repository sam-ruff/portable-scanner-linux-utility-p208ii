use std::time::Duration;

use log::{debug, trace, warn};

use crate::cdb::{self, SENSE_LEN, Sense, SenseStatus, USB_STATUS_LEN};
use crate::error::ScanError;
use crate::usb::BulkTransport;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub cdb: Vec<u8>,
    pub data_out: Option<Vec<u8>>,
    pub read_len: usize,
    pub timeout: Duration,
    /// Ask the scanner why a command failed rather than returning a bare I/O error.
    pub request_sense: bool,
}

impl Request {
    pub fn new(cdb: Vec<u8>) -> Self {
        Self {
            cdb,
            data_out: None,
            read_len: 0,
            timeout: DEFAULT_TIMEOUT,
            request_sense: true,
        }
    }

    pub fn with_data(mut self, payload: Vec<u8>) -> Self {
        self.data_out = Some(payload);
        self
    }

    pub fn reading(mut self, len: usize) -> Self {
        self.read_len = len;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn without_sense(mut self) -> Self {
        self.request_sense = false;
        self
    }

    pub fn opcode(&self) -> u8 {
        self.cdb.first().copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub data: Vec<u8>,
    /// Fewer bytes arrived than were requested, which marks the end of an image.
    pub short: bool,
}

/// Executes SCSI style commands against the scanner.
#[cfg_attr(test, mockall::automock)]
pub trait ScsiChannel: Send {
    fn execute(&self, request: Request) -> Result<Reply, ScanError>;
}

impl<C: ScsiChannel + ?Sized> ScsiChannel for Box<C> {
    fn execute(&self, request: Request) -> Result<Reply, ScanError> {
        (**self).execute(request)
    }
}

/// Speaks Canon's command, data and status phases over a bulk USB pipe.
pub struct UsbScsiChannel<T: BulkTransport> {
    transport: T,
    recovery_delay: Duration,
}

impl<T: BulkTransport> UsbScsiChannel<T> {
    pub fn new(transport: T, recovery_delay: Duration) -> Self {
        Self {
            transport,
            recovery_delay,
        }
    }

    fn send_packet(&self, packet: &[u8], timeout: Duration) -> Result<(), ScanError> {
        let written = self.transport.write(packet, timeout)?;
        if written != packet.len() {
            return Err(ScanError::Io(format!(
                "short USB write: {written} of {} bytes",
                packet.len()
            )));
        }
        Ok(())
    }

    fn run(&self, request: &Request) -> Result<Reply, ScanError> {
        trace!("cmd >> {:02x?}", request.cdb);
        self.send_packet(&cdb::frame_command(&request.cdb), request.timeout)?;

        if let Some(payload) = &request.data_out {
            trace!("out >> {} bytes", payload.len());
            self.send_packet(&cdb::frame_data_out(payload), request.timeout)?;
        }

        let mut data = None;
        if request.read_len > 0 {
            let received = self
                .transport
                .read(request.read_len, request.timeout)
                .unwrap_or_else(|err| {
                    debug!("in: read failed ({err}), recovering");
                    Vec::new()
                });
            if received.is_empty() {
                let status = self.recover(true, request.request_sense)?;
                return Ok(Reply {
                    data: Vec::new(),
                    short: status != SenseStatus::Good,
                });
            }
            trace!("in << {} bytes", received.len());
            data = Some(received);
        }

        let status = self.read_status(request)?;
        let Some(mut data) = data else {
            return Ok(Reply {
                data: Vec::new(),
                short: status != SenseStatus::Good,
            });
        };

        if let SenseStatus::ShortRead { residual } = status {
            let expected = request.read_len.saturating_sub(residual as usize);
            if data.len() > expected {
                data.truncate(expected);
            }
        }
        let short = data.len() != request.read_len;
        Ok(Reply { data, short })
    }

    fn read_status(&self, request: &Request) -> Result<SenseStatus, ScanError> {
        match self.transport.read(USB_STATUS_LEN, request.timeout) {
            Err(err) => {
                debug!("status: read failed ({err}), recovering");
                self.recover(true, request.request_sense)
            }
            Ok(status) if status.len() != USB_STATUS_LEN => {
                debug!("status: short read of {} bytes, recovering", status.len());
                self.recover(true, request.request_sense)
            }
            Ok(status) if status[USB_STATUS_LEN - 1] != 0 => {
                trace!("status: check condition {:#x}", status[USB_STATUS_LEN - 1]);
                self.recover(false, request.request_sense)
            }
            Ok(_) => Ok(SenseStatus::Good),
        }
    }

    fn recover(&self, clear_halt: bool, request_sense: bool) -> Result<SenseStatus, ScanError> {
        std::thread::sleep(self.recovery_delay);
        if clear_halt {
            self.transport.clear_halt()?;
        }
        if !request_sense {
            return Err(ScanError::Io("scanner reported an error".into()));
        }

        let sense_request = Request::new(cdb::request_sense())
            .reading(SENSE_LEN)
            .without_sense();
        let reply = self.run(&sense_request)?;
        if reply.short {
            warn!("request sense returned a short reply");
            return Err(ScanError::Io("could not read scanner error details".into()));
        }
        let sense = Sense::parse(&reply.data)?;
        debug!(
            "sense: key {:#x} asc {:#x} ascq {:#x} ili {} info {}",
            sense.key, sense.asc, sense.ascq, sense.ili, sense.info
        );
        sense.outcome()
    }
}

impl<T: BulkTransport> ScsiChannel for UsbScsiChannel<T> {
    fn execute(&self, request: Request) -> Result<Reply, ScanError> {
        self.run(&request)
    }
}

#[cfg(test)]
mod tests {
    use mockall::Sequence;
    use mockall::predicate::*;

    use super::*;
    use crate::cdb::put_be;
    use crate::error::TransportError;
    use crate::usb::MockBulkTransport;

    const GOOD_STATUS: [u8; 4] = [0, 0, 0, 0];
    const CHECK_STATUS: [u8; 4] = [0, 0, 0, 2];

    fn sense_reply(key: u8, asc: u8, ascq: u8, ili: bool, info: u32) -> Vec<u8> {
        let mut data = vec![0u8; SENSE_LEN];
        data[2] = key | if ili { 0x20 } else { 0 };
        put_be(&mut data[3..7], info);
        data[0x0c] = asc;
        data[0x0d] = ascq;
        data
    }

    fn channel(transport: MockBulkTransport) -> UsbScsiChannel<MockBulkTransport> {
        UsbScsiChannel::new(transport, Duration::ZERO)
    }

    fn expect_write(mock: &mut MockBulkTransport, seq: &mut Sequence, expected: Vec<u8>) {
        mock.expect_write()
            .withf(move |data, _| data == expected.as_slice())
            .times(1)
            .in_sequence(seq)
            .returning(|data, _| Ok(data.len()));
    }

    fn expect_read(mock: &mut MockBulkTransport, seq: &mut Sequence, len: usize, reply: Vec<u8>) {
        mock.expect_read()
            .with(eq(len), always())
            .times(1)
            .in_sequence(seq)
            .return_once(move |_, _| Ok(reply));
    }

    #[test]
    fn command_without_data_reads_status() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::test_unit_ready()),
        );
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let reply = channel(mock)
            .execute(Request::new(cdb::test_unit_ready()))
            .expect("command succeeds");
        assert_eq!(reply, Reply::default());
    }

    #[test]
    fn data_out_is_framed_after_command() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        let payload = cdb::buffer_page(true);
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::set_scan_mode()),
        );
        expect_write(&mut mock, &mut seq, cdb::frame_data_out(&payload));
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        channel(mock)
            .execute(Request::new(cdb::set_scan_mode()).with_data(payload))
            .expect("command succeeds");
    }

    #[test]
    fn full_read_is_not_short() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(&mut mock, &mut seq, cdb::frame_command(&cdb::read(0, 8)));
        expect_read(&mut mock, &mut seq, 8, vec![7; 8]);
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let reply = channel(mock)
            .execute(Request::new(cdb::read(0, 8)).reading(8))
            .expect("read succeeds");
        assert_eq!(reply.data, vec![7; 8]);
        assert!(!reply.short);
    }

    #[test]
    fn end_of_image_uses_sense_residual() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(&mut mock, &mut seq, cdb::frame_command(&cdb::read(0, 100)));
        expect_read(&mut mock, &mut seq, 100, vec![1; 64]);
        expect_read(&mut mock, &mut seq, 4, CHECK_STATUS.to_vec());
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::request_sense()),
        );
        expect_read(
            &mut mock,
            &mut seq,
            SENSE_LEN,
            sense_reply(0, 0, 0, true, 60),
        );
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let reply = channel(mock)
            .execute(Request::new(cdb::read(0, 100)).reading(100))
            .expect("read succeeds");
        assert_eq!(reply.data.len(), 40);
        assert!(reply.short);
    }

    #[test]
    fn check_condition_maps_sense_to_error() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::object_position(true)),
        );
        expect_read(&mut mock, &mut seq, 4, CHECK_STATUS.to_vec());
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::request_sense()),
        );
        expect_read(
            &mut mock,
            &mut seq,
            SENSE_LEN,
            sense_reply(3, 0x3a, 0, false, 0),
        );
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let result = channel(mock).execute(Request::new(cdb::object_position(true)));
        assert_eq!(result, Err(ScanError::NoDocuments));
    }

    #[test]
    fn failed_status_read_clears_halt_before_sense() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::test_unit_ready()),
        );
        mock.expect_read()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Err(TransportError::Stall));
        mock.expect_clear_halt()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Ok(()));
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::request_sense()),
        );
        expect_read(
            &mut mock,
            &mut seq,
            SENSE_LEN,
            sense_reply(6, 0x29, 0, false, 0),
        );
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let reply = channel(mock)
            .execute(Request::new(cdb::test_unit_ready()))
            .expect("unit attention after reset is not an error");
        assert!(!reply.short);
    }

    #[test]
    fn empty_read_recovers_without_status_phase() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(&mut mock, &mut seq, cdb::frame_command(&cdb::read(0, 16)));
        expect_read(&mut mock, &mut seq, 16, Vec::new());
        mock.expect_clear_halt()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| Ok(()));
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::request_sense()),
        );
        expect_read(
            &mut mock,
            &mut seq,
            SENSE_LEN,
            sense_reply(2, 4, 1, false, 0),
        );
        expect_read(&mut mock, &mut seq, 4, GOOD_STATUS.to_vec());

        let result = channel(mock).execute(Request::new(cdb::read(0, 16)).reading(16));
        assert_eq!(result, Err(ScanError::Busy));
    }

    #[test]
    fn error_without_sense_is_io_error() {
        let mut mock = MockBulkTransport::new();
        let mut seq = Sequence::new();
        expect_write(
            &mut mock,
            &mut seq,
            cdb::frame_command(&cdb::test_unit_ready()),
        );
        expect_read(&mut mock, &mut seq, 4, CHECK_STATUS.to_vec());

        let result = channel(mock).execute(Request::new(cdb::test_unit_ready()).without_sense());
        assert!(matches!(result, Err(ScanError::Io(_))));
    }

    #[test]
    fn write_failure_is_reported() {
        let mut mock = MockBulkTransport::new();
        mock.expect_write()
            .returning(|_, _| Err(TransportError::Disconnected));

        let result = channel(mock).execute(Request::new(cdb::test_unit_ready()));
        assert_eq!(
            result,
            Err(ScanError::Transport(TransportError::Disconnected))
        );
    }

    #[test]
    fn partial_write_is_an_error() {
        let mut mock = MockBulkTransport::new();
        mock.expect_write().returning(|_, _| Ok(3));

        let result = channel(mock).execute(Request::new(cdb::test_unit_ready()));
        assert!(matches!(result, Err(ScanError::Io(_))));
    }
}
