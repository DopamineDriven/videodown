//! Pure Rust core. Nothing in here depends on napi, so it is unit-testable
//! with plain `cargo test` and reusable outside the Node binding.

pub(crate) mod audio;
pub(crate) mod boxes;
pub(crate) mod meta;
pub(crate) mod mkv;
pub(crate) mod mp4;
pub(crate) mod sniff;
pub(crate) mod waveform;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use audio::{AudioOutcome, DecodeConfig, decode};
use meta::{VideoMeta, parse_video};
use waveform::{Waveform, WaveformConfig, peaks};

/// What the audio-track envelope call produced, so a missing waveform is
/// never confused with a failure.
#[derive(Debug)]
pub(crate) enum WaveformOutcome {
  Peaks(Waveform),
  NoAudio,
  Unsupported(String),
  /// The buffer is a prefix of the file; decoding it would yield a partial
  /// envelope presented as whole, so none is produced.
  Truncated,
}

pub(crate) fn audio_peaks(
  data: Arc<Vec<u8>>,
  decode_config: DecodeConfig,
  waveform_config: WaveformConfig,
) -> Result<WaveformOutcome, String> {
  if meta::is_truncated(&data)? {
    return Ok(WaveformOutcome::Truncated);
  }
  match decode(data, decode_config)? {
    AudioOutcome::Pcm(pcm) => peaks(&pcm, waveform_config).map(WaveformOutcome::Peaks),
    AudioOutcome::NoAudio => Ok(WaveformOutcome::NoAudio),
    AudioOutcome::Unsupported(reason) => Ok(WaveformOutcome::Unsupported(reason)),
  }
}

/// Metadata plus the audio-track envelope from one input snapshot.
///
/// Header parsing touches a few kilobytes; decoding walks the whole audio
/// track. They are independent, so they run under `rayon::join`, and the
/// waveform reduction itself fans out across the pool.
pub(crate) fn analyze(
  data: Arc<Vec<u8>>,
  decode_config: DecodeConfig,
  waveform_config: WaveformConfig,
) -> Result<(VideoMeta, WaveformOutcome), String> {
  let (meta, wave) = rayon::join(
    || parse_video(&data),
    || audio_peaks(Arc::clone(&data), decode_config, waveform_config),
  );
  Ok((meta?, wave?))
}
