mod config;
mod cpu;
mod gpu;
mod usb;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Parser;

use config::{Config, FromConfigFile};
use cpu::default_cpu_device;
use gpu::{AvailableGpu, default_gpu_device};
use usb::UsbDevice;

#[derive(clap::Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, default_value = "~/.config/af-pro-display/config.toml")]
    config: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = shellexpand::tilde(&cli.config).to_string();
    let config_path = PathBuf::from(&config_path);
    if fs::File::open(&config_path).is_err() {
        eprintln!("Config file not found at: {}", config_path.display());
        eprintln!("Creating default config file...");

        let config_dir = config_path.parent().ok_or(anyhow::anyhow!(
            "Failed to get parent directory of config file"
        ))?;
        fs::create_dir_all(config_dir)?;
        fs::write(&config_path, toml::to_string(&Config::default())?)?;
    }

    let running = Arc::new(AtomicBool::new(true));
    let config = Config::from_config_file(&config_path)?;
    let cpu = match config.cpu_device.clone() {
        Some(path) => {
            println!("Using CPU temp sensor from config: {path}");
            Some(path)
        }
        None => default_cpu_device(),
    };
    let gpu_device = match config.gpu_device.clone() {
        Some(path) => {
            println!("Using GPU temp sensor from config: {path}");
            Some(path)
        }
        None => default_gpu_device(),
    };
    let gpu = AvailableGpu::get_available_gpu(gpu_device.as_deref());

    // Handle CTRL+C and other termination gracefully
    let run = running.clone();
    ctrlc::set_handler(move || {
        run.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let polling_interval = Duration::from_millis(config.polling_interval);

    // The display disappears and comes back whenever the machine suspends, so
    // hold the handle as an Option and re-open it when a write fails. Backing
    // off matters: retrying at the polling interval means hammering a port that
    // is still resuming, and a cancelled transfer on a port that has not come
    // back can take the whole USB host controller down with it.
    let mut device: Option<UsbDevice> = None;
    let mut backoff = polling_interval;
    // Only report a given outage once, rather than once per retry.
    let mut reported_outage = false;

    // Loop until the program is terminated
    while running.load(Ordering::SeqCst) {
        if device.is_none() {
            match UsbDevice::open(usb::VENDOR_ID, usb::PRODUCT_ID) {
                Ok(opened) => {
                    if reported_outage {
                        println!("Display reconnected");
                    }
                    device = Some(opened);
                    backoff = polling_interval;
                    reported_outage = false;
                }
                Err(e) => {
                    if !reported_outage {
                        eprintln!("Waiting for the display: {e}");
                        reported_outage = true;
                    }
                    sleep_until_terminated(&running, backoff);
                    backoff = next_backoff(backoff);
                    continue;
                }
            }
        }

        let cpu_temp = &cpu.as_ref().and_then(|path| cpu::read_temp(path));
        let gpu_temp = &gpu.temp();

        if let Some(connected) = &device
            && let Err(e) = connected.send_payload(cpu_temp, gpu_temp)
        {
            if !reported_outage {
                eprintln!("Lost the display ({e}), will re-open it");
                reported_outage = true;
            }
            // Drop the handle so the interface is released and the device is
            // closed in an orderly way, then open a fresh one next time round
            // rather than writing to a stale handle.
            device = None;
            sleep_until_terminated(&running, backoff);
            backoff = next_backoff(backoff);
            continue;
        }

        sleep_until_terminated(&running, polling_interval);
    }

    // Finally, set the temps to zero before exiting
    if let Some(connected) = &device
        && let Err(e) = connected.send_payload(&Some(0.0), &Some(0.0))
    {
        eprintln!("Could not blank the display on exit: {e}");
    }

    Ok(())
}

const MAX_BACKOFF: Duration = Duration::from_secs(30);

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(MAX_BACKOFF)
}

/// Sleep in short slices so that Ctrl-C and `systemctl stop` are still acted on
/// promptly while we are backing off.
fn sleep_until_terminated(running: &AtomicBool, duration: Duration) {
    const SLICE: Duration = Duration::from_millis(250);

    let mut remaining = duration;
    while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
        let slice = remaining.min(SLICE);
        std::thread::sleep(slice);
        remaining -= slice;
    }
}
