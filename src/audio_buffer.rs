//! Pre-decoded audio buffer for the real-time output thread.
//!
//! rodio decodes samples on the cpal output callback by default. A short CPU
//! spike in that thread is directly audible as a dropout. `PrefetchSource`
//! decodes ahead on a background thread into a fixed-size queue (15 seconds)
//! and the output callback only copies already-decoded samples.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use rodio::source::SeekError;
use rodio::{Decoder, Source};

/// How much audio to keep decoded ahead of playback.
pub const PREFETCH_SECONDS: usize = 15;
/// Default wait before handing the source to the sink, so playback never
/// starts with an empty buffer.
const PREFILL_SECONDS: f32 = 1.0;
/// Amount decoded after a seek before the callback is allowed to continue.
const SEEK_PREFILL_SECONDS: f32 = 0.5;
/// Decode/publish in chunks instead of one sample at a time. Keeps lock
/// contention low while still allowing a seek to take effect quickly.
const CHUNK_SAMPLES: usize = 8192;
const BACKPRESSURE_SLEEP: Duration = Duration::from_millis(5);
const IDLE_SLEEP: Duration = Duration::from_millis(20);
const PREFILL_TIMEOUT: Duration = Duration::from_secs(3);
const SEEK_TIMEOUT: Duration = Duration::from_secs(3);

struct State {
    samples: VecDeque<i16>,
    finished: bool,
    error: Option<String>,
    stop: bool,
    seek: Option<Duration>,
    seek_seq: u64,
    seek_done_seq: u64,
    seek_result: Option<Result<(), String>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    capacity_samples: usize,
    seek_prefill: usize,
}

/// A rodio source backed by a background decoder thread.
///
/// `try_seek` blocks the audio callback until the decoder has actually seeked
/// and refilled a small buffer. This keeps rodio's position tracking in sync
/// with what is audible and makes seek failures explicit instead of turning
/// them into a bogus "track ended" event.
pub struct PrefetchSource {
    shared: Arc<Shared>,
    chunk: Vec<i16>,
    chunk_pos: usize,
    channels: u16,
    sample_rate: u32,
    total_duration: Option<Duration>,
    underrun_logged: bool,
    seeking: bool,
    silence_left: usize,
}

impl PrefetchSource {
    pub fn new(decoder: Decoder<BufReader<File>>) -> Self {
        Self::new_with_prefill(decoder, PREFILL_SECONDS)
    }

    pub fn new_with_prefill(mut decoder: Decoder<BufReader<File>>, prefill_seconds: f32) -> Self {
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();
        let total_duration = decoder.total_duration();
        let frames_per_second = (sample_rate as usize).saturating_mul(channels as usize);
        let capacity_samples = frames_per_second
            .saturating_mul(PREFETCH_SECONDS)
            .max(CHUNK_SAMPLES * 2);
        let prefill_samples = ((frames_per_second as f32) * prefill_seconds.max(0.0)) as usize;
        let prefill_samples = prefill_samples.min(capacity_samples);
        let seek_prefill = ((frames_per_second as f32) * SEEK_PREFILL_SECONDS) as usize;
        let seek_prefill = seek_prefill.max(CHUNK_SAMPLES);

        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                samples: VecDeque::with_capacity(capacity_samples.min(1 << 20)),
                finished: false,
                error: None,
                stop: false,
                seek: None,
                seek_seq: 0,
                seek_done_seq: 0,
                seek_result: None,
            }),
            wake: Condvar::new(),
            capacity_samples,
            seek_prefill,
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
            seeking: false,
            silence_left: 0,
        }
    }
}

/// Emit one full frame of silence. Partial frames would shift the
/// interleaved L/R sample stream and permanently swap channels.
fn take_silence(silence_left: &mut usize, channels: u16) -> i16 {
    if *silence_left == 0 {
        *silence_left = channels.max(1) as usize;
    }
    *silence_left -= 1;
    0
}

impl Iterator for PrefetchSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let mut silence_left = self.silence_left;
        let result = self.next_with_silence(&mut silence_left);
        self.silence_left = silence_left;
        result
    }
}

