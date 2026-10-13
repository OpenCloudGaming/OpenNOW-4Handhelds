//! Clock and battery readings for the status area.

pub struct LocalTime {
    pub hour: u32,
    pub minute: u32,
}

#[cfg(unix)]
pub fn local_time() -> Option<LocalTime> {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut parts: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut parts).is_null() {
            return None;
        }
        Some(LocalTime { hour: parts.tm_hour as u32, minute: parts.tm_min as u32 })
    }
}

#[cfg(windows)]
pub fn local_time() -> Option<LocalTime> {
    let mut parts = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut parts) };
    Some(LocalTime { hour: parts.wHour as u32, minute: parts.wMinute as u32 })
}

#[cfg(not(any(unix, windows)))]
pub fn local_time() -> Option<LocalTime> {
    None
}

/// Battery charge in percent, or `None` on machines without a battery.
#[cfg(target_os = "linux")]
pub fn battery_percent() -> Option<u8> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        if let Ok(capacity) = std::fs::read_to_string(path.join("capacity")) {
            if let Ok(value) = capacity.trim().parse::<u8>() {
                return Some(value.min(100));
            }
        }
    }
    None
}

#[cfg(windows)]
pub fn battery_percent() -> Option<u8> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 || status.BatteryFlag & 128 != 0 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some(status.BatteryLifePercent)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn battery_percent() -> Option<u8> {
    None
}
