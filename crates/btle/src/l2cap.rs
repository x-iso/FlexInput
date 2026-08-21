//! L2CAP signalling: connection-oriented channels for BR/EDR.
//!
//! ⭐ **This is the piece the LE stack never needed.** Everything built for
//! Joy-Con 2 speaks ATT on CID `0x0004` — a FIXED channel that exists the moment
//! the link does, with no negotiation at all. Classic HID has no such thing:
//! each channel must be requested by PSM, granted a pair of channel ids, and
//! then CONFIGURED by both ends before a single byte of payload may cross it.
//!
//! The exchange, per channel, is four or five packets:
//!
//! ```text
//!   host                                    controller
//!    |-- Connection Request  (PSM, our CID) ---->|
//!    |<-- Connection Response (their CID, ok) ---|
//!    |-- Configuration Request (MTU) ----------->|
//!    |<-- Configuration Response (ok) -----------|
//!    |<-- Configuration Request -----------------|   (they configure us)
//!    |-- Configuration Response (ok) ----------->|
//!    |                  channel open             |
//! ```
//!
//! ❗ **Both directions must be configured.** A host that configures its own
//! side and ignores the remote's request gets a channel that looks connected
//! and never delivers anything — the controller is waiting for a response it
//! will not get. That failure is silent and looks exactly like a device that
//! is not sending reports.
//!
//! HID uses two channels, and the PSMs are fixed by the HID profile rather than
//! discovered: `0x0011` carries control (feature reports, set-protocol),
//! `0x0013` carries interrupt (the input reports that actually matter). SDP
//! would be needed to read a device's REPORT DESCRIPTOR, but not to find these.

/// L2CAP signalling channel — where connection and configuration live.
pub const CID_SIGNALLING: u16 = 0x0001;

/// HID control PSM, from the HID profile.
pub const PSM_HID_CONTROL: u16 = 0x0011;
/// HID interrupt PSM: input reports arrive here.
pub const PSM_HID_INTERRUPT: u16 = 0x0013;

/// Signalling command codes.
pub const SIG_CONNECTION_REQUEST: u8 = 0x02;
pub const SIG_CONNECTION_RESPONSE: u8 = 0x03;
pub const SIG_CONFIGURE_REQUEST: u8 = 0x04;
pub const SIG_CONFIGURE_RESPONSE: u8 = 0x05;
pub const SIG_DISCONNECTION_REQUEST: u8 = 0x06;
pub const SIG_DISCONNECTION_RESPONSE: u8 = 0x07;

/// First dynamically allocated channel id. Everything below `0x0040` is
/// reserved for the fixed channels.
pub const FIRST_DYNAMIC_CID: u16 = 0x0040;

/// One decoded signalling command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub code: u8,
    pub identifier: u8,
    pub data: Vec<u8>,
}

/// Wrap a signalling command for sending on [`CID_SIGNALLING`].
pub fn encode_signal(code: u8, identifier: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + data.len());
    v.push(code);
    v.push(identifier);
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(data);
    v
}

/// Decode one signalling command from an L2CAP payload.
///
/// ❗ Returns `None` rather than guessing at a short packet: a signalling
/// command decoded from the wrong bytes produces a plausible channel id, and
/// acting on it corrupts a channel that was working.
pub fn parse_signal(payload: &[u8]) -> Option<Signal> {
    if payload.len() < 4 {
        return None;
    }
    let len = u16::from_le_bytes([payload[2], payload[3]]) as usize;
    if payload.len() < 4 + len {
        return None;
    }
    Some(Signal {
        code: payload[0],
        identifier: payload[1],
        data: payload[4..4 + len].to_vec(),
    })
}

/// `Connection Request`: which service, and the channel id we will listen on.
pub fn connection_request(psm: u16, source_cid: u16) -> Vec<u8> {
    let mut d = Vec::with_capacity(4);
    d.extend_from_slice(&psm.to_le_bytes());
    d.extend_from_slice(&source_cid.to_le_bytes());
    d
}

