//! JS boundary types. Everything here is emitted into `index.d.ts` by napi-rs;
//! the `From` impls and builders translate the pure-Rust core types into them.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::core::WaveformOutcome;
use crate::core::audio::Pcm;
use crate::core::meta::{self as meta, AudioTrackMeta, Keyframes, VideoMeta, VideoTrackMeta};
use crate::core::sniff;
use crate::core::waveform::Waveform;

fn nullable<T>(value: Option<T>) -> Either<T, Null> {
  match value {
    Some(v) => Either::A(v),
    None => Either::B(Null),
  }
}

fn count(value: Option<u64>) -> Either<f64, Null> {
  nullable(value.map(|n| n as f64))
}

// ── Enums (string unions in TypeScript) ─────────────────────────

#[napi(string_enum = "lowercase")]
pub enum VideoKind {
  Mp4,
  Mov,
  Webm,
  Mkv,
}

impl From<sniff::VideoKind> for VideoKind {
  fn from(kind: sniff::VideoKind) -> Self {
    match kind {
      sniff::VideoKind::Mp4 => VideoKind::Mp4,
      sniff::VideoKind::Mov => VideoKind::Mov,
      sniff::VideoKind::Webm => VideoKind::Webm,
      sniff::VideoKind::Mkv => VideoKind::Mkv,
    }
  }
}

#[napi(string_enum = "lowercase")]
pub enum VideoContainer {
  Isobmff,
  Matroska,
}

impl From<sniff::VideoContainer> for VideoContainer {
  fn from(container: sniff::VideoContainer) -> Self {
    match container {
      sniff::VideoContainer::Isobmff => VideoContainer::Isobmff,
      sniff::VideoContainer::Matroska => VideoContainer::Matroska,
    }
  }
}

#[napi(string_enum = "kebab-case")]
pub enum KeyframeSource {
  /// The `stss` sync-sample table.
  Stss,
  /// No `stss`: per ISOBMFF every sample is a sync sample.
  AllSamples,
  /// Matroska `Cues`.
  Cues,
  /// Matroska sync blocks seen while walking the clusters (no `Cues`).
  Blocks,
  /// Nothing indexed: no video track, or the container arm does not read one yet.
  None,
}

impl From<meta::KeyframeSource> for KeyframeSource {
  fn from(source: meta::KeyframeSource) -> Self {
    match source {
      meta::KeyframeSource::Stss => KeyframeSource::Stss,
      meta::KeyframeSource::AllSamples => KeyframeSource::AllSamples,
      meta::KeyframeSource::Cues => KeyframeSource::Cues,
      meta::KeyframeSource::Blocks => KeyframeSource::Blocks,
      meta::KeyframeSource::None => KeyframeSource::None,
    }
  }
}

#[napi(string_enum = "lowercase")]
pub enum Orientation {
  Landscape,
  Portrait,
  Square,
}

impl From<meta::Orientation> for Orientation {
  fn from(o: meta::Orientation) -> Self {
    match o {
      meta::Orientation::Landscape => Orientation::Landscape,
      meta::Orientation::Portrait => Orientation::Portrait,
      meta::Orientation::Square => Orientation::Square,
    }
  }
}

#[napi(string_enum = "kebab-case")]
pub enum WaveformStatus {
  /// `waveform` holds the audio-track envelope.
  Ok,
  /// The container has no audio track.
  NoAudio,
  /// An audio track exists but no decoder is compiled in for its codec (Opus in 0.1).
  UnsupportedCodec,
  /// The buffer is a prefix of the file, so no envelope was computed.
  Truncated,
}

// ── Metadata ────────────────────────────────────────────────────

#[napi(object, object_from_js = false)]
pub struct VideoBrands {
  pub major: String,
  pub minor_version: u32,
  pub compatible: Vec<String>,
}

