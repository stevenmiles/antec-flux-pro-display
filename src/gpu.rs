use anyhow::{Context, Result};
use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor};
use std::fs;

pub struct NvidiaGpu {
    nvml: Nvml,
    device_index: u32,
}

impl NvidiaGpu {
    pub fn new(nvml: Nvml) -> Self {
        Self {
            nvml,
            device_index: 0,
        }
    }

    pub fn temp(&self) -> Option<f32> {
        self.nvml
            .device_by_index(self.device_index)
            .inspect_err(|e| eprintln!("Error getting Nvidia GPU device: {e:?}"))
            .and_then(|device| device.temperature(TemperatureSensor::Gpu))
            .inspect_err(|e| eprintln!("Error getting Nvidia GPU temperature: {e:?}"))
            .map(|temp| temp as f32)
            .ok()
    }
}

pub struct AmdGpu {
    device_path: String,
}

impl AmdGpu {
    pub fn new(device_path: String) -> Self {
        Self { device_path }
    }

    pub fn temp(&self) -> Option<f32> {
        crate::cpu::read_temp(&self.device_path)
    }
}

pub enum AvailableGpu {
    Nvidia(Box<NvidiaGpu>),
    Amd(AmdGpu),
    Unknown,
}

impl AvailableGpu {
    pub fn get_available_gpu(gpu_device: Option<&str>) -> AvailableGpu {
        if let Some(path) = gpu_device {
            return AvailableGpu::Amd(AmdGpu::new(path.to_string()));
        }

        let maybe_nvidia =
            try_get_nvidia_gpu().inspect_err(|e| eprintln!("Failed to get Nvidia GPU. Error: {e}"));

        if let Ok(gpu) = maybe_nvidia {
            return gpu;
        }

        eprintln!(
            "No GPU temp sensor found: no gpu_device in config, no NVIDIA GPU, and no amdgpu hwmon node with a junction/edge sensor. Set gpu_device in config.toml to override (e.g. /sys/class/hwmon/hwmon2/temp2_input)."
        );
        AvailableGpu::Unknown
    }

    pub fn temp(&self) -> Option<f32> {
        match self {
            AvailableGpu::Nvidia(gpu) => gpu.temp(),
            AvailableGpu::Amd(gpu) => gpu.temp(),
            AvailableGpu::Unknown => None,
        }
    }
}

/// Finds an `amdgpu` hwmon temp path by driver name and sensor label rather
/// than a hardcoded hwmon index, since hwmon numbering shifts across
/// reboots/kernel updates.
///
/// Prefers a `junction` sensor: it's present on discrete AMD GPUs but absent
/// on iGPUs, so it disambiguates the two on systems that expose both. Falls
/// back to `edge` for GPUs/APUs that don't report junction. The junction pass
/// runs across all nodes before the edge pass so a discrete card always wins
/// over an iGPU's edge sensor.
pub fn default_gpu_device() -> Option<String> {
    for label in ["junction", "edge"] {
        if let Some(device) = find_amdgpu_temp(label) {
            return Some(device);
        }
    }

    None
}

fn find_amdgpu_temp(label: &str) -> Option<String> {
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        if name.trim() != "amdgpu" {
            continue;
        }

        for n in 1..=3 {
            let found = fs::read_to_string(path.join(format!("temp{n}_label")))
                .is_ok_and(|l| l.trim() == label);
            if found {
                let device = path
                    .join(format!("temp{n}_input"))
                    .to_string_lossy()
                    .into_owned();
                println!(
                    "Detected AMD GPU temp sensor: {device} ({}, label: {label})",
                    path.display()
                );
                return Some(device);
            }
        }
    }

    None
}

fn try_get_nvidia_gpu() -> Result<AvailableGpu> {
    let nvml = Nvml::builder()
        .lib_path(std::ffi::OsStr::new("libnvidia-ml.so.1"))
        .init()
        .context("Failed to initialize NVML")?;

    let driver_version = nvml
        .sys_driver_version()
        .context("Failed to get NVML driver version")?;
    println!("NVML initialized, driver version: {driver_version}");

    let device_count = nvml
        .device_count()
        .context("Failed to get NVML device count")?;

    println!("Found {device_count} NVML-supported GPUs");
    Ok(AvailableGpu::Nvidia(Box::new(NvidiaGpu::new(nvml))))
}
