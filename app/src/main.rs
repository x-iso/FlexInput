#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Installs a panic hook that intercepts an unrecoverable GPU panic and
/// relaunches FlexInput instead of dumping a Rust backtrace.
///
/// When a fullscreen game resets the GPU (driver TDR / exclusive-fullscreen
/// mode switch), our device can be lost. The vendored egui-wgpu (see
/// `vendor/egui-wgpu/src/renderer.rs`) normally catches this case, raises the
/// `GPU_LOST` flag instead of panicking, and `FlexInputApp::update` then saves
/// state and relaunches gracefully. This hook is the *last-ditch net* for any
/// device-loss path that still reaches a panic — older "Failed to create
/// staging buffer" messages, or a panic raised from a context our flag check
/// hasn't run in yet.
///
/// We pair both with `PowerPreference::LowPower` (see `native_options` below)
/// so the overlay lives on the integrated GPU and is not caught in the game's
/// device reset in the first place — that removes the root cause for the common
/// case. The user's latest work is already on disk via the always-on recovery
/// snapshot, so the relaunched instance restores it seamlessly.
fn install_gpu_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        // Source location of the panic (file:line:col), when the std runtime
        // provides it — included in the AppData crash log for diagnosis.
        let location = info
            .location()
            .map(|l| format!(" at {}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        // Device-reset signatures. When another app (usually a fullscreen game)
        // resets the graphics device, wgpu objects created against the old
        // device become invalid; the next frame's egui texture upload then
        // panics from a context our in-renderer GPU_LOST guard didn't cover.
        // Match those signatures here so the *last-ditch* net still relaunches
        // instead of dumping a backtrace.
        //   - "Failed to create staging buffer" / "GPU device lost": older paths.
        //   - egui_texid_Managed(N) ... is invalid: a managed egui texture
        //     whose backing texture was orphaned by the reset (seen as
        //     `Texture::create_view` / bind-group validation errors).
        //   - "Device is lost" / "device was lost": wgpu's own DeviceLost text.
        // We intentionally do NOT treat every "Validation Error" as device
        // loss — only ones carrying a managed-egui-texture-invalid or
        // device-lost signature — so a genuine logic bug can't loop-relaunch.
        // OpenGL/WGL context loss — the signature seen on wake from sleep or
        // hibernation (and on a display-topology change) when the OpenGL backend
        // is active, which on this machine is the Auto pick for AMD. wgpu-hal's
        // GL backend hard-`unwrap()`s the `wglMakeCurrent` result inside
        // `AdapterContext::lock()` (src/gles/{device,wgl}.rs); once the context
        // is invalidated on wake that unwrap panics with a raw Windows error
        // whose *text* is localized ("The operation completed successfully.",
        // HRESULT 0) and can't be matched — but the panic's *source location* is
        // stable (`#[track_caller]` points it at the wgpu-hal `gles` module).
        // A fresh process rebuilds the GL context cleanly, so treat it as
        // recoverable device loss.
        //
        // Relaunching from the hook is also what prevents the abort the user
        // hit: the hook runs BEFORE unwinding and the relaunch helpers
        // `process::exit` without running drops. If we instead let unwinding
        // proceed, the Painter/Queue drop calls `queue.wait()` →
        // `AdapterContext::lock().unwrap()` a SECOND time, and the double-panic
        // aborts the process with STATUS_STACK_BUFFER_OVERRUN (0xc0000409).
        let gl_context_lost =
            location.contains("wgpu-hal") && location.contains("gles");

        let gpu_lost = gl_context_lost
            || msg.contains("Failed to create staging buffer")
            || msg.contains("GPU device lost")
            || msg.contains("Device is lost")
            || msg.contains("device was lost")
            || (msg.contains("egui_texid_Managed") && msg.contains("is invalid"));

        // Monitor hot-plug: when a display disconnects/reconnects (e.g. a monitor
        // briefly drops, a KVM switch, or a GPU re-scan), winit's monitor
        // enumeration can `unwrap()` an `Invalid monitor handle` (Win32 err 1461)
        // on a stale `HMONITOR` from inside its own event loop — a context we
        // can't wrap in `catch_unwind`. It's transient: a fresh process
        // re-enumerates monitors cleanly. So relaunch + restore from the recovery
        // snapshot rather than dying. Match the OS error text (stable across winit
        // patch versions) and, defensively, the winit monitor source path.
        let monitor_lost = msg.contains("Invalid monitor handle")
            || (msg.contains("monitor.rs") && msg.contains("1461"));
        if monitor_lost && !gpu_lost {
            let note = "A display was disconnected/reconnected and the windowing \
                 layer hit an invalid monitor handle. FlexInput is relaunching to \
                 recover; your patch is restored from the crash-recovery snapshot.";
            eprintln!("{note}");
            flexinput_ui::log_crash(
                "monitor-lost (relaunching)",
                &format!("{note}\n\npanic: {msg}{location}"),
            );
            // Unlike GPU loss, no game owns the device here — a plain relaunch is
            // correct (the fresh process enumerates monitors fine and rebuilds the
            // window immediately).
            flexinput_ui::relaunch_self_and_exit();
        }

        if gpu_lost {
            // The release build is `windows_subsystem = "windows"` with no
            // logger surfaced to a console, so drop a breadcrumb in AppData
            // (next to settings.json) for the user/support to see why FlexInput
            // blinked mid-game.
            let note = "GPU device/context was lost (a fullscreen game reset the \
                 graphics device, or the system woke from sleep/hibernation and the \
                 OpenGL context was invalidated). FlexInput is relaunching to recover; \
                 your patch is restored from the crash-recovery snapshot.";
            eprintln!("{note}");
            flexinput_ui::log_crash(
                "gpu-lost (relaunching)",
                &format!("{note}\n\npanic: {msg}{location}"),
            );
            // Renderer cascade (Auto only): a panic here means the device was
            // lost at RUNTIME (init failures surface as a `run_native` Err, not a
            // panic — handled in `main`). So re-assert the CURRENT backend for
            // the recovery boot: a fresh process usually re-inits the same
            // backend fine after a transient loss (game TDR, wake). If it can't,
            // that child's own init-Err path advances the cascade. We never
            // advance from here, so a recoverable blip can't silently demote a
            // healthy backend (e.g. DX12 → GL).
            use std::sync::atomic::Ordering;
            if RENDER_AUTO.load(Ordering::Relaxed) {
                flexinput_ui::write_render_attempt(RENDER_IDX.load(Ordering::Relaxed));
            }
            // Spawn a fresh instance and exit this dead-GPU process. The
            // recovery snapshot the child restores from was written by the
            // app's autosave-on-settle (and forced on the GPU_LOST path); we
            // do NOT delete it here — the child consumes it on boot.
            //
            // If we were NOT the foreground window, a game owns the GPU. We
            // can't resume a panicked frame, so we must relaunch — but the
            // fresh process must not render against the game-held device (it'd
            // lose it again and loop). Boot it straight into the GUI stall; it
            // keeps input/engine alive and rebuilds the UI once FlexInput is
            // foreground again.
            #[cfg(windows)]
            {
                if flexinput_ui::process_list::foreground_exe().is_some() {
                    flexinput_ui::relaunch_self_stalled_and_exit();
                }
            }
            flexinput_ui::relaunch_self_and_exit();
        }
        // Any other panic: we don't relaunch (it's not a known-recoverable
        // device-loss signature), but still leave a durable breadcrumb in
        // AppData before the default hook prints the backtrace and the process
        // unwinds/aborts. Note this does NOT catch native faults like a
        // STATUS_ACCESS_VIOLATION (0xc0000005) — those bypass the panic
        // machinery entirely; the in-renderer GPU_LOST guards are what prevent
        // the device-loss AV from being reached in the first place.
        flexinput_ui::log_crash("panic (unhandled)", &format!("{msg}{location}"));
        default_hook(info);
    }));
}

