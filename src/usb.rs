use std::sync::Mutex;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out, TransferError};

use crate::error::{ScanError, TransportError};

pub const CANON_VENDOR_ID: u16 = 0x1083;

/// USB product IDs of scanners this driver knows how to talk to.
pub const SUPPORTED_MODELS: [(u16, &str); 2] = [(0x165f, "P-208II"), (0x165d, "DR-P208II")];

/// Raw bulk pipe to the scanner.
#[cfg_attr(test, mockall::automock)]
pub trait BulkTransport: Send {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize, TransportError>;
    /// Reads a single transfer of at most `len` bytes.
    fn read(&self, len: usize, timeout: Duration) -> Result<Vec<u8>, TransportError>;
    fn clear_halt(&self) -> Result<(), TransportError>;
}

pub struct NusbTransport {
    bulk_out: Mutex<nusb::Endpoint<Bulk, Out>>,
    bulk_in: Mutex<nusb::Endpoint<Bulk, In>>,
}

#[derive(Debug, Clone)]
pub struct FoundDevice {
    pub model: &'static str,
    info: nusb::DeviceInfo,
}

impl FoundDevice {
    pub fn location(&self) -> String {
        format!(
            "bus {} device {}",
            self.info.busnum(),
            self.info.device_address()
        )
    }
}

/// Finds the first supported scanner, explaining common reasons it may be missing.
pub fn find_scanner() -> Result<FoundDevice, ScanError> {
    let devices: Vec<_> = nusb::list_devices()
        .wait()
        .map_err(|e| ScanError::NotFound(format!("could not list USB devices: {e}")))?
        .filter(|d| d.vendor_id() == CANON_VENDOR_ID)
        .collect();

    let found = devices.iter().find_map(|info| {
        SUPPORTED_MODELS
            .iter()
            .find(|(pid, _)| *pid == info.product_id())
            .map(|(_, model)| FoundDevice {
                model,
                info: info.clone(),
            })
    });
    if let Some(found) = found {
        return Ok(found);
    }
    if !devices.is_empty() {
        return Err(ScanError::NotFound(
            "found a Canon device that is not in scanner mode; set the AUTO START switch on the \
             back of the scanner to OFF and reconnect it"
                .into(),
        ));
    }
    Err(ScanError::NotFound(
        "no Canon P-208II found; check the USB cable and that the scanner is switched on".into(),
    ))
}

fn usb_error(err: nusb::Error) -> TransportError {
    TransportError::Other(err.to_string())
}

fn permission_hint(err: nusb::Error) -> ScanError {
    if err.kind() == nusb::ErrorKind::PermissionDenied {
        return ScanError::NotFound(
            "permission denied opening the scanner; install the udev rule from the packaging \
             directory and reconnect the scanner"
                .into(),
        );
    }
    ScanError::Transport(usb_error(err))
}

impl NusbTransport {
    pub fn open(found: &FoundDevice) -> Result<Self, ScanError> {
        let device = found.info.open().wait().map_err(permission_hint)?;
        let config = device
            .active_configuration()
            .map_err(|e| ScanError::Protocol(format!("no active USB configuration: {e}")))?;

        let (interface_number, in_addr, out_addr) = config
            .interface_alt_settings()
            .find_map(|alt| {
                let bulk = |dir| {
                    alt.endpoints()
                        .find(|ep| {
                            ep.transfer_type() == TransferType::Bulk && ep.direction() == dir
                        })
                        .map(|ep| ep.address())
                };
                Some((
                    alt.interface_number(),
                    bulk(Direction::In)?,
                    bulk(Direction::Out)?,
                ))
            })
            .ok_or_else(|| ScanError::Protocol("scanner has no bulk endpoints".into()))?;

        let interface = device
            .detach_and_claim_interface(interface_number)
            .wait()
            .map_err(permission_hint)?;
        let bulk_in = interface.endpoint::<Bulk, In>(in_addr).map_err(usb_error)?;
        let bulk_out = interface
            .endpoint::<Bulk, Out>(out_addr)
            .map_err(usb_error)?;

        let transport = Self {
            bulk_out: Mutex::new(bulk_out),
            bulk_in: Mutex::new(bulk_in),
        };
        transport.clear_halt()?;
        Ok(transport)
    }
}

fn transfer_error(err: TransferError) -> TransportError {
    match err {
        TransferError::Cancelled => TransportError::Timeout,
        TransferError::Stall => TransportError::Stall,
        TransferError::Disconnected => TransportError::Disconnected,
        other => TransportError::Other(other.to_string()),
    }
}

fn poisoned<T>(_: T) -> TransportError {
    TransportError::Other("USB endpoint lock poisoned".into())
}

impl BulkTransport for NusbTransport {
    fn write(&self, data: &[u8], timeout: Duration) -> Result<usize, TransportError> {
        let mut endpoint = self.bulk_out.lock().map_err(poisoned)?;
        let completion = endpoint.transfer_blocking(Buffer::from(data.to_vec()), timeout);
        completion.status.map_err(transfer_error)?;
        Ok(completion.actual_len)
    }

    fn read(&self, len: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let mut endpoint = self.bulk_in.lock().map_err(poisoned)?;
        // IN transfers must be whole packets; the device ends each reply with a
        // short packet so rounding up never swallows the next reply.
        let packet = endpoint.max_packet_size().max(1);
        let requested = len.max(1).div_ceil(packet) * packet;
        let completion = endpoint.transfer_blocking(Buffer::new(requested), timeout);
        completion.status.map_err(transfer_error)?;
        let mut data = completion.buffer.into_vec();
        data.truncate(completion.actual_len);
        Ok(data)
    }

    fn clear_halt(&self) -> Result<(), TransportError> {
        self.bulk_out
            .lock()
            .map_err(poisoned)?
            .clear_halt()
            .wait()
            .map_err(usb_error)?;
        self.bulk_in
            .lock()
            .map_err(poisoned)?
            .clear_halt()
            .wait()
            .map_err(usb_error)
    }
}