/// The PSM and remote channel id out of an incoming `Connection Request`.
///
/// ⭐ **The device opens the HID channels, not the host.** A reconnecting
/// Bluetooth HID device sends its own `Connection Request` for PSM 0x11 and
/// 0x13 and waits to be answered. A host that instead sends requests of its own
/// and waits gets silence from a device that is already waiting on it — both
/// sides blocked, until the link dies of an LMP timeout ten seconds later.
///
/// That is the difference between an OUTGOING connection, where paging the
/// device makes us the initiator, and an INCOMING one, where it is.
pub fn parse_connection_request(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() < 4 {
        return None;
    }
    Some((
        u16::from_le_bytes([data[0], data[1]]),
        u16::from_le_bytes([data[2], data[3]]),
    ))
}

/// Answer a `Connection Request`.
///
/// `dest_cid` is the id the REMOTE will send to (ours); `source_cid` is the id
/// it asked to receive on. `result` is `0x0000` for success.
pub fn connection_response(dest_cid: u16, source_cid: u16, result: u16) -> Vec<u8> {
    let mut d = Vec::with_capacity(8);
    d.extend_from_slice(&dest_cid.to_le_bytes());
    d.extend_from_slice(&source_cid.to_le_bytes());
    d.extend_from_slice(&result.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // status: no further information
    d
}

/// The outcome of a `Connection Response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionResponse {
    /// The channel id to SEND to.
    pub dest_cid: u16,
    /// The channel id we asked to receive on.
    pub source_cid: u16,
    /// `0x0000` success, `0x0001` pending, anything else a refusal.
    pub result: u16,
    pub status: u16,
}

pub fn parse_connection_response(data: &[u8]) -> Option<ConnectionResponse> {
    if data.len() < 8 {
        return None;
    }
    Some(ConnectionResponse {
        dest_cid: u16::from_le_bytes([data[0], data[1]]),
        source_cid: u16::from_le_bytes([data[2], data[3]]),
        result: u16::from_le_bytes([data[4], data[5]]),
        status: u16::from_le_bytes([data[6], data[7]]),
    })
}