impl PrefetchSource {
    fn next_with_silence(&mut self, silence_left: &mut usize) -> Option<i16> {
        if *silence_left > 0 {
            *silence_left -= 1;
            return Some(0);
        }
        if self.chunk_pos < self.chunk.len() {
            let sample = self.chunk[self.chunk_pos];
            self.chunk_pos += 1;
            return Some(sample);
        }

        let locked = self.shared.state.try_lock();
        let mut state = match locked {
            Ok(state) => state,
            // Never block the real-time output thread on the decoder thread.
            Err(TryLockError::WouldBlock) => {
                return Some(take_silence(silence_left, self.channels))
            }
            Err(TryLockError::Poisoned(_)) => return None,
        };
        if state.samples.is_empty() {
            // A fatal decode error ends the track; a failed seek leaves the
            // source alive with silence so it is not mistaken for "ended".
            if state.finished && state.error.is_none() {
                return None;
            }
            let seeking = self.seeking;
            drop(state);
            if !seeking && !self.underrun_logged {
                self.underrun_logged = true;
                crate::logger::log("warn", "audio", "prefetch underrun: buffer empty");
            }
            return Some(take_silence(silence_left, self.channels));
        }

        let take = state.samples.len().min(CHUNK_SAMPLES);
        self.chunk.clear();
        self.chunk.extend(state.samples.drain(..take));
        self.chunk_pos = 1;
        *silence_left = 0;
        self.seeking = false;
        self.underrun_logged = false;
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
        self.silence_left = 0;
        self.seeking = true;

        let mut state = match self.shared.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Err(SeekError::NotSupported {
                    underlying_source: "PrefetchSource",
                })
            }
        };
        state.seek_seq = state.seek_seq.wrapping_add(1);
        let seq = state.seek_seq;
        state.seek = Some(pos);
        state.samples.clear();
        state.finished = false;
        state.error = None;
        state.seek_result = None;
        self.shared.wake.notify_all();

        let deadline = Instant::now() + SEEK_TIMEOUT;
        while state.seek_done_seq != seq && !state.stop {
            let (guard, timeout) = match self.shared.wake.wait_timeout(state, SEEK_TIMEOUT) {
                Ok(result) => result,
                Err(_) => {
                    return Err(SeekError::NotSupported {
                        underlying_source: "PrefetchSource",
                    })
                }
            };
            state = guard;
            if timeout.timed_out() && Instant::now() >= deadline {
                crate::logger::log("warn", "audio", "prefetch seek timed out");
                return Err(SeekError::NotSupported {
                    underlying_source: "PrefetchSource",
                });
            }
        }
        if state.stop {
            return Err(SeekError::NotSupported {
                underlying_source: "PrefetchSource",
            });
        }
        match state.seek_result.take() {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => {
                crate::logger::log("warn", "audio", &format!("prefetch seek failed: {error}"));
                Err(SeekError::NotSupported {
                    underlying_source: "PrefetchSource",
                })
            }
            None => Err(SeekError::NotSupported {
                underlying_source: "PrefetchSource",
            }),
        }
    }
}

impl Drop for PrefetchSource {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
            state.samples.clear();
        }
        self.shared.wake.notify_all();
    }
}

struct FinishedGuard<'a>(&'a Shared);

impl Drop for FinishedGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.finished = true;
        }
        self.0.wake.notify_all();
    }
}

/// Decode one chunk and publish it. Returns false when the caller must stop
/// (shutdown or a newer seek arrived while decoding).
fn decode_chunk(
    decoder: &mut Decoder<BufReader<File>>,
    chunk: &mut [i16],
    shared: &Shared,
) -> bool {
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
        Err(_) => return false,
    };
    if state.stop || state.seek.is_some() {
        return false;
    }
    if count == 0 {
        state.finished = true;
    } else {
        state.samples.extend(chunk[..count].iter().copied());
        if count < chunk.len() {
            state.finished = true;
        }
    }
    true
}

fn decode_ahead(decoder: &mut Decoder<BufReader<File>>, shared: &Shared) {
    // If the thread exits for any reason, let the source drain what it has
    // and then end instead of emitting silence forever.
    let _finished = FinishedGuard(shared);
    let mut chunk = vec![0i16; CHUNK_SAMPLES];
    loop {
        enum Action {
            Stop,
            Seek { target: Duration, seq: u64 },
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
                    Action::Seek {
                        target,
                        seq: state.seek_seq,
                    }
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
            Action::Seek { target, seq } => {
                let result = decoder
                    .try_seek(target)
                    .map_err(|error| format!("{error:?}"));
                if result.is_ok() {
                    // Refill before releasing the audio callback so rodio's
                    // position is not inflated by silence frames.
                    loop {
                        let len = shared
                            .state
                            .lock()
                            .map(|state| state.samples.len())
                            .unwrap_or(0);
                        if len >= shared.seek_prefill {
                            break;
                        }
                        if !decode_chunk(decoder, &mut chunk, shared) {
                            break;
                        }
                    }
                } else if let Ok(mut state) = shared.state.lock() {
                    state.error = Some("解码器不支持跳转到该位置".to_string());
                    state.samples.clear();
                }
                if let Ok(mut state) = shared.state.lock() {
                    state.seek_result = Some(result);
                    state.seek_done_seq = seq;
                }
                shared.wake.notify_all();
            }
            Action::Idle => thread::sleep(IDLE_SLEEP),
            Action::Full => thread::sleep(BACKPRESSURE_SLEEP),
            Action::Decode => {
                decode_chunk(decoder, &mut chunk, shared);
            }
        }
    }
}
