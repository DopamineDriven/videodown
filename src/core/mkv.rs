//! Matroska / WebM through a purpose-built EBML reader: the header `DocType`,
//! `Info`, `Tracks`, `Cues` for the keyframe index, and a walk of every
//! `Cluster`'s block headers (payloads skipped by size) for the fallback
//! index, per-track block counts and bytes, and the duration of recordings
//! that carry neither `Duration` nor `Cues`. Unknown-size Segments and
//! Clusters (MediaRecorder, `ffmpeg -live 1`) are handled, and so is a prefix
//! of the file: `complete` once `Tracks` has been read, `truncated` when any
//! element runs past the buffer. Every read is bounds-checked.

use super::meta::{self, AudioTrackMeta, KeyframeSource, Keyframes, VideoMeta, VideoTrackMeta};
use super::sniff::{EBML_MAGIC, VideoKind};

mod id {
  pub const EBML: u32 = 0x1A45_DFA3;
  pub const DOC_TYPE: u32 = 0x4282;
  pub const SEGMENT: u32 = 0x1853_8067;
  pub const SEEK_HEAD: u32 = 0x114D_9B74;
  pub const INFO: u32 = 0x1549_A966;
  pub const TIMECODE_SCALE: u32 = 0x002A_D7B1;
  pub const DURATION: u32 = 0x4489;
  pub const DATE_UTC: u32 = 0x4461;
  pub const TITLE: u32 = 0x7BA9;
  pub const WRITING_APP: u32 = 0x5741;
  pub const TRACKS: u32 = 0x1654_AE6B;
  pub const TRACK_ENTRY: u32 = 0xAE;
  pub const TRACK_NUMBER: u32 = 0xD7;
  pub const TRACK_TYPE: u32 = 0x83;
  pub const DEFAULT_DURATION: u32 = 0x0023_E383;
  pub const LANGUAGE: u32 = 0x0022_B59C;
  pub const LANGUAGE_IETF: u32 = 0x0022_B59D;
  pub const CODEC_ID: u32 = 0x86;
  pub const VIDEO: u32 = 0xE0;
  pub const PIXEL_WIDTH: u32 = 0xB0;
  pub const PIXEL_HEIGHT: u32 = 0xBA;
  pub const DISPLAY_WIDTH: u32 = 0x54B0;
  pub const DISPLAY_HEIGHT: u32 = 0x54BA;
  pub const DISPLAY_UNIT: u32 = 0x54B2;
  pub const AUDIO: u32 = 0xE1;
  pub const SAMPLING_FREQUENCY: u32 = 0xB5;
  pub const CHANNELS: u32 = 0x9F;
  pub const BIT_DEPTH: u32 = 0x6264;
  pub const CUES: u32 = 0x1C53_BB6B;
  pub const CUE_POINT: u32 = 0xBB;
  pub const CUE_TIME: u32 = 0xB3;
  pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
  pub const CUE_TRACK: u32 = 0xF7;
  pub const CLUSTER: u32 = 0x1F43_B675;
  pub const TIMECODE: u32 = 0xE7;
  pub const SIMPLE_BLOCK: u32 = 0xA3;
  pub const BLOCK_GROUP: u32 = 0xA0;
  pub const BLOCK: u32 = 0xA1;
  pub const REFERENCE_BLOCK: u32 = 0xFB;
  pub const TAGS: u32 = 0x1254_C367;
  pub const ATTACHMENTS: u32 = 0x1941_A469;
  pub const CHAPTERS: u32 = 0x1043_A770;
}

/// Segment-level ids: any of these terminates an unknown-size master.
const LEVEL1: [u32; 8] = [
  id::SEEK_HEAD,
  id::INFO,
  id::TRACKS,
  id::CUES,
  id::CLUSTER,
  id::TAGS,
  id::ATTACHMENTS,
  id::CHAPTERS,
];

const TRACK_VIDEO: u64 = 1;
const TRACK_AUDIO: u64 = 2;
const DEFAULT_TIMECODE_SCALE: u64 = 1_000_000;

// ── EBML primitives ─────────────────────────────────────────────

