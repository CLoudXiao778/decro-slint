//! Pre-decoded audio buffer for the real-time output thread.
//!
//! rodio decodes samples on the cpal output callback by default. A short CPU
//! spike in that thread is directly audible as a dropout. `PrefetchSource`
//! decodes ahead on a background thread into a fixed-size queue (15 seconds)
//! and the output callback only copies already-decoded samples.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use rodio::source::SeekError;
use rodio::{Decoder, Source};

/// How much audio to keep decoded ahead of playback.
pub const PREFETCH_SECONDS: usize = 15;
/// Wait for this much audio before handing the source to the sink, so playback
/// never starts with an empty buffer.
const PREFILL_SECONDS: usize = 1;
/// Decode/publish in chunks instead of one sample at a time. Keeps lock
/// contention low while still allowing a seek to take effect quickly.
const CHUNK_SAMPLES: usize = 8192;
const BACKPRESSURE_SLEEP: Duration = Duration::from_millis(5);
const IDLE_SLEEP: Duration = Duration::from_millis(20);
const PREFILL_TIMEOUT: Duration = Duration::from_secs(3);

struct State {
    samples: VecDeque<i16>,
    finished: bool,
    error: Option<String>,
    stop: bool,
    seek: Option<Duration>,
}

struct Shared {
    state: Mutex<State>,
    capacity_samples: usize,
}

/// A rodio source backed by a background decoder thread.
///
/// `try_seek` is asynchronous: it clears the queued samples and asks the
/// decoder thread to seek, then returns immediately. The output thread emits
/// silence for the few milliseconds until the new position is decoded.
pub struct PrefetchSource {
    shared: Arc<Shared>,
    chunk: Vec<i16>,
    chunk_pos: usize,
    channels: u16,
    sample_rate: u32,
    total_duration: Option<Duration>,
    underrun_logged: bool,
}

impl PrefetchSource {
    pub fn new(mut decoder: Decoder<BufReader<File>>) -> Self {
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();
        let total_duration = decoder.total_duration();
        let frames_per_second = (sample_rate as usize).saturating_mul(channels as usize);
        let capacity_samples = frames_per_second
            .saturating_mul(PREFETCH_SECONDS)
            .max(CHUNK_SAMPLES * 2);
        let prefill_samples = frames_per_second
            .saturating_mul(PREFILL_SECONDS)
            .min(capacity_samples);

        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                samples: VecDeque::with_capacity(capacity_samples.min(1 << 20)),
                finished: false,
                error: None,
                stop: false,
                seek: None,
            }),
            capacity_samples,
        });

        let thread_shared = shared.clone();
        let spawned = thread::Builder::new()
            .name("decro-audio-prefetch".into())
            .spawn(move || decode_ahead(&mut decoder, &thread_shared));
        if let Err(error) = spawned {
            if let Ok(mut state) = shared.state.lock() {
                state.error = Some(format!("解码缓冲线程启动失败: {error}"));
                state.finished = true;
            }
            crate::logger::log(
                "error",
                "audio",
                &format!("prefetch thread failed: {error}"),
            );
        }

        // Block the audio control thread (not the output callback) until a
        // little audio is ready. Decoding one second is normally a few ms.
        let started = Instant::now();
        loop {
            let ready = shared
                .state
                .lock()
                .map(|state| {
                    state.samples.len() >= prefill_samples
                        || state.finished
                        || state.error.is_some()
                })
                .unwrap_or(true);
            if ready {
                break;
            }
            if started.elapsed() >= PREFILL_TIMEOUT {
                crate::logger::log("warn", "audio", "prefetch prefill timed out");
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        Self {
            shared,
            chunk: Vec::with_capacity(CHUNK_SAMPLES),
            chunk_pos: 0,
            channels,
            sample_rate,
            total_duration,
            underrun_logged: false,
        }
    }
}

impl Iterator for PrefetchSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.chunk_pos < self.chunk.len() {
            let sample = self.chunk[self.chunk_pos];
            self.chunk_pos += 1;
            return Some(sample);
        }

        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            // Never block the real-time output thread on the decoder thread.
            // The producer holds this lock only while publishing a chunk.
            Err(TryLockError::WouldBlock) => return Some(0),
            Err(TryLockError::Poisoned(_)) => return None,
        };
        if state.samples.is_empty() {
            if state.finished {
                return None;
            }
            drop(state);
            if !self.underrun_logged {
                self.underrun_logged = true;
                crate::logger::log("warn", "audio", "prefetch underrun: buffer empty");
            }
            // Keep the sink alive while the decoder thread catches up. A
            // dropout here is still better than ending the track early.
            return Some(0);
        }

        let take = state.samples.len().min(CHUNK_SAMPLES);
        self.chunk.clear();
        self.chunk.extend(state.samples.drain(..take));
        self.chunk_pos = 1;
        self.chunk.first().copied()
    }
}

impl Source for PrefetchSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.chunk.clear();
        self.chunk_pos = 0;
        let mut state = match self.shared.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Err(SeekError::NotSupported {
                    underlying_source: "PrefetchSource",
                })
            }
        };
        state.samples.clear();
        state.finished = false;
        state.error = None;
        state.seek = Some(pos);
        Ok(())
    }
}

impl Drop for PrefetchSource {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
            state.samples.clear();
        }
    }
}

struct FinishedGuard<'a>(&'a Shared);

impl Drop for FinishedGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.finished = true;
        }
    }
}

fn decode_ahead(decoder: &mut Decoder<BufReader<File>>, shared: &Shared) {
    // If the thread exits for any reason, let the source drain what it has
    // and then end instead of emitting silence forever.
    let _finished = FinishedGuard(shared);
    let mut chunk = vec![0i16; CHUNK_SAMPLES];
    loop {
        enum Action {
            Stop,
            Seek(Duration),
            Idle,
            Full,
            Decode,
        }

        let action = match shared.state.lock() {
            Ok(mut state) => {
                if state.stop {
                    Action::Stop
                } else if let Some(target) = state.seek.take() {
                    state.samples.clear();
                    state.finished = false;
                    state.error = None;
                    Action::Seek(target)
                } else if state.finished || state.error.is_some() {
                    Action::Idle
                } else if state.samples.len() >= shared.capacity_samples {
                    Action::Full
                } else {
                    Action::Decode
                }
            }
            Err(_) => Action::Stop,
        };

        match action {
            Action::Stop => return,
            Action::Seek(target) => {
                if let Err(error) = decoder.try_seek(target) {
                    if let Ok(mut state) = shared.state.lock() {
                        state.error = Some(format!("音频跳转失败: {error}"));
                        state.finished = true;
                    }
                    crate::logger::log("warn", "audio", &format!("prefetch seek failed: {error}"));
                }
            }
            Action::Idle => thread::sleep(IDLE_SLEEP),
            Action::Full => thread::sleep(BACKPRESSURE_SLEEP),
            Action::Decode => {
                let mut count = 0;
                while count < chunk.len() {
                    match decoder.next() {
                        Some(sample) => {
                            chunk[count] = sample;
                            count += 1;
                        }
                        None => break,
                    }
                }

                let mut state = match shared.state.lock() {
                    Ok(state) => state,
                    Err(_) => return,
                };
                if state.stop {
                    return;
                }
                // A seek arrived while this chunk was being decoded. Drop the
                // chunk and let the loop seek the decoder first.
                if state.seek.is_some() {
                    continue;
                }
                if count == 0 {
                    state.finished = true;
                } else {
                    state.samples.extend(chunk[..count].iter().copied());
                    if count < chunk.len() {
                        state.finished = true;
                    }
                }
            }
        }
    }
}
