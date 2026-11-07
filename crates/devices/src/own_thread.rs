//! A device backend on a thread of its own: see [`OwnThread`].

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use flexinput_core::raw_pad::RawPadProfiles;
use flexinput_core::Signal;

use crate::{DeviceBackend, PhysicalDevice};

/// How long the I/O loop waits for a poll before going on without it: far past
/// a normal one, well short of a stall.
const POLL_WAIT: Duration = Duration::from_millis(2);
/// And for the device list, which may open a device.
const ENUMERATE_WAIT: Duration = Duration::from_millis(20);
/// Sends held while the backend is busy, past which the oldest half goes.
const MAX_HELD_SENDS: usize = 4096;

type Signals = Vec<(String, String, Signal)>;

enum Job {
    Poll,
    Enumerate,
    Sends(Signals),
    SpikeFilter(String, bool, f32, u8),
    GyroDrift(String, Option<[f32; 3]>),
    RawPadProfiles(Arc<RawPadProfiles>),
    SdlAllPads(bool),
}

/// What the backend's thread hands back.
#[derive(Default)]
struct Answers {
    /// Polls answered, and the latest answer until it is taken.
    polls: u64,
    polled: Option<Signals>,
    /// A copy of the latest answer, for a poll that goes on without one.
    last: Signals,
    enumerations: u64,
    devices: Option<Vec<PhysicalDevice>>,
    counts: HashMap<String, u32>,
}

struct Shared {
    answers: Mutex<Answers>,
    ready: Condvar,
}

/// A backend whose calls can block for seconds inside a library — SDL's update
/// waits on a Steam Controller's puck for a feature report with no timeout —
/// on a thread of its own, so only its own pads wait, not the I/O loop and
/// every other device with it.
///
/// A poll waits for the backend's answer briefly. Past that it goes on with the
/// last answer, its motion stopped so a held gyro can't keep turning, and takes
/// the late answer on the next poll, so no press within it is lost. Sends
/// gather through an output pass and go over together at its end; settings go
/// over only when they change.
pub struct OwnThread {
    name: &'static str,
    /// The device ids this backend owns.
    prefix: &'static str,
    jobs: mpsc::Sender<Job>,
    shared: Arc<Shared>,
    polls_asked: u64,
    enumerations_asked: u64,
    devices: Vec<PhysicalDevice>,
    sends: Signals,
    // What was last handed over.
    spike: HashMap<String, (bool, f32, u8)>,
    drift: HashMap<String, Option<[f32; 3]>>,
    profiles: Option<Arc<RawPadProfiles>>,
    all_pads: Option<bool>,
}

impl OwnThread {
    /// `backend` on a thread named after it, owning the device ids under
    /// `prefix`. `raise` lifts the thread as the I/O loop's (on Linux; Windows
    /// raises it here), since its pads' input feeds that loop.
    pub fn spawn<B: DeviceBackend + 'static>(backend: B, prefix: &'static str, raise: fn(&str)) -> Self {
        let name = backend.name();
        let shared = Arc::new(Shared { answers: Mutex::new(Answers::default()), ready: Condvar::new() });
        let (jobs, rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name(format!("{name} input"))
            .spawn(move || {
                raise_priority(name, raise);
                run(backend, rx, thread_shared);
            });
        if let Err(e) = spawned {
            eprintln!("[{name}] no thread of its own ({e}): its pads stay silent");
        }
        Self {
            name,
            prefix,
            jobs,
            shared,
            polls_asked: 0,
            enumerations_asked: 0,
            devices: Vec::new(),
            sends: Vec::new(),
            spike: HashMap::new(),
            drift: HashMap::new(),
            profiles: None,
            all_pads: None,
        }
    }
}

/// The backend's answers, locked: a free function so the guard borrows only
/// `shared`, and the caller's other fields stay free.
fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Answers> {
    shared.answers.lock().unwrap_or_else(|e| e.into_inner())
}

/// The input threads run above everything else (see the I/O loop's spawn).
fn raise_priority(name: &str, raise: fn(&str)) {
    #[cfg(windows)]
    // SAFETY: plain Win32 calls on the current thread's pseudo-handle.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    raise(name);
}

