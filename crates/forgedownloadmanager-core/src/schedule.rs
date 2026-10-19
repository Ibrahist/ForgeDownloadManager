use std::path::Path;
use std::process::Command;
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::model::now_ms;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnCompleteAction {
    None,
    OpenFile,
    RunCommand,
    Sleep,
    Shutdown,
    Quit,
}

impl OnCompleteAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenFile => "open_file",
            Self::RunCommand => "run_command",
            Self::Sleep => "sleep",
            Self::Shutdown => "shutdown",
            Self::Quit => "quit",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "none" => Self::None,
            "open_file" => Self::OpenFile,
            "run_command" => Self::RunCommand,
            "sleep" => Self::Sleep,
            "shutdown" => Self::Shutdown,
            "quit" => Self::Quit,
            other => return Err(Error::Failed(format!("unknown on_complete action '{other}'"))),
        })
    }
}

impl Default for OnCompleteAction {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub id: Uuid,
    pub name: String,
    pub concurrency: usize,
    pub speed_limit_bps: Option<u64>,
    /// "HH:MM" local time string. Null/None means always open.
    pub schedule_start: Option<String>,
    /// "HH:MM" local time string. Null/None means always open.
    pub schedule_end: Option<String>,
    /// Bit 0: Sunday, Bit 1: Monday, ..., Bit 6: Saturday. Default 127 (all days).
    pub days_mask: u8,
    pub on_complete: OnCompleteAction,
    pub on_complete_command: Option<String>,
    pub created_at: i64,
}

impl Queue {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            concurrency: 3,
            speed_limit_bps: None,
            schedule_start: None,
            schedule_end: None,
            days_mask: 127,
            on_complete: OnCompleteAction::None,
            on_complete_command: None,
            created_at: now_ms(),
        }
    }

    /// Check if this queue is currently open based on local time.
    pub fn is_open(&self) -> bool {
        let (hour, minute, weekday) = current_local_time();
        self.is_open_at(hour, minute, weekday)
    }

    /// Check if this queue is open at a specific time.
    pub fn is_open_at(&self, hour: u32, minute: u32, weekday: u8) -> bool {
        is_window_active(
            self.schedule_start.as_deref(),
            self.schedule_end.as_deref(),
            self.days_mask,
            hour,
            minute,
            weekday,
        )
    }
}

/// Evaluates if a schedule window is active at `(hour, minute, weekday)`.
/// `days_mask`: bit 0 = Sun .. bit 6 = Sat.
pub fn is_window_active(
    start: Option<&str>,
    end: Option<&str>,
    days_mask: u8,
    hour: u32,
    minute: u32,
    weekday: u8,
) -> bool {
    // Check day of week (bit 0 = Sun, bit 1 = Mon, ..., bit 6 = Sat)
    if weekday > 6 {
        return false;
    }
    if (days_mask & (1 << weekday)) == 0 {
        return false;
    }

    let (start_h, start_m) = match start.and_then(parse_hh_mm) {
        Some(t) => t,
        None => return true, // No start defined -> open
    };
    let (end_h, end_m) = match end.and_then(parse_hh_mm) {
        Some(t) => t,
        None => return true, // No end defined -> open
    };

    let current_mins = hour * 60 + minute;
    let start_mins = start_h * 60 + start_m;
    let end_mins = end_h * 60 + end_m;

    if start_mins == end_mins {
        // Equal start and end means running 24 hours on active days.
        return true;
    }

    if start_mins < end_mins {
        // Window within the same day, e.g. 08:00 to 18:00
        current_mins >= start_mins && current_mins < end_mins
    } else {
        // Window crosses midnight, e.g. 22:00 to 06:00
        current_mins >= start_mins || current_mins < end_mins
    }
}

/// Parses "HH:MM" into `(hour, minute)`.
pub fn parse_hh_mm(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let (h_str, m_str) = s.split_once(':')?;
    let h: u32 = h_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    if h < 24 && m < 60 {
        Some((h, m))
    } else {
        None
    }
}

/// Returns `(hour, minute, weekday)` where weekday is 0 = Sunday .. 6 = Saturday.
pub fn current_local_time() -> (u32, u32, u8) {
    #[cfg(windows)]
    unsafe {
        #[repr(C)]
        struct SystemTimeWin {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16, // 0 = Sun .. 6 = Sat
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        extern "system" {
            fn GetLocalTime(lp_system_time: *mut SystemTimeWin);
        }
        let mut st = std::mem::zeroed();
        GetLocalTime(&mut st);
        (st.w_hour as u32, st.w_minute as u32, (st.w_day_of_week as u8) % 7)
    }

    #[cfg(not(windows))]
    {
        // Portable fallback using UTC duration if libc is not configured
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 1970-01-01 was Thursday (day 4 in 0=Sun..6=Sat)
        let days = (secs / 86400) as i64;
        let weekday = (((days + 4) % 7 + 7) % 7) as u8;
        let day_secs = secs % 86400;
        let hour = (day_secs / 3600) as u32;
        let minute = ((day_secs % 3600) / 60) as u32;
        (hour, minute, weekday)
    }
}

/// Triggers completion action for a queue.
pub async fn trigger_completion_action(
    action: OnCompleteAction,
    command: Option<&str>,
    last_file: Option<&Path>,
) -> Result<()> {
    match action {
        OnCompleteAction::None => Ok(()),
        OnCompleteAction::RunCommand => {
            if let Some(cmd) = command.filter(|c| !c.trim().is_empty()) {
                tracing::info!("queue completion: running command '{cmd}'");
                run_shell_command(cmd).await?;
            }
            Ok(())
        }
        OnCompleteAction::OpenFile => {
            if let Some(path) = last_file {
                tracing::info!("queue completion: opening file {}", path.display());
                open_path(path).await?;
            }
            Ok(())
        }
        OnCompleteAction::Sleep => {
            tracing::info!("queue completion: triggering system sleep");
            trigger_system_sleep().await
        }
        OnCompleteAction::Shutdown => {
            tracing::info!("queue completion: triggering system shutdown");
            trigger_system_shutdown().await
        }
        OnCompleteAction::Quit => {
            tracing::info!("queue completion: quit requested");
            Ok(())
        }
    }
}

async fn run_shell_command(cmd: &str) -> Result<()> {
    let cmd = cmd.to_string();
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("cmd");
            c.args(["/C", &cmd]);
            c
        };

        #[cfg(not(windows))]
        let mut command = {
            let mut c = Command::new("sh");
            c.args(["-c", &cmd]);
            c
        };

        match command.status() {
            Ok(status) if !status.success() => {
                tracing::warn!("queue completion command exited with {status}");
            }
            Err(err) => {
                tracing::warn!("failed to run queue completion command: {err}");
            }
            _ => {}
        }
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    Ok(())
}

