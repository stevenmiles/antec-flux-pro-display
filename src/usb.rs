use std::time::Duration;

use anyhow::{Result, anyhow};

pub const VENDOR_ID: u16 = 0x2022;
pub const PRODUCT_ID: u16 = 0x0522;

const INTERFACE: u8 = 0;

/// The display acknowledges a write in a couple of milliseconds, so this only
/// ever expires when the device is not really there.
///
/// Keep it short. A transfer that times out has to be cancelled, and cancelling
/// makes the kernel send a Stop Endpoint command to the host controller. If the
/// device's port has not come back from suspend yet, the controller never
/// answers that command and the kernel declares the whole controller dead
/// ("xHCI host controller not responding, assume dead"), which takes down every
/// other device on it until the machine is rebooted.
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// The endpoint the display listens on, resolved once when we open the device
/// rather than re-walking the descriptors on every write.
struct Endpoint {
    address: u8,
    transfer_type: rusb::TransferType,
}

pub struct UsbDevice {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
    endpoint: Endpoint,
}

impl UsbDevice {
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self> {
        let Some(handle) = rusb::open_device_with_vid_pid(vendor_id, product_id) else {
            return Err(describe_missing_device(vendor_id, product_id));
        };

        // usbhid binds this interface, so hand it over for as long as we hold
        // the handle. Auto-detach re-attaches the kernel driver when the handle
        // is released, so dropping us leaves the device in a usable state.
        handle
            .set_auto_detach_kernel_driver(true)
            .map_err(|e| anyhow!("could not detach the kernel driver: {e}"))?;
        handle
            .claim_interface(INTERFACE)
            .map_err(|e| anyhow!("could not claim interface {INTERFACE}: {e}"))?;

        let endpoint = resolve_out_endpoint(&handle)?;
        Ok(Self { handle, endpoint })
    }

    /// Write the temperatures to the display.
    ///
    /// Returns the underlying USB error rather than exiting, so that a device
    /// that has gone away - across a suspend/resume, say - can be dealt with by
    /// dropping this handle and opening a fresh one.
    pub fn send_payload(&self, cpu_temp: &Option<f32>, gpu_temp: &Option<f32>) -> rusb::Result<()> {
        let payload = generate_payload(cpu_temp, gpu_temp);

        let written = match self.endpoint.transfer_type {
            rusb::TransferType::Bulk => {
                self.handle
                    .write_bulk(self.endpoint.address, &payload, WRITE_TIMEOUT)?
            }
            // Both of the display's endpoints are interrupt endpoints.
            _ => self
                .handle
                .write_interrupt(self.endpoint.address, &payload, WRITE_TIMEOUT)?,
        };

        if written != payload.len() {
            return Err(rusb::Error::Io);
        }
        Ok(())
    }
}

/// Prefer the interrupt OUT endpoint, which is what this display exposes, but
/// accept a bulk one so a revised device still works.
fn resolve_out_endpoint(handle: &rusb::DeviceHandle<rusb::GlobalContext>) -> Result<Endpoint> {
    let config_desc = handle
        .device()
        .config_descriptor(0)
        .map_err(|e| anyhow!("could not read the device's config descriptor: {e}"))?;

    let candidates: Vec<Endpoint> = config_desc
        .interfaces()
        .flat_map(|interface| interface.descriptors())
        .flat_map(|desc| desc.endpoint_descriptors())
        .filter(|endpoint| endpoint.direction() == rusb::Direction::Out)
        .filter(|endpoint| {
            matches!(
                endpoint.transfer_type(),
                rusb::TransferType::Interrupt | rusb::TransferType::Bulk
            )
        })
        .map(|endpoint| Endpoint {
            address: endpoint.address(),
            transfer_type: endpoint.transfer_type(),
        })
        .collect();

    let endpoint = candidates
        .iter()
        .find(|endpoint| endpoint.transfer_type == rusb::TransferType::Interrupt)
        .or_else(|| candidates.first())
        .map(|endpoint| Endpoint {
            address: endpoint.address,
            transfer_type: endpoint.transfer_type,
        })
        // The interrupt OUT endpoint on every unit seen so far.
        .unwrap_or(Endpoint {
            address: 0x03,
            transfer_type: rusb::TransferType::Interrupt,
        });

    Ok(endpoint)
}

/// Work out why `open_device_with_vid_pid` came back empty, so the user gets
/// told about a udev problem rather than an unplugged cable.
fn describe_missing_device(vendor_id: u16, product_id: u16) -> anyhow::Error {
    let devices = match rusb::devices() {
        Ok(devices) => devices,
        Err(e) => return anyhow!("could not enumerate USB devices: {e}"),
    };

    let visible = devices.iter().any(|device| {
        device
            .device_descriptor()
            .is_ok_and(|desc| desc.vendor_id() == vendor_id && desc.product_id() == product_id)
    });

    if visible {
        anyhow!(
            "permission denied accessing USB device {vendor_id:04x}:{product_id:04x}. \
             Please ensure udev rules are properly configured."
        )
    } else {
        anyhow!("USB device {vendor_id:04x}:{product_id:04x} not found. Is it connected?")
    }
}

fn generate_payload(cpu_temp: &Option<f32>, gpu_temp: &Option<f32>) -> Vec<u8> {
    let mut payload: Vec<u8> = vec![85, 170, 1, 1, 6];

    let encoded_temp = encode_temperature(cpu_temp);
    payload.push(encoded_temp.0);
    payload.push(encoded_temp.1);
    payload.push(encoded_temp.2);

    let encoded_temp = encode_temperature(gpu_temp);
    payload.push(encoded_temp.0);
    payload.push(encoded_temp.1);
    payload.push(encoded_temp.2);

    let checksum = payload.iter().fold(0u8, |acc, e| acc.wrapping_add(*e));
    payload.push(checksum);
    payload
}

fn encode_temperature(temp: &Option<f32>) -> (u8, u8, u8) {
    if let Some(temp) = temp {
        let ones = (temp / 10.0) as u8;
        let tens = (temp % 10.0) as u8;
        let tenths = ((temp * 10.0) % 10.0) as u8;
        return (ones, tens, tenths);
    }
    (238, 238, 238)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_generate_payload() {
        let actual = generate_payload(&Some(24.0), &Some(16.0));
        let expected = vec![85, 170, 1, 1, 6, 2, 4, 0, 1, 6, 0, 20];
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_generate_payload_with_no_gpu() {
        let actual = generate_payload(&Some(24.0), &None);
        let expected = vec![85, 170, 1, 1, 6, 2, 4, 0, 238, 238, 238, 215];
        assert_eq!(expected, actual);
    }
}
