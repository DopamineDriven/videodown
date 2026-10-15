#![deny(clippy::all)]

//! `@d0paminedriven/videodown`: MP4 / MOV / WebM / MKV container metadata,
//! the keyframe index and audio-track waveform peaks for Node. Demux and
//! headers only: no video decoding, so the crate stays pure Rust and builds
//! for every target including wasm.
//!
//! Layout follows audiodown: this file is the Node-API surface (functions,
//! promise-returning async variants, classes), `types.rs` holds the JS
//! boundary types and `core/` is pure Rust.
//!
//! Every `*Async` function and method returns an [`AsyncBlock`]: the input is
//! snapshotted synchronously on the JS thread, the CPU work runs on tokio's
//! blocking pool via [`spawn_blocking`], and the result is converted to JS
//! values back on the JS thread when the promise resolves. Nothing blocks the
//! event loop, and nothing competes with Node's libuv pool.

use std::sync::Arc;

use napi::Env;
use napi::bindgen_prelude::*;
use napi_derive::napi;

mod core;
mod types;

pub use types::{
  AudioTrack, CoverArt, CoverArtInfo, DecodeOptions, DecodedAudio, KeyframeIndex, KeyframeSource,
  Orientation, VideoAnalysis, VideoBrands, VideoContainer, VideoKind, VideoSpecs, VideoTrack,
  WaveformChannel, WaveformOptions, WaveformPeaks, WaveformStatus,
};

use crate::core::WaveformOutcome;
use crate::core::audio::{self, AudioOutcome, DecodeConfig, Pcm};
use crate::core::meta::{self, VideoMeta};
use crate::core::sniff;
use crate::core::waveform::{self, Resolution, Waveform, WaveformConfig};

// ── Errors ──────────────────────────────────────────────────────

fn fail(message: impl Into<String>) -> Error {
  Error::new(Status::GenericFailure, message.into())
}

fn invalid(message: impl Into<String>) -> Error {
  Error::new(Status::InvalidArg, message.into())
}

const PREFIX_MESSAGE: &str =
  "buffer is a prefix of the file; fetch the whole object before decoding its audio";

/// A JS number that must be an integer in `1..=max`. Rejects `1.5`, `NaN`
/// and `Infinity` instead of silently truncating them.
fn integer(value: f64, name: &str, max: usize) -> Result<usize> {
  if !value.is_finite() || value.fract() != 0.0 || value < 1.0 || value > max as f64 {
    return Err(invalid(format!("{name} must be an integer in 1..={max}")));
  }
  Ok(value as usize)
}

fn or_null<T>(value: Option<T>) -> Either<T, Null> {
  match value {
    Some(v) => Either::A(v),
    None => Either::B(Null),
  }
}

// ── Promises ────────────────────────────────────────────────────

/// Run CPU-bound `work` on tokio's blocking pool and resolve a JS promise
/// with it. `map` runs on the JS thread once the work finishes, which is where
/// typed arrays and class instances have to be created.
fn promise<V, T, W, M>(env: &Env, work: W, map: M) -> Result<AsyncBlock<T>>
where
  V: Send + 'static,
  T: ToNapiValue + 'static,
  W: FnOnce() -> Result<V> + Send + 'static,
  M: FnOnce(Env, V) -> Result<T> + 'static,
{
  // Native: hand the CPU work to tokio's blocking pool so runtime workers
  // stay free. wasm32-wasip1-threads: run it inline on the runtime worker
  // instead. `spawn_blocking` there creates a thread from a non-JS thread,
  // which the wasm runtime can only satisfy by relaying through the JS
  // thread, and that relay deadlocks under concurrent load.
  #[cfg(not(target_family = "wasm"))]
  let future = async move {
    spawn_blocking(work)
      .await
      .map_err(|e| fail(format!("videodown worker failed: {e}")))?
  };
  #[cfg(target_family = "wasm")]
  let future = async move { work() };
  AsyncBlockBuilder::build_with_map(env, future, map)
}