async fn open_path(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("cmd");
            c.args(["/C", "start", "", path.to_string_lossy().as_ref()]);
            c
        };

        #[cfg(target_os = "macos")]
        let mut command = {
            let mut c = Command::new("open");
            c.arg(&path);
            c
        };

        #[cfg(all(not(windows), not(target_os = "macos")))]
        let mut command = {
            let mut c = Command::new("xdg-open");
            c.arg(&path);
            c
        };

        let _ = command.status();
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    Ok(())
}

async fn trigger_system_sleep() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("rundll32.exe");
            c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
            c
        };

        #[cfg(target_os = "macos")]
        let mut command = {
            let mut c = Command::new("pmset");
            c.arg("sleepnow");
            c
        };

        #[cfg(all(not(windows), not(target_os = "macos")))]
        let mut command = {
            let mut c = Command::new("systemctl");
            c.arg("suspend");
            c
        };

        let _ = command.status();
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    Ok(())
}

async fn trigger_system_shutdown() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        #[cfg(windows)]
        let mut command = {
            let mut c = Command::new("shutdown");
            c.args(["/s", "/t", "30"]);
            c
        };

        #[cfg(not(windows))]
        let mut command = {
            let mut c = Command::new("shutdown");
            c.args(["-h", "+1"]);
            c
        };

        let _ = command.status();
    })
    .await
    .map_err(|e| Error::Failed(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hh_mm_valid() {
        assert_eq!(parse_hh_mm("00:00"), Some((0, 0)));
        assert_eq!(parse_hh_mm("08:30"), Some((8, 30)));
        assert_eq!(parse_hh_mm("23:59"), Some((23, 59)));
        assert_eq!(parse_hh_mm("8:5"), Some((8, 5)));
        assert_eq!(parse_hh_mm("24:00"), None);
        assert_eq!(parse_hh_mm("12:60"), None);
        assert_eq!(parse_hh_mm("invalid"), None);
    }

    #[test]
    fn window_active_daytime() {
        let start = Some("09:00");
        let end = Some("17:00");
        let mask = 127; // all days

        assert!(is_window_active(start, end, mask, 9, 0, 1));
        assert!(is_window_active(start, end, mask, 12, 30, 2));
        assert!(is_window_active(start, end, mask, 16, 59, 3));
        assert!(!is_window_active(start, end, mask, 8, 59, 1));
        assert!(!is_window_active(start, end, mask, 17, 0, 1));
        assert!(!is_window_active(start, end, mask, 20, 0, 1));
    }

    #[test]
    fn window_active_cross_midnight() {
        let start = Some("22:00");
        let end = Some("06:00");
        let mask = 127;

        assert!(is_window_active(start, end, mask, 22, 0, 0));
        assert!(is_window_active(start, end, mask, 23, 45, 0));
        assert!(is_window_active(start, end, mask, 0, 0, 0));
        assert!(is_window_active(start, end, mask, 5, 59, 0));
        assert!(!is_window_active(start, end, mask, 6, 0, 0));
        assert!(!is_window_active(start, end, mask, 12, 0, 0));
        assert!(!is_window_active(start, end, mask, 21, 59, 0));
    }

    #[test]
    fn window_active_days_mask() {
        // Only Monday (bit 1 = 2) and Wednesday (bit 3 = 8) -> mask = 10
        let mask = 10;
        let start = Some("00:00");
        let end = Some("23:59");

        assert!(is_window_active(start, end, mask, 12, 0, 1)); // Mon
        assert!(is_window_active(start, end, mask, 12, 0, 3)); // Wed
        assert!(!is_window_active(start, end, mask, 12, 0, 0)); // Sun
        assert!(!is_window_active(start, end, mask, 12, 0, 2)); // Tue
        assert!(!is_window_active(start, end, mask, 12, 0, 4)); // Thu
        assert!(!is_window_active(start, end, mask, 12, 0, 5)); // Fri
        assert!(!is_window_active(start, end, mask, 12, 0, 6)); // Sat
    }

    #[test]
    fn window_always_open_when_none_or_equal() {
        assert!(is_window_active(None, None, 127, 12, 0, 0));
        assert!(is_window_active(Some("08:00"), None, 127, 12, 0, 0));
        assert!(is_window_active(None, Some("18:00"), 127, 12, 0, 0));
        assert!(is_window_active(Some("10:00"), Some("10:00"), 127, 12, 0, 0));
    }
}
