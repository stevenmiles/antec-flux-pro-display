use std::{fs, str::FromStr};

pub fn read_temp(device: &str) -> Option<f32> {
    fs::read_to_string(device)
        .inspect_err(|e| eprintln!("Error reading temp from {device}: {e}"))
        .ok()
        .and_then(|content| f32::from_str(content.trim()).ok())
        .map(|num| num / 1000.0)
}

pub fn default_cpu_device() -> Option<String> {
    // Loop through all hwmon devices, find the one that matches CPU temp with the temp1_label that matches Tctl
    // This is common for AMD CPUs where Tctl is used to represent the CPU temperature.
    for hwmon in fs::read_dir("/sys/class/hwmon").ok()? {
        let path = hwmon.ok()?.path();
        if let Ok(label) = fs::read_to_string(format!("{}/temp1_label", path.display()))
            && label.trim() == "Tctl"
        {
            let device = format!("{}/temp1_input", path.display());
            println!(
                "Detected CPU temp sensor: {device} ({}, label: Tctl)",
                path.display()
            );
            return Some(device);
        }
    }

    if fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").is_ok() {
        println!("Detected CPU temp sensor: /sys/class/thermal/thermal_zone0/temp (thermal_zone0 fallback)");
        return Some("/sys/class/thermal/thermal_zone0/temp".to_string());
    }

    if fs::read_to_string("/sys/class/hwmon/hwmon0/temp1_input").is_ok() {
        println!("Detected CPU temp sensor: /sys/class/hwmon/hwmon0/temp1_input (hwmon0 fallback)");
        return Some("/sys/class/hwmon/hwmon0/temp1_input".to_string());
    }

    eprintln!("Could not find CPU temp path");
    None
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Write;

    fn temp_file(tag: &str, contents: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("af-pro-display-test-{}-{tag}", std::process::id()));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_read_temp_parses_millidegrees() {
        let path = temp_file("millidegrees", "36000\n");
        assert_eq!(read_temp(path.to_str().unwrap()), Some(36.0));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_read_temp_returns_none_on_garbage_instead_of_panicking() {
        let path = temp_file("garbage", "not-a-number");
        assert_eq!(read_temp(path.to_str().unwrap()), None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_read_temp_returns_none_on_missing_file() {
        assert_eq!(read_temp("/nonexistent/af-pro-display/temp"), None);
    }
}