// ── Option resolution ───────────────────────────────────────────

fn decode_config(o: &DecodeOptions) -> Result<DecodeConfig> {
  Ok(DecodeConfig {
    gapless: o.gapless.unwrap_or(true),
    strict: o.strict.unwrap_or(true),
    max_decoded_samples: integer(
      o.max_decoded_samples
        .unwrap_or(audio::DEFAULT_MAX_DECODED_SAMPLES as f64),
      "maxDecodedSamples",
      u32::MAX as usize,
    )?,
  })
}

fn waveform_configs(o: &WaveformOptions) -> Result<(DecodeConfig, WaveformConfig)> {
  if o.peak_count.is_some() && o.samples_per_peak.is_some() {
    return Err(invalid("pass either peakCount or samplesPerPeak, not both"));
  }
  let resolution = if let Some(step) = o.samples_per_peak {
    Resolution::SamplesPerPeak(integer(step, "samplesPerPeak", u32::MAX as usize)?)
  } else {
    Resolution::PeakCount(integer(
      o.peak_count.unwrap_or(1024.0),
      "peakCount",
      waveform::MAX_PEAK_COUNT,
    )?)
  };
  Ok((
    decode_config(&o.decode_options())?,
    WaveformConfig {
      resolution,
      parallel: o.parallel.unwrap_or(true),
    },
  ))
}

// ── Core wrappers ───────────────────────────────────────────────

fn snapshot(data: &[u8]) -> Arc<Vec<u8>> {
  Arc::new(data.to_vec())
}

fn parse_meta(data: &[u8]) -> Result<VideoMeta> {
  meta::parse_video(data).map_err(fail)
}

fn track_duration(m: &VideoMeta) -> Option<f64> {
  m.video
    .as_ref()
    .and_then(|v| v.duration_sec)
    .or(m.duration_sec)
}

fn specs(data: &[u8], source: Option<String>) -> Result<VideoSpecs> {
  Ok(types::video_specs(parse_meta(data)?, data.len(), source))
}

fn specs_async(
  env: &Env,
  data: Arc<Vec<u8>>,
  source: Option<String>,
) -> Result<AsyncBlock<VideoSpecs>> {
  let len = data.len();
  promise(
    env,
    move || parse_meta(&data),
    move |_, m| Ok(types::video_specs(m, len, source)),
  )
}

fn index(data: &[u8]) -> Result<KeyframeIndex> {
  let m = parse_meta(data)?;
  Ok(types::keyframe_index(&m.keyframes, track_duration(&m)))
}

fn index_async(env: &Env, data: Arc<Vec<u8>>) -> Result<AsyncBlock<KeyframeIndex>> {
  promise(
    env,
    move || parse_meta(&data),
    |_, m| Ok(types::keyframe_index(&m.keyframes, track_duration(&m))),
  )
}

/// `Ok(None)` only when the container has no audio track. A prefix buffer
/// and an undecodable codec are errors here; `analyze` reports them as a
/// status instead.
fn decode_track(data: Arc<Vec<u8>>, config: DecodeConfig) -> Result<Option<Pcm>> {
  if meta::is_truncated(&data).map_err(fail)? {
    return Err(fail(PREFIX_MESSAGE));
  }
  match audio::decode(data, config).map_err(fail)? {
    AudioOutcome::Pcm(pcm) => Ok(Some(pcm)),
    AudioOutcome::NoAudio => Ok(None),
    AudioOutcome::Unsupported(reason) => Err(fail(reason)),
  }
}

fn decode_track_async(
  env: &Env,
  data: Arc<Vec<u8>>,
  config: DecodeConfig,
) -> Result<AsyncBlock<Either<DecodedAudio, Null>>> {
  promise(
    env,
    move || decode_track(data, config),
    |_, pcm| Ok(or_null(pcm.map(Into::into))),
  )
}

