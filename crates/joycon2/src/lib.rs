//! Joy-Con 2 support over Bluetooth LE.
//!
//! Covers Nintendo's Joy-Con 2 and third-party controllers that speak the same
//! protocol (the Mobapad M12-S, for example). Pro Controller 2 and the NSO
//! GameCube pad share the transport but use different report characteristics
//! and layouts; they are deliberately out of scope here.
//!
//! # Why this exists at all
//!
//! Switch 2 controllers are Bluetooth LE but implement none of the standard
//! profiles. There is no HID-over-GATT service, so Windows binds no driver and
//! the pad never appears to `hidapi`, gilrs, SDL, or XInput. Worse, they do not
//! implement SMP: a host that runs the normal LE pairing flow — which the
//! Windows "Add a device" wizard does — gets disconnected. The only way in is
//! to act as a plain GATT central against Nintendo's vendor service and speak
//! their command protocol.
//!
//! A useful side effect: because Windows never sees a gamepad here, there is no
//! phantom pad to suppress and HidHide is not involved.
//!
//! # Shape
//!
//! [`Joycon2Hub`] owns one thread running a current-thread tokio runtime. All
//! of its public API is synchronous and non-blocking so FlexInput's device-I/O
//! loop can call it every tick without ever waiting on Bluetooth.
//!
//! Protocol reference: <https://github.com/ndeadly/switch2_controller_research>

pub mod dongle;
pub mod host_addr;
pub mod hub;
pub mod pairing;
pub mod protocol;
pub mod reports;
pub mod usb;
#[cfg(windows)]
pub mod win_pair;

pub use hub::{Joycon2Hub, PadKey, PadState};
pub use dongle::Joycon2DongleHub;
pub use usb::Joycon2UsbHub;
pub use protocol::{Side, NINTENDO_VID, PID_JOYCON2_L, PID_JOYCON2_R};
pub use reports::{Buttons, Motion, Mouse, PadSnapshot, Power, StickCalib};
