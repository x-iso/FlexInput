// Dev probe: deploy one persona by hand and print what the host controller said.
//
// A persona deploy never reaches HIDMaestro's helper log — it is served from
// this process and plugged in through usbip-win2 — so the helper's log says
// nothing when one fails, and stderr is lost when the app starts from a
// shortcut. This runs the same deploy on its own and prints the outcome:
// whether the device reached the controller, and if not, the reason. Run it
// twice, on a kind that works and one that doesn't, and the difference says
// whether the fault is in that device's profile or in the controller.
//
//   cargo run -p flexinput-virtual --example persona_probe -- virtual.usb.dualsense
//   cargo run -p flexinput-virtual --example persona_probe -- virtual.usb.dualsense_edge
//
// It leaves the system as it found it: the persona is dropped at the end, and
// dropping it unplugs it.

use flexinput_usbip::{profiles, vhci};
use flexinput_virtual::composite_device::CompositeDevice;
use flexinput_virtual::VirtualDevice;

fn show(tag: &str) {
    match vhci::imported() {
        Ok(rows) => {
            println!("{tag}: {} imported", rows.len());
            for r in rows {
                println!("  port {} busid {} at {}:{}", r.port, r.busid, r.host, r.service);
            }
        }
        Err(e) => println!("{tag}: the import list failed: {e}"),
    }
}

fn profile_for(kind: &str) -> &'static str {
    match kind {
        "virtual.usb.dualsense" => profiles::DUALSENSE_COMPOSITE_JSON,
        "virtual.usb.dualsense_edge" => profiles::DUALSENSE_EDGE_COMPOSITE_JSON,
        _ => panic!("unknown persona kind '{kind}'"),
    }
}

fn main() {
    // `--list` only asks the host controller what it has imported, so the app can
    // be left running and its own personas inspected: the vhci list is the truth
    // about what is plugged in, where Device Manager shows ghosts of previous
    // sessions and HidHide changes what an application can see at all.
    if std::env::args().nth(1).is_some_and(|a| a == "--list") {
        println!("vhci available: {}", vhci::is_available());
        show("now");
        return;
    }

    // Every kind on the command line is deployed and held at once: two personas
    // in one process is what a workspace with an audio pad and an audio Edge
    // asks for, and a deploy that only fails alongside its neighbour shows up
    // here and nowhere else.
    let kinds: Vec<String> = std::env::args().skip(1).collect();
    let kinds = if kinds.is_empty() { vec!["virtual.usb.dualsense_edge".into()] } else { kinds };

    println!("vhci available: {}", vhci::is_available());
    println!("driver layout: {:?}", vhci::installed_layout());
    show("before");

    let mut held: Vec<CompositeDevice> = Vec::new();
    for spec in &kinds {
        // `kind@N` is the instance-qualified device id, spelled the way the app
        // spells it. The id is what fixes a persona's serial and, through it,
        // its pairing MAC — so it, not a numeric index, is what keeps two pads
        // apart in the eyes of the host.
        let kind = spec.split_once('@').map_or_else(|| spec.clone(), |(k, _)| k.to_string());
        let id = spec.clone();
        let started = std::time::Instant::now();
        let dev = CompositeDevice::create(&id, &id, profile_for(&kind));
        match dev {
            Some(dev) => {
                println!(
                    "{id} after {:?}: connected={} deploy_error={:?} serial={} mac={:02X?}",
                    started.elapsed(),
                    dev.is_connected(),
                    dev.deploy_error(),
                    flexinput_usbip::serial_for(&id),
                    // Two personas printing the same MAC is the bug: a host
                    // that files a PlayStation pad by its MAC keeps one of them
                    // and drops the other.
                    flexinput_usbip::mac_tail(&flexinput_usbip::serial_for(&id)),
                );
                held.push(dev);
            }
            None => println!("{id}: no device at all after {:?}: the profile did not parse", started.elapsed()),
        }
        show(&format!("with {}", held.len()));
    }
    // HOLD_SECS keeps the personas plugged in so the Windows side can be looked
    // at from another window while they're there: importing a device is not the
    // same as it enumerating, and only PnP knows the difference.
    if let Ok(secs) = std::env::var("HOLD_SECS") {
        let secs: u64 = secs.parse().unwrap_or(20);
        println!("holding for {secs} s (HOLD_SECS); PnP can be inspected now");
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    println!("dropping them now (that unplugs them)");
    drop(held);
    show("after");
}