#[napi(object, object_from_js = false)]
pub struct VideoTrack {
  pub track_id: u32,
  /// `h264`, `hevc`, `vp9`, `av1`, `mpeg4`, ... or `unknown`.
  pub codec: String,
  /// Sample-entry fourcc (`avc1`, `hvc1`) or Matroska codec id (`V_VP9`).
  pub codec_tag: Either<String, Null>,
  /// Coded dimensions.
  pub width: u32,
  pub height: u32,
  /// Presentation dimensions after the track matrix / display size, rotation applied.
  pub display_width: u32,
  pub display_height: u32,
  /// Clockwise degrees the player must rotate the coded frame.
  #[napi(ts_type = "0 | 90 | 180 | 270")]
  pub rotation: u32,
  pub duration_sec: Either<f64, Null>,
  pub timescale: Either<u32, Null>,
  /// Container samples: frames for video.
  pub sample_count: Either<f64, Null>,
  pub frame_rate: Either<f64, Null>,
  pub constant_frame_rate: Either<bool, Null>,
  /// Average bits per second of this track's samples.
  pub bitrate: Either<u32, Null>,
  pub language: Either<String, Null>,
}

impl From<VideoTrackMeta> for VideoTrack {
  fn from(v: VideoTrackMeta) -> Self {
    VideoTrack {
      track_id: v.track_id,
      codec: v.codec,
      codec_tag: nullable(v.codec_tag),
      width: v.width,
      height: v.height,
      display_width: v.display_width,
      display_height: v.display_height,
      rotation: v.rotation,
      duration_sec: nullable(v.duration_sec),
      timescale: nullable(v.timescale),
      sample_count: count(v.sample_count),
      frame_rate: nullable(v.frame_rate),
      constant_frame_rate: nullable(v.constant_frame_rate),
      bitrate: nullable(v.bitrate),
      language: nullable(v.language),
    }
  }
}

#[napi(object, object_from_js = false)]
pub struct AudioTrack {
  pub track_id: u32,
  /// `aac`, `opus`, `vorbis`, `flac`, `pcm`, ... or `unknown`.
  pub codec: String,
  pub codec_tag: Either<String, Null>,
  pub channels: u32,
  pub sample_rate: u32,
  pub bits_per_sample: Either<u32, Null>,
  pub duration_sec: Either<f64, Null>,
  /// Container samples: packets for audio, not PCM frames.
  pub sample_count: Either<f64, Null>,
  pub bitrate: Either<u32, Null>,
  pub language: Either<String, Null>,
}

impl From<AudioTrackMeta> for AudioTrack {
  fn from(a: AudioTrackMeta) -> Self {
    AudioTrack {
      track_id: a.track_id,
      codec: a.codec,
      codec_tag: nullable(a.codec_tag),
      channels: a.channels,
      sample_rate: a.sample_rate,
      bits_per_sample: nullable(a.bits_per_sample),
      duration_sec: nullable(a.duration_sec),
      sample_count: count(a.sample_count),
      bitrate: nullable(a.bitrate),
      language: nullable(a.language),
    }
  }
}

/// Flat, nullable video specs shaped like audiodown's `AudioSpecs`, with the
/// Prisma `VideoMetadata` columns folded in so one object feeds the asset
/// pipeline, the extractor and the spec sheet.
#[napi(object, object_from_js = false, js_name = "VideoSpecsBase")]
pub struct VideoSpecs {
  // ── asset identity ──
  #[napi(js_name = "type", ts_type = "'VIDEO'")]
  pub asset_type: String,
  /// Discriminant for narrowing; equals `format` and `ext`.
  pub kind: VideoKind,
  pub format: VideoKind,
  pub ext: VideoKind,
  #[napi(ts_type = "'video/mp4' | 'video/quicktime' | 'video/webm' | 'video/x-matroska'")]
  pub mime: String,
  /// Same value as `mime`.
  #[napi(ts_type = "'video/mp4' | 'video/quicktime' | 'video/webm' | 'video/x-matroska'")]
  pub mime_type: String,
  /// Same value as `mime`.
  #[napi(ts_type = "'video/mp4' | 'video/quicktime' | 'video/webm' | 'video/x-matroska'")]
  pub content_type: String,
  pub container: VideoContainer,
  /// Length of the parsed buffer in bytes.
  pub byte_size: f64,
  /// Same as `byteSize`: the bytes this parse actually saw.
  pub fetched_bytes: f64,
  /// Same as `byteSize`.
  pub size: f64,
  /// Passed through from the caller, e.g. a CDN URL.
  pub source: Option<String>,