/// Element id: the length marker stays part of the value.
fn read_id(b: &[u8], at: usize) -> Option<(u32, usize)> {
  let first = *b.get(at)?;
  let len = first.leading_zeros() as usize + 1;
  if len > 4 {
    return None;
  }
  let bytes = b.get(at..at + len)?;
  let id = bytes.iter().fold(0u32, |acc, &c| (acc << 8) | u32::from(c));
  Some((id, len))
}

/// Element size: the marker bit is stripped. `None` in the value position
/// is the reserved all-ones "unknown size".
fn read_size(b: &[u8], at: usize) -> Option<(Option<u64>, usize)> {
  let first = *b.get(at)?;
  let len = first.leading_zeros() as usize + 1;
  if len > 8 {
    return None;
  }
  let bytes = b.get(at..at + len)?;
  // An eight-byte size carries no value bits in its first byte.
  let mask = if len >= 8 { 0 } else { 0xffu8 >> len };
  let mut value = u64::from(first & mask);
  for &c in &bytes[1..] {
    value = (value << 8) | u64::from(c);
  }
  let all_ones = (1u64 << (7 * len)) - 1;
  Some(((value != all_ones).then_some(value), len))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Element {
  id: u32,
  /// Offset of the first data byte.
  data: usize,
  /// Declared data size; `None` for unknown size.
  size: Option<u64>,
}

impl Element {
  fn declared_end(&self) -> Option<u64> {
    self.size.and_then(|s| (self.data as u64).checked_add(s))
  }

  /// Where the data stops inside `[.., limit)`.
  fn end(&self, limit: usize) -> usize {
    match self.declared_end() {
      Some(e) => usize::try_from(e).map_or(limit, |e| e.min(limit)),
      None => limit,
    }
  }

  fn fits(&self, limit: usize) -> bool {
    self.declared_end().is_none_or(|e| e <= limit as u64)
  }
}

fn element(b: &[u8], at: usize, limit: usize) -> Option<Element> {
  if at >= limit {
    return None;
  }
  let (id, id_len) = read_id(b, at)?;
  let (size, size_len) = read_size(b, at + id_len)?;
  let data = at + id_len + size_len;
  if data > limit {
    return None;
  }
  Some(Element { id, data, size })
}

fn bytes<'a>(b: &'a [u8], el: &Element, limit: usize) -> Option<&'a [u8]> {
  if !el.fits(limit) {
    return None;
  }
  b.get(el.data..el.end(limit))
}

fn uint(b: &[u8], el: &Element, limit: usize) -> Option<u64> {
  let bytes = bytes(b, el, limit)?;
  if bytes.len() > 8 {
    return None;
  }
  Some(bytes.iter().fold(0u64, |acc, &c| (acc << 8) | u64::from(c)))
}

fn int(b: &[u8], el: &Element, limit: usize) -> Option<i64> {
  let bytes = bytes(b, el, limit)?;
  if bytes.is_empty() || bytes.len() > 8 {
    return None;
  }
  let mut value = i64::from(bytes[0] as i8);
  for &c in &bytes[1..] {
    value = (value << 8) | i64::from(c);
  }
  Some(value)
}

fn float(b: &[u8], el: &Element, limit: usize) -> Option<f64> {
  let bytes = bytes(b, el, limit)?;
  match bytes.len() {
    4 => Some(f64::from(f32::from_be_bytes([
      bytes[0], bytes[1], bytes[2], bytes[3],
    ]))),
    8 => Some(f64::from_be_bytes([
      bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])),
    _ => None,
  }
}

fn string(b: &[u8], el: &Element, limit: usize) -> Option<String> {
  let text = String::from_utf8_lossy(bytes(b, el, limit)?)
    .trim_end_matches('\0')
    .to_owned();
  (!text.is_empty()).then_some(text)
}

/// The children of a master element that fit inside it.
fn children(b: &[u8], el: &Element, limit: usize) -> Vec<Element> {
  let end = el.end(limit);
  let mut out = Vec::new();
  let mut at = el.data;
  while let Some(child) = element(b, at, end) {
    if !child.fits(end) {
      break;
    }
    out.push(child);
    at = child.end(end);
  }
  out
}

/// `DocType` from the EBML header, when present.
pub fn doc_type(b: &[u8]) -> Option<String> {
  if b.len() < 4 || b[..4] != EBML_MAGIC {
    return None;
  }
  let header = element(b, 0, b.len())?;
  children(b, &header, b.len())
    .iter()
    .find(|c| c.id == id::DOC_TYPE)
    .and_then(|c| string(b, c, b.len()))
}

