//! The third-party licence texts behind Settings → Credits → "Third-party
//! licenses".
//!
//! Every entry embeds its licence **verbatim** with `include_str!` — never
//! retyped — so a notice here can't drift from the real text:
//!
//! * vendored dependencies point at the licence file the dependency itself
//!   ships (`vendor/…`, `crates/hidmaestro/driver/LICENSE`);
//! * everything else points at `licenses/`, which holds copies taken from the
//!   published crate source or the upstream repo at the version we build
//!   against (see `licenses/README.md` for where each one came from).
//!
//! Scope is what FlexInput actually **redistributes**: code linked into
//! `flexinput.exe` (including SDL3, which we build from source and link
//! statically), the driver binaries vendored under `crates/hidmaestro/driver`,
//! and the bundled art assets. Tools the user installs separately — HidHide,
//! ViGEmBus — are not redistributed and so are not listed here.

/// One entry in the licence viewer.
pub struct License {
    /// Display name in the left-hand list.
    pub name: &'static str,
    /// One line on what it is and why its bytes are in the build.
    pub what: &'static str,
    /// SPDX expression as the project declares it. Where a dependency offers a
    /// choice ("MIT OR Apache-2.0") the text below is the option FlexInput
    /// takes, and this string still records the full choice on offer.
    pub spdx: &'static str,
    /// Upstream home page.
    pub url: &'static str,
    /// Extra attribution the bare licence text doesn't carry (CC BY requires
    /// naming authors, for instance).
    pub note: Option<&'static str>,
    /// The licence, verbatim.
    pub text: &'static str,
}

/// Shown first (FlexInput's own licence), then third-party entries grouped
/// roughly as the Credits section names them.
pub const LICENSES: &[License] = &[
    License {
        name: "FlexInput",
        what: "This application.",
        spdx: "MIT",
        url: "https://github.com/x-iso/FlexInput",
        note: None,
        text: include_str!("../../../LICENSE"),
    },
    License {
        name: "HIDMaestro",
        what: "Virtual HID/XInput driver. The signed driver package under \
               crates/hidmaestro/driver is redistributed as-is; the Rust client \
               in crates/hidmaestro is a port of its shared-memory protocol.",
        spdx: "MIT",
        url: "https://github.com/hifihedgehog/HIDMaestro",
        note: Some("Driver binaries from release v1.3.17."),
        text: include_str!("../../../crates/hidmaestro/driver/LICENSE"),
    },
    License {
        name: "egui / eframe",
        what: "The GUI toolkit and its windowing shell — egui, eframe, \
               egui_extras, and the vendored egui-wgpu / egui-winit forks.",
        spdx: "MIT OR Apache-2.0",
        url: "https://github.com/emilk/egui",
        note: None,
        text: include_str!("../../../licenses/egui.txt"),
    },
    License {
        name: "egui-snarl",
        what: "The node-graph canvas widget. Vendored under vendor/egui-snarl.",
        spdx: "MIT OR Apache-2.0",
        url: "https://github.com/zakarumych/egui-snarl",
        note: None,
        text: include_str!("../../../vendor/egui-snarl/LICENSE-MIT"),
    },
    License {
        name: "btleplug",
        what: "Cross-platform Bluetooth LE. Vendored under vendor/btleplug with \
               a WinRT connection-keepalive fix.",
        spdx: "BSD-3-Clause",
        url: "https://github.com/deviceplug/btleplug",
        note: None,
        text: include_str!("../../../vendor/btleplug/LICENSE.md"),
    },
    License {
        name: "gilrs",
        what: "Gamepad input backend.",
        spdx: "Apache-2.0 OR MIT",
        url: "https://gitlab.com/gilrs-project/gilrs",
        note: None,
        text: include_str!("../../../licenses/gilrs.txt"),
    },
    License {
        name: "SDL3",
        what: "Second gamepad backend, for controllers FlexInput doesn't parse \
               natively. Built from source and linked statically, so SDL itself \
               ships inside flexinput.exe.",
        spdx: "Zlib",
        url: "https://github.com/libsdl-org/SDL",
        note: Some("Bundled version: SDL 3.4.10."),
        text: include_str!("../../../licenses/SDL.txt"),
    },
    License {
        name: "sdl3 (Rust bindings)",
        what: "Safe Rust wrapper over SDL3.",
        spdx: "MIT",
        url: "https://github.com/vhspace/sdl3-rs",
        note: None,
        text: include_str!("../../../licenses/sdl3.txt"),
    },
    License {
        name: "sdl3-sys",
        what: "Raw FFI bindings to SDL3, and the build script that compiles it.",
        spdx: "Zlib",
        url: "https://github.com/maia-s/sdl3-sys-rs",
        note: None,
        text: include_str!("../../../licenses/sdl3-sys.txt"),
    },
    License {
        name: "midir",
        what: "MIDI input and output.",
        spdx: "MIT",
        url: "https://github.com/Boddlnagg/midir",
        note: None,
        text: include_str!("../../../licenses/midir.txt"),
    },
    License {
        name: "rfd",
        what: "Native file open/save dialogs.",
        spdx: "MIT",
        url: "https://github.com/PolyMeilex/rfd",
        note: None,
        text: include_str!("../../../licenses/rfd.txt"),
    },
    License {
        name: "serde",
        what: "Serialization for patches, profiles and settings.",
        spdx: "MIT OR Apache-2.0",
        url: "https://github.com/serde-rs/serde",
        note: None,
        text: include_str!("../../../licenses/serde.txt"),
    },
    License {
        name: "enigo",
        what: "Fallback keyboard/mouse synthesis when the HIDMaestro virtual \
               devices aren't available.",
        spdx: "MIT",
        url: "https://github.com/enigo-rs/enigo",
        note: None,
        text: include_str!("../../../licenses/enigo.txt"),
    },
    License {
        name: "3D controller models",
        what: "The controller meshes in app/assets/models, adapted from \
               larfingshnew/3d-controller-overlay.",
        spdx: "MIT",
        url: "https://github.com/larfingshnew/3d-controller-overlay",
        note: None,
        text: include_str!("../../../licenses/3d-controller-overlay.txt"),
    },
    License {
        name: "game-icons.net icons",
        what: "Macro and menu icons in app/assets/general.",
        spdx: "CC-BY-3.0",
        url: "https://game-icons.net/",
        note: Some(
            "CC BY 3.0 requires crediting the authors. The icons bundled here are by:\n\
             Lorc, Delapouite, John Colburn, Felbrigg, John Redman, Carl Olsen, Sbed, \
             PriorBlue, Willdabeast, Viscious Speed, Lord Berandas, Irongamer, HeavenlyDog, \
             Lucas, Faithtoken, Skoll, Andy Meneely, Cathelineau, Kier Heyl, Aussiesim, \
             Sparker, Zeromancer, Rihlsul, Quoting, Guard13007, DarkZaitzev, SpencerDub, \
             GeneralAce135, Zajkonur, Catsu, Starseeker, Pepijn Poolman, Pierre Leducq, \
             Caro Asercion.\n\
             Each icon names its author on its game-icons.net page.",
        ),
        text: include_str!("../../../licenses/CC-BY-3.0.txt"),
    },
    License {
        name: "Kenney input prompts",
        what: "Button-prompt SVG icons used by the remapper and overlays.",
        spdx: "CC0-1.0",
        url: "https://kenney.nl/assets/input-prompts",
        note: Some("CC0 waives the attribution requirement; credited anyway."),
        text: include_str!("../../../licenses/CC0-1.0.txt"),
    },
];
