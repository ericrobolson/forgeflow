use std::path::Path;
use std::process::Command;

/// What the local machine offers for running models.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub chip: String,
    pub ram_bytes: u64,
    pub apple_silicon: bool,
    /// Memory bandwidth in GB/s, when the chip is known.
    pub bandwidth_gbps: Option<f64>,
}

impl Device {
    pub fn probe() -> Self {
        if cfg!(target_os = "macos") {
            let chip = sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "unknown".into());
            let ram_bytes = sysctl("hw.memsize")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let apple_silicon = chip.starts_with("Apple");
            Self {
                bandwidth_gbps: apple_bandwidth_gbps(&chip),
                chip,
                ram_bytes,
                apple_silicon,
            }
        } else {
            let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
            let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            Self {
                chip: cpuinfo
                    .lines()
                    .find_map(|l| l.strip_prefix("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "unknown".into()),
                ram_bytes: parse_meminfo_total(&meminfo).unwrap_or(0),
                apple_silicon: false,
                bandwidth_gbps: None,
            }
        }
    }

    /// Bytes a model may use. Apple Silicon lets the GPU wire about 2/3 of RAM up to
    /// 36 GB and 3/4 above; elsewhere leave room for the OS and other programs.
    pub fn model_budget_bytes(&self) -> u64 {
        let fraction = if !self.apple_silicon {
            0.6
        } else if self.ram_bytes <= 36 * GB {
            0.67
        } else {
            0.75
        };
        (self.ram_bytes as f64 * fraction) as u64
    }
}

pub const GB: u64 = 1_000_000_000;

fn sysctl(name: &str) -> Option<String> {
    let output = Command::new("sysctl").args(["-n", name]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_meminfo_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Published memory bandwidth for Apple Silicon. Where a chip ships in several
/// bandwidth tiers, the lower one is used so estimates stay conservative.
pub fn apple_bandwidth_gbps(chip: &str) -> Option<f64> {
    // Matched as a whole-word suffix, so "M4 Pro" never matches "M4" and
    // unlisted variants such as "M5 Pro" stay unknown rather than guessed.
    const TABLE: [(&str, f64); 16] = [
        ("M1 Ultra", 800.0),
        ("M1 Max", 400.0),
        ("M1 Pro", 200.0),
        ("M1", 68.0),
        ("M2 Ultra", 800.0),
        ("M2 Max", 400.0),
        ("M2 Pro", 200.0),
        ("M2", 100.0),
        ("M3 Ultra", 819.0),
        ("M3 Max", 300.0),
        ("M3 Pro", 150.0),
        ("M3", 100.0),
        ("M4 Max", 410.0),
        ("M4 Pro", 273.0),
        ("M4", 120.0),
        ("M5", 153.0),
    ];
    let chip = format!(" {}", chip.trim());
    TABLE
        .iter()
        .find(|(name, _)| chip.ends_with(&format!(" {name}")))
        .map(|(_, gbps)| *gbps)
}

/// Free bytes on the volume holding `path` (or its nearest existing parent).
pub fn free_disk_bytes(path: &Path) -> Option<u64> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent()?;
    }
    let output = Command::new("df").arg("-k").arg(existing).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_df_available(&text)
}

fn parse_df_available(df: &str) -> Option<u64> {
    let line = df.lines().nth(1)?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_apple_bandwidth_without_prefix_collisions() {
        let cases = [
            ("Apple M4 Pro", Some(273.0)),
            ("Apple M4", Some(120.0)),
            ("Apple M4 Max", Some(410.0)),
            ("Apple M1", Some(68.0)),
            ("Apple M2 Ultra", Some(800.0)),
            ("Apple M5 Pro", None),
            ("Intel(R) Core(TM) i9", None),
        ];
        for (chip, expected) in cases {
            assert_eq!(apple_bandwidth_gbps(chip), expected, "{chip}");
        }
    }

    #[test]
    fn budget_follows_apple_wired_memory_limits() {
        let device = |ram_gb: u64, apple_silicon| Device {
            chip: String::new(),
            ram_bytes: ram_gb * GB,
            apple_silicon,
            bandwidth_gbps: None,
        };
        assert_eq!(device(24, true).model_budget_bytes(), (24.0 * 0.67 * GB as f64) as u64);
        assert_eq!(device(64, true).model_budget_bytes(), 48 * GB);
        assert_eq!(device(10, false).model_budget_bytes(), 6 * GB);
    }

    #[test]
    fn parses_linux_meminfo_and_df() {
        assert_eq!(
            parse_meminfo_total("MemTotal:       16000000 kB\nMemFree: 1 kB"),
            Some(16_000_000 * 1024)
        );
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s5 482797652 379973820 71452920 85% /System/Volumes/Data";
        assert_eq!(parse_df_available(df), Some(71_452_920 * 1024));
    }
}
