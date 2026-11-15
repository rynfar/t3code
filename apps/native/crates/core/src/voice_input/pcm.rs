//! Pure-Rust PCM resampling, capture metering/chunking, i16/f32 conversions, and VAD framing.
//!
//! Mirrors `apps/web/src/speech/pcmResampler.ts`, `apps/web/src/speech/pcmWorklet.ts`,
//! and the Silero/energy hysteresis framing in `apps/server/src/speech/audio.ts`.

use std::f64::consts::PI;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
pub const CAPTURE_CHUNK_SAMPLES: usize = 8_000;
pub const MAX_CAPTURE_SECONDS: usize = 300;
pub const MAX_CAPTURE_SAMPLES: usize = (TARGET_SAMPLE_RATE as usize) * MAX_CAPTURE_SECONDS;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PcmResamplerError {
    #[error("Microphone sample rate must be at least 16 kHz.")]
    SampleRateTooLow,
    #[error("PCM byte length must be a multiple of {0} bytes.")]
    InvalidByteAlignment(usize),
}

/// Low-pass windowed-sinc resampler down to 16 kHz mono, preserving filter history and sample phase
/// across capture blocks (`apps/web/src/speech/pcmResampler.ts`).
#[derive(Debug, Clone)]
pub struct PcmResampler {
    coefficients: Vec<f64>,
    history: Vec<f32>,
    position: usize,
    phase: f64,
    ratio: f64,
}

impl PcmResampler {
    pub fn new(sample_rate: u32) -> Result<Self, PcmResamplerError> {
        if sample_rate < TARGET_SAMPLE_RATE {
            return Err(PcmResamplerError::SampleRateTooLow);
        }
        let ratio = (sample_rate as f64) / (TARGET_SAMPLE_RATE as f64);
        let tap_count = if sample_rate == TARGET_SAMPLE_RATE { 1 } else { 63 };
        let mut coefficients = vec![0.0_f64; tap_count];
        let history = vec![0.0_f32; tap_count];
        let middle = (tap_count - 1) as f64 / 2.0;
        let cutoff = 7_200.0 / (sample_rate as f64);
        let mut sum = 0.0_f64;

        for (i, coeff) in coefficients.iter_mut().enumerate() {
            let x = (i as f64) - middle;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * x).sin() / (PI * x)
            };
            let window = if middle == 0.0 {
                1.0
            } else {
                0.54 - 0.46 * ((PI * (i as f64)) / middle).cos()
            };
            *coeff = sinc * window;
            sum += *coeff;
        }
        for coeff in &mut coefficients {
            *coeff /= sum;
        }

        Ok(Self {
            coefficients,
            history,
            position: 0,
            phase: 0.0,
            ratio,
        })
    }

    pub const fn ratio(&self) -> f64 {
        self.ratio
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(((samples.len() as f64) / self.ratio).ceil() as usize + 1);
        let len = self.history.len();
        for &sample in samples {
            self.history[self.position] = sample;
            self.phase += 1.0;
            if self.phase >= self.ratio {
                self.phase -= self.ratio;
                let mut value = 0.0_f64;
                for (tap, &coeff) in self.coefficients.iter().enumerate() {
                    let index = (self.position + len - tap) % len;
                    value += f64::from(self.history[index]) * coeff;
                }
                output.push(value.clamp(-1.0, 1.0) as f32);
            }
            self.position = (self.position + 1) % len;
        }
        output
    }

    pub fn flush(&mut self) -> Vec<f32> {
        if self.coefficients.len() == 1 {
            Vec::new()
        } else {
            let zeros = vec![0.0_f32; self.history.len() - 1];
            self.push(&zeros)
        }
    }

    pub fn push_i16(&mut self, samples: &[f32]) -> Vec<i16> {
        f32_slice_to_i16(&self.push(samples))
    }

    pub fn flush_i16(&mut self) -> Vec<i16> {
        f32_slice_to_i16(&self.flush())
    }
}

/// Converts a single `[-1.0, 1.0]` normalized f32 sample to signed 16-bit PCM.
pub fn f32_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    let clamped = sample.clamp(-1.0, 1.0);
    let scaled = if clamped < 0.0 { clamped * 32_768.0 } else { clamped * 32_767.0 };
    scaled.round().clamp(-32_768.0, 32_767.0) as i16
}

pub fn i16_to_f32(sample: i16) -> f32 {
    if sample < 0 {
        f32::from(sample) / 32_768.0
    } else {
        f32::from(sample) / 32_767.0
    }
}