// ── Scan ────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct TrackEntry {
  number: u64,
  kind: u64,
  codec_id: String,
  default_duration_ns: Option<u64>,
  language: Option<String>,
  pixel: Option<(u64, u64)>,
  display: Option<(Option<u64>, Option<u64>, u64)>,
  audio: Option<(f64, u64, Option<u64>)>,
}

#[derive(Debug, Default)]
struct TrackStats {
  number: u64,
  blocks: u64,
  bytes: u64,
  first_ticks: i64,
  last_ticks: i64,
}

#[derive(Debug)]
struct Scan {
  timecode_scale: u64,
  duration_ticks: Option<f64>,
  date_utc: Option<i64>,
  title: Option<String>,
  writing_app: Option<String>,
  tracks: Vec<TrackEntry>,
  tracks_seen: bool,
  /// `(time in ticks, track number)` from `Cues`.
  cues: Vec<(u64, Option<u64>)>,
  clusters: u64,
  /// `(track number, absolute ticks)` of sync blocks, video tracks only once `Tracks` is known.
  block_keyframes: Vec<(u64, i64)>,
  stats: Vec<TrackStats>,
  truncated: bool,
}

impl Scan {
  fn new() -> Self {
    Scan {
      timecode_scale: DEFAULT_TIMECODE_SCALE,
      duration_ticks: None,
      date_utc: None,
      title: None,
      writing_app: None,
      tracks: Vec::new(),
      tracks_seen: false,
      cues: Vec::new(),
      clusters: 0,
      block_keyframes: Vec::new(),
      stats: Vec::new(),
      truncated: false,
    }
  }

  fn is_video_or_unknown(&self, track: u64) -> bool {
    !self.tracks_seen
      || self
        .tracks
        .iter()
        .any(|t| t.number == track && t.kind == TRACK_VIDEO)
  }

  fn stats_for(&mut self, track: u64) -> &mut TrackStats {
    if let Some(i) = self.stats.iter().position(|s| s.number == track) {
      return &mut self.stats[i];
    }
    self.stats.push(TrackStats {
      number: track,
      ..TrackStats::default()
    });
    self.stats.last_mut().expect("just pushed")
  }

  fn info(&mut self, b: &[u8], el: &Element, limit: usize) {
    for c in children(b, el, limit) {
      match c.id {
        id::TIMECODE_SCALE => {
          if let Some(scale) = uint(b, &c, limit).filter(|s| *s > 0) {
            self.timecode_scale = scale;
          }
        }
        id::DURATION => self.duration_ticks = float(b, &c, limit).filter(|d| *d > 0.0),
        id::DATE_UTC => self.date_utc = int(b, &c, limit),
        id::TITLE => self.title = string(b, &c, limit),
        id::WRITING_APP => self.writing_app = string(b, &c, limit),
        _ => {}
      }
    }
  }