/// `Configuration Request` carrying just an MTU option.
///
/// ⭐ One option, deliberately. The spec allows flush timeout, QoS and
/// retransmission modes, and a HID device accepts basic mode with a plain MTU —
/// offering more is more that can be rejected, and a rejected option means
/// renegotiating rather than connecting.
pub fn configure_request(dest_cid: u16, mtu: u16) -> Vec<u8> {
    let mut d = Vec::with_capacity(8);
    d.extend_from_slice(&dest_cid.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // flags: no continuation
    d.push(0x01); // option type: MTU
    d.push(0x02); // option length
    d.extend_from_slice(&mtu.to_le_bytes());
    d
}

/// `Configuration Response` accepting whatever the remote asked for.
///
/// ❗ The remote's own options are echoed back unchanged. Answering with an
/// empty option list is legal only when nothing was proposed; a controller that
/// asked for an MTU and got a bare "success" may re-request forever.
pub fn configure_response(source_cid: u16, options: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(6 + options.len());
    d.extend_from_slice(&source_cid.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // flags
    d.extend_from_slice(&0u16.to_le_bytes()); // result: success
    d.extend_from_slice(options);
    d
}

/// The destination CID and option bytes out of an incoming `Configuration
/// Request`.
pub fn parse_configure_request(data: &[u8]) -> Option<(u16, Vec<u8>)> {
    if data.len() < 4 {
        return None;
    }
    Some((
        u16::from_le_bytes([data[0], data[1]]),
        data[4..].to_vec(),
    ))
}

/// The channel id AND result of a `Configuration Response`.
///
/// ⭐ The id matters as soon as more than one channel is being set up at a
/// time: `Source CID` names the endpoint on the device RECEIVING the response,
/// which is our own local id, and without it a success for the control channel
/// is indistinguishable from one for the interrupt channel.
pub fn parse_configure_response_full(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() < 6 {
        return None;
    }
    Some((
        u16::from_le_bytes([data[0], data[1]]),
        u16::from_le_bytes([data[4], data[5]]),
    ))
}

/// The result field of a `Configuration Response`.
pub fn parse_configure_response(data: &[u8]) -> Option<u16> {
    if data.len() < 6 {
        return None;
    }
    Some(u16::from_le_bytes([data[4], data[5]]))
}

/// `Disconnection Request`, so a channel is closed rather than abandoned.
pub fn disconnection_request(dest_cid: u16, source_cid: u16) -> Vec<u8> {
    let mut d = Vec::with_capacity(4);
    d.extend_from_slice(&dest_cid.to_le_bytes());
    d.extend_from_slice(&source_cid.to_le_bytes());
    d
}

/// A negotiated channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub psm: u16,
    /// Our id — what inbound packets for this channel are addressed to.
    pub local_cid: u16,
    /// Their id — what outbound packets must be addressed to.
    pub remote_cid: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signalling_command_round_trips() {
        let body = connection_request(PSM_HID_INTERRUPT, 0x0041);
        let raw = encode_signal(SIG_CONNECTION_REQUEST, 7, &body);
        let s = parse_signal(&raw).expect("must decode");
        assert_eq!(s.code, SIG_CONNECTION_REQUEST);
        assert_eq!(s.identifier, 7);
        assert_eq!(s.data, body);
        assert_eq!(u16::from_le_bytes([s.data[0], s.data[1]]), 0x0013);
        assert_eq!(u16::from_le_bytes([s.data[2], s.data[3]]), 0x0041);
    }

    /// ⛔ A short signalling packet must decode to nothing.
    ///
    /// Guessing yields a plausible channel id, and acting on one corrupts a
    /// channel that was working — the damage lands somewhere other than where
    /// the bad packet arrived.
    #[test]
    fn a_truncated_signal_is_refused() {
        assert!(parse_signal(&[0x02, 0x01]).is_none());
        // Claims eight bytes of data, carries two.
        assert!(parse_signal(&[0x02, 0x01, 0x08, 0x00, 0xAA, 0xBB]).is_none());
    }

    /// The two channel ids in a connection response are NOT interchangeable:
    /// `dest_cid` is what we must send to, `source_cid` is what we asked to
    /// receive on. Swapping them yields a channel that transmits into nothing.
    #[test]
    fn a_connection_response_keeps_the_two_channel_ids_apart() {
        let data = [0x45, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00];
        let r = parse_connection_response(&data).expect("must decode");
        assert_eq!(r.dest_cid, 0x0045, "the id to SEND to");
        assert_eq!(r.source_cid, 0x0041, "the id we RECEIVE on");
        assert_eq!(r.result, 0, "success");
    }

    #[test]
    fn a_refused_connection_is_reported_as_such() {
        let data = [0x00, 0x00, 0x41, 0x00, 0x02, 0x00, 0x00, 0x00];
        let r = parse_connection_response(&data).expect("must decode");
        assert_eq!(r.result, 2, "PSM not supported");
    }

    /// A configuration request's options must survive being echoed back, or the
    /// remote re-requests forever.
    #[test]
    fn configuration_options_are_echoed_back_unchanged() {
        let opts = [0x01, 0x02, 0x40, 0x00]; // MTU 0x0040
        let mut req = Vec::new();
        req.extend_from_slice(&0x0041u16.to_le_bytes());
        req.extend_from_slice(&0u16.to_le_bytes());
        req.extend_from_slice(&opts);
        let (cid, got) = parse_configure_request(&req).expect("must decode");
        assert_eq!(cid, 0x0041);
        assert_eq!(got, opts);
        let resp = configure_response(0x0045, &got);
        assert_eq!(&resp[6..], &opts, "options were not echoed");
        assert_eq!(u16::from_le_bytes([resp[4], resp[5]]), 0, "must say success");
    }
}
