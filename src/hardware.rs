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
            } else if cfg!(feature = "cuda") {
                "whisper"
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
    if let Ok(output) = std::process::Command::new("nvidia-smi")
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
    if let Ok(output) = std::process::Command::new("vulkaninfo")
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