  fn tracks(&mut self, b: &[u8], el: &Element, limit: usize) {
    for entry in children(b, el, limit)
      .iter()
      .filter(|c| c.id == id::TRACK_ENTRY)
    {
      let mut track = TrackEntry::default();
      for c in children(b, entry, limit) {
        match c.id {
          id::TRACK_NUMBER => track.number = uint(b, &c, limit).unwrap_or(0),
          id::TRACK_TYPE => track.kind = uint(b, &c, limit).unwrap_or(0),
          id::CODEC_ID => track.codec_id = string(b, &c, limit).unwrap_or_default(),
          id::DEFAULT_DURATION => track.default_duration_ns = uint(b, &c, limit).filter(|d| *d > 0),
          id::LANGUAGE_IETF => track.language = string(b, &c, limit).or(track.language.take()),
          id::LANGUAGE => {
            if track.language.is_none() {
              track.language = string(b, &c, limit);
            }
          }
          id::VIDEO => {
            let (mut pw, mut ph, mut dw, mut dh, mut unit) = (0, 0, None, None, 0);
            for v in children(b, &c, limit) {
              match v.id {
                id::PIXEL_WIDTH => pw = uint(b, &v, limit).unwrap_or(0),
                id::PIXEL_HEIGHT => ph = uint(b, &v, limit).unwrap_or(0),
                id::DISPLAY_WIDTH => dw = uint(b, &v, limit),
                id::DISPLAY_HEIGHT => dh = uint(b, &v, limit),
                id::DISPLAY_UNIT => unit = uint(b, &v, limit).unwrap_or(0),
                _ => {}
              }
            }
            track.pixel = Some((pw, ph));
            track.display = Some((dw, dh, unit));
          }
          id::AUDIO => {
            let (mut rate, mut channels, mut depth) = (0.0, 1, None);
            for a in children(b, &c, limit) {
              match a.id {
                id::SAMPLING_FREQUENCY => rate = float(b, &a, limit).unwrap_or(0.0),
                id::CHANNELS => channels = uint(b, &a, limit).unwrap_or(1),
                id::BIT_DEPTH => depth = uint(b, &a, limit),
                _ => {}
              }
            }
            track.audio = Some((rate, channels, depth));
          }
          _ => {}
        }
      }
      self.tracks.push(track);
    }
    // A cut Tracks element leaves entries behind; the header is not whole.
    self.tracks_seen = el.fits(limit);
  }

  fn cues(&mut self, b: &[u8], el: &Element, limit: usize) {
    for point in children(b, el, limit)
      .iter()
      .filter(|c| c.id == id::CUE_POINT)
    {
      let mut time = None;
      let mut track = None;
      for c in children(b, point, limit) {
        match c.id {
          id::CUE_TIME => time = uint(b, &c, limit),
          id::CUE_TRACK_POSITIONS => {
            track = children(b, &c, limit)
              .iter()
              .find(|p| p.id == id::CUE_TRACK)
              .and_then(|p| uint(b, p, limit));
          }
          _ => {}
        }
      }
      if let Some(time) = time {
        self.cues.push((time, track));
      }
    }
  }

  /// A `SimpleBlock` or a `BlockGroup`'s `Block`: track vint, `i16` timecode
  /// relative to the cluster, flags. Payload bytes are counted, never read.
  fn block(&mut self, b: &[u8], data: usize, end: usize, cluster_ticks: i64, sync: Option<bool>) {
    let Some((Some(track), track_len)) = read_size(b, data) else {
      return;
    };
    let head = data + track_len;
    let Some(fields) = b.get(head..head + 3) else {
      return;
    };
    let relative = i64::from(i16::from_be_bytes([fields[0], fields[1]]));
    let keyframe = sync.unwrap_or(fields[2] & 0x80 != 0);
    let ticks = cluster_ticks + relative;
    let payload = end.saturating_sub(head + 3) as u64;
    let stats = self.stats_for(track);
    if stats.blocks == 0 {
      stats.first_ticks = ticks;
      stats.last_ticks = ticks;
    } else {
      stats.first_ticks = stats.first_ticks.min(ticks);
      stats.last_ticks = stats.last_ticks.max(ticks);
    }
    stats.blocks += 1;
    stats.bytes += payload;
    if keyframe && self.is_video_or_unknown(track) {
      self.block_keyframes.push((track, ticks));
    }
  }

  /// Walks one cluster and returns where the next segment-level element
  /// starts: the declared end, or for an unknown-size cluster the offset of
  /// the first level-1 id met.
  fn cluster(&mut self, b: &[u8], el: &Element, limit: usize) -> usize {
    self.clusters += 1;
    let known = el.size.is_some();
    let end = el.end(limit);
    let mut timecode = 0i64;
    let mut at = el.data;
    while let Some(child) = element(b, at, end) {
      if !known && LEVEL1.contains(&child.id) {
        return at;
      }
      if !child.fits(end) {
        self.truncated = true;
      }
      let child_end = child.end(end);
      match child.id {
        id::TIMECODE => timecode = uint(b, &child, end).map_or(0, |t| t as i64),
        id::SIMPLE_BLOCK => self.block(b, child.data, child_end, timecode, None),
        id::BLOCK_GROUP => {
          let mut block = None;
          let mut referenced = false;
          for g in children(b, &child, end) {
            match g.id {
              id::BLOCK => block = Some((g.data, g.end(end))),
              id::REFERENCE_BLOCK => referenced = true,
              _ => {}
            }
          }
          if let Some((data, block_end)) = block {
            self.block(b, data, block_end, timecode, Some(!referenced));
          }
        }
        _ => {}
      }
      if child.size.is_none() || !child.fits(end) {
        // Nothing reliable follows an unknown-size or cut child.
        return end;
      }
      at = child_end;
    }
    end
  }

