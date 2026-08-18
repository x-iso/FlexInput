//! Joy-Con 2 attribute handles, learned rather than discovered.
//!
//! These are confirmed against a real GATT discovery walk AND against the UUID
//! constants in TommyWabg/Switch2Connect, which drives these controllers
//! successfully. An earlier version took them from an HCI capture of the
//! Windows stack and guessed two of them wrong — see [`HANDLE_CMD_WRITE`].
//!
//! The trade-off is stated plainly: if a firmware revision moves these, writes
//! go to the wrong attribute and notifications never arrive.
//! [`plausible_layout`] exists so that failure is checked rather than assumed,
//! and a discovery pass can be added later without changing anything else.

/// Vendor command characteristic, UUID `649d4ac9-8eb7-4e6c-af44-1ea54fe5f005`.
///
/// ❗ **Was `0x0016` and that was WRONG.** `0x0016` carries a per-side UUID
/// (`ce49a830…` on the left, `65a724b3…` on the right); the real command
/// channel is IDENTICAL on both halves, as a shared channel must be. Every
/// command went to the wrong characteristic, which explains the whole cluster
/// of symptoms at once: no command response EVER arrived, the feature mask
/// never applied, the rich input characteristic never started streaming, and
/// the magnetometer bit appeared to do nothing.
///
/// Confirmed against the discovered attribute table and TommyWabg/Switch2Connect.
///
/// ❗ **But it does NOT replace [`HANDLE_CMD_WRITE_PERSIDE`].** Sending the init
/// here INSTEAD reduced the per-side stream to stubs — counter incrementing,
/// every other byte zero — while the common stream still never started. The
/// device evidently has two parallel channel sets and the per-side one drives
/// the per-side input:
///
/// | | common | per-side |
/// |---|---|---|
/// | input | `0x000A` | `0x000E` |
/// | command | `0x0014` | `0x0016` |
/// | response | `0x001A` | `0x001E` |
/// | rumble | — | `0x0012` |
///
/// ⛔ **INERT ON REAL HARDWARE. Do not send the init here.**
///
/// This is the handle the reference implementation uses, and it looked like the
/// obvious correction because its UUID is identical on both halves while
/// `0x0016` carries a per-side UUID. That reasoning was sound and the conclusion
/// was still wrong: **the controller accepts writes here and silently discards
/// them.**
///
/// Proven physically, not inferred. Four alternating player-LED patterns sent
/// here moved nothing, while the same command on [`HANDLE_CMD_WRITE`] visibly
/// drives the LEDs. Its command-response channel never answered either, until
/// the init was rerouted — at which point fifteen acknowledgements arrived at
/// once.
///
/// ❗ **Pointing the dongle's init here cost a full regression**: every
/// handshake step, memory read and feature-select went into the void, the
/// controller never left stub mode, and it reported `motion_len = 0` with every
/// field zero — which reads downstream as "the accelerometer is dead".
///
/// Kept only so the probe can keep testing it. A genuine Joy-Con 2 may well
/// implement it; this third-party pad does not.
pub const HANDLE_CMD_WRITE_COMMON: u16 = 0x0014;

/// ⭐ **THE command channel. Everything that must actually reach the controller
/// goes here**, framed with the 17-byte rumble prefix (`protocol::rumble_cmd_frame`).
///
/// Shared with rumble, which is why it carries that prefix and why its UUID
/// differs per half. Confirmed working three separate ways: the player LEDs
/// respond, the report leaves stub mode, and the init draws command
/// acknowledgements on [`HANDLE_CMD_RESPONSE`].
///
/// The name is deliberately the plain one so that the working handle is what
/// any new code reaches for by default.
pub const HANDLE_CMD_WRITE: u16 = 0x0016;

/// Input report characteristic. Notifications arrive here at the connection
/// interval, carrying the 63-byte report `flexinput_joycon2::reports` parses.
pub const HANDLE_INPUT_VALUE: u16 = 0x000E;

/// Common input characteristic, UUID `ab7de9be-89fe-49ad-828f-118f09df7fd2`.
///
/// ⭐ **This is the real input stream**, not the per-side `0x000E`. It stays
/// silent until the feature command enables motion — which never happened while
/// commands were going to the wrong handle, so it was measured as "exists but
/// never notifies" and wrongly written off.
///
/// Its report carries accelerometer and gyroscope as plain contiguous signed
/// 16-bit values, not the sparse strided block on `0x000E`.
pub const HANDLE_INPUT_COMMON: u16 = 0x000A;
pub const HANDLE_INPUT_COMMON_CCCD: u16 = HANDLE_INPUT_COMMON + 1;
/// Report-rate descriptor for the common input, mirroring [`HANDLE_INPUT_REPORT_RATE`].
///
/// ⭐ **Nothing ever wrote to this**, and that asymmetry went unexamined for the
/// whole gyro search. The per-side input has the identical descriptor at
/// `0x0010`, the init has always written [`REPORT_RATE_PAYLOAD`] there, and the
/// note on that constant records that skipping it leaves the controller
/// emitting stubs. The common input was subscribed and enabled but never had
/// its rate set — then measured as "exists but never notifies" and written off
/// as a real negative.
pub const HANDLE_INPUT_COMMON_RATE: u16 = HANDLE_INPUT_COMMON + 2;

/// Client Characteristic Configuration descriptor for the input characteristic.
///
/// A CCCD sits immediately after the value handle it configures, which is the
/// convention every GATT server follows and the reason this can be derived
/// rather than captured.
pub const HANDLE_INPUT_CCCD: u16 = HANDLE_INPUT_VALUE + 1;

