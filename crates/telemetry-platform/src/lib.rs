//! Explicit platform capabilities and opt-in current-process measurements.
//!
//! Platform/UI adapters must submit their own focus and visibility events. This
//! crate does not inspect other applications, install input hooks, identify a
//! device, or sample machine-wide resources. Sampling requires the
//! `process-metrics` feature and an explicit call; it never runs automatically.

use serde::Serialize;
use std::{fmt, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub process_rss_bytes: bool,
    pub process_cpu_ticks: bool,
    pub process_cpu_percent: bool,
    pub automatic_window_state: bool,
    pub gpu: bool,
    pub energy: bool,
    pub native_crash_dumps: bool,
}

pub fn capabilities() -> Capabilities {
    let linux_metrics = cfg!(all(target_os = "linux", feature = "process-metrics"));
    Capabilities {
        process_rss_bytes: linux_metrics,
        process_cpu_ticks: linux_metrics,
        process_cpu_percent: false,
        automatic_window_state: false,
        gpu: false,
        energy: false,
        native_crash_dumps: false,
    }
}

/// Counters cover this application process, including its threads.
/// `cpu_ticks` is cumulative user + system CPU in OS clock ticks, not elapsed
/// time or utilization. Converting to seconds requires this host's CLK_TCK;
/// no fixed tick rate is assumed. For deltas, use two samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessSample {
    pub elapsed_ms: u64,
    pub rss_bytes: u64,
    pub cpu_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformError {
    Unsupported,
    Unavailable,
    InvalidMeasurement,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "process measurements are unsupported or disabled",
            Self::Unavailable => "process measurements are unavailable",
            Self::InvalidMeasurement => "process measurement format is invalid",
        })
    }
}

impl std::error::Error for PlatformError {}

/// No filesystem work occurs until `sample` is explicitly called.
pub struct ProcessSampler {
    started: Instant,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    pub fn sample(&self) -> Result<ProcessSample, PlatformError> {
        #[cfg(all(target_os = "linux", feature = "process-metrics"))]
        {
            let status = read_bounded("/proc/self/status")?;
            let stat = read_bounded("/proc/self/stat")?;
            Ok(ProcessSample {
                elapsed_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                rss_bytes: parse_rss(&status)?,
                cpu_ticks: parse_cpu_ticks(&stat)?,
            })
        }
        #[cfg(not(all(target_os = "linux", feature = "process-metrics")))]
        {
            // Read the clock so construction remains uniform across platforms.
            let _ = self.started.elapsed();
            Err(PlatformError::Unsupported)
        }
    }
}

#[cfg(all(target_os = "linux", feature = "process-metrics"))]
fn read_bounded(path: &str) -> Result<String, PlatformError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| PlatformError::Unavailable)?;
    let mut contents = String::new();
    file.take(64 * 1024)
        .read_to_string(&mut contents)
        .map_err(|_| PlatformError::Unavailable)?;
    if contents.len() >= 64 * 1024 {
        return Err(PlatformError::InvalidMeasurement);
    }
    Ok(contents)
}

#[cfg(any(test, all(target_os = "linux", feature = "process-metrics")))]
fn parse_rss(status: &str) -> Result<u64, PlatformError> {
    let fields = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .ok_or(PlatformError::InvalidMeasurement)?
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() != 2 || fields[1] != "kB" {
        return Err(PlatformError::InvalidMeasurement);
    }
    fields[0]
        .parse::<u64>()
        .ok()
        .and_then(|kilobytes| kilobytes.checked_mul(1024))
        .ok_or(PlatformError::InvalidMeasurement)
}

#[cfg(any(test, all(target_os = "linux", feature = "process-metrics")))]
fn parse_cpu_ticks(stat: &str) -> Result<u64, PlatformError> {
    // comm (field 2) can contain spaces and parentheses; split at the final ')'.
    let (_, fields) = stat
        .rsplit_once(')')
        .ok_or(PlatformError::InvalidMeasurement)?;
    let mut fields = fields.split_whitespace();
    let user = fields
        .nth(11)
        .ok_or(PlatformError::InvalidMeasurement)?
        .parse::<u64>()
        .map_err(|_| PlatformError::InvalidMeasurement)?;
    let system = fields
        .next()
        .ok_or(PlatformError::InvalidMeasurement)?
        .parse::<u64>()
        .map_err(|_| PlatformError::InvalidMeasurement)?;
    user.checked_add(system)
        .ok_or(PlatformError::InvalidMeasurement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_requires_units_and_checks_overflow() {
        assert_eq!(
            parse_rss("Name:\tapp\nVmRSS:\t1234 kB\n").unwrap(),
            1_263_616
        );
        for status in [
            "",
            "VmRSS: 9 MB",
            "VmRSS: -1 kB",
            "VmRSS: 18446744073709551615 kB",
            "VmRSS: 1 kB secret",
        ] {
            assert_eq!(parse_rss(status), Err(PlatformError::InvalidMeasurement));
        }
    }

    #[test]
    fn stat_parser_handles_spaces_and_parentheses_in_process_name() {
        let stat = "12 (app with ) strange(name) S 1 2 3 4 5 6 7 8 9 10 120 30 0 0";
        assert_eq!(parse_cpu_ticks(stat).unwrap(), 150);
        assert_eq!(
            parse_cpu_ticks("invalid"),
            Err(PlatformError::InvalidMeasurement)
        );
        assert_eq!(
            parse_cpu_ticks("1 (app) S 0"),
            Err(PlatformError::InvalidMeasurement)
        );
    }

    #[test]
    fn capabilities_do_not_claim_window_tracking_or_unsupported_metrics() {
        let capabilities = capabilities();
        assert!(!capabilities.automatic_window_state);
        assert!(!capabilities.process_cpu_percent);
        assert!(!capabilities.gpu);
        assert!(!capabilities.energy);
        assert!(!capabilities.native_crash_dumps);
    }

    #[test]
    fn sampler_reports_capability_without_silent_fallback() {
        let result = ProcessSampler::new().sample();
        if cfg!(all(target_os = "linux", feature = "process-metrics")) {
            let sample = result.unwrap();
            assert!(sample.rss_bytes > 0);
            let json = serde_json_for_test(sample);
            assert!(!json.contains("/proc"));
        } else {
            assert_eq!(result, Err(PlatformError::Unsupported));
        }
    }

    fn serde_json_for_test(sample: ProcessSample) -> String {
        format!("{sample:?}")
    }
}