/// If launched as the elevated HIDMaestro helper (`--hidmaestro-helper`), run
/// the named-pipe server and exit *before* touching the GPU / window. The app
/// re-execs itself with this flag (see `flexinput_hidmaestro::helper`) so a
/// single binary ships instead of a separate helper exe.
///
/// Returns true if this process was the helper (caller should exit).
#[cfg(windows)]
fn run_as_helper_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == flexinput_hidmaestro::helper::HELPER_FLAG) {
        return false;
    }
    let mut parent_pid: Option<u32> = None;
    let mut persist = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--parent-pid" => parent_pid = it.next().and_then(|s| s.parse().ok()),
            "--persist" => persist = true,
            _ => {}
        }
    }
    flexinput_hidmaestro::run_helper_server(parent_pid, persist);
    true
}

#[cfg(not(windows))]
fn run_as_helper_if_requested() -> bool {
    false
}

/// Ordered backend cascade for `RendererChoice::Auto`. Startup tries element
/// `[0]`; if that backend fails to *initialize* (surface/device creation errors
/// out of `run_native`), the process advances to the next element and relaunches
/// (see `main` and `flexinput_ui::{read,write,clear}_render_attempt`).
///
/// AMD-on-Windows leads with **DX12**: it's the only AMD backend that both
/// composites the transparent window AND survives sleep/wake. Their Vulkan
/// win32 surface is opaque-only (no see-through), and their GL/WGL context is
/// invalidated on wake — wgpu-hal then hard-`unwrap()`s `wglMakeCurrent` and the
/// process aborts (the crash this cascade exists to escape). Order for AMD:
/// DX12 (transparency + wake-safe) → Vulkan (stable, opaque) → GL (transparency
/// but wake-crash) as the last resort. NVIDIA/Intel keep Vulkan first (healthy
/// there, transparency included) with DX12/GL as fallbacks.
///
/// The Vulkan probe below only enumerates adapters (vendor id) — it never
/// creates a surface, so it's cheap and safe even when Vulkan is the flaky path.
/// `WGPU_BACKEND` and a non-Auto Settings choice both bypass this entirely.
#[cfg(windows)]
fn auto_cascade() -> Vec<wgpu::Backends> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN);
    // Emulate the HighPerformance default: a discrete GPU wins over integrated.
    let pick = adapters
        .iter()
        .find(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
        .or_else(|| adapters.first());
    const VENDOR_AMD: u32 = 0x1002;
    let is_amd = pick.map(|a| a.get_info().vendor == VENDOR_AMD).unwrap_or(false);
    if is_amd {
        if let Some(a) = pick {
            eprintln!("[gpu] auto: AMD adapter \"{}\" — cascade DX12 → Vulkan → GL", a.get_info().name);
        }
        vec![wgpu::Backends::DX12, wgpu::Backends::VULKAN, wgpu::Backends::GL]
    } else {
        vec![wgpu::Backends::VULKAN, wgpu::Backends::DX12, wgpu::Backends::GL]
    }
}