  fn segment(&mut self, b: &[u8], el: &Element, limit: usize) {
    let end = el.end(limit);
    let mut at = el.data;
    while let Some(child) = element(b, at, end) {
      if !child.fits(end) {
        self.truncated = true;
      }
      match child.id {
        id::INFO => self.info(b, &child, end),
        id::TRACKS => self.tracks(b, &child, end),
        id::CUES => self.cues(b, &child, end),
        id::CLUSTER => {
          at = self.cluster(b, &child, end);
          continue;
        }
        _ => {}
      }
      if child.size.is_none() {
        // An unknown-size master other than a cluster: step through its
        // children until a level-1 id appears.
        let mut inner = child.data;
        while let Some(grand) = element(b, inner, end) {
          if LEVEL1.contains(&grand.id) || grand.size.is_none() || !grand.fits(end) {
            break;
          }
          inner = grand.end(end);
        }
        at = inner;
        continue;
      }
      if !child.fits(end) {
        break;
      }
      at = child.end(end);
    }
  }
}

// ── Assembly ────────────────────────────────────────────────────

/// Seconds per tick.
fn tick(scale: u64) -> f64 {
  scale as f64 / 1e9
}

/// Span of one track's blocks plus one frame, as the duration estimate when
/// `Info` carries none.
fn span_sec(stats: &TrackStats, scale: u64, default_duration_ns: Option<u64>) -> Option<f64> {
  if stats.blocks == 0 {
    return None;
  }
  let span = (stats.last_ticks - stats.first_ticks) as f64 * tick(scale);
  let frame = match default_duration_ns {
    Some(ns) => ns as f64 / 1e9,
    None if stats.blocks > 1 => span / (stats.blocks - 1) as f64,
    None => 0.0,
  };
  Some(span + frame)
}

