//! Symphonia-backed decoding of the container's default audio track to
//! interleaved `f32` PCM. A port of audiodown's decoder over the `isomp4`
//! and `mkv` demuxers; the waveform reduction downstream is shared verbatim.

use std::io::{Cursor, ErrorKind};
use std::sync::Arc;

use symphonia::core::audio::AudioSpec;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use super::sniff::sniff;

pub const DEFAULT_MAX_DECODED_SAMPLES: usize = 64_000_000;

#[derive(Clone, Copy, Debug)]
pub struct DecodeConfig {
  /// Trim encoder delay / padding when the stream carries gapless metadata.
  pub gapless: bool,
  /// Fail on the first decoder error instead of skipping the packet.
  pub strict: bool,
  /// Upper bound on interleaved scalar samples kept in memory.
  pub max_decoded_samples: usize,
}

impl Default for DecodeConfig {
  fn default() -> Self {
    Self {
      gapless: true,
      strict: true,
      max_decoded_samples: DEFAULT_MAX_DECODED_SAMPLES,
    }
  }
}

/// Decoded audio. Samples are interleaved frames (`L0 R0 L1 R1 ...`) exactly
/// as the decoder produced them: no resampling, downmixing, clipping or
/// normalization.
#[derive(Clone, Debug)]
pub struct Pcm {
  pub samples: Vec<f32>,
  pub sample_rate: u32,
  pub channels: usize,
  pub gapless_enabled: bool,
  pub skipped_packets: u32,
}

impl Pcm {
  pub fn frames(&self) -> usize {
    self.samples.len() / self.channels
  }

  pub fn duration_sec(&self) -> f64 {
    self.frames() as f64 / f64::from(self.sample_rate)
  }
}

/// Why a container yielded no PCM, distinct from a decode failure.
#[derive(Debug)]
pub enum AudioOutcome {
  Pcm(Pcm),
  /// The container has no audio track.
  NoAudio,
  /// There is an audio track, but no decoder for its codec is compiled in
  /// (Opus in 0.1) or its parameters are unreadable.
  Unsupported(String),
}

/// Lets Symphonia read shared, immutable bytes without copying them.
struct SharedBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBytes {
  fn as_ref(&self) -> &[u8] {
    &self.0
  }
}

pub fn decode(data: Arc<Vec<u8>>, config: DecodeConfig) -> Result<AudioOutcome, String> {
  if config.max_decoded_samples == 0 {
    return Err("maxDecodedSamples must be positive".into());
  }
  let kind = sniff(&data).ok_or("unrecognized video magic")?;

  let mut hint = Hint::new();
  hint.with_extension(kind.as_str());
  hint.mime_type(kind.mime());
  let source = MediaSourceStream::new(
    Box::new(Cursor::new(SharedBytes(data))),
    MediaSourceStreamOptions::default(),
  );
  let mut reader = symphonia::default::get_probe()
    .probe(
      &hint,
      source,
      FormatOptions::default(),
      MetadataOptions::default(),
    )
    .map_err(|e| format!("video probe failed: {e}"))?;

  let Some(track) = reader.default_track(TrackType::Audio) else {
    return Ok(AudioOutcome::NoAudio);
  };
  let track_id = track.id;
  let params = match &track.codec_params {
    Some(CodecParameters::Audio(params)) => params,
    _ => {
      return Ok(AudioOutcome::Unsupported(
        "audio track has no readable codec parameters".into(),
      ));
    }
  };
  let mut decoder = match symphonia::default::get_codecs().make_audio_decoder(
    params,
    &AudioDecoderOptions::default().gapless(config.gapless),
  ) {
    Ok(decoder) => decoder,
    Err(e) => {
      return Ok(AudioOutcome::Unsupported(format!(
        "unsupported audio codec: {e}"
      )));
    }
  };

  let mut samples = Vec::new();
  let mut scratch: Vec<f32> = Vec::new();
  let mut spec: Option<AudioSpec> = None;
  let mut skipped_packets = 0u32;

  loop {
    let packet = match reader.next_packet() {
      Ok(Some(packet)) => packet,
      Ok(None) => break,
      Err(Error::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
      Err(e) => return Err(format!("audio demux failed: {e}")),
    };
    if packet.track_id != track_id {
      continue;
    }
    let decoded = match decoder.decode(&packet) {
      Ok(buffer) => buffer,
      Err(Error::DecodeError(_) | Error::ResetRequired) if !config.strict => {
        decoder.reset();
        skipped_packets = skipped_packets.saturating_add(1);
        continue;
      }
      Err(e) => return Err(format!("audio decode failed: {e}")),
    };

    let current = decoded.spec();
    let channels = current.channels().count();
    if current.rate() == 0 || channels == 0 {
      return Err("invalid decoded signal specification".into());
    }
    match &spec {
      Some(previous) if previous != current => {
        return Err("audio signal specification changed midstream".into());
      }
      Some(_) => {}
      None => spec = Some(current.clone()),
    }

    let additional = decoded
      .frames()
      .checked_mul(channels)
      .ok_or("decoded sample count overflow")?;
    let next = samples
      .len()
      .checked_add(additional)
      .ok_or("decoded sample count overflow")?;
    if next > config.max_decoded_samples {
      return Err("decoded audio exceeds maxDecodedSamples".into());
    }

    scratch.clear();
    decoded.copy_to_vec_interleaved::<f32>(&mut scratch);
    if scratch.iter().any(|v| !v.is_finite()) {
      return Err("decoded PCM contains non-finite samples".into());
    }
    samples
      .try_reserve(scratch.len())
      .map_err(|e| format!("PCM allocation failed: {e}"))?;
    samples.extend_from_slice(&scratch);
  }

  let spec = spec.ok_or("audio track contained no decodable packets")?;
  if samples.is_empty() {
    return Err("audio track contained no decoded samples".into());
  }
  Ok(AudioOutcome::Pcm(Pcm {
    samples,
    sample_rate: spec.rate(),
    channels: spec.channels().count(),
    gapless_enabled: config.gapless,
    skipped_packets,
  }))
}
