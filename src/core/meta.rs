//! The container-agnostic result of a metadata parse, plus the small pure
//! helpers both container arms share: codec naming, rotation from the
//! transformation matrix, ISO 8601 timestamps, resolution and aspect labels.

use super::sniff::{VideoKind, sniff};
use super::{mkv, mp4};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyframeSource {
  /// The `stss` sync-sample table.
  Stss,
  /// No `stss`: per ISOBMFF every sample is a sync sample.
  AllSamples,
  /// Matroska `Cues`.
  Cues,
  /// Matroska sync blocks seen while walking the clusters (no `Cues`).
  Blocks,
  /// Nothing to index: no video track, or the container arm does not read one yet.
  #[default]
  None,
}

impl KeyframeSource {
  pub fn as_str(self) -> &'static str {
    match self {
      KeyframeSource::Stss => "stss",
      KeyframeSource::AllSamples => "all-samples",
      KeyframeSource::Cues => "cues",
      KeyframeSource::Blocks => "blocks",
      KeyframeSource::None => "none",
    }
  }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keyframes {
  /// Presentation times in seconds, ascending.
  pub times_sec: Vec<f64>,
  pub source: KeyframeSource,
  /// Video samples in the track the index was built from.
  pub sample_count: Option<u64>,
  /// The index covers the whole file; `false` when it was read from the
  /// blocks of a truncated buffer.
  pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brands {
  pub major: String,
  pub minor_version: u32,
  pub compatible: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoTrackMeta {
  pub track_id: u32,
  pub codec: String,
  /// Sample-entry fourcc (`avc1`, `hvc1`) or Matroska codec id (`V_VP9`).
  pub codec_tag: Option<String>,
  /// Coded dimensions.
  pub width: u32,
  pub height: u32,
  /// Presentation dimensions after the track matrix / display size, rotation applied.
  pub display_width: u32,
  pub display_height: u32,
  /// Clockwise degrees the player must rotate the coded frame: 0, 90, 180 or 270.
  pub rotation: u32,
  pub duration_sec: Option<f64>,
  pub timescale: Option<u32>,
  pub sample_count: Option<u64>,
  pub frame_rate: Option<f64>,
  pub constant_frame_rate: Option<bool>,
  /// Average bits per second of this track's samples.
  pub bitrate: Option<u32>,
  pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrackMeta {
  pub track_id: u32,
  pub codec: String,
  pub codec_tag: Option<String>,
  pub channels: u32,
  pub sample_rate: u32,
  pub bits_per_sample: Option<u32>,
  pub duration_sec: Option<f64>,
  /// Container samples (packets), not PCM frames.
  pub sample_count: Option<u64>,
  pub bitrate: Option<u32>,
  pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoMeta {
  pub kind: VideoKind,
  /// The movie header (`moov` / Matroska `Info` + `Tracks`) was found whole and parsed.
  pub complete: bool,
  /// The buffer is a prefix: a declared box or segment runs past it.
  pub truncated: bool,
  /// End offset of the last top-level box or the Segment, when sizes are
  /// declared. For a whole file this is its length; for a prefix cut inside a
  /// box it is a lower bound, since later boxes have not been seen.
  pub declared_size: Option<u64>,
  pub fragmented: bool,
  /// Top-level ISOBMFF box types in file order; empty for Matroska.
  pub boxes: Vec<String>,
  pub moov_before_mdat: Option<bool>,
  pub brands: Option<Brands>,
  pub creation_time: Option<String>,
  pub modification_time: Option<String>,
  pub timescale: Option<u32>,
  pub duration_sec: Option<f64>,
  pub track_count: u32,
  pub video_track_count: u32,
  pub video: Option<VideoTrackMeta>,
  pub audio: Vec<AudioTrackMeta>,
  pub keyframes: Keyframes,
  /// Muxing application, when the container records one (Matroska `WritingApp`).
  pub writing_app: Option<String>,
  pub title: Option<String>,
}

impl VideoMeta {
  pub fn empty(kind: VideoKind) -> Self {
    VideoMeta {
      kind,
      complete: false,
      truncated: false,
      declared_size: None,
      fragmented: false,
      boxes: Vec::new(),
      moov_before_mdat: None,
      brands: None,
      creation_time: None,
      modification_time: None,
      timescale: None,
      duration_sec: None,
      track_count: 0,
      video_track_count: 0,
      video: None,
      audio: Vec::new(),
      keyframes: Keyframes::default(),
      writing_app: None,
      title: None,
    }
  }

  /// Whole-file average bitrate from the declared size (or the buffer when
  /// the container does not declare one) over the movie duration.
  pub fn bitrate_bps(&self, byte_size: usize) -> Option<u32> {
    let duration = self.duration_sec.filter(|d| *d > 0.0)?;
    let bytes = self.declared_size.unwrap_or(byte_size as u64) as f64;
    let bps = (bytes * 8.0 / duration).round();
    if bps.is_finite() && bps >= 0.0 {
      Some(bps.min(f64::from(u32::MAX)) as u32)
    } else {
      None
    }
  }
}

pub fn parse_video(b: &[u8]) -> Result<VideoMeta, String> {
  match sniff(b) {
    Some(kind) if kind.is_isobmff() => mp4::parse(b, kind),
    Some(kind) => mkv::parse(b, kind),
    None => Err("unrecognized video magic".into()),
  }
}

/// Cheap prefix check without a full parse: the box walk for ISOBMFF, the
/// EBML header and segment size for Matroska.
pub fn is_truncated(b: &[u8]) -> Result<bool, String> {
  match sniff(b) {
    Some(kind) if kind.is_isobmff() => Ok(super::boxes::layout(b).truncated),
    Some(kind) => mkv::parse(b, kind).map(|m| m.truncated),
    None => Err("unrecognized video magic".into()),
  }
}

// ── Codec naming ────────────────────────────────────────────────

pub fn video_codec_from_tag(tag: &str) -> String {
  match tag {
    "avc1" | "avc2" | "avc3" | "avc4" => "h264",
    "hvc1" | "hev1" | "dvh1" | "dvhe" => "hevc",
    "vvc1" | "vvi1" => "vvc",
    "vp08" => "vp8",
    "vp09" => "vp9",
    "av01" => "av1",
    "mp4v" => "mpeg4",
    "s263" | "h263" => "h263",
    "jpeg" => "mjpeg",
    "apch" | "apcn" | "apcs" | "apco" | "ap4h" | "ap4x" => "prores",
    "encv" => "encrypted",
    other => return other.trim().to_ascii_lowercase(),
  }
  .to_owned()
}

pub fn audio_codec_from_tag(tag: &str) -> String {
  match tag {
    "mp4a" => "aac",
    "Opus" => "opus",
    "fLaC" => "flac",
    "alac" => "alac",
    "ac-3" => "ac3",
    "ec-3" => "eac3",
    ".mp3" => "mp3",
    "samr" => "amr-nb",
    "sawb" => "amr-wb",
    "lpcm" | "sowt" | "twos" | "in24" | "in32" | "fl32" | "fl64" | "raw " | "NONE" => "pcm",
    "ulaw" => "mulaw",
    "alaw" => "alaw",
    "enca" => "encrypted",
    other => return other.trim().to_ascii_lowercase(),
  }
  .to_owned()
}

pub fn codec_from_matroska_id(id: &str) -> String {
  match id {
    "V_MPEG4/ISO/AVC" => "h264",
    "V_MPEGH/ISO/HEVC" => "hevc",
    "V_VP9" => "vp9",
    "V_VP8" => "vp8",
    "V_AV1" => "av1",
    "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/AP" => "mpeg4",
    "V_MPEG1" => "mpeg1",
    "V_MPEG2" => "mpeg2",
    "V_THEORA" => "theora",
    "V_MJPEG" => "mjpeg",
    "V_PRORES" => "prores",
    "V_FFV1" => "ffv1",
    "A_VORBIS" => "vorbis",
    "A_OPUS" => "opus",
    "A_FLAC" => "flac",
    "A_MPEG/L3" => "mp3",
    "A_MPEG/L2" => "mp2",
    "A_MPEG/L1" => "mp1",
    "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => "ac3",
    "A_EAC3" => "eac3",
    "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => "dts",
    "A_TRUEHD" => "truehd",
    "A_ALAC" => "alac",
    other if other.starts_with("A_AAC") => "aac",
    other if other.starts_with("A_PCM") => "pcm",
    other => return other.to_ascii_lowercase(),
  }
  .to_owned()
}

// ── Geometry ────────────────────────────────────────────────────

/// Clockwise display rotation from the first row of the `tkhd` matrix
/// (`a`, `b` in 16.16 fixed point), snapped to a multiple of 90. This is the
/// `rotate` tag convention: `[0 1; -1 0]` is 90, `[-1 0; 0 -1]` is 180.
pub fn rotation_from_matrix(a: i32, b: i32) -> u32 {
  if a == 0 && b == 0 {
    return 0;
  }
  let degrees = (f64::from(b) / 65_536.0)
    .atan2(f64::from(a) / 65_536.0)
    .to_degrees();
  let quarter_turns = (degrees / 90.0).round() as i64;
  (quarter_turns * 90).rem_euclid(360) as u32
}

pub fn rotated_dims(width: u32, height: u32, rotation: u32) -> (u32, u32) {
  if rotation == 90 || rotation == 270 {
    (height, width)
  } else {
    (width, height)
  }
}

/// `"1080p"`, `"720p"`, `"4K"`, `"8K"` from the shorter display edge, so a
/// portrait 1080×1920 clip is 1080p like its landscape twin.
pub fn resolution_label(width: u32, height: u32) -> Option<String> {
  if width == 0 || height == 0 {
    return None;
  }
  Some(match width.min(height) {
    2160 => "4K".to_owned(),
    4320 => "8K".to_owned(),
    short => format!("{short}p"),
  })
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
  while b != 0 {
    (a, b) = (b, a % b);
  }
  a
}

const COMMON_ASPECTS: [(u32, u32); 10] = [
  (16, 9),
  (9, 16),
  (4, 3),
  (3, 4),
  (1, 1),
  (21, 9),
  (3, 2),
  (2, 3),
  (5, 4),
  (4, 5),
];

/// Reduced `W:H`; when the reduced pair is unwieldy (odd encoder sizes like
/// 1366×768), the nearest common ratio within 2 % is reported instead.
pub fn aspect_label(width: u32, height: u32) -> Option<String> {
  if width == 0 || height == 0 {
    return None;
  }
  let g = gcd(width, height);
  let (rw, rh) = (width / g, height / g);
  if rw <= 32 && rh <= 32 {
    return Some(format!("{rw}:{rh}"));
  }
  let ratio = f64::from(width) / f64::from(height);
  let snapped = COMMON_ASPECTS.iter().find(|(a, b)| {
    let candidate = f64::from(*a) / f64::from(*b);
    ((candidate - ratio) / ratio).abs() < 0.02
  });
  Some(match snapped {
    Some((a, b)) => format!("{a}:{b}"),
    None => format!("{rw}:{rh}"),
  })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
  Landscape,
  Portrait,
  Square,
}

impl Orientation {
  pub fn of(width: u32, height: u32) -> Option<Self> {
    if width == 0 || height == 0 {
      return None;
    }
    Some(match width.cmp(&height) {
      std::cmp::Ordering::Greater => Orientation::Landscape,
      std::cmp::Ordering::Less => Orientation::Portrait,
      std::cmp::Ordering::Equal => Orientation::Square,
    })
  }

  pub fn as_str(self) -> &'static str {
    match self {
      Orientation::Landscape => "landscape",
      Orientation::Portrait => "portrait",
      Orientation::Square => "square",
    }
  }
}

// ── Time ────────────────────────────────────────────────────────

/// Seconds between the ISOBMFF epoch (1904-01-01) and the Unix epoch.
pub const MP4_EPOCH_OFFSET: i64 = 2_082_844_800;
/// Seconds between the Unix epoch and the Matroska epoch (2001-01-01).
pub const MKV_EPOCH_OFFSET: i64 = 978_307_200;

/// `YYYY-MM-DDTHH:MM:SSZ` from Unix seconds (proleptic Gregorian, no leap
/// seconds), via the days-to-civil algorithm, so no date crate is needed.
pub fn iso8601(unix_seconds: i64) -> String {
  let days = unix_seconds.div_euclid(86_400);
  let secs = unix_seconds.rem_euclid(86_400);
  let z = days + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let day = doy - (153 * mp + 2) / 5 + 1;
  let month = if mp < 10 { mp + 3 } else { mp - 9 };
  let year = yoe + era * 400 + i64::from(month <= 2);
  format!(
    "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
    secs / 3_600,
    (secs % 3_600) / 60,
    secs % 60
  )
}

pub fn mp4_time(seconds_since_1904: u64) -> Option<String> {
  if seconds_since_1904 == 0 {
    return None;
  }
  let unix = i64::try_from(seconds_since_1904).ok()? - MP4_EPOCH_OFFSET;
  Some(iso8601(unix))
}

pub fn mkv_time(nanos_since_2001: i64) -> String {
  iso8601(nanos_since_2001.div_euclid(1_000_000_000) + MKV_EPOCH_OFFSET)
}

/// Bits per second from a byte total over a duration, saturating at `u32::MAX`.
pub fn bitrate(bytes: u64, duration_sec: f64) -> Option<u32> {
  if duration_sec <= 0.0 || bytes == 0 {
    return None;
  }
  let bps = (bytes as f64 * 8.0 / duration_sec).round();
  Some(bps.min(f64::from(u32::MAX)) as u32)
}
