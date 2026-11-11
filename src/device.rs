use std::path::Path;
use std::process::Command;

/// What the local machine offers for running models.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub chip: String,
    pub ram_bytes: u64,
    pub apple_silicon: bool,
    /// Memory bandwidth in GB/s of whatever runs the model (the GPU when there is a
    /// discrete one), when known.
    pub bandwidth_gbps: Option<f64>,
    /// A discrete GPU that llama-server offloads layers to.
    pub gpu: Option<Gpu>,
}

/// A discrete NVIDIA GPU, as reported by `nvidia-smi`. With several cards the memory
/// is summed, since llama.cpp splits layers across them.
#[derive(Debug, Clone, PartialEq)]
pub struct Gpu {
    pub name: String,
    pub count: usize,
    pub vram_bytes: u64,
    /// CUDA compute capability without the dot, such as "86" for an RTX 3090.
    pub compute_caps: Vec<String>,
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
                gpu: None,
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
                gpu: None,
            }
            .with_gpu(probe_nvidia())
        }
    }

    /// Runs models on `gpu` when there is one, taking its bandwidth for speed estimates.
    pub fn with_gpu(self, gpu: Option<Gpu>) -> Self {
        match gpu {
            Some(gpu) => Self {
                bandwidth_gbps: nvidia_bandwidth_gbps(&gpu.name),
                gpu: Some(gpu),
                ..self
            },
            None => self,
        }
    }

    /// One line describing the machine for model listings.
    pub fn summary(&self) -> String {
        let gpu = self.gpu.as_ref().map(|g| {
            let count = if g.count > 1 { format!("{}× ", g.count) } else { String::new() };
            format!(" · {count}{} {} VRAM", g.name, crate::models::gb(g.vram_bytes))
        });
        format!(
            "{} · {} RAM{} · ~{} model budget",
            self.chip,
            crate::models::gb(self.ram_bytes),
            gpu.unwrap_or_default(),
            crate::models::gb(self.model_budget_bytes()),
        )
    }

    /// Bytes a model may use. Apple Silicon lets the GPU wire about 2/3 of RAM up to
    /// 36 GB and 3/4 above; elsewhere leave room for the OS and other programs.
    /// With a discrete GPU every layer is offloaded, so the model must fit in VRAM, less
    /// a margin for the display and the CUDA context.
    pub fn model_budget_bytes(&self) -> u64 {
        if let Some(gpu) = &self.gpu {
            return (gpu.vram_bytes as f64 * 0.9) as u64;
        }
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

fn probe_nvidia() -> Option<Gpu> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total,compute_cap", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `name, memory MiB, compute capability` lines from `nvidia-smi`.
fn parse_nvidia_smi(csv: &str) -> Option<Gpu> {
    let cards: Vec<(String, u64, String)> = csv
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, mib, cap] = fields[..] else { return None };
            Some((name.to_string(), mib.parse::<u64>().ok()? * 1024 * 1024, cap.replace('.', "")))
        })
        .collect();
    let (name, _, _) = cards.first()?.clone();
    let mut compute_caps: Vec<String> = cards.iter().map(|(_, _, cap)| cap.clone()).filter(|c| !c.is_empty()).collect();
    compute_caps.sort();
    compute_caps.dedup();
    Some(Gpu {
        name,
        count: cards.len(),
        vram_bytes: cards.iter().map(|(_, bytes, _)| bytes).sum(),
        compute_caps,
    })
}

/// Published memory bandwidth for NVIDIA cards, matched as a whole-word suffix so
/// "RTX 4070" never matches "RTX 4070 Ti". Unlisted cards stay unknown.
pub fn nvidia_bandwidth_gbps(name: &str) -> Option<f64> {
    const TABLE: [(&str, f64); 31] = [
        ("RTX 3060", 360.0),
        ("RTX 3060 Ti", 448.0),
        ("RTX 3070", 448.0),
        ("RTX 3070 Ti", 608.0),
        ("RTX 3080", 760.0),
        ("RTX 3080 Ti", 912.0),
        ("RTX 3090", 936.0),
        ("RTX 3090 Ti", 1008.0),
        ("RTX 4060", 272.0),
        ("RTX 4060 Ti", 288.0),
        ("RTX 4070", 504.0),
        ("RTX 4070 SUPER", 504.0),
        ("RTX 4070 Ti", 504.0),
        ("RTX 4070 Ti SUPER", 672.0),
        ("RTX 4080", 717.0),
        ("RTX 4080 SUPER", 736.0),
        ("RTX 4090", 1008.0),
        ("RTX 5060", 448.0),
        ("RTX 5060 Ti", 448.0),
        ("RTX 5070", 672.0),
        ("RTX 5070 Ti", 896.0),
        ("RTX 5080", 960.0),
        ("RTX 5090", 1792.0),
        ("RTX A4000", 448.0),
        ("RTX A5000", 768.0),
        ("RTX A6000", 768.0),
        ("RTX 6000 Ada Generation", 960.0),
        ("A100-PCIE-40GB", 1555.0),
        ("A100-SXM4-80GB", 2039.0),
        ("H100 PCIe", 2000.0),
        ("L40S", 864.0),
    ];
    let name = format!(" {}", name.trim()).to_lowercase();
    TABLE
        .iter()
        .find(|(card, _)| name.ends_with(&format!(" {}", card.to_lowercase())))
        .map(|(_, gbps)| *gbps)
}

