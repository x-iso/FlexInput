//! End-to-end loopback tests over real UDP sockets on 127.0.0.1, driving the
//! actual NetManager + worker threads exactly as the engine does. Exercises the
//! full path: publish_send_frame → socket → set_latest_input, and the feedback
//! return leg publish_feedback_frame → socket → set_latest_feedback.
//!
//! Uses high, distinct uids per test so the process-global slots don't collide.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use flexinput_core::signal::Signal;
use flexinput_net::{BusFrame, FeedbackFrame, NetManager, NetNodeConfig, Transport};

/// Each test spins up a NetManager whose `reconcile` calls `retain_all`, which
/// drops every process-global slot uid not in that manager's live set — so two
/// tests running concurrently would wipe each other's frames. Serialize them.
static SERIAL: Mutex<()> = Mutex::new(());

/// Poll a closure until it returns Some or the deadline passes.
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn run_link(transport: Transport, psk: &str, send_uid: usize, recv_uid: usize, port: u16) {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut mgr = NetManager::new();
    mgr.reconcile(&[
        (
            recv_uid,
            NetNodeConfig::Recv {
                transport,
                bind_port: port,
                stale_ms: 500,
                fb_rate_hz: 500,
                psk: psk.to_string(),
                secret_key: String::new(),
            },
        ),
        (
            send_uid,
            NetNodeConfig::Send {
                transport,
                host: "127.0.0.1".to_string(),
                port,
                rate_hz: 500,
                psk: psk.to_string(),
                peer_code: String::new(),
            },
        ),
    ]);

    // Forward: a known bus frame published on the send node must arrive at the
    // recv node's input slot with the same values.
    let mut tx = BusFrame::empty();
    tx.set("left_stick", Signal::Vec2(glam::Vec2::new(0.5, -0.25)));
    tx.set("btn_south", Signal::Bool(true));
    tx.set("right_trigger", Signal::Float(0.75));
    flexinput_net::publish_send_frame(send_uid, tx.clone());

    let got = wait_for(Duration::from_secs(3), || {
        flexinput_net::latest_input(recv_uid).map(|(f, _)| f)
    });
    let got = got.expect("recv worker never received the forward frame");
    let li = flexinput_net::frame::input_layout();
    assert_eq!(got.get_idx(li.pin_index("left_stick").unwrap()), tx.get_idx(li.pin_index("left_stick").unwrap()));
    assert_eq!(got.get_idx(li.pin_index("btn_south").unwrap()), Some(Signal::Bool(true)));
    assert_eq!(got.get_idx(li.pin_index("right_trigger").unwrap()), Some(Signal::Float(0.75)));

    // Feedback return leg: the recv side must have latched the sender's address
    // from the forward packet, so a feedback frame published there rides back to
    // the send node's feedback slot.
    let mut fb = FeedbackFrame::empty();
    fb.set("rumble_strong", 0.9);
    fb.set("lightbar_r", 0.4);
    flexinput_net::publish_feedback_frame(recv_uid, fb);

    let got_fb = wait_for(Duration::from_secs(3), || {
        flexinput_net::latest_feedback(send_uid).map(|(f, _)| f)
    });
    let got_fb = got_fb.expect("send worker never received the feedback frame");
    let present: std::collections::HashMap<&str, f32> = got_fb.iter_present().collect();
    assert_eq!(present.get("rumble_strong").copied(), Some(0.9));
    assert_eq!(present.get("lightbar_r").copied(), Some(0.4));

    // Tear down: dropping the manager stops + joins both workers, freeing the port.
    drop(mgr);
}

#[test]
fn udp_plaintext_loopback_bidirectional() {
    run_link(Transport::Udp, "", 0xE2E_00001, 0xE2E_00002, 47811);
}

#[test]
fn psk_encrypted_loopback_bidirectional() {
    run_link(Transport::Psk, "correct horse battery staple", 0xE2E_00011, 0xE2E_00012, 47812);
}

/// A PSK mismatch must leave the receiver silent — no input frame is published,
/// because every packet fails authentication.
#[test]
fn psk_mismatch_publishes_nothing() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (send_uid, recv_uid, port) = (0xE2E_00021, 0xE2E_00022, 47813);
    let mut mgr = NetManager::new();
    mgr.reconcile(&[
        (
            recv_uid,
            NetNodeConfig::Recv {
                transport: Transport::Psk,
                bind_port: port,
                stale_ms: 500,
                fb_rate_hz: 200,
                psk: "alpha".to_string(),
                secret_key: String::new(),
            },
        ),
        (
            send_uid,
            NetNodeConfig::Send {
                transport: Transport::Psk,
                host: "127.0.0.1".to_string(),
                port,
                rate_hz: 500,
                psk: "bravo".to_string(),
                peer_code: String::new(),
            },
        ),
    ]);

    let mut tx = BusFrame::empty();
    tx.set("btn_north", Signal::Bool(true));
    flexinput_net::publish_send_frame(send_uid, tx);

    // Give packets ~400 ms to (fail to) arrive.
    let leaked = wait_for(Duration::from_millis(400), || {
        flexinput_net::latest_input(recv_uid).map(|_| ())
    });
    assert!(leaked.is_none(), "receiver accepted a packet under the wrong passphrase");

    drop(mgr);
}
