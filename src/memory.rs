//! Windows working-set accounting and trimming for the single-process Slint app.
//!
//! This mirrors the Tauri build's memory diagnostics: log working set/private
//! bytes at startup, periodically, and around minimize/restore, and page out
//! the process working set when the window is minimized. Private bytes are not
//! changed by trimming; pages fault back in on demand.

#[cfg(windows)]
mod imp {
    use std::mem::{size_of, zeroed};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::System::ProcessStatus::{
        EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    const MB: f64 = 1024.0 * 1024.0;
    static LAST_TRIM: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

    fn stats() -> Option<(usize, usize)> {
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
            counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            let ok = GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters as *mut _ as *mut _,
                counters.cb,
            );
            if ok == 0 {
                None
            } else {
                Some((counters.WorkingSetSize, counters.PrivateUsage))
            }
        }
    }

    pub fn log_stats(tag: &str, level: &str) {
        if let Some((working, private)) = stats() {
            crate::logger::log(
                level,
                "memory",
                &format!(
                    "{tag}: processes=1 working={:.1}MB private={:.1}MB",
                    working as f64 / MB,
                    private as f64 / MB
                ),
            );
        }
    }

    pub fn trim_working_set(tag: &str, level: &str) {
        let lock = LAST_TRIM.get_or_init(|| Mutex::new(None));
        if let Ok(mut last) = lock.lock() {
            if last.is_some_and(|at| at.elapsed() < Duration::from_secs(8)) {
                return;
            }
            *last = Some(Instant::now());
        }

        log_stats(&format!("{tag}-before"), level);
        unsafe {
            let _ = EmptyWorkingSet(GetCurrentProcess());
        }
        log_stats(&format!("{tag}-after"), level);
    }
}

#[cfg(windows)]
pub use imp::{log_stats, trim_working_set};

#[cfg(not(windows))]
pub fn log_stats(_tag: &str, _level: &str) {}

#[cfg(not(windows))]
pub fn trim_working_set(_tag: &str, _level: &str) {}