fn peaks(data: Arc<Vec<u8>>, dc: DecodeConfig, wc: WaveformConfig) -> Result<Option<Waveform>> {
  match core::audio_peaks(data, dc, wc).map_err(fail)? {
    WaveformOutcome::Peaks(w) => Ok(Some(w)),
    WaveformOutcome::NoAudio => Ok(None),
    WaveformOutcome::Unsupported(reason) => Err(fail(reason)),
    WaveformOutcome::Truncated => Err(fail(PREFIX_MESSAGE)),
  }
}

fn peaks_async(
  env: &Env,
  data: Arc<Vec<u8>>,
  dc: DecodeConfig,
  wc: WaveformConfig,
) -> Result<AsyncBlock<Either<WaveformPeaks, Null>>> {
  promise(
    env,
    move || peaks(data, dc, wc),
    |_, w| Ok(or_null(w.map(Into::into))),
  )
}

/// The embedded poster's bytes, copied out of the buffer.
fn cover_bytes(m: &VideoMeta, data: &[u8]) -> Option<Vec<u8>> {
  m.cover_art
    .as_ref()
    .and_then(|c| c.bytes(data))
    .map(<[u8]>::to_vec)
}

fn cover(data: &[u8]) -> Result<Option<(meta::CoverArt, Vec<u8>)>> {
  let m = parse_meta(data)?;
  Ok(
    cover_bytes(&m, data)
      .zip(m.cover_art)
      .map(|(bytes, c)| (c, bytes)),
  )
}

fn cover_async(env: &Env, data: Arc<Vec<u8>>) -> Result<AsyncBlock<Either<CoverArt, Null>>> {
  promise(
    env,
    move || cover(&data),
    |_, found| Ok(or_null(found.map(|(c, bytes)| types::cover_art(&c, bytes)))),
  )
}

fn analysis(
  data: Arc<Vec<u8>>,
  dc: DecodeConfig,
  wc: WaveformConfig,
  source: Option<String>,
) -> Result<VideoAnalysis> {
  let len = data.len();
  let (m, outcome) = core::analyze(Arc::clone(&data), dc, wc).map_err(fail)?;
  let cover = cover_bytes(&m, &data);
  Ok(types::video_analysis(m, outcome, cover, len, source))
}

fn analysis_async(
  env: &Env,
  data: Arc<Vec<u8>>,
  dc: DecodeConfig,
  wc: WaveformConfig,
  source: Option<String>,
) -> Result<AsyncBlock<VideoAnalysis>> {
  let len = data.len();
  promise(
    env,
    move || {
      let (m, outcome) = core::analyze(Arc::clone(&data), dc, wc).map_err(fail)?;
      let cover = cover_bytes(&m, &data);
      Ok((m, outcome, cover))
    },
    move |_, (m, outcome, cover)| Ok(types::video_analysis(m, outcome, cover, len, source)),
  )
}

// ── Standalone functions ────────────────────────────────────────

/// Cheap magic sniff: `'mp4'`, `'mov'`, `'webm'`, `'mkv'` or `null`.
#[napi]
pub fn sniff_video(data: &[u8]) -> Option<VideoKind> {
  sniff::sniff(data).map(Into::into)
}

/// Container metadata from the headers alone. Hand it a prefix and read
/// `complete`: `false` means `moov` was not inside the bytes supplied.
#[napi(ts_return_type = "VideoSpecs")]
pub fn parse_video(data: &[u8], source: Option<String>) -> Result<VideoSpecs> {
  specs(data, source)
}

#[napi(ts_return_type = "Promise<VideoSpecs>")]
pub fn parse_video_async(
  env: &Env,
  data: &[u8],
  source: Option<String>,
) -> Result<AsyncBlock<VideoSpecs>> {
  specs_async(env, snapshot(data), source)
}

/// Sync-sample presentation times from the container's own index; no decoding.
#[napi]
pub fn keyframes(data: &[u8]) -> Result<KeyframeIndex> {
  index(data)
}