pub fn f32_slice_to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().copied().map(f32_to_i16).collect()
}

pub fn i16_slice_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().copied().map(i16_to_f32).collect()
}

pub fn f32_slice_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

pub fn i16_slice_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

pub fn le_bytes_to_f32_vec(bytes: &[u8]) -> Result<Vec<f32>, PcmResamplerError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(PcmResamplerError::InvalidByteAlignment(4));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

pub fn le_bytes_to_i16_vec(bytes: &[u8]) -> Result<Vec<i16>, PcmResamplerError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(PcmResamplerError::InvalidByteAlignment(2));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub enum PcmCaptureEvent {
    Level(f32),
    PcmChunk(Vec<f32>),
    Limit,
    Stopped,
}

/// Captures multi-channel microphone blocks, computes smoothed meter levels every 50 ms,
/// and resamples to 16 kHz mono half-second PCM chunks (`apps/web/src/speech/pcmWorklet.ts`).
#[derive(Debug, Clone)]
pub struct PcmCaptureProcessor {
    sample_rate: u32,
    resampler: PcmResampler,
    chunk: Vec<f32>,
    recording: bool,
    flushed: bool,
    samples: usize,
    level_samples: usize,
    level_energy: f64,
    level: f64,
    meter_only: bool,
}

impl PcmCaptureProcessor {
    pub fn new(sample_rate: u32, meter_only: bool) -> Result<Self, PcmResamplerError> {
        Ok(Self {
            sample_rate,
            resampler: PcmResampler::new(sample_rate)?,
            chunk: Vec::with_capacity(CAPTURE_CHUNK_SAMPLES),
            recording: false,
            flushed: false,
            samples: 0,
            level_samples: 0,
            level_energy: 0.0,
            level: 0.0,
            meter_only,
        })
    }

    pub const fn is_recording(&self) -> bool {
        self.recording
    }

    pub const fn meter_only(&self) -> bool {
        self.meter_only
    }

    pub const fn level(&self) -> f32 {
        self.level as f32
    }

    pub const fn total_samples(&self) -> usize {
        self.samples
    }

    pub fn start(&mut self) {
        self.recording = true;
    }

    pub fn stop(&mut self) -> Vec<PcmCaptureEvent> {
        let mut events = Vec::new();
        self.recording = false;
        if !self.meter_only && !self.flushed {
            let tail = self.resampler.flush();
            self.append(&tail, &mut events);
        }
        self.flushed = true;
        if !self.chunk.is_empty() {
            events.push(PcmCaptureEvent::PcmChunk(std::mem::take(&mut self.chunk)));
        }
        events.push(PcmCaptureEvent::Stopped);
        events
    }

    pub fn process(&mut self, channels: &[&[f32]]) -> Vec<PcmCaptureEvent> {
        let mut events = Vec::new();
        if !self.recording {
            return events;
        }
        let Some(first) = channels.first() else {
            return events;
        };
        let frame_len = first.len();
        if frame_len == 0 {
            return events;
        }
        let channel_count = channels.len() as f32;
        let mut mono = vec![0.0_f32; frame_len];
        for channel in channels {
            for (i, sample) in channel.iter().take(frame_len).enumerate() {
                mono[i] += *sample / channel_count;
            }
        }

        let rate = f64::from(self.sample_rate);
        let threshold = rate / 20.0;
        for &sample in &mono {
            let s = f64::from(sample);
            self.level_energy += s * s;
            self.level_samples += 1;
            if (self.level_samples as f64) >= threshold {
                let target = ((self.level_energy / (self.level_samples as f64)).sqrt() * 4.0).min(1.0);
                let time_constant = if target > self.level { 0.075 } else { 0.2 };
                let decay = (-(self.level_samples as f64) / rate / time_constant).exp();
                self.level = target + (self.level - target) * decay;
                events.push(PcmCaptureEvent::Level(self.level as f32));
                self.level_energy = 0.0;
                self.level_samples = 0;
            }
        }

        if !self.meter_only {
            let resampled = self.resampler.push(&mono);
            self.append(&resampled, &mut events);
        }
        events
    }

    fn append(&mut self, pcm: &[f32], events: &mut Vec<PcmCaptureEvent>) {
        for &sample in pcm {
            if self.samples == MAX_CAPTURE_SAMPLES {
                self.recording = false;
                events.push(PcmCaptureEvent::Limit);
                break;
            }
            self.chunk.push(sample);
            self.samples += 1;
            if self.chunk.len() == CAPTURE_CHUNK_SAMPLES {
                events.push(PcmCaptureEvent::PcmChunk(std::mem::take(&mut self.chunk)));
            }
        }
    }
}

