//! Which of this process's threads the CPU time goes to, from `/proc` (Linux),
//! for Settings → Performance. Threads sharing a name are summed (the driver's
//! and the runtimes' worker pools). Sampled at most once a second.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One thread name's share.
#[derive(Clone)]
pub(crate) struct ThreadCpu {
    pub name: String,
    /// Percent of one core over the last sample.
    pub percent: f32,
    /// Threads with this name.
    pub count: usize,
    /// The lowest (highest-priority) nice level among them.
    pub nice: i32,
}

/// `/proc`'s time unit: USER_HZ, 100 on every architecture FlexInput builds for.
const TICKS_PER_SEC: f32 = 100.0;

struct Sample {
    at: Instant,
    /// tid → (name, utime + stime in ticks, nice)
    threads: HashMap<u32, (String, u64, i32)>,
    rows: Vec<ThreadCpu>,
}

/// The busiest thread names over the last second or so, busiest first. Empty
/// on the first call (a rate needs two samples).
pub(crate) fn snapshot() -> Vec<ThreadCpu> {
    static LAST: Mutex<Option<Sample>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = last.as_ref() {
        if s.at.elapsed() < Duration::from_secs(1) {
            return s.rows.clone();
        }
    }
    let at = Instant::now();
    let threads = read_threads();
    let rows = last.as_ref().map(|prev| rows(prev, &threads, at)).unwrap_or_default();
    *last = Some(Sample { at, threads, rows: rows.clone() });
    rows
}

fn rows(prev: &Sample, now: &HashMap<u32, (String, u64, i32)>, at: Instant) -> Vec<ThreadCpu> {
    let secs = at.duration_since(prev.at).as_secs_f32().max(1e-3);
    let mut by_name: HashMap<&str, ThreadCpu> = HashMap::new();
    for (tid, (name, ticks, nice)) in now {
        // A thread born since the last sample counts from zero.
        let before = prev.threads.get(tid).map_or(0, |t| t.1);
        let percent = ticks.saturating_sub(before) as f32 / TICKS_PER_SEC / secs * 100.0;
        let row = by_name.entry(name).or_insert_with(|| ThreadCpu {
            name: name.clone(),
            percent: 0.0,
            count: 0,
            nice: *nice,
        });
        row.percent += percent;
        row.count += 1;
        row.nice = row.nice.min(*nice);
    }
    let mut rows: Vec<ThreadCpu> = by_name.into_values().collect();
    rows.sort_by(|a, b| b.percent.total_cmp(&a.percent).then_with(|| a.name.cmp(&b.name)));
    rows
}

fn read_threads() -> HashMap<u32, (String, u64, i32)> {
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else { return out };
    for entry in dir.flatten() {
        let Some(tid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if let Some(t) = std::fs::read_to_string(entry.path().join("stat")).ok().and_then(|s| parse_stat(&s)) {
            out.insert(tid, t);
        }
    }
    out
}

/// A `/proc/<pid>/task/<tid>/stat` line: `tid (name) state …`, utime and stime
/// the 14th and 15th fields, nice the 19th. The name can hold spaces and
/// parentheses, so it runs to the LAST `)`.
fn parse_stat(line: &str) -> Option<(String, u64, i32)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let name = line.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
    // rest[0] is field 3 (state).
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    let nice: i32 = rest.get(16)?.parse().ok()?;
    Some((name, utime + stime, nice))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_lines_parse_with_awkward_names() {
        let line = "4242 (my (odd) thread) S 1 4242 4242 0 -1 4194368 120 0 0 0 37 5 0 0 20 -15 1 0 900 0 0";
        assert_eq!(parse_stat(line), Some(("my (odd) thread".into(), 42, -15)));
        assert_eq!(parse_stat("1 (x) S 1"), None, "too short");
    }

    #[test]
    fn rates_come_from_two_samples_and_group_by_name() {
        let t = |name: &str, ticks, nice| (name.to_string(), ticks, nice);
        let prev = Sample {
            at: Instant::now() - Duration::from_secs(2),
            threads: HashMap::from([(1, t("device-io", 100, -15)), (2, t("worker", 10, 0))]),
            rows: Vec::new(),
        };
        let now = HashMap::from([(1, t("device-io", 160, -15)), (2, t("worker", 30, 0)), (3, t("worker", 20, 0))]);
        let rows = rows(&prev, &now, prev.at + Duration::from_secs(2));
        assert_eq!(rows[0].name, "device-io");
        assert!((rows[0].percent - 30.0).abs() < 0.01, "60 ticks over 2 s");
        assert_eq!((rows[1].name.as_str(), rows[1].count), ("worker", 2));
        assert!((rows[1].percent - 20.0).abs() < 0.01, "20 + 20 ticks over 2 s");
        assert_eq!(rows[0].nice, -15);
    }

    #[test]
    fn this_process_reads_back() {
        let threads = read_threads();
        assert!(!threads.is_empty(), "/proc/self/task lists at least this thread");
    }
}
