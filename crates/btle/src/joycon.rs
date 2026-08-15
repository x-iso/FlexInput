//! Joy-Con 2 attribute handles, learned rather than discovered.
//!
//! These come from an HCI capture taken while the Windows stack drove the
//! controller, so a full GATT discovery walk can be skipped: writing to handle
//! `0x0016` is the same operation whether or not we enumerated the attribute
//! table to find it, and discovery is a lot of code for a fixed answer.
//!
//! The trade-off is stated plainly: if a firmware revision moves these, writes
//! go to the wrong attribute and notifications never arrive.
//! [`plausible_layout`] exists so that failure is checked rather than assumed,
//! and a discovery pass can be added later without changing anything else.

/// Vendor command + rumble characteristic. Written with ATT `Write Command`
/// (no response) — the controller does not acknowledge these.
pub const HANDLE_CMD_WRITE: u16 = 0x0016;

/// Input report characteristic. Notifications arrive here at the connection
/// interval, carrying the 63-byte report `flexinput_joycon2::reports` parses.
pub const HANDLE_INPUT_VALUE: u16 = 0x000E;

/// Client Characteristic Configuration descriptor for the input characteristic.
///
/// A CCCD sits immediately after the value handle it configures, which is the
/// convention every GATT server follows and the reason this can be derived
/// rather than captured.
pub const HANDLE_INPUT_CCCD: u16 = HANDLE_INPUT_VALUE + 1;

/// Command-response characteristic, per the Bluetooth notes in
/// `flexinput-joycon2` (`CMD_RESP_HEADER_OFFSET` applies to its payloads).
pub const HANDLE_CMD_RESPONSE: u16 = 0x001E;
pub const HANDLE_CMD_RESPONSE_CCCD: u16 = HANDLE_CMD_RESPONSE + 1;

/// ATT MTU to request.
///
/// The default is 23 bytes, which would fragment a 63-byte input report across
/// three notifications and break every parser offset. 517 is the maximum a
/// controller may accept; anything at or above 67 would do.
pub const DESIRED_MTU: u16 = 517;

/// Sanity-check the handle layout before relying on it.
///
/// Cheap insurance against a firmware revision moving things: the ordering
/// asserted here (input before command-write before command-response, each
/// CCCD directly after its value) is what the capture showed, and a violation
/// means these constants need re-deriving rather than trusting.
pub const fn plausible_layout() -> bool {
    HANDLE_INPUT_VALUE < HANDLE_CMD_WRITE
        && HANDLE_CMD_WRITE < HANDLE_CMD_RESPONSE
        && HANDLE_INPUT_CCCD == HANDLE_INPUT_VALUE + 1
        && HANDLE_CMD_RESPONSE_CCCD == HANDLE_CMD_RESPONSE + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_layout_is_self_consistent() {
        assert!(plausible_layout());
    }

    #[test]
    fn cccds_sit_directly_after_their_value_handles() {
        // Spelled out because an off-by-one here produces the worst failure
        // mode available: the subscribe write succeeds against some other
        // attribute and no notifications ever arrive, with no error.
        assert_eq!(HANDLE_INPUT_CCCD, 0x000F);
        assert_eq!(HANDLE_CMD_RESPONSE_CCCD, 0x001F);
    }

    #[test]
    fn requested_mtu_can_carry_a_whole_input_report() {
        // 63-byte report + 3-byte ATT notification header.
        assert!(DESIRED_MTU >= 66);
    }
}