  // ── layout ──
  /// The movie header (`moov`, or Matroska `Info` + `Tracks`) was found whole and parsed.
  /// `false` with `truncated: false` means the file carries no header at all.
  pub complete: bool,
  /// The buffer is a prefix: a declared box or segment runs past it.
  pub truncated: bool,
  /// End offset of the last top-level box or the Segment, when sizes are declared: the file
  /// length for a whole file, a lower bound for a prefix cut inside a box.
  pub declared_size: Either<f64, Null>,
  /// Fragmented MP4 (`mvex` / `moof`).
  pub fragmented: bool,
  /// Top-level ISOBMFF box types in file order; empty for Matroska.
  pub boxes: Vec<String>,
  /// Progressive layout: `moov` precedes `mdat`. `null` when either is absent, and for Matroska.
  pub moov_before_mdat: Either<bool, Null>,
  pub brands: Either<VideoBrands, Null>,
  /// ISO 8601 UTC from `mvhd` / Matroska `DateUTC`.
  pub creation_time: Either<String, Null>,
  pub modification_time: Either<String, Null>,
  pub title: Either<String, Null>,
  pub writing_app: Either<String, Null>,
  /// Movie timescale in ticks per second (`mvhd`); `null` for Matroska.
  pub timescale: Either<u32, Null>,

  // ── timing ──
  pub duration_sec: Either<f64, Null>,
  /// Rounded milliseconds (Prisma `VideoMetadata.duration`).
  pub duration_ms: Either<f64, Null>,
  /// Whole-file average bits per second over the movie duration.
  pub bitrate: Either<u32, Null>,
  /// `bitrate / 1000`, floored (Prisma `VideoMetadata.bitrate`).
  pub bitrate_kbps: Either<u32, Null>,

  // ── tracks ──
  pub track_count: u32,
  pub video_track_count: u32,
  pub audio_track_count: u32,

  // ── picture (first video track) ──
  pub width: Either<u32, Null>,
  pub height: Either<u32, Null>,
  pub display_width: Either<u32, Null>,
  pub display_height: Either<u32, Null>,
  #[napi(ts_type = "0 | 90 | 180 | 270 | null")]
  pub rotation: Either<u32, Null>,
  /// `displayWidth / displayHeight` (Prisma `VideoMetadata.aspectRatio`).
  pub aspect_ratio: Either<f64, Null>,
  /// `"16:9"`, `"9:16"`, ...
  pub aspect_ratio_label: Either<String, Null>,
  pub orientation: Either<Orientation, Null>,
  /// `"1080p"`, `"720p"`, `"4K"` from the shorter display edge (Prisma `VideoMetadata.resolution`).
  pub resolution: Either<String, Null>,
  pub frame_rate: Either<f64, Null>,
  pub constant_frame_rate: Either<bool, Null>,
  pub frame_count: Either<f64, Null>,
  pub video_codec: Either<String, Null>,
  pub video_codec_tag: Either<String, Null>,
  pub video_bitrate: Either<u32, Null>,

  // ── sound (first audio track) ──
  pub has_audio: bool,
  pub audio_codec: Either<String, Null>,
  pub audio_codec_tag: Either<String, Null>,
  pub audio_channels: Either<u32, Null>,
  pub audio_sample_rate: Either<u32, Null>,
  pub audio_bitrate: Either<u32, Null>,

  // ── structured tracks ──
  pub video: Either<VideoTrack, Null>,
  pub audio_tracks: Vec<AudioTrack>,

  // ── keyframes ──
  pub keyframe_count: u32,
  pub keyframe_source: KeyframeSource,
}