fn run(mut backend: impl DeviceBackend, jobs: mpsc::Receiver<Job>, shared: Arc<Shared>) {
    let mut copy = Signals::new();
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Poll => {
                let out = backend.poll();
                let counts = backend.take_event_counts();
                copy_signals(&mut copy, &out);
                let mut a = lock(&shared);
                std::mem::swap(&mut a.last, &mut copy);
                a.polled = Some(out);
                a.polls += 1;
                for (dev, n) in counts {
                    *a.counts.entry(dev).or_insert(0) += n;
                }
                drop(a);
                shared.ready.notify_all();
            }
            Job::Enumerate => {
                let devices = backend.enumerate();
                let mut a = lock(&shared);
                a.devices = Some(devices);
                a.enumerations += 1;
                drop(a);
                shared.ready.notify_all();
            }
            Job::Sends(sends) => {
                for (dev, pin, signal) in sends {
                    backend.send(&dev, &pin, signal);
                }
                backend.end_output_pass();
            }
            Job::SpikeFilter(dev, on, sensitivity, window) => backend.set_spike_filter(&dev, on, sensitivity, window),
            Job::GyroDrift(dev, drift) => backend.set_gyro_drift(&dev, drift),
            Job::RawPadProfiles(profiles) => backend.set_raw_pad_profiles(&profiles),
            Job::SdlAllPads(on) => backend.set_sdl_all_pads(on),
        }
    }
}

/// `src` into `dst`, reusing `dst`'s strings where it has them.
fn copy_signals(dst: &mut Signals, src: &Signals) {
    dst.truncate(src.len());
    for (d, s) in dst.iter_mut().zip(src) {
        d.0.clone_from(&s.0);
        d.1.clone_from(&s.1);
        d.2 = s.2;
    }
    let have = dst.len();
    dst.extend(src[have..].iter().cloned());
}

/// The last answer again, for a poll that goes on without a new one: whatever
/// was held stays held, but a rate stops, or a held turn would keep turning.
fn stalled(last: &Signals) -> Signals {
    last.iter()
        .map(|(dev, pin, signal)| {
            let signal = if pin.starts_with("gyro") { Signal::Float(0.0) } else { *signal };
            (dev.clone(), pin.clone(), signal)
        })
        .collect()
}