/// Vendor "report rate" descriptor on the input characteristic
/// (`679d5510-5a24-4dee-9557-95df80486ecb`).
///
/// DERIVED, not captured: descriptors follow their characteristic's value
/// handle, so with the value at `0x000e` and the CCCD at `0x000f` this is the
/// next one along. Write it with an acknowledged Write Request so a wrong guess
/// surfaces as an ATT error rather than silence.
pub const HANDLE_INPUT_REPORT_RATE: u16 = HANDLE_INPUT_CCCD + 1;

/// Payload for the descriptor above, copied verbatim from the captured init.
///
/// Official software writes this as its second-to-last init step; the research
/// doc labels it "Set Report Rate?". Without it the controller keeps emitting
/// STUB reports — counter incrementing, every field zero — which is
/// indistinguishable from a parser bug.
pub const REPORT_RATE_PAYLOAD: [u8; 2] = [0x85, 0x00];

/// Command-response characteristic, UUID `c765a961-d9d8-4d36-a20a-5315b111836a`.
///
/// ❗ Was `0x001E`, also per-side and also wrong. Same UUID on both halves.
pub const HANDLE_CMD_RESPONSE: u16 = 0x001A;
pub const HANDLE_CMD_RESPONSE_CCCD: u16 = HANDLE_CMD_RESPONSE + 1;

/// ⭐ The PER-SIDE command-response characteristic — the one that answers the
/// handle we actually send commands to.
///
/// ❗ Never subscribed, for the whole life of this project. Commands go to
/// [`HANDLE_CMD_WRITE`] (`0x0016`, per-side, the handle the controller visibly
/// executes from), but the only response channel ever enabled was `0x001A`, the
/// COMMON one. So replies to nearly every command sent have been arriving on an
/// unsubscribed characteristic and were never delivered at all.
///
/// The note above — "was `0x001E`, also per-side and also wrong" — was about
/// which handle carries the replies we were then looking for. It is not a reason
/// to leave this one dark: the controller answers where it was asked, and we
/// have been asking on the per-side channel.
pub const HANDLE_CMD_RESPONSE_PERSIDE: u16 = 0x001E;
pub const HANDLE_CMD_RESPONSE_PERSIDE_CCCD: u16 = HANDLE_CMD_RESPONSE_PERSIDE + 1;

/// ⭐ A THIRD notifiable stream, `ab7de9be-…-7fde`, never subscribed.
///
/// Found by finally walking the controller's whole attribute table: 48
/// attributes, handles `0x0001`..`0x0030`. This one matters because of its
/// SHAPE — it is the only characteristic besides the two known input streams
/// that carries both a CCCD and its own report-rate descriptor:
///
/// ```text
///   0x000a  ab7de9be-…-7fd2   CCCD 0x000b   rate 0x000c   common input
///   0x000e  d5a9e01e-2ffc-…   CCCD 0x000f   rate 0x0010   per-side input
///   0x0026  ab7de9be-…-7fde   CCCD 0x0027   rate 0x0028   ← this one
/// ```
///
/// Same vendor UUID family as the common input, one digit apart, with the same
/// "subscribe then set a report rate" machinery. Everything about it says
/// periodic sensor stream, and the report we do receive is missing exactly one
/// periodic sensor.
///
/// The whole hunt assumed the motion had to be inside the per-side report
/// because that was the only stream known to exist. The attribute table says
/// otherwise, and it was never consulted until now.
pub const HANDLE_INPUT_EXTRA: u16 = 0x0026;
pub const HANDLE_INPUT_EXTRA_CCCD: u16 = HANDLE_INPUT_EXTRA + 1;
pub const HANDLE_INPUT_EXTRA_RATE: u16 = HANDLE_INPUT_EXTRA + 2;

/// A fourth notifiable characteristic, `d3bd69d2-841c-4241-ab15-f86f406d2a80`.
///
/// Has a CCCD (`0x0023`) but NO report-rate descriptor, so it is more likely
/// event-driven than periodic. Subscribed alongside [`HANDLE_INPUT_EXTRA`]
/// because it costs one write and nothing else in the attribute table is
/// unexplored.
pub const HANDLE_NOTIFY_EXTRA2: u16 = 0x0022;
pub const HANDLE_NOTIFY_EXTRA2_CCCD: u16 = HANDLE_NOTIFY_EXTRA2 + 1;

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

/// Is this handle one the controller actually executes commands from?
///
/// Exists so the distinction is checkable rather than a comment. Writing the
/// init to the wrong one fails **completely silently** — the writes are
/// accepted, nothing happens, and the controller streams stub reports that look
/// like a broken accelerometer several layers away.
pub const fn executes_commands(handle: u16) -> bool {
    handle == HANDLE_CMD_WRITE
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
        assert_eq!(HANDLE_INPUT_COMMON_CCCD, 0x000B);
        assert_eq!(HANDLE_CMD_RESPONSE_CCCD, 0x001B);
    }

    #[test]
    fn the_command_handle_is_the_one_hardware_executes() {
        // Pinned as a bare number because this constant was changed to 0x0014
        // on documentary evidence — the reference's UUID map — and hardware
        // then proved 0x0014 inert. The dongle init went silently nowhere and
        // the controller looked like it had a dead IMU.
        assert_eq!(HANDLE_CMD_WRITE, 0x0016);
        assert!(executes_commands(HANDLE_CMD_WRITE));
        assert!(!executes_commands(HANDLE_CMD_WRITE_COMMON));
        // The two must stay distinct: collapsing them is how the mistake gets
        // made a second time.
        assert_ne!(HANDLE_CMD_WRITE, HANDLE_CMD_WRITE_COMMON);
    }

    #[test]
    fn requested_mtu_can_carry_a_whole_input_report() {
        // 63-byte report + 3-byte ATT notification header.
        assert!(DESIRED_MTU >= 66);
    }
}
