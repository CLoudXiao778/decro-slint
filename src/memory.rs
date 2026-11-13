//! Windows working-set accounting and trimming for the single-process Slint app.
//!
//! This mirrors the Tauri build's memory diagnostics: log working set/private
//! bytes at startup, periodically, and around minimize/restore, and page out
//! the process working set when the window is minimized. Private bytes are not
//! changed by trimming; pages fault back in on demand. Trimming is skipped
//! while audio is playing because faulting decoder pages back in on the audio
//! thread is audible as a short dropout.

#[cfg(windows)]
mod imp {
    use std::mem::{size_of, zeroed};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::System::ProcessStatus::{
        EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    const MB: f64 = 1024.0 * 1024.0;
    static LAST_TRIM: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    static AUDIO_ACTIVE: AtomicBool = AtomicBool::new(false);

    pub fn set_audio_active(active: bool) {
        AUDIO_ACTIVE.store(active, Ordering::Relaxed);
    }

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
        // EmptyWorkingSet evicts the decoder's code and heap pages too. While a
        // sink is playing, the decoder runs on the real-time audio thread and
        // those page faults show up as an audible 0.2 s dropout, so never trim
        // mid-playback; the memory saving still happens when paused/stopped.
        if AUDIO_ACTIVE.load(Ordering::Relaxed) {
            crate::logger::log(level, "memory", &format!("{tag}-skip: audio playing"));
            return;
        }
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
pub use imp::{log_stats, set_audio_active, trim_working_set};

#[cfg(not(windows))]
pub fn log_stats(_tag: &str, _level: &str) {}

#[cfg(not(windows))]
pub fn set_audio_active(_active: bool) {}

#[cfg(not(windows))]
pub fn trim_working_set(_tag: &str, _level: &str) {}