#[napi]
pub fn keyframes_async(env: &Env, data: &[u8]) -> Result<AsyncBlock<KeyframeIndex>> {
  index_async(env, snapshot(data))
}

/// The embedded poster (ISOBMFF `covr`, or a Matroska image attachment) with
/// its bytes; `null` when the container carries none. Header-only, no decoder.
#[napi]
pub fn cover_art(data: &[u8]) -> Result<Either<CoverArt, Null>> {
  Ok(or_null(
    cover(data)?.map(|(c, bytes)| types::cover_art(&c, bytes)),
  ))
}

#[napi]
pub fn cover_art_async(env: &Env, data: &[u8]) -> Result<AsyncBlock<Either<CoverArt, Null>>> {
  cover_async(env, snapshot(data))
}

/// The default audio track as interleaved `f32` PCM; `null` when the
/// container has no audio track. Throws for a prefix buffer or a codec no
/// decoder is compiled in for.
#[napi]
pub fn decode_audio(
  data: &[u8],
  options: Option<DecodeOptions>,
) -> Result<Either<DecodedAudio, Null>> {
  let config = decode_config(&options.unwrap_or_default())?;
  Ok(or_null(
    decode_track(snapshot(data), config)?.map(Into::into),
  ))
}

#[napi]
pub fn decode_audio_async(
  env: &Env,
  data: &[u8],
  options: Option<DecodeOptions>,
) -> Result<AsyncBlock<Either<DecodedAudio, Null>>> {
  let config = decode_config(&options.unwrap_or_default())?;
  decode_track_async(env, snapshot(data), config)
}

/// Waveform peaks of the default audio track (audiodown's shape); `null`
/// when the container has no audio track. Throws for a prefix buffer or an
/// undecodable codec.
#[napi]
pub fn audio_peaks(
  data: &[u8],
  options: Option<WaveformOptions>,
) -> Result<Either<WaveformPeaks, Null>> {
  let (dc, wc) = waveform_configs(&options.unwrap_or_default())?;
  Ok(or_null(peaks(snapshot(data), dc, wc)?.map(Into::into)))
}

#[napi]
pub fn audio_peaks_async(
  env: &Env,
  data: &[u8],
  options: Option<WaveformOptions>,
) -> Result<AsyncBlock<Either<WaveformPeaks, Null>>> {
  let (dc, wc) = waveform_configs(&options.unwrap_or_default())?;
  peaks_async(env, snapshot(data), dc, wc)
}

/// Specs, keyframes and the audio envelope from one snapshot; the envelope's
/// absence is reported through `waveformStatus`, never thrown.
#[napi]
pub fn analyze_video(
  data: &[u8],
  options: Option<WaveformOptions>,
  source: Option<String>,
) -> Result<VideoAnalysis> {
  let (dc, wc) = waveform_configs(&options.unwrap_or_default())?;
  analysis(snapshot(data), dc, wc, source)
}

#[napi]
pub fn analyze_video_async(
  env: &Env,
  data: &[u8],
  options: Option<WaveformOptions>,
  source: Option<String>,
) -> Result<AsyncBlock<VideoAnalysis>> {
  let (dc, wc) = waveform_configs(&options.unwrap_or_default())?;
  analysis_async(env, snapshot(data), dc, wc, source)
}

// ── VideoService: stateless service, audiodown's shape ──────────

/// Stateless service. Construct once, call many; every method is also
/// available as a standalone function. Optional constructor defaults are
/// merged under per-call options.
#[napi]
pub struct VideoService {
  defaults: WaveformOptions,
}

#[napi]
impl VideoService {
  #[napi(constructor)]
  pub fn new(defaults: Option<WaveformOptions>) -> Result<Self> {
    let defaults = defaults.unwrap_or_default();
    waveform_configs(&defaults)?;
    Ok(VideoService { defaults })
  }

  #[napi(getter)]
  pub fn defaults(&self) -> WaveformOptions {
    self.defaults.clone()
  }

  #[napi]
  pub fn sniff_video(&self, data: &[u8]) -> Option<VideoKind> {
    sniff_video(data)
  }

