pub mod automap;
pub mod module;
pub mod patch;
pub mod signal;

pub use module::{Module, ModuleDescriptor, ModuleFactory, ModuleRegistration, PinDescriptor};
pub use patch::{NodeInstance, Patch, SubPatch, SubPatchPin, Wire, PATCH_VERSION};
pub use signal::{Signal, SignalType};

/// Suffix appended to a FlexInput-emulated virtual controller's Windows naming
/// (FriendlyName / DeviceDesc / BusReportedDeviceDesc). It lets the input
/// enumerator recognize FlexInput's own emulated pad and tell it apart from a
/// real controller with the same VID/PID — Windows/gilrs expose no per-instance
/// device path over WindowsGamingInput, so the name is the only reliable
/// discriminator. The USB *product* string is intentionally left unmarked so
/// games still see a faithful "Wireless Controller".
///
/// Lives in `core` so both `flexinput-hidmaestro` (which writes it onto the
/// device node) and `flexinput-devices` (which detects it during enumeration)
/// share one definition. Keep the leading space.
pub const VIRTUAL_DEVICE_NAME_MARKER: &str = " (FlexInput)";
