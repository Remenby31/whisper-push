//! Hardware detection — identify GPU, recommend best transcription engine.

use tracing::info;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HardwareInfo {
    pub os: &'static str,
    pub arch: &'static str,
    pub gpu: GpuInfo,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum GpuInfo {
    AppleSilicon { chip: String },
    NvidiaCuda { name: String },
    AmdVulkan,
    IntelArc,
    CpuOnly,
    Unknown,
}

impl GpuInfo {
    pub fn label(&self) -> &str {
        match self {
            GpuInfo::AppleSilicon { .. } => "Apple Silicon (Metal)",
            GpuInfo::NvidiaCuda { .. } => "NVIDIA (CUDA)",
            GpuInfo::AmdVulkan => "AMD (Vulkan)",
            GpuInfo::IntelArc => "Intel Arc (Vulkan)",
            GpuInfo::CpuOnly => "CPU only",
            GpuInfo::Unknown => "Unknown",
        }
    }
}

/// Physical RAM, in bytes. 0 when the platform query fails — callers treat that
/// as "unknown", never as "no memory".
pub fn total_ram_bytes() -> u64 {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        #[cfg(target_os = "macos")]
        {
            let mut out: u64 = 0;
            let mut len = std::mem::size_of::<u64>();
            let ok = unsafe {
                libc::sysctlbyname(
                    c"hw.memsize".as_ptr(),
                    (&raw mut out).cast(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            return if ok == 0 { out } else { 0 };
        }
        #[cfg(target_os = "linux")]
        {
            // MemTotal is in kB.
            return std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("MemTotal:"))?
                        .split_whitespace()
                        .nth(1)?
                        .parse::<u64>()
                        .ok()
                })
                .map(|kb| kb * 1024)
                .unwrap_or(0);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut st = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        unsafe {
            if GlobalMemoryStatusEx(&mut st).is_ok() {
                return st.ullTotalPhys;
            }
        }
        0
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        0
    }
}

/// Free space on the volume holding `path`, in bytes (0 when unknown).
pub fn free_disk_bytes(path: &std::path::Path) -> u64 {
    // The path may not exist yet on a first run — ask about the nearest
    // ancestor that does, which is on the same volume.
    let mut probe = path;
    loop {
        if probe.exists() {
            return fs4::available_space(probe).unwrap_or(0);
        }
        match probe.parent() {
            Some(p) => probe = p,
            None => return 0,
        }
    }
}

/// Detect hardware and recommend the best engine.
pub fn detect() -> HardwareInfo {
    let gpu = detect_gpu();
    let info = HardwareInfo {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        gpu,
    };
    info!("Hardware: {} {} — GPU: {:?}", info.os, info.arch, info.gpu);
    info
}

/// Recommend the best backend based on hardware.
pub fn recommend_backend(hw: &HardwareInfo) -> &'static str {
    match &hw.gpu {
        GpuInfo::AppleSilicon { .. } => {
            // Parakeet via WebGPU/Metal is fastest, but Whisper Metal is proven
            if cfg!(feature = "parakeet") {
                "parakeet"
            } else {
                "whisper"
            }
        }
        GpuInfo::NvidiaCuda { .. } => {
            if cfg!(feature = "parakeet") {
                "parakeet"
            } else {
                "whisper"
            }
        }
        _ => "whisper",
    }
}

fn detect_gpu() -> GpuInfo {
    #[cfg(target_os = "macos")]
    {
        if std::env::consts::ARCH == "aarch64" {
            let chip = detect_apple_chip();
            return GpuInfo::AppleSilicon { chip };
        }
    }

    // Check for NVIDIA
    // `quiet_command`: on Windows the daemon is a GUI-subsystem binary, so
    // spawning these console tools would flash a black window at every launch.
    if let Ok(output) = crate::util::quiet_command("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader,nounits")
        .output()
    {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return GpuInfo::NvidiaCuda { name };
            }
        }
    }

    // Check for Vulkan (AMD/Intel)
    if let Ok(output) = crate::util::quiet_command("vulkaninfo")
        .arg("--summary")
        .output()
    {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            if out.contains("AMD") || out.contains("Radeon") {
                return GpuInfo::AmdVulkan;
            }
            if out.contains("Intel") {
                return GpuInfo::IntelArc;
            }
        }
    }

    GpuInfo::CpuOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_labels() {
        assert_eq!(
            GpuInfo::AppleSilicon { chip: "M4".into() }.label(),
            "Apple Silicon (Metal)"
        );
        assert_eq!(
            GpuInfo::NvidiaCuda {
                name: "RTX 4090".into()
            }
            .label(),
            "NVIDIA (CUDA)"
        );
        assert_eq!(GpuInfo::CpuOnly.label(), "CPU only");
        assert_eq!(GpuInfo::Unknown.label(), "Unknown");
    }

    #[test]
    fn test_recommend_cpu_only() {
        let hw = HardwareInfo {
            os: "linux",
            arch: "x86_64",
            gpu: GpuInfo::CpuOnly,
        };
        assert_eq!(recommend_backend(&hw), "whisper");
    }

    #[test]
    fn test_recommend_apple_silicon() {
        let hw = HardwareInfo {
            os: "macos",
            arch: "aarch64",
            gpu: GpuInfo::AppleSilicon { chip: "M4".into() },
        };
        let backend = recommend_backend(&hw);
        // Either parakeet (if feature enabled) or whisper
        assert!(backend == "whisper" || backend == "parakeet");
    }

    #[test]
    fn test_detect_returns_valid() {
        let hw = detect();
        assert!(!hw.os.is_empty());
        assert!(!hw.arch.is_empty());
    }
}

#[cfg(target_os = "macos")]
fn detect_apple_chip() -> String {
    if let Ok(output) = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("machdep.cpu.brand_string")
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    "Apple Silicon".to_string()
}