  #[napi(ts_return_type = "VideoSpecs")]
  pub fn parse_video(&self, data: &[u8], source: Option<String>) -> Result<VideoSpecs> {
    parse_video(data, source)
  }

  #[napi(ts_return_type = "Promise<VideoSpecs>")]
  pub fn parse_video_async(
    &self,
    env: &Env,
    data: &[u8],
    source: Option<String>,
  ) -> Result<AsyncBlock<VideoSpecs>> {
    parse_video_async(env, data, source)
  }

  #[napi]
  pub fn keyframes(&self, data: &[u8]) -> Result<KeyframeIndex> {
    keyframes(data)
  }

  #[napi]
  pub fn keyframes_async(&self, env: &Env, data: &[u8]) -> Result<AsyncBlock<KeyframeIndex>> {
    keyframes_async(env, data)
  }

  #[napi]
  pub fn cover_art(&self, data: &[u8]) -> Result<Either<CoverArt, Null>> {
    cover_art(data)
  }

  #[napi]
  pub fn cover_art_async(
    &self,
    env: &Env,
    data: &[u8],
  ) -> Result<AsyncBlock<Either<CoverArt, Null>>> {
    cover_art_async(env, data)
  }

  #[napi]
  pub fn decode_audio(
    &self,
    data: &[u8],
    options: Option<DecodeOptions>,
  ) -> Result<Either<DecodedAudio, Null>> {
    decode_audio(data, Some(self.defaults.merged_decode(options)))
  }

  #[napi]
  pub fn decode_audio_async(
    &self,
    env: &Env,
    data: &[u8],
    options: Option<DecodeOptions>,
  ) -> Result<AsyncBlock<Either<DecodedAudio, Null>>> {
    decode_audio_async(env, data, Some(self.defaults.merged_decode(options)))
  }

  #[napi]
  pub fn audio_peaks(
    &self,
    data: &[u8],
    options: Option<WaveformOptions>,
  ) -> Result<Either<WaveformPeaks, Null>> {
    audio_peaks(data, Some(self.defaults.merged(options)))
  }

  #[napi]
  pub fn audio_peaks_async(
    &self,
    env: &Env,
    data: &[u8],
    options: Option<WaveformOptions>,
  ) -> Result<AsyncBlock<Either<WaveformPeaks, Null>>> {
    audio_peaks_async(env, data, Some(self.defaults.merged(options)))
  }

  #[napi]
  pub fn analyze_video(
    &self,
    data: &[u8],
    options: Option<WaveformOptions>,
    source: Option<String>,
  ) -> Result<VideoAnalysis> {
    analyze_video(data, Some(self.defaults.merged(options)), source)
  }

  #[napi]
  pub fn analyze_video_async(
    &self,
    env: &Env,
    data: &[u8],
    options: Option<WaveformOptions>,
    source: Option<String>,
  ) -> Result<AsyncBlock<VideoAnalysis>> {
    analyze_video_async(env, data, Some(self.defaults.merged(options)), source)
  }

  /// Snapshot a buffer once and reuse it across metadata, keyframe, decode,
  /// waveform and analyze calls. Inherits this service's defaults.
  #[napi]
  pub fn open(&self, data: &[u8], source: Option<String>) -> Result<VideoDown> {
    VideoDown::from_bytes(snapshot(data), source, self.defaults.clone())
  }
}

// ── VideoDown: open once, call many ─────────────────────────────

/// One video buffer, copied exactly once into shared memory. Sync methods
/// borrow it; async methods share it with the worker through `Arc`.
#[napi]
pub struct VideoDown {
  data: Arc<Vec<u8>>,
  kind: sniff::VideoKind,
  source: Option<String>,
  defaults: WaveformOptions,
}

