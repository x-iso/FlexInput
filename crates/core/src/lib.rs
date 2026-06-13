pub mod automap;
pub mod module;
pub mod patch;
pub mod signal;

pub use module::{Module, ModuleDescriptor, ModuleFactory, ModuleRegistration, PinDescriptor};
pub use patch::{NodeInstance, Patch, SubPatch, SubPatchPin, Wire, PATCH_VERSION};
pub use signal::{Signal, SignalType};

/// Suffix appended to a FlexInput-emulated virtual controller's **USB product
/// string** (e.g. `Wireless Controller (FlexInput)`). It lets the input
/// enumerator recognize FlexInput's own emulated pad and tell it apart from a
/// real controller with the same VID/PID — Windows/gilrs expose no per-instance
/// device path over WindowsGamingInput.
///
/// The product string is the discriminator because it is the *only* device-
/// supplied field gilrs's WGI backend surfaces: `gilrs::Gamepad::name()` comes
/// from `RawGameController.DisplayName()` (the HID product string), while
/// `uuid()` is `Uuid::nil()` for every WGI gamepad and there is no instance-id
/// accessor. The Windows FriendlyName / DeviceDesc are *not* read by WGI, so a
/// marker there never reaches the enumerator (this was the first, failed
/// approach). Games that show a controller name will see the suffix; most show a
/// generic glyph, and VID/PID are unchanged so PS-native feature detection
/// (lightbar, triggers, touchpad) is unaffected.
///
/// Lives in `core` so both `flexinput-hidmaestro` (which writes it onto the
/// device's product string) and `flexinput-devices` (which detects it during
/// enumeration via `pad.name()`) share one definition. Keep the leading space.
pub const VIRTUAL_DEVICE_NAME_MARKER: &str = " (FlexInput)";