#[cfg(not(windows))]
fn auto_cascade() -> Vec<wgpu::Backends> {
    vec![wgpu::Backends::PRIMARY | wgpu::Backends::GL]
}

// Renderer-cascade state, set once in `main` after backend selection and read by
// the panic hook. `RENDER_AUTO` gates all of it — an explicit Settings/env
// backend never cascades (it relaunches to itself). `RENDER_IDX`/`RENDER_LEN`
// are the current cascade position and length.
static RENDER_AUTO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDER_IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static RENDER_LEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

#[cfg(not(windows))]
fn auto_backends() -> wgpu::Backends {
    wgpu::Backends::PRIMARY | wgpu::Backends::GL
}

fn main() -> eframe::Result<()> {
    // Helper mode short-circuits everything else (no GPU, no window).
    if run_as_helper_if_requested() {
        return Ok(());
    }

    install_gpu_panic_hook();

    // Route `log` records from wgpu/winit/egui to stderr. warn+ by default —
    // wgpu reports swapchain/DComp failures ONLY through `log::error!`, and
    // without a logger they vanish (that silence hid the overlay-viewport
    // "white ghost" swapchain bug). `RUST_LOG` overrides for deeper digging.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn"),
    )
    .format_timestamp_millis()
    .try_init();

    // Window / taskbar icon — decoded from the pre-baked 256px logo PNG
    // (rendered from icon_v2.svg). Decoding is instant; rasterizing the
    // source SVG at 256px takes ~45s and was stalling startup.
    let icon = flexinput_ui::render_app_icon().expect("bundled app icon PNG is valid");

    // Transparent viewport is enabled at startup so the runtime "see-through"
    // toggle (eye icon next to the zoom controls) can show whatever is behind
    // FlexInput. With `with_transparent(true)` the compositor allocates an
    // RGBA surface; whether anything bleeds through is controlled per-frame
    // by the alpha values in panel/window fills (see
    // `settings::apply_theme_and_contrast`).
    // GPU selection: use eframe's default (`PowerPreference::HighPerformance`),
    // i.e. the discrete GPU on a dual-GPU machine.
    //
    // We previously forced `LowPower` to dodge a known crash: when a fullscreen
    // game on the discrete GPU triggers a driver reset (TDR) or an exclusive-
    // fullscreen mode switch, our device is lost and egui-wgpu used to `panic!`
    // the whole process on the next frame. That root-cause avoidance is no
    // longer necessary — the vendored egui-wgpu now turns device loss into a
    // skipped frame + `GPU_LOST` flag, and FlexInput saves its workspace and
    // relaunches cleanly (see `install_gpu_panic_hook`, the `GPU_LOST` check in
    // `FlexInputApp::update`, and `vendor/egui-wgpu/src/renderer.rs`).
    //
    // Running on the discrete GPU deliberately keeps us in the device-reset
    // blast radius, which is the worst case for the recovery path — the best
    // way to keep that path honest. The plain default config preserves
    // `present_mode: AutoVsync` and the surface-error handler.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    // Backend pick, priority: `WGPU_BACKEND` env (dev escape hatch, matches
    // plain wgpu behavior) > Settings → Renderer > Auto cascade (see
    // `auto_cascade`). Only Auto cascades on init failure; an explicit env or
    // Settings backend is used verbatim and, on a GPU-recovery relaunch, retries
    // itself. When Auto is active we record the cascade position in RENDER_* for
    // the panic hook and the `run_native` init-failure handler below.
    use std::sync::atomic::Ordering;
    let backends = match wgpu::Backends::from_env() {
        Some(env_backends) => env_backends,
        None => match flexinput_ui::startup_renderer_choice() {
            flexinput_ui::RendererChoice::Vulkan => wgpu::Backends::VULKAN,
            flexinput_ui::RendererChoice::OpenGl => wgpu::Backends::GL,
            flexinput_ui::RendererChoice::Dx12 => wgpu::Backends::DX12,
            flexinput_ui::RendererChoice::Auto => {
                let cascade = auto_cascade();
                // Only a GPU-recovery relaunch inherits the cascade position; a
                // fresh (manual) launch always restarts from the preferred
                // backend and clears any stale marker.
                let recovery = std::env::var(flexinput_ui::GPU_RECOVERY_ENV).is_ok();
                let attempt = if recovery {
                    flexinput_ui::read_render_attempt().unwrap_or(0)
                } else {
                    flexinput_ui::clear_render_attempt();
                    0
                };
                let idx = attempt.min(cascade.len() - 1);
                // Persist the current position so a subsequent relaunch (panic
                // hook = retry same; init-Err below = advance) starts from here.
                flexinput_ui::write_render_attempt(idx);
                RENDER_AUTO.store(true, Ordering::Relaxed);
                RENDER_IDX.store(idx, Ordering::Relaxed);
                RENDER_LEN.store(cascade.len(), Ordering::Relaxed);
                eprintln!("[gpu] auto: cascade attempt {}/{} → {:?}", idx + 1, cascade.len(), cascade[idx]);
                cascade[idx]
            }
        },
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = backends;
        // DX12 must present through a DirectComposition visual, not a plain
        // HWND swapchain: the HWND path is opaque-only, while the DComp path
        // supports pre-multiplied per-pixel alpha — the only Windows backend
        // where see-through works on AMD (their Vulkan surface is
        // COMPOSITE_ALPHA_OPAQUE-only). No-op for Vulkan/GL surfaces. Costs
        // RenderDoc capture support on DX12; set
        // WGPU_DX12_PRESENTATION_SYSTEM=Hwnd if a capture is ever needed.
        // (`Dx12SwapchainKind` is missing from wgpu 27's root re-exports;
        // reach through the public `wgt` alias.)
        setup.instance_descriptor.backend_options.dx12.presentation_system =
            wgpu::wgt::Dx12SwapchainKind::from_env()
                .unwrap_or(wgpu::wgt::Dx12SwapchainKind::DxgiFromVisual);
    }
    // Defense-in-depth for device loss. The default handler treats every
    // SurfaceError except `Outdated` as a skipped frame and logs a warning —
    // including `SurfaceError::Lost`, the case where the swapchain *does* report
    // the device went away. Skipping the frame alone isn't enough: the device is
    // gone and the next frame faults again. Raise the same process-global
    // GPU_LOST flag the renderer's buffer-staging guards use, so `update()`
    // sees it and relaunches/stalls. The in-renderer GPU_LOST short-circuit
    // (vendor/egui-wgpu/src/winit.rs) handles the common Windows case where
    // get_current_texture() returns Ok with a stale frame and this handler
    // never fires; this covers the case where it *does* fire.
    wgpu_options.on_surface_error = std::sync::Arc::new(|err| {
        use eframe::egui_wgpu::SurfaceErrorAction;
        match err {
            wgpu::SurfaceError::Outdated => {
                // App minimized on Windows — benign, don't spam the log.
            }
            wgpu::SurfaceError::Lost | wgpu::SurfaceError::OutOfMemory => {
                eframe::egui_wgpu::GPU_LOST
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                eprintln!("Surface error {err:?} — signalling GPU_LOST.");
            }
            _ => eprintln!("Dropped frame with surface error: {err}"),
        }
        SurfaceErrorAction::SkipFrame
    });

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("FlexInput")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(true)
            .with_icon(icon),
        wgpu_options,
        // 32-bit depth buffer for egui's render pass (Depth32Float). egui's own
        // pipeline ignores it (depth_compare: Always, no write), so 2D UI is
        // unaffected — but it lets the 3D Controller viewer's paint callback do
        // real depth testing so parts occlude correctly instead of drawing
        // see-through. Depth is separate from color, so this does not touch the
        // transparent-window / DirectComposition alpha path.
        depth_buffer: 32,
        ..Default::default()
    };

    let result = eframe::run_native(
        "FlexInput",
        native_options,
        Box::new(|cc| Ok(Box::new(flexinput_ui::FlexInputApp::new(cc)))),
    );

    // Renderer cascade — the init-failure half (the panic hook handles runtime
    // loss). `run_native` returns Err when the chosen backend can't stand up its
    // surface/device (e.g. `FailedToCreateSurfaceForAnyBackend`, seen when the GL
    // context can't be created right after wake). In Auto mode, advance to the
    // next backend in the cascade and relaunch; if the cascade is exhausted,
    // clear the marker (so the next manual launch starts fresh) and surface the
    // error. A clean exit clears the marker so the cascade resets to the
    // preferred backend next time.
    match result {
        Ok(()) => {
            if RENDER_AUTO.load(Ordering::Relaxed) {
                flexinput_ui::clear_render_attempt();
            }
            Ok(())
        }
        Err(e) => {
            if RENDER_AUTO.load(Ordering::Relaxed) {
                // Match wgpu/surface/adapter/device init failures. The concrete
                // error is `eframe::Error::Wgpu(..)` here; a Debug substring match
                // is robust across the exact variant wording.
                let s = format!("{e:?}");
                let gpu_init_err = s.contains("Wgpu")
                    || s.contains("Surface")
                    || s.contains("Adapter")
                    || s.contains("Device")
                    || s.contains("Backend");
                let idx = RENDER_IDX.load(Ordering::Relaxed);
                let len = RENDER_LEN.load(Ordering::Relaxed);
                if gpu_init_err && idx + 1 < len {
                    flexinput_ui::write_render_attempt(idx + 1);
                    flexinput_ui::log_crash(
                        "renderer-init-failed (cascading)",
                        &format!("backend attempt {} of {len} failed: {e:?}\nretrying next backend", idx + 1),
                    );
                    eprintln!("[gpu] backend attempt {} failed; cascading to attempt {}", idx + 1, idx + 2);
                    flexinput_ui::relaunch_self_and_exit();
                }
                // Exhausted (or a non-GPU error): reset so a manual relaunch
                // starts from the preferred backend rather than pinning to the
                // last-failed one.
                flexinput_ui::clear_render_attempt();
                if gpu_init_err {
                    flexinput_ui::log_crash(
                        "renderer-init-failed (exhausted)",
                        &format!("all {len} Auto backends failed to initialize; last error: {e:?}"),
                    );
                }
            }
            Err(e)
        }
    }
}