impl VideoDown {
  fn from_bytes(
    data: Arc<Vec<u8>>,
    source: Option<String>,
    defaults: WaveformOptions,
  ) -> Result<Self> {
    let kind = sniff::sniff(&data).ok_or_else(|| fail("unrecognized video magic"))?;
    Ok(VideoDown {
      data,
      kind,
      source,
      defaults,
    })
  }
}

#[napi]
impl VideoDown {
  #[napi(constructor)]
  pub fn new(data: &[u8], source: Option<String>) -> Result<Self> {
    Self::from_bytes(snapshot(data), source, WaveformOptions::default())
  }

  #[napi(getter)]
  pub fn kind(&self) -> VideoKind {
    self.kind.into()
  }

  #[napi(getter)]
  pub fn byte_size(&self) -> f64 {
    self.data.len() as f64
  }

  #[napi(getter)]
  pub fn source(&self) -> Option<String> {
    self.source.clone()
  }

  #[napi(ts_return_type = "VideoSpecs")]
  pub fn metadata(&self) -> Result<VideoSpecs> {
    specs(&self.data, self.source.clone())
  }

  #[napi(ts_return_type = "Promise<VideoSpecs>")]
  pub fn metadata_async(&self, env: &Env) -> Result<AsyncBlock<VideoSpecs>> {
    specs_async(env, Arc::clone(&self.data), self.source.clone())
  }

  #[napi]
  pub fn keyframes(&self) -> Result<KeyframeIndex> {
    index(&self.data)
  }

  #[napi]
  pub fn keyframes_async(&self, env: &Env) -> Result<AsyncBlock<KeyframeIndex>> {
    index_async(env, Arc::clone(&self.data))
  }

  #[napi]
  pub fn cover_art(&self) -> Result<Either<CoverArt, Null>> {
    cover_art(&self.data)
  }

  #[napi]
  pub fn cover_art_async(&self, env: &Env) -> Result<AsyncBlock<Either<CoverArt, Null>>> {
    cover_async(env, Arc::clone(&self.data))
  }

  #[napi]
  pub fn decode_audio(&self, options: Option<DecodeOptions>) -> Result<Either<DecodedAudio, Null>> {
    let config = decode_config(&self.defaults.merged_decode(options))?;
    Ok(or_null(
      decode_track(Arc::clone(&self.data), config)?.map(Into::into),
    ))
  }

  #[napi]
  pub fn decode_audio_async(
    &self,
    env: &Env,
    options: Option<DecodeOptions>,
  ) -> Result<AsyncBlock<Either<DecodedAudio, Null>>> {
    let config = decode_config(&self.defaults.merged_decode(options))?;
    decode_track_async(env, Arc::clone(&self.data), config)
  }

  #[napi]
  pub fn audio_peaks(
    &self,
    options: Option<WaveformOptions>,
  ) -> Result<Either<WaveformPeaks, Null>> {
    let (dc, wc) = waveform_configs(&self.defaults.merged(options))?;
    Ok(or_null(
      peaks(Arc::clone(&self.data), dc, wc)?.map(Into::into),
    ))
  }

  #[napi]
  pub fn audio_peaks_async(
    &self,
    env: &Env,
    options: Option<WaveformOptions>,
  ) -> Result<AsyncBlock<Either<WaveformPeaks, Null>>> {
    let (dc, wc) = waveform_configs(&self.defaults.merged(options))?;
    peaks_async(env, Arc::clone(&self.data), dc, wc)
  }

  #[napi]
  pub fn analyze(&self, options: Option<WaveformOptions>) -> Result<VideoAnalysis> {
    let (dc, wc) = waveform_configs(&self.defaults.merged(options))?;
    analysis(Arc::clone(&self.data), dc, wc, self.source.clone())
  }

  #[napi]
  pub fn analyze_async(
    &self,
    env: &Env,
    options: Option<WaveformOptions>,
  ) -> Result<AsyncBlock<VideoAnalysis>> {
    let (dc, wc) = waveform_configs(&self.defaults.merged(options))?;
    analysis_async(env, Arc::clone(&self.data), dc, wc, self.source.clone())
  }
}
