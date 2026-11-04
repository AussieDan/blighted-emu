// SPDX-License-Identifier: AGPL-3.0-or-later
//! The resources of the machine the admin runs on (the game machine when the site is served
//! through a relay), for the home page: memory from `/proc/meminfo`, and the processor cores and
//! their use from `/proc/stat` (`GET /api/host`). Linux only: without `/proc` each part is null.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::live::cpu::PROC_DIR;

/// The route of the machine's resources.
pub const HOST_ROUTE: &str = "/api/host";
/// Time between the two processor samples of one answer (our choice: short for the page, long
/// enough for a steady figure).
pub const CPU_SAMPLE_GAP: Duration = Duration::from_millis(250);
const MEMINFO_FILE: &str = "meminfo";
const STAT_FILE: &str = "stat";
const MEMORY_TOTAL: &str = "MemTotal:";
const MEMORY_AVAILABLE: &str = "MemAvailable:";
const BYTES_PER_KIB: u64 = 1_024;
/// The line of all cores together in `/proc/stat`; each core has its own `cpu<N>` line.
const ALL_CORES: &str = "cpu";
/// Fields of a `cpu` line, from `user`: user, nice, system, idle, iowait, irq, softirq, steal.
/// The guest fields after them are already counted in user and nice.
const TIME_FIELDS: usize = 8;
const IDLE_FIELD: usize = 3;
const IOWAIT_FIELD: usize = 4;
const PERCENT: f64 = 100.0;

/// The machine's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Memory {
    /// All of it, in bytes.
    pub total_bytes: u64,
    /// In use: all of it less what programs can still get (`MemAvailable`), in bytes.
    pub used_bytes: u64,
}

/// The time counters of all cores together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Every tick counted.
    pub total: u64,
    /// The ticks not idle and not waiting for a disk.
    pub busy: u64,
}

/// The machine's processor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Processor {
    /// Logical cores.
    pub cores: usize,
    /// Use of all cores together, 0 to 100.
    pub percent: f64,
}

/// What the home page shows.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HostView {
    /// Memory; null without `/proc`.
    pub memory: Option<Memory>,
    /// Processor; null without `/proc`.
    pub cpu: Option<Processor>,
}

/// The memory of a `/proc/meminfo` text.
pub fn parse_meminfo(text: &str) -> Option<Memory> {
    let kib = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value * BYTES_PER_KIB)
    };
    let total_bytes = kib(MEMORY_TOTAL)?;
    let available = kib(MEMORY_AVAILABLE)?;
    Some(Memory {
        total_bytes,
        used_bytes: total_bytes.saturating_sub(available),
    })
}

/// The all-cores counters and the core count of a `/proc/stat` text.
pub fn parse_stat(text: &str) -> Option<(CpuTimes, usize)> {
    let mut times = None;
    let mut cores = 0;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        if name == ALL_CORES {
            let values: Vec<u64> = fields
                .take(TIME_FIELDS)
                .map(|value| value.parse().ok())
                .collect::<Option<_>>()?;
            let total: u64 = values.iter().sum();
            let idle = values.get(IDLE_FIELD)? + values.get(IOWAIT_FIELD).copied().unwrap_or(0);
            times = Some(CpuTimes {
                total,
                busy: total.saturating_sub(idle),
            });
        } else if name
            .strip_prefix(ALL_CORES)
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        {
            cores += 1;
        }
    }
    Some((times?, cores))
}

/// The use of all cores between two samples, 0 to 100; `None` when no tick passed.
pub fn cpu_percent(earlier: CpuTimes, later: CpuTimes) -> Option<f64> {
    let total = later.total.checked_sub(earlier.total)?;
    let busy = later.busy.saturating_sub(earlier.busy);
    (total > 0).then(|| (busy as f64 / total as f64 * PERCENT).clamp(0.0, PERCENT))
}

/// Reads the machine's resources from `proc_dir`, the processor over [`CPU_SAMPLE_GAP`].
pub async fn read_host(proc_dir: &Path) -> HostView {
    let read = |name: &str| std::fs::read_to_string(proc_dir.join(name)).ok();
    let memory = read(MEMINFO_FILE).as_deref().and_then(parse_meminfo);
    let first = read(STAT_FILE).as_deref().and_then(parse_stat);
    let cpu = match first {
        Some((earlier, cores)) => {
            tokio::time::sleep(CPU_SAMPLE_GAP).await;
            read(STAT_FILE)
                .as_deref()
                .and_then(parse_stat)
                .and_then(|(later, _)| cpu_percent(earlier, later))
                .map(|percent| Processor { cores, percent })
        }
        None => None,
    };
    HostView { memory, cpu }
}

/// `GET`: the resources of this machine.
pub(crate) async fn host() -> Response {
    Json(read_host(&PathBuf::from(PROC_DIR)).await).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:       16000000 kB\nMemFree:          350000 kB\n\
        MemAvailable:   10000000 kB\nBuffers:           1000 kB\n";
    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 50 0 25 400 25 0 0 0 0 0\n\
        cpu1 50 0 25 400 25 0 0 0 0 0\nintr 12345\nctxt 999\n";

    #[test]
    fn memory_used_is_total_less_available() {
        let memory = parse_meminfo(MEMINFO).expect("memory");
        assert_eq!(memory.total_bytes, 16_000_000 * BYTES_PER_KIB);
        assert_eq!(memory.used_bytes, 6_000_000 * BYTES_PER_KIB);
        assert_eq!(parse_meminfo("MemTotal: 5 kB\n"), None, "no MemAvailable");
    }

    #[test]
    fn stat_gives_the_all_core_counters_and_counts_each_core() {
        let (times, cores) = parse_stat(STAT).expect("stat");
        assert_eq!(cores, 2);
        assert_eq!(
            times,
            CpuTimes {
                total: 1_000,
                busy: 150
            }
        );
        assert_eq!(parse_stat("intr 1\n"), None, "no cpu line");
    }

    #[test]
    fn cpu_use_is_the_busy_share_of_the_ticks_between_samples() {
        let earlier = CpuTimes {
            total: 1_000,
            busy: 150,
        };
        let later = CpuTimes {
            total: 1_400,
            busy: 250,
        };
        assert_eq!(cpu_percent(earlier, later), Some(25.0));
        assert_eq!(cpu_percent(later, later), None, "no tick passed");
    }

    #[tokio::test]
    async fn a_machine_without_proc_has_no_figures() {
        let dir = tempfile::tempdir().expect("dir");
        let view = read_host(dir.path()).await;
        assert_eq!(
            view,
            HostView {
                memory: None,
                cpu: None
            }
        );
    }

    #[tokio::test]
    async fn a_proc_folder_gives_memory_and_cores() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join(MEMINFO_FILE), MEMINFO).expect("meminfo");
        std::fs::write(dir.path().join(STAT_FILE), STAT).expect("stat");
        let view = read_host(dir.path()).await;
        assert_eq!(
            view.memory.map(|memory| memory.used_bytes),
            Some(6_000_000 * BYTES_PER_KIB)
        );
        assert_eq!(view.cpu, None, "the same counters twice: no tick passed");
    }
}