pub fn video_specs(m: VideoMeta, byte_size: usize, source: Option<String>) -> VideoSpecs {
  let kind: VideoKind = m.kind.into();
  let mime = m.kind.mime();
  let bitrate = m.bitrate_bps(byte_size);
  let video = m.video.as_ref();
  let audio = m.audio.first();
  let display = video.map(|v| (v.display_width, v.display_height));
  VideoSpecs {
    asset_type: "VIDEO".into(),
    kind,
    format: m.kind.into(),
    ext: m.kind.into(),
    mime: mime.into(),
    mime_type: mime.into(),
    content_type: mime.into(),
    container: m.kind.container().into(),
    byte_size: byte_size as f64,
    fetched_bytes: byte_size as f64,
    size: byte_size as f64,
    source,
    complete: m.complete,
    truncated: m.truncated,
    declared_size: count(m.declared_size),
    fragmented: m.fragmented,
    boxes: m.boxes.clone(),
    moov_before_mdat: nullable(m.moov_before_mdat),
    brands: nullable(m.brands.as_ref().map(|b| VideoBrands {
      major: b.major.clone(),
      minor_version: b.minor_version,
      compatible: b.compatible.clone(),
    })),
    creation_time: nullable(m.creation_time.clone()),
    modification_time: nullable(m.modification_time.clone()),
    title: nullable(m.title.clone()),
    writing_app: nullable(m.writing_app.clone()),
    timescale: nullable(m.timescale),
    duration_sec: nullable(m.duration_sec),
    duration_ms: nullable(m.duration_sec.map(|s| (s * 1000.0).round())),
    bitrate: nullable(bitrate),
    bitrate_kbps: nullable(bitrate.map(|b| b / 1000)),
    track_count: m.track_count,
    video_track_count: m.video_track_count,
    audio_track_count: m.audio.len() as u32,
    width: nullable(video.map(|v| v.width)),
    height: nullable(video.map(|v| v.height)),
    display_width: nullable(display.map(|d| d.0)),
    display_height: nullable(display.map(|d| d.1)),
    rotation: nullable(video.map(|v| v.rotation)),
    aspect_ratio: nullable(
      display
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| f64::from(w) / f64::from(h)),
    ),
    aspect_ratio_label: nullable(display.and_then(|(w, h)| meta::aspect_label(w, h))),
    orientation: nullable(
      display
        .and_then(|(w, h)| meta::Orientation::of(w, h))
        .map(Into::into),
    ),
    resolution: nullable(display.and_then(|(w, h)| meta::resolution_label(w, h))),
    frame_rate: nullable(video.and_then(|v| v.frame_rate)),
    constant_frame_rate: nullable(video.and_then(|v| v.constant_frame_rate)),
    frame_count: count(video.and_then(|v| v.sample_count)),
    video_codec: nullable(video.map(|v| v.codec.clone())),
    video_codec_tag: nullable(video.and_then(|v| v.codec_tag.clone())),
    video_bitrate: nullable(video.and_then(|v| v.bitrate)),
    has_audio: audio.is_some(),
    audio_codec: nullable(audio.map(|a| a.codec.clone())),
    audio_codec_tag: nullable(audio.and_then(|a| a.codec_tag.clone())),
    audio_channels: nullable(audio.map(|a| a.channels)),
    audio_sample_rate: nullable(audio.map(|a| a.sample_rate)),
    audio_bitrate: nullable(audio.and_then(|a| a.bitrate)),
    keyframe_count: m.keyframes.times_sec.len() as u32,
    keyframe_source: m.keyframes.source.into(),
    video: nullable(m.video.map(Into::into)),
    audio_tracks: m.audio.into_iter().map(Into::into).collect(),
  }
}

// ── Keyframes ───────────────────────────────────────────────────

#[napi(object, object_from_js = false)]
pub struct KeyframeIndex {
  pub source: KeyframeSource,
  /// The index covers the whole file; `false` when read from the blocks of a truncated buffer.
  pub complete: bool,
  pub count: u32,
  /// Presentation times in whole milliseconds, ascending (Prisma `VideoMetadata.keyframeTimes`).
  pub times_ms: Uint32Array,
  /// The same instants in seconds, unrounded.
  pub times_sec: Float64Array,
  /// Video samples in the indexed track.
  pub sample_count: Either<f64, Null>,
  /// Duration of the indexed track in seconds.
  pub duration_sec: Either<f64, Null>,
  /// `durationSec / count`: the average GOP length.
  pub mean_interval_sec: Either<f64, Null>,
}

