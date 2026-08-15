//! Windows-side bonding for Joy-Con 2 controllers.
//!
//! # Why
//!
//! An HCI capture of a live session settled what six earlier theories could
//! not. Windows sends `HCI_Disconnect` **31.1 s** after `LE Create Connection`,
//! with `Reason = 0x16` ("Connection Terminated by Local Host"). The controller
//! never terminates anything. At the moment of the kill the link was perfectly
//! healthy: notifications arriving every 15 ms, the last one 11 ms earlier, and
//! our keep-alive write going out fine. Nothing preceded the disconnect — no
//! SMP, no security request, no failed command. It is a **timer**, and neither
//! `GattSession.MaintainConnection = true` nor constant traffic in both
//! directions stops it.
//!
//! The remaining difference between this connection and one Windows does not
//! reclaim is that the device is **unpaired**. So: pair it.
//!
//! # Why this might not work, stated up front
//!
//! The protocol research warns that "attempting to pair controllers using SMP
//! (as many platforms do automatically) will cause the controller to terminate
//! the connection". If that holds, this trades a 30-second link for none at
//! all. It is gated behind an environment variable and fully reversible via
//! [`unpair`] for exactly that reason.
//!
//! But the capture also shows Windows has **never** attempted SMP with this
//! controller — there is not one security PDU in the whole trace — so that
//! warning is untested here rather than confirmed. And this asks for
//! [`DevicePairingProtectionLevel::None`], the weakest level, rather than the
//! encrypted/authenticated pairing the warning is presumably about.

#![cfg(windows)]

use windows::core::Result as WinResult;
use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Enumeration::{
    DeviceInformationCustomPairing, DevicePairingKinds, DevicePairingProtectionLevel,
    DevicePairingRequestedEventArgs, DevicePairingResultStatus,
};
use windows::Foundation::TypedEventHandler;

/// Run a WinRT async operation to completion on the calling thread.
///
/// windows-future 0.2 had `IAsyncOperation::get()` for this; 0.3 moved it to an
/// `Async::join` on a trait the crate does not export, leaving `IntoFuture` as
/// the only public route. Driving that future on a local executor is equivalent
/// and keeps every WinRT object inside this module's plain functions, which is
/// what stops them poisoning the hub's `Send` futures.
fn block<F: std::future::IntoFuture>(op: F) -> F::Output {
    futures::executor::block_on(op.into_future())
}

/// Pack a BD_ADDR in natural (display) order into the u64 WinRT expects, where
/// the first displayed octet is the most significant byte.
fn address_to_u64(address: [u8; 6]) -> u64 {
    address.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

// Everything here is SYNCHRONOUS on purpose, blocking on each WinRT operation
// with `.get()` rather than `.await`.
//
// WinRT objects are COM apartment-bound and therefore `!Send`. Holding one
// across an `.await` makes the enclosing future `!Send`, and the hub's pad
// tasks go through `tokio::spawn`, which requires `Send` — so an async version
// of this module does not compile. Keeping the objects inside plain functions
// means they are created and dropped without ever crossing an await point.
//
// `spawn_blocking` would be the other way out, but it would move the calls to a
// pool thread with no initialised COM apartment. These run on the hub's own
// thread, which already makes WinRT calls through btleplug, so the apartment is
// known good. The block is bounded and happens once per connection.

fn device_for(address: [u8; 6]) -> WinResult<BluetoothLEDevice> {
    block(BluetoothLEDevice::FromBluetoothAddressAsync(address_to_u64(address))?)
}

/// Whether Windows currently holds a bond for this controller.
pub fn is_paired(address: [u8; 6]) -> Result<bool, String> {
    let dev = device_for(address).map_err(|e| e.to_string())?;
    dev.DeviceInformation()
        .and_then(|i| i.Pairing())
        .and_then(|p| p.IsPaired())
        .map_err(|e| e.to_string())
}

/// Ask Windows to bond with the controller at the weakest protection level.
///
/// Returns the pairing status as a string either way — `Paired`,
/// `AlreadyPaired`, `Failed`, `RejectedByHandler`, and so on are all useful to
/// see, so a non-success status is reported rather than turned into an error.
pub fn pair(address: [u8; 6]) -> Result<String, String> {
    let dev = device_for(address).map_err(|e| e.to_string())?;
    let pairing = dev
        .DeviceInformation()
        .and_then(|i| i.Pairing())
        .map_err(|e| e.to_string())?;

    if pairing.IsPaired().unwrap_or(false) {
        return Ok("AlreadyPaired".to_string());
    }

    let custom: DeviceInformationCustomPairing = pairing.Custom().map_err(|e| e.to_string())?;

    // Without a handler that accepts, `ConfirmOnly` pairing is rejected — there
    // is no UI here to click through, so this is the whole ceremony.
    let handler = TypedEventHandler::<
        DeviceInformationCustomPairing,
        DevicePairingRequestedEventArgs,
    >::new(|_, args| {
        if let Some(args) = args.as_ref() {
            eprintln!("[jc2-winpair] pairing requested, accepting");
            let _ = args.Accept();
        }
        Ok(())
    });
    let token = custom
        .PairingRequested(&handler)
        .map_err(|e| e.to_string())?;

    let result = block(
        custom
            .PairWithProtectionLevelAsync(
                DevicePairingKinds::ConfirmOnly,
                DevicePairingProtectionLevel::None,
            )
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let status = result.Status().map_err(|e| e.to_string())?;
    let _ = custom.RemovePairingRequested(token);

    Ok(describe(status))
}

/// Remove Windows' bond, undoing [`pair`].
///
/// The escape hatch: if bonding makes the controller refuse to talk, this puts
/// things back without a trip through Settings.
pub fn unpair(address: [u8; 6]) -> Result<String, String> {
    let dev = device_for(address).map_err(|e| e.to_string())?;
    let pairing = dev
        .DeviceInformation()
        .and_then(|i| i.Pairing())
        .map_err(|e| e.to_string())?;
    let result = block(pairing.UnpairAsync().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let status = result.Status().map_err(|e| e.to_string())?;
    Ok(format!("{status:?}"))
}

/// Name the statuses worth recognising; anything else falls back to Debug.
///
/// `Failed` and `AuthenticationFailure` mean very different things here — the
/// first suggests Windows never got a usable response, the second that SMP ran
/// and the controller refused it, which would confirm the research's warning.
fn describe(status: DevicePairingResultStatus) -> String {
    match status {
        DevicePairingResultStatus::Paired => "Paired".into(),
        DevicePairingResultStatus::AlreadyPaired => "AlreadyPaired".into(),
        DevicePairingResultStatus::NotReadyToPair => "NotReadyToPair".into(),
        DevicePairingResultStatus::ConnectionRejected => "ConnectionRejected".into(),
        DevicePairingResultStatus::AuthenticationFailure => "AuthenticationFailure".into(),
        DevicePairingResultStatus::AuthenticationTimeout => "AuthenticationTimeout".into(),
        DevicePairingResultStatus::RejectedByHandler => "RejectedByHandler".into(),
        DevicePairingResultStatus::Failed => "Failed".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_packs_with_the_first_octet_most_significant() {
        // c8:48:05:fd:1b:78 — the R half's address, in the order it is printed.
        assert_eq!(
            address_to_u64([0xc8, 0x48, 0x05, 0xfd, 0x1b, 0x78]),
            0x0000_c848_05fd_1b78,
        );
    }
}
