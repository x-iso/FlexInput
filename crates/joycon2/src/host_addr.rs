//! Local Bluetooth adapter address lookup.
//!
//! The pairing handshake sends the host's BD_ADDR to the controller, which
//! stores it and later embeds it in its reconnection advertisements. btleplug
//! exposes no portable way to read the local adapter address, so this is done
//! per-platform. Only Windows is implemented; elsewhere pairing is skipped.

/// Identity of the host Bluetooth radio.
///
/// Worth surfacing because BLE link stability with these controllers is very
/// much a function of the radio: they use a proprietary, non-HID GATT profile
/// with an unusually fast connection interval, and adapters differ wildly in
/// how well they hold such a link — especially while also carrying Bluetooth
/// Classic traffic on the same antenna.
#[derive(Debug, Clone)]
pub struct RadioInfo {
    /// Bluetooth SIG company identifier of the radio manufacturer.
    pub manufacturer_id: u16,
    /// Human-readable name for the common ones, else `"unknown"`.
    pub manufacturer: &'static str,
}

/// Bluetooth SIG company identifiers for the radio vendors that actually turn
/// up in PCs. Not exhaustive — anything else reports its raw id.
fn company_name(id: u16) -> &'static str {
    match id {
        2 => "Intel",
        15 => "Broadcom",
        70 => "MediaTek",
        93 => "Realtek",
        10 => "Cambridge Silicon Radio (CSR)",
        29 => "Qualcomm",
        _ => "unknown",
    }
}

/// Read the primary Bluetooth radio's manufacturer.
#[cfg(windows)]
pub fn radio_info() -> Option<RadioInfo> {
    let (_, manufacturer_id) = read_radio()?;
    Some(RadioInfo {
        manufacturer_id,
        manufacturer: company_name(manufacturer_id),
    })
}

#[cfg(not(windows))]
pub fn radio_info() -> Option<RadioInfo> {
    None
}

/// Read the primary Bluetooth radio's address in natural (display) order.
///
/// Returns `None` when there is no radio, or on any platform without an
/// implementation — callers treat that as "pairing unavailable" rather than an
/// error, since streaming input works fine without it.
#[cfg(windows)]
pub fn local_bluetooth_address() -> Option<[u8; 6]> {
    read_radio().map(|(addr, _)| addr)
}

/// Shared radio query: returns `(address, manufacturer company id)`.
#[cfg(windows)]
fn read_radio() -> Option<([u8; 6], u16)> {
    use std::mem::size_of;
    use windows_sys::Win32::Devices::Bluetooth::{
        BluetoothFindFirstRadio, BluetoothFindRadioClose, BluetoothGetRadioInfo,
        BLUETOOTH_FIND_RADIO_PARAMS, BLUETOOTH_RADIO_INFO,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};

    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio: HANDLE = std::ptr::null_mut();
        let find = BluetoothFindFirstRadio(&params, &mut radio);
        if find.is_null() {
            return None;
        }

        let mut info: BLUETOOTH_RADIO_INFO = std::mem::zeroed();
        info.dwSize = size_of::<BLUETOOTH_RADIO_INFO>() as u32;
        let rc = BluetoothGetRadioInfo(radio, &mut info);

        CloseHandle(radio);
        BluetoothFindRadioClose(find);

        if rc != ERROR_SUCCESS {
            return None;
        }

        // The union stores the address little-endian in `rgBytes`; reverse to
        // the natural order the rest of this crate passes around.
        let mut addr = info.address.Anonymous.rgBytes;
        addr.reverse();
        Some((addr, info.manufacturer))
    }
}

#[cfg(not(windows))]
pub fn local_bluetooth_address() -> Option<[u8; 6]> {
    None
}