pub fn keyframe_index(k: &Keyframes, duration_sec: Option<f64>) -> KeyframeIndex {
  let count_n = k.times_sec.len();
  let times_ms = k
    .times_sec
    .iter()
    .map(|t| (t * 1000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32)
    .collect::<Vec<u32>>();
  KeyframeIndex {
    source: k.source.into(),
    complete: k.complete,
    count: count_n as u32,
    times_ms: Uint32Array::new(times_ms),
    times_sec: Float64Array::new(k.times_sec.clone()),
    sample_count: count(k.sample_count),
    duration_sec: nullable(duration_sec),
    mean_interval_sec: nullable(
      duration_sec
        .filter(|_| count_n > 0)
        .map(|d| d / count_n as f64),
    ),
  }
}

// ── Options ─────────────────────────────────────────────────────

#[napi(object)]
#[derive(Clone, Default)]
pub struct DecodeOptions {
  /// Trim encoder delay/padding when gapless metadata is present. Default `true`.
  pub gapless: Option<bool>,
  /// Fail on the first decoder error instead of skipping the packet. Default `true`.
  pub strict: Option<bool>,
  /// Cap on interleaved scalar samples kept in memory. Default `64_000_000`.
  pub max_decoded_samples: Option<f64>,
}

#[napi(object)]
#[derive(Clone, Default)]
pub struct WaveformOptions {
  /// Target bucket count (capped at the frame count). Default `1024`. Exclusive with `samplesPerPeak`.
  pub peak_count: Option<f64>,
  /// Fixed frames per bucket. Exclusive with `peakCount`.
  pub samples_per_peak: Option<f64>,
  /// Reduce buckets on the rayon pool for large inputs. Default `true`.
  pub parallel: Option<bool>,
  pub gapless: Option<bool>,
  pub strict: Option<bool>,
  pub max_decoded_samples: Option<f64>,
}

impl WaveformOptions {
  /// Per-call options over service defaults. A call that names either
  /// resolution selector replaces both defaults so they never conflict.
  pub fn merged(&self, call: Option<WaveformOptions>) -> WaveformOptions {
    let Some(call) = call else {
      return self.clone();
    };
    let resolution_overridden = call.peak_count.is_some() || call.samples_per_peak.is_some();
    WaveformOptions {
      peak_count: if resolution_overridden {
        call.peak_count
      } else {
        self.peak_count
      },
      samples_per_peak: if resolution_overridden {
        call.samples_per_peak
      } else {
        self.samples_per_peak
      },
      parallel: call.parallel.or(self.parallel),
      gapless: call.gapless.or(self.gapless),
      strict: call.strict.or(self.strict),
      max_decoded_samples: call.max_decoded_samples.or(self.max_decoded_samples),
    }
  }

  pub fn merged_decode(&self, call: Option<DecodeOptions>) -> DecodeOptions {
    let call = call.unwrap_or_default();
    DecodeOptions {
      gapless: call.gapless.or(self.gapless),
      strict: call.strict.or(self.strict),
      max_decoded_samples: call.max_decoded_samples.or(self.max_decoded_samples),
    }
  }

  pub fn decode_options(&self) -> DecodeOptions {
    DecodeOptions {
      gapless: self.gapless,
      strict: self.strict,
      max_decoded_samples: self.max_decoded_samples,
    }
  }
}

// ── Decoded audio / waveform (audiodown shape, so `Array.from(envelope)` feeds the same column) ──

#[napi(object, object_from_js = false)]
pub struct DecodedAudio {
  /// Interleaved frames: `L0 R0 L1 R1 ...` for stereo.
  pub samples: Float32Array,
  pub sample_rate: u32,
  pub channels: u32,
  /// Frames per channel, not total scalar samples.
  pub frame_count: u32,
  pub duration_sec: f64,
  pub gapless_enabled: bool,
  pub skipped_packets: u32,
}

impl From<Pcm> for DecodedAudio {
  fn from(p: Pcm) -> Self {
    let frame_count = u32::try_from(p.frames()).unwrap_or(u32::MAX);
    let duration_sec = p.duration_sec();
    DecodedAudio {
      samples: Float32Array::new(p.samples),
      sample_rate: p.sample_rate,
      channels: p.channels as u32,
      frame_count,
      duration_sec,
      gapless_enabled: p.gapless_enabled,
      skipped_packets: p.skipped_packets,
    }
  }
}

#[napi(object, object_from_js = false)]
pub struct WaveformChannel {
  /// Signed minimum per bucket.
  pub min: Float32Array,
  /// Signed maximum per bucket.
  pub max: Float32Array,
  /// `max(|min|, |max|)` per bucket.
  pub peak: Float32Array,
  /// `sqrt(sum(sample²) / frames)` per bucket, accumulated in f64.
  pub rms: Float64Array,
}

#[napi(object, object_from_js = false)]
pub struct WaveformPeaks {
  pub sample_rate: u32,
  pub channel_count: u32,
  /// Frames per channel.
  pub frame_count: u32,
  /// Decoded duration: `frameCount / sampleRate`.
  pub duration_sec: f64,
  /// Actual bucket count (`peakCount` is capped at `frameCount`).
  pub peak_count: u32,
  /// `peakCount + 1` frame offsets; bucket `i` covers `[boundaries[i], boundaries[i + 1])`.
  pub boundaries: Uint32Array,
  pub channels: Vec<WaveformChannel>,
  /// `max(channel.peak[i])` across channels: the single-line display envelope.
  /// `Array.from(envelope)` is what a `waveformPeaks` column wants.
  pub envelope: Float32Array,
  pub gapless_enabled: bool,
  pub skipped_packets: u32,
}

impl From<Waveform> for WaveformPeaks {
  fn from(w: Waveform) -> Self {
    let count = w.boundaries.len() - 1;
    let mut envelope = vec![0f32; count];
    for channel in &w.channels {
      for (e, p) in envelope.iter_mut().zip(&channel.peak) {
        *e = e.max(*p);
      }
    }
    WaveformPeaks {
      sample_rate: w.sample_rate,
      channel_count: w.channel_count as u32,
      frame_count: w.frame_count as u32,
      duration_sec: w.duration_sec,
      peak_count: count as u32,
      boundaries: Uint32Array::new(w.boundaries),
      envelope: Float32Array::new(envelope),
      channels: w
        .channels
        .into_iter()
        .map(|c| WaveformChannel {
          min: Float32Array::new(c.min),
          max: Float32Array::new(c.max),
          peak: Float32Array::new(c.peak),
          rms: Float64Array::new(c.rms),
        })
        .collect(),
      gapless_enabled: w.gapless_enabled,
      skipped_packets: w.skipped_packets,
    }
  }
}

// ── Analysis ────────────────────────────────────────────────────

#[napi(object, object_from_js = false)]
pub struct VideoAnalysis {
  #[napi(ts_type = "VideoSpecs")]
  pub specs: VideoSpecs,
  pub keyframes: KeyframeIndex,
  /// Audio-track envelope; `null` whenever `waveformStatus` is not `ok`.
  pub waveform: Either<WaveformPeaks, Null>,
  pub waveform_status: WaveformStatus,
  /// The decoder's reason when `waveformStatus` is `unsupported-codec`.
  pub waveform_reason: Either<String, Null>,
}

pub fn video_analysis(
  m: VideoMeta,
  outcome: WaveformOutcome,
  byte_size: usize,
  source: Option<String>,
) -> VideoAnalysis {
  let track_duration = m
    .video
    .as_ref()
    .and_then(|v| v.duration_sec)
    .or(m.duration_sec);
  let keyframes = keyframe_index(&m.keyframes, track_duration);
  let (waveform, waveform_status, waveform_reason) = match outcome {
    WaveformOutcome::Peaks(w) => (Either::A(w.into()), WaveformStatus::Ok, Either::B(Null)),
    WaveformOutcome::NoAudio => (Either::B(Null), WaveformStatus::NoAudio, Either::B(Null)),
    WaveformOutcome::Unsupported(reason) => (
      Either::B(Null),
      WaveformStatus::UnsupportedCodec,
      Either::A(reason),
    ),
    WaveformOutcome::Truncated => (Either::B(Null), WaveformStatus::Truncated, Either::B(Null)),
  };
  VideoAnalysis {
    specs: video_specs(m, byte_size, source),
    keyframes,
    waveform,
    waveform_status,
    waveform_reason,
  }
}