pub fn parse(b: &[u8], kind: VideoKind) -> Result<VideoMeta, String> {
  let limit = b.len();
  let header = element(b, 0, limit)
    .filter(|e| e.id == id::EBML)
    .ok_or("not an EBML stream")?;
  let mut m = VideoMeta::empty(kind);
  if !header.fits(limit) {
    m.truncated = true;
    return Ok(m);
  }
  let mut at = header.end(limit);
  let segment = loop {
    let Some(el) = element(b, at, limit) else {
      m.truncated = true;
      return Ok(m);
    };
    if el.id == id::SEGMENT {
      break el;
    }
    if el.size.is_none() {
      return Err("unknown-size element before the Segment".into());
    }
    if !el.fits(limit) {
      m.truncated = true;
      return Ok(m);
    }
    at = el.end(limit);
  };

  let mut scan = Scan::new();
  m.declared_size = segment.declared_end();
  if !segment.fits(limit) {
    scan.truncated = true;
  }
  scan.segment(b, &segment, limit);

  m.truncated = scan.truncated;
  m.complete = scan.tracks_seen;
  m.creation_time = scan.date_utc.map(meta::mkv_time);
  m.title = scan.title.clone();
  m.writing_app = scan.writing_app.clone();
  m.track_count = scan.tracks.len() as u32;
  let scale = scan.timecode_scale;

  let video_entry = scan
    .tracks
    .iter()
    .find(|t| t.kind == TRACK_VIDEO && t.pixel.is_some());
  let stats_of = |number: u64| scan.stats.iter().find(|s| s.number == number);

  // Movie duration: Info first, else the longest track span.
  m.duration_sec = scan.duration_ticks.map(|d| d * tick(scale)).or_else(|| {
    scan
      .tracks
      .iter()
      .filter_map(|t| stats_of(t.number).and_then(|s| span_sec(s, scale, t.default_duration_ns)))
      .reduce(f64::max)
  });

  m.video_track_count = scan.tracks.iter().filter(|t| t.kind == TRACK_VIDEO).count() as u32;
  if let Some(t) = video_entry {
    let (pw, ph) = t.pixel.unwrap_or((0, 0));
    let width = u32::try_from(pw).unwrap_or(u32::MAX);
    let height = u32::try_from(ph).unwrap_or(u32::MAX);
    let (display_width, display_height) = match t.display {
      // DisplayUnit 0 is pixels; 3 is a display aspect ratio.
      Some((Some(dw), Some(dh), 0)) if dw > 0 && dh > 0 => (
        u32::try_from(dw).unwrap_or(u32::MAX),
        u32::try_from(dh).unwrap_or(u32::MAX),
      ),
      Some((Some(dw), Some(dh), 3)) if dw > 0 && dh > 0 && ph > 0 => (
        ((ph as f64 * dw as f64 / dh as f64).round() as u64)
          .try_into()
          .unwrap_or(u32::MAX),
        height,
      ),
      _ => (width, height),
    };
    let stats = stats_of(t.number);
    let frame_rate = t.default_duration_ns.map(|ns| 1e9 / ns as f64).or_else(|| {
      let s = stats?;
      let span = (s.last_ticks - s.first_ticks) as f64 * tick(scale);
      (s.blocks > 1 && span > 0.0).then(|| (s.blocks - 1) as f64 / span)
    });
    let duration_sec = m.duration_sec;
    m.video = Some(VideoTrackMeta {
      track_id: u32::try_from(t.number).unwrap_or(u32::MAX),
      codec: meta::codec_from_matroska_id(&t.codec_id),
      codec_tag: Some(t.codec_id.clone()),
      width,
      height,
      display_width,
      display_height,
      rotation: 0,
      duration_sec,
      timescale: None,
      sample_count: stats.map(|s| s.blocks),
      frame_rate,
      constant_frame_rate: t.default_duration_ns.map(|_| true),
      bitrate: stats
        .zip(duration_sec)
        .and_then(|(s, d)| meta::bitrate(s.bytes, d)),
      language: t.language.clone(),
    });
  }

  for t in scan.tracks.iter().filter(|t| t.kind == TRACK_AUDIO) {
    let (rate, channels, depth) = t.audio.unwrap_or((0.0, 0, None));
    let stats = stats_of(t.number);
    m.audio.push(AudioTrackMeta {
      track_id: u32::try_from(t.number).unwrap_or(u32::MAX),
      codec: meta::codec_from_matroska_id(&t.codec_id),
      codec_tag: Some(t.codec_id.clone()),
      channels: u32::try_from(channels).unwrap_or(u32::MAX),
      sample_rate: rate.round().max(0.0) as u32,
      bits_per_sample: depth.and_then(|d| u32::try_from(d).ok()),
      duration_sec: m.duration_sec,
      sample_count: stats.map(|s| s.blocks),
      bitrate: stats
        .zip(m.duration_sec)
        .and_then(|(s, d)| meta::bitrate(s.bytes, d)),
      language: t.language.clone(),
    });
  }

  // Keyframes: Cues when the file has them, else the sync blocks seen.
  if let Some(t) = video_entry {
    let number = t.number;
    let mut from_cues: Vec<f64> = scan
      .cues
      .iter()
      .filter(|(_, track)| track.is_none_or(|n| n == number))
      .map(|(ticks, _)| *ticks as f64 * tick(scale))
      .collect();
    from_cues.sort_by(f64::total_cmp);
    from_cues.dedup();
    let sample_count = stats_of(number).map(|s| s.blocks);
    m.keyframes = if !from_cues.is_empty() {
      Keyframes {
        times_sec: from_cues,
        source: KeyframeSource::Cues,
        sample_count,
        complete: true,
      }
    } else {
      let mut from_blocks: Vec<f64> = scan
        .block_keyframes
        .iter()
        .filter(|(track, _)| *track == number)
        .map(|(_, ticks)| (*ticks).max(0) as f64 * tick(scale))
        .collect();
      from_blocks.sort_by(f64::total_cmp);
      from_blocks.dedup();
      if from_blocks.is_empty() {
        Keyframes::default()
      } else {
        Keyframes {
          times_sec: from_blocks,
          source: KeyframeSource::Blocks,
          sample_count,
          complete: !scan.truncated,
        }
      }
    };
  }
  Ok(m)
}