/// Computes root-mean-square energy of `f32` PCM samples.
pub fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum_sq / (samples.len() as f64)).sqrt() as f32
}

/// Computes root-mean-square energy of normalized `i16` PCM samples (`0.0..=1.0`).
pub fn rms_energy_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let f = f64::from(i16_to_f32(s));
            f * f
        })
        .sum();
    (sum_sq / (samples.len() as f64)).sqrt() as f32
}

pub fn meter_level_from_rms(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        0.0
    } else {
        (rms * 4.0).min(1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    pub frame_samples: usize,
    pub energy_threshold: f32,
    pub prefill_frames: usize,
    pub onset_frames: usize,
    pub hangover_frames: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        // Matches `apps/server/src/speech/audio.ts` (450ms prefill, 60ms onset, 450ms hangover at 16 kHz / 512-sample frames).
        let frame_samples = 512;
        let rate = TARGET_SAMPLE_RATE as usize;
        Self {
            frame_samples,
            energy_threshold: 0.015,
            prefill_frames: (450 * rate).div_ceil(1000 * frame_samples),
            onset_frames: (60 * rate).div_ceil(1000 * frame_samples),
            hangover_frames: (450 * rate).div_ceil(1000 * frame_samples),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VadFrame {
    pub samples: Vec<f32>,
    pub rms: f32,
    pub voiced: bool,
    pub keep: bool,
}

/// Fixed-size 16 kHz VAD framer with onset/prefill/hangover hysteresis.
#[derive(Debug, Clone)]
pub struct VadFramingState {
    config: VadConfig,
    pending: Vec<f32>,
    frames: Vec<VadFrame>,
    in_speech: bool,
    onset_count: usize,
    hangover: usize,
}

impl VadFramingState {
    pub fn new(config: VadConfig) -> Self {
        Self {
            config,
            pending: Vec::new(),
            frames: Vec::new(),
            in_speech: false,
            onset_count: 0,
            hangover: 0,
        }
    }

    pub fn push(&mut self, pcm_16khz: &[f32]) -> usize {
        self.pending.extend_from_slice(pcm_16khz);
        let mut produced = 0;
        while self.pending.len() >= self.config.frame_samples {
            let frame_samples: Vec<f32> = self.pending.drain(..self.config.frame_samples).collect();
            let rms = rms_energy(&frame_samples);
            let voiced = rms >= self.config.energy_threshold;
            self.push_scored_frame(frame_samples, rms, voiced);
            produced += 1;
        }
        produced
    }

    pub fn flush(&mut self) -> usize {
        if self.pending.is_empty() {
            return 0;
        }
        let frame_samples = std::mem::take(&mut self.pending);
        let rms = rms_energy(&frame_samples);
        let voiced = rms >= self.config.energy_threshold;
        self.push_scored_frame(frame_samples, rms, voiced);
        // Preserve a very short utterance stopped before onset confirms.
        if self.onset_count > 0 && !self.frames.is_empty() {
            let start = self.frames.len().saturating_sub(self.config.prefill_frames + 1);
            for frame in &mut self.frames[start..] {
                frame.keep = true;
            }
        }
        1
    }

    pub fn frames(&self) -> &[VadFrame] {
        &self.frames
    }

    pub fn kept_pcm(&self) -> Vec<f32> {
        let mut out = Vec::new();
        for frame in &self.frames {
            if frame.keep {
                out.extend_from_slice(&frame.samples);
            }
        }
        out
    }

    fn push_scored_frame(&mut self, samples: Vec<f32>, rms: f32, voiced: bool) {
        let idx = self.frames.len();
        let mut keep = false;
        if !self.in_speech && voiced {
            self.onset_count += 1;
            if self.onset_count >= self.config.onset_frames.max(1) {
                self.in_speech = true;
                self.hangover = self.config.hangover_frames;
                self.onset_count = 0;
                let prefill_start = idx.saturating_sub(self.config.prefill_frames);
                for prev in &mut self.frames[prefill_start..] {
                    prev.keep = true;
                }
                keep = true;
            }
        } else if self.in_speech && voiced {
            self.hangover = self.config.hangover_frames;
            keep = true;
        } else if self.in_speech && self.hangover > 0 {
            self.hangover -= 1;
            keep = true;
        } else {
            self.in_speech = false;
            self.onset_count = 0;
        }
        self.frames.push(VadFrame { samples, rms, voiced, keep });
    }
}