impl DeviceBackend for OwnThread {
    fn name(&self) -> &'static str {
        self.name
    }

    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        let mut a = lock(&self.shared);
        if let Some(devices) = a.devices.take() {
            self.devices = devices;
        }
        if a.enumerations == self.enumerations_asked && self.jobs.send(Job::Enumerate).is_ok() {
            self.enumerations_asked += 1;
            let deadline = Instant::now() + ENUMERATE_WAIT;
            while a.enumerations < self.enumerations_asked {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                a = self.shared.ready.wait_timeout(a, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            if let Some(devices) = a.devices.take() {
                self.devices = devices;
            }
        }
        self.devices.clone()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        let mut a = lock(&self.shared);
        if a.polls < self.polls_asked {
            // Still on an earlier poll: go on without it, and without waiting.
            return stalled(&a.last);
        }
        if let Some(late) = a.polled.take() {
            // The answer to the poll that went on without it.
            return late;
        }
        if self.jobs.send(Job::Poll).is_err() {
            return Vec::new();
        }
        self.polls_asked += 1;
        let deadline = Instant::now() + POLL_WAIT;
        while a.polls < self.polls_asked {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return stalled(&a.last);
            }
            a = self.shared.ready.wait_timeout(a, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        a.polled.take().unwrap_or_default()
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        if !device_id.starts_with(self.prefix) {
            return;
        }
        if self.sends.len() >= MAX_HELD_SENDS {
            self.sends.drain(..MAX_HELD_SENDS / 2);
        }
        self.sends.push((device_id.to_owned(), pin_id.to_owned(), signal));
    }

    fn end_output_pass(&mut self) {
        if self.sends.is_empty() {
            return;
        }
        // Busy on a poll: hold them until it's back.
        if lock(&self.shared).polls < self.polls_asked {
            return;
        }
        let _ = self.jobs.send(Job::Sends(std::mem::take(&mut self.sends)));
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        lock(&self.shared).counts.drain().collect()
    }

    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32, window: u8) {
        let v = (enabled, sensitivity_pct, window);
        if !device_id.starts_with(self.prefix) || self.spike.get(device_id) == Some(&v) {
            return;
        }
        self.spike.insert(device_id.to_owned(), v);
        let _ = self.jobs.send(Job::SpikeFilter(device_id.to_owned(), enabled, sensitivity_pct, window));
    }

    fn set_sdl_all_pads(&mut self, on: bool) {
        if self.all_pads.replace(on) != Some(on) {
            let _ = self.jobs.send(Job::SdlAllPads(on));
        }
    }

    fn set_gyro_drift(&mut self, device_id: &str, drift: Option<[f32; 3]>) {
        if !device_id.starts_with(self.prefix) || self.drift.get(device_id) == Some(&drift) {
            return;
        }
        self.drift.insert(device_id.to_owned(), drift);
        let _ = self.jobs.send(Job::GyroDrift(device_id.to_owned(), drift));
    }

    fn set_raw_pad_profiles(&mut self, profiles: &Arc<RawPadProfiles>) {
        if self.profiles.as_ref().is_some_and(|p| Arc::ptr_eq(p, profiles)) {
            return;
        }
        self.profiles = Some(Arc::clone(profiles));
        let _ = self.jobs.send(Job::RawPadProfiles(Arc::clone(profiles)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers each poll with a press count and a gyro, after `delay`; records sends.
    struct Fake {
        delay: Arc<Mutex<Duration>>,
        polls: u32,
        sent: Arc<Mutex<Vec<String>>>,
    }

    impl DeviceBackend for Fake {
        fn enumerate(&mut self) -> Vec<PhysicalDevice> {
            Vec::new()
        }
        fn poll(&mut self) -> Vec<(String, String, Signal)> {
            std::thread::sleep(*self.delay.lock().unwrap());
            self.polls += 1;
            vec![
                ("fake:0".into(), "polls".into(), Signal::Float(self.polls as f32)),
                ("fake:0".into(), "gyro_x".into(), Signal::Float(1.0)),
            ]
        }
        fn send(&mut self, _: &str, pin_id: &str, _: Signal) {
            self.sent.lock().unwrap().push(pin_id.to_owned());
        }
    }

    fn value(out: &Signals, pin: &str) -> f32 {
        out.iter().find(|(_, p, _)| p == pin).map_or(f32::NAN, |(_, _, s)| s.as_float())
    }

    #[test]
    fn a_slow_poll_goes_on_with_the_last_answer_then_takes_the_late_one() {
        let delay = Arc::new(Mutex::new(Duration::ZERO));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake { delay: Arc::clone(&delay), polls: 0, sent: Arc::clone(&sent) };
        let mut b = OwnThread::spawn(fake, "fake:", |_| {});
        // Polls until one answers with motion: a fresh or late answer, however
        // slow the machine running the test.
        let answered = |b: &mut OwnThread| {
            for _ in 0..200 {
                let out = b.poll();
                if value(&out, "gyro_x") == 1.0 {
                    return value(&out, "polls");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("no answer");
        };
        let n = answered(&mut b);

        *delay.lock().unwrap() = Duration::from_millis(60);
        let started = Instant::now();
        let stale = b.poll();
        assert!(started.elapsed() < Duration::from_millis(30), "didn't wait out the stall");
        assert_eq!(value(&stale, "polls"), n, "the last answer");
        assert_eq!(value(&stale, "gyro_x"), 0.0, "its motion stopped");
        let started = Instant::now();
        assert_eq!(value(&b.poll(), "polls"), n, "still busy: the last answer");
        assert!(started.elapsed() < Duration::from_millis(30), "without waiting");
        *delay.lock().unwrap() = Duration::ZERO;
        std::thread::sleep(Duration::from_millis(100));
        let late = b.poll();
        assert_eq!(value(&late, "polls"), n + 1.0, "the late answer, whole");
        assert_eq!(value(&late, "gyro_x"), 1.0);

        // Sends for its own ids go over at the end of the pass, before the
        // next poll.
        b.send("fake:0", "rumble", Signal::Float(1.0));
        b.send("other:0", "rumble", Signal::Float(1.0));
        b.end_output_pass();
        answered(&mut b);
        assert_eq!(*sent.lock().unwrap(), ["rumble"]);
    }
}