/// A process using GPU memory.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuProcess {
    pub pid: u32,
    pub name: String,
    pub bytes: u64,
}

/// The processes using NVIDIA GPU memory, largest first; empty without `nvidia-smi`.
pub fn gpu_processes() -> Vec<GpuProcess> {
    let Ok(output) = Command::new("nvidia-smi")
        .args(["--query-compute-apps=pid,process_name,used_memory", "--format=csv,noheader,nounits"])
        .output()
    else {
        return vec![];
    };
    let mut processes = parse_gpu_processes(&String::from_utf8_lossy(&output.stdout));
    processes.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    processes
}

fn parse_gpu_processes(csv: &str) -> Vec<GpuProcess> {
    csv.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [pid, name, mib] = fields[..] else { return None };
            let name = Path::new(name).file_name().map_or(name.to_string(), |n| n.to_string_lossy().into_owned());
            Some(GpuProcess { pid: pid.parse().ok()?, name, bytes: mib.parse::<u64>().ok()? * 1024 * 1024 })
        })
        .collect()
}

/// Free memory on the NVIDIA GPU with the least free, or `None` without one.
pub fn gpu_free_bytes() -> Option<u64> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .min()
        .map(|mib| mib * 1024 * 1024)
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
            gpu: None,
        };
        assert_eq!(device(24, true).model_budget_bytes(), (24.0 * 0.67 * GB as f64) as u64);
        assert_eq!(device(64, true).model_budget_bytes(), 48 * GB);
        assert_eq!(device(10, false).model_budget_bytes(), 6 * GB);
    }

    #[test]
    fn detects_nvidia_cards_and_budgets_by_vram() {
        let gpu = parse_nvidia_smi("NVIDIA GeForce RTX 3090, 24576, 8.6\n").unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 3090");
        assert_eq!(gpu.vram_bytes, 24576 * 1024 * 1024);
        assert_eq!(gpu.compute_caps, ["86"]);
        let device = Device { chip: "i9".into(), ram_bytes: 32 * GB, apple_silicon: false, bandwidth_gbps: None, gpu: None }
            .with_gpu(Some(gpu));
        assert_eq!(device.bandwidth_gbps, Some(936.0));
        assert_eq!(device.model_budget_bytes(), (24576.0 * 1024.0 * 1024.0 * 0.9) as u64);

        let pair = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564, 8.9\nNVIDIA GeForce RTX 3090, 24576, 8.6").unwrap();
        assert_eq!((pair.count, pair.compute_caps.len()), (2, 2));
        assert_eq!(parse_nvidia_smi("No devices were found"), None);
    }

    #[test]
    fn reads_gpu_processes() {
        let processes = parse_gpu_processes("2914972, /home/eric/local-models/llama.cpp/build/bin/llama-server, 19454\n11905, /usr/bin/sunshine, 262\nbad line");
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0], GpuProcess { pid: 2914972, name: "llama-server".into(), bytes: 19454 * 1024 * 1024 });
    }

    #[test]
    fn nvidia_bandwidth_has_no_prefix_collisions() {
        assert_eq!(nvidia_bandwidth_gbps("NVIDIA GeForce RTX 4070"), Some(504.0));
        assert_eq!(nvidia_bandwidth_gbps("NVIDIA GeForce RTX 4070 Ti SUPER"), Some(672.0));
        assert_eq!(nvidia_bandwidth_gbps("NVIDIA GeForce RTX 3090 Ti"), Some(1008.0));
        assert_eq!(nvidia_bandwidth_gbps("NVIDIA GeForce GTX 1080"), None);
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
