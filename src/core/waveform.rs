//! Exhaustive PCM envelope reduction. Every sample belongs to exactly one
//! bucket, so a one-sample transient survives inside any bucket size.
//!
//! The hot loop walks interleaved frames once per bucket and updates every
//! channel as it goes, so memory is read exactly once regardless of channel
//! count. Buckets are independent, so rayon splits them across the pool with
//! `par_chunks_mut`; each bucket is still reduced sequentially, which is why
//! parallel and serial output are bit-identical.

use rayon::prelude::*;

use super::audio::Pcm;

pub const MAX_PEAK_COUNT: usize = 1_000_000;
pub const MAX_CHANNEL_BUCKETS: usize = 4_000_000;
/// Below this many interleaved samples the fork/join overhead outweighs the work.
pub const PARALLEL_THRESHOLD: usize = 262_144;

#[derive(Clone, Copy, Debug)]
pub enum Resolution {
  /// Target bucket count; capped at the frame count so no bucket is empty.
  PeakCount(usize),
  /// Fixed frames per bucket; the final bucket may be shorter.
  SamplesPerPeak(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct WaveformConfig {
  pub resolution: Resolution,
  pub parallel: bool,
}

impl Default for WaveformConfig {
  fn default() -> Self {
    Self {
      resolution: Resolution::PeakCount(1024),
      parallel: true,
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPeaks {
  pub min: Vec<f32>,
  pub max: Vec<f32>,
  pub peak: Vec<f32>,
  pub rms: Vec<f64>,
}

impl ChannelPeaks {
  fn with_capacity(count: usize) -> Self {
    Self {
      min: Vec::with_capacity(count),
      max: Vec::with_capacity(count),
      peak: Vec::with_capacity(count),
      rms: Vec::with_capacity(count),
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
  pub sample_rate: u32,
  pub channel_count: usize,
  pub frame_count: usize,
  pub duration_sec: f64,
  /// Half-open frame boundaries; bucket `i` covers `[boundaries[i], boundaries[i + 1])`.
  pub boundaries: Vec<u32>,
  pub channels: Vec<ChannelPeaks>,
  pub gapless_enabled: bool,
  pub skipped_packets: u32,
}

#[derive(Clone, Copy, Debug)]
struct Stats {
  min: f32,
  max: f32,
  energy: f64,
}

const EMPTY: Stats = Stats {
  min: f32::INFINITY,
  max: f32::NEG_INFINITY,
  energy: 0.0,
};

/// Reduce frames `[start, end)` into `slot`, one `Stats` per channel.
///
/// `f32::min`/`max` ignore NaN, so non-finite input is detected afterwards
/// through `energy`: a NaN or infinite sample poisons the squared sum, while
/// any finite `f32` squared and summed over `u32::MAX` frames stays finite.
fn reduce_bucket(samples: &[f32], channels: usize, start: usize, end: usize, slot: &mut [Stats]) {
  let frames = &samples[start * channels..end * channels];
  if channels == 1 {
    let stats = &mut slot[0];
    for &sample in frames {
      stats.min = stats.min.min(sample);
      stats.max = stats.max.max(sample);
      stats.energy += f64::from(sample) * f64::from(sample);
    }
    return;
  }
  for frame in frames.chunks_exact(channels) {
    for (stats, &sample) in slot.iter_mut().zip(frame) {
      stats.min = stats.min.min(sample);
      stats.max = stats.max.max(sample);
      stats.energy += f64::from(sample) * f64::from(sample);
    }
  }
}

pub fn peaks(pcm: &Pcm, config: WaveformConfig) -> Result<Waveform, String> {
  if pcm.channels == 0
    || pcm.sample_rate == 0
    || pcm.samples.is_empty()
    || !pcm.samples.len().is_multiple_of(pcm.channels)
  {
    return Err("PCM must contain complete sample frames, positive channels and sampleRate".into());
  }
  let frames = pcm.frames();
  if frames > u32::MAX as usize {
    return Err("waveform frame count exceeds Uint32 boundaries".into());
  }

  let (count, step) = match config.resolution {
    Resolution::PeakCount(0) | Resolution::SamplesPerPeak(0) => {
      return Err("peakCount and samplesPerPeak must be positive".into());
    }
    Resolution::PeakCount(count) => {
      if count > MAX_PEAK_COUNT {
        return Err(format!("peakCount exceeds {MAX_PEAK_COUNT}"));
      }
      (count.min(frames), None)
    }
    Resolution::SamplesPerPeak(step) => (frames.div_ceil(step), Some(step)),
  };
  if count > MAX_PEAK_COUNT {
    return Err(format!(
      "resolution produces more than {MAX_PEAK_COUNT} buckets"
    ));
  }
  if count
    .checked_mul(pcm.channels)
    .is_none_or(|total| total > MAX_CHANNEL_BUCKETS)
  {
    return Err(format!(
      "resolution produces more than {MAX_CHANNEL_BUCKETS} total channel-buckets"
    ));
  }

  let boundaries: Vec<u32> = (0..=count)
    .map(|i| match step {
      Some(step) => i.saturating_mul(step).min(frames) as u32,
      None => ((i as u64 * frames as u64) / count as u64) as u32,
    })
    .collect();

  let channels = pcm.channels;
  let mut stats = vec![EMPTY; count * channels];
  let reduce = |(i, slot): (usize, &mut [Stats])| {
    reduce_bucket(
      &pcm.samples,
      channels,
      boundaries[i] as usize,
      boundaries[i + 1] as usize,
      slot,
    );
  };
  let run_parallel = config.parallel && pcm.samples.len() >= PARALLEL_THRESHOLD && count > 1;
  if run_parallel {
    stats.par_chunks_mut(channels).enumerate().for_each(reduce);
  } else {
    stats.chunks_mut(channels).enumerate().for_each(reduce);
  }
  if stats.iter().any(|s| !s.energy.is_finite()) {
    return Err("PCM contains non-finite samples".into());
  }

  let mut out: Vec<ChannelPeaks> = (0..channels)
    .map(|_| ChannelPeaks::with_capacity(count))
    .collect();
  for (i, slot) in stats.chunks_exact(channels).enumerate() {
    let bucket_frames = f64::from(boundaries[i + 1] - boundaries[i]);
    for (channel, s) in out.iter_mut().zip(slot) {
      channel.min.push(s.min);
      channel.max.push(s.max);
      channel.peak.push(s.min.abs().max(s.max.abs()));
      channel.rms.push((s.energy / bucket_frames).sqrt());
    }
  }

  Ok(Waveform {
    sample_rate: pcm.sample_rate,
    channel_count: channels,
    frame_count: frames,
    duration_sec: pcm.duration_sec(),
    boundaries,
    channels: out,
    gapless_enabled: pcm.gapless_enabled,
    skipped_packets: pcm.skipped_packets,
  })
}
