//! ISOBMFF (MP4 / MOV) metadata through `mp4parse`, fed only the `moov` box
//! the walker located, so a truncated `mdat` never makes the parse fail.
//! The walker supplies what `mp4parse` keeps private: brands, `mvhd` times,
//! the sample-entry fourcc and the `mdhd` language.

use std::io::Cursor;

use mp4parse::{AudioCodecSpecific, CodecType, SampleEntry, TimeOffsetVersion, Track, TrackType};

use super::boxes::{self, Layout, Trak, fourcc_string};
use super::meta::{
  self, AudioTrackMeta, Brands, KeyframeSource, Keyframes, VideoMeta, VideoTrackMeta,
};
use super::sniff::VideoKind;

fn from_layout(kind: VideoKind, layout: &Layout) -> VideoMeta {
  let mut m = VideoMeta::empty(kind);
  m.truncated = layout.truncated;
  m.declared_size = layout.declared_size;
  m.fragmented = layout.fragmented;
  m.boxes = layout.order.clone();
  m.moov_before_mdat = layout.moov_before_mdat;
  m.brands = layout.ftyp.as_ref().map(|f| Brands {
    major: f.major.clone(),
    minor_version: f.minor_version,
    compatible: f.compatible.clone(),
  });
  if let Some(mvhd) = &layout.mvhd {
    m.timescale = (mvhd.timescale > 0).then_some(mvhd.timescale);
    m.creation_time = meta::mp4_time(mvhd.creation);
    m.modification_time = meta::mp4_time(mvhd.modification);
    // All-ones means "unknown" (fragmented files); zero means unset.
    if mvhd.timescale > 0
      && mvhd.duration > 0
      && mvhd.duration != u64::from(u32::MAX)
      && mvhd.duration != u64::MAX
    {
      m.duration_sec = Some(mvhd.duration as f64 / f64::from(mvhd.timescale));
    }
  }
  m
}

/// Walks run-length tables (`stts`, `ctts`) by ascending 0-based sample
/// index, accumulating the per-sample value up to the target.
pub(crate) struct RunCursor<'a, T> {
  runs: &'a [T],
  run: usize,
  consumed_in_run: u64,
  sample: u64,
  total: i64,
}

impl<'a, T> RunCursor<'a, T> {
  pub(crate) fn new(runs: &'a [T]) -> Self {
    RunCursor {
      runs,
      run: 0,
      consumed_in_run: 0,
      sample: 0,
      total: 0,
    }
  }

  /// Sum of per-sample values for samples `[0, target)`, i.e. the value
  /// accumulated before `target`. Returns `None` past the table's end.
  pub(crate) fn advance(
    &mut self,
    target: u64,
    count: impl Fn(&T) -> u64,
    value: impl Fn(&T) -> i64,
  ) -> Option<i64> {
    while self.sample < target {
      let run = self.runs.get(self.run)?;
      let remaining = count(run).saturating_sub(self.consumed_in_run);
      let step = remaining.min(target - self.sample);
      if step == 0 {
        self.run += 1;
        self.consumed_in_run = 0;
        continue;
      }
      self.total = self
        .total
        .saturating_add(value(run).saturating_mul(step as i64));
      self.sample += step;
      self.consumed_in_run += step;
      if self.consumed_in_run >= count(run) {
        self.run += 1;
        self.consumed_in_run = 0;
      }
    }
    Some(self.total)
  }

  /// The per-sample value at the cursor's current sample.
  pub(crate) fn current(&self, value: impl Fn(&T) -> i64) -> Option<i64> {
    self.runs.get(self.run).map(value)
  }
}

struct Timing {
  timescale: Option<u32>,
  duration_sec: Option<f64>,
  sample_count: Option<u64>,
  /// Duration in track ticks as summed from `stts`.
  stts_ticks: Option<u64>,
  constant_delta: Option<bool>,
}

fn timing(track: &Track) -> Timing {
  let timescale = track
    .timescale
    .and_then(|t| u32::try_from(t.0).ok())
    .filter(|t| *t > 0);
  let duration_sec = match (track.duration, timescale) {
    (Some(d), Some(ts)) if d.0 > 0 && d.0 != u64::MAX && d.0 != u64::from(u32::MAX) => {
      Some(d.0 as f64 / f64::from(ts))
    }
    _ => None,
  };
  let (sample_count, stts_ticks, constant_delta) = match &track.stts {
    Some(stts) if !stts.samples.is_empty() => {
      let count = stts
        .samples
        .iter()
        .map(|s| u64::from(s.sample_count))
        .sum::<u64>();
      let ticks = stts
        .samples
        .iter()
        .map(|s| u64::from(s.sample_count) * u64::from(s.sample_delta))
        .sum::<u64>();
      // The final run may hold one differently timed sample without the
      // stream being variable rate.
      let body = &stts.samples[..stts.samples.len().saturating_sub(1)];
      let first = stts.samples[0].sample_delta;
      let constant = body.iter().all(|s| s.sample_delta == first)
        && (stts.samples.len() == 1
          || stts
            .samples
            .last()
            .is_some_and(|s| s.sample_count <= 1 || s.sample_delta == first));
      (Some(count), Some(ticks), Some(constant))
    }
    _ => (None, None, None),
  };
  Timing {
    timescale,
    duration_sec,
    sample_count,
    stts_ticks,
    constant_delta,
  }
}

fn total_sample_bytes(track: &Track, sample_count: Option<u64>) -> Option<u64> {
  let stsz = track.stsz.as_ref()?;
  if stsz.sample_size != 0 {
    Some(u64::from(stsz.sample_size) * sample_count.unwrap_or(stsz.sample_sizes.len() as u64))
  } else {
    Some(stsz.sample_sizes.iter().map(|s| u64::from(*s)).sum())
  }
}

fn tag_of(trak: Option<&Trak>) -> Option<String> {
  trak
    .and_then(|t| t.sample_entry.as_ref())
    .map(fourcc_string)
}

fn video_codec(codec_type: CodecType, tag: Option<&str>) -> String {
  let known = match codec_type {
    CodecType::H264 => Some("h264"),
    CodecType::MP4V => Some("mpeg4"),
    CodecType::AV1 => Some("av1"),
    CodecType::VP9 => Some("vp9"),
    CodecType::VP8 => Some("vp8"),
    CodecType::H263 => Some("h263"),
    CodecType::EncryptedVideo => Some("encrypted"),
    _ => None,
  };
  known
    .map(str::to_owned)
    .or_else(|| tag.map(meta::video_codec_from_tag))
    .unwrap_or_else(|| "unknown".to_owned())
}

fn audio_codec(codec_type: CodecType, tag: Option<&str>) -> String {
  let known = match codec_type {
    CodecType::AAC => Some("aac"),
    CodecType::MP3 => Some("mp3"),
    CodecType::FLAC => Some("flac"),
    CodecType::Opus => Some("opus"),
    CodecType::ALAC => Some("alac"),
    CodecType::LPCM => Some("pcm"),
    CodecType::EncryptedAudio => Some("encrypted"),
    _ => None,
  };
  known
    .map(str::to_owned)
    .or_else(|| tag.map(meta::audio_codec_from_tag))
    .unwrap_or_else(|| "unknown".to_owned())
}

fn keyframes(track: &Track, timing: &Timing, movie_timescale: Option<u32>) -> Keyframes {
  let Some(timescale) = timing.timescale else {
    return Keyframes::default();
  };
  let Some(stts) = track.stts.as_ref().filter(|s| !s.samples.is_empty()) else {
    return Keyframes::default();
  };
  let sample_count = timing.sample_count.unwrap_or(0);
  // 0-based sample indices of the sync samples.
  let (targets, source): (Vec<u64>, KeyframeSource) = match &track.stss {
    Some(stss) => (
      stss
        .samples
        .iter()
        .filter(|n| **n > 0)
        .map(|n| u64::from(*n) - 1)
        .collect(),
      KeyframeSource::Stss,
    ),
    None => ((0..sample_count).collect(), KeyframeSource::AllSamples),
  };
  let media_time = track.media_time.map_or(0, |m| m.0 as i64);
  let empty_lead = match (track.empty_duration, movie_timescale) {
    (Some(e), Some(ts)) if ts > 0 => e.0 as f64 / f64::from(ts),
    _ => 0.0,
  };
  let mut dts = RunCursor::new(stts.samples.as_slice());
  let ctts = track
    .ctts
    .as_ref()
    .map(|c| RunCursor::new(c.samples.as_slice()));
  let mut ctts = ctts;
  let scale = f64::from(timescale);
  let mut times = Vec::with_capacity(targets.len());
  for target in targets {
    let Some(decode) = dts.advance(
      target,
      |s| u64::from(s.sample_count),
      |s| i64::from(s.sample_delta),
    ) else {
      break;
    };
    let offset = match ctts.as_mut() {
      Some(c) => {
        c.advance(
          target,
          |o| u64::from(o.sample_count),
          |o| match o.time_offset {
            TimeOffsetVersion::Version0(v) => i64::from(v),
            TimeOffsetVersion::Version1(v) => i64::from(v),
          },
        );
        c.current(|o| match o.time_offset {
          TimeOffsetVersion::Version0(v) => i64::from(v),
          TimeOffsetVersion::Version1(v) => i64::from(v),
        })
        .unwrap_or(0)
      }
      None => 0,
    };
    let pts = (decode + offset - media_time) as f64 / scale + empty_lead;
    times.push(pts.max(0.0));
  }
  Keyframes {
    times_sec: times,
    source,
    sample_count: Some(sample_count),
    complete: true,
  }
}

fn video_track(track: &Track, trak: Option<&Trak>) -> VideoTrackMeta {
  let timing = timing(track);
  let tag = tag_of(trak);
  let entry = track.stsd.as_ref().and_then(|s| s.descriptions.first());
  let (codec_type, coded) = match entry {
    Some(SampleEntry::Video(v)) => (
      v.codec_type,
      Some((u32::from(v.width), u32::from(v.height))),
    ),
    _ => (CodecType::Unknown, None),
  };
  let tkhd = track.tkhd.as_ref();
  let header_dims = tkhd
    .map(|t| (t.width >> 16, t.height >> 16))
    .filter(|(w, h)| *w > 0 && *h > 0);
  let (width, height) = coded.or(header_dims).unwrap_or((0, 0));
  let rotation = tkhd.map_or(0, |t| meta::rotation_from_matrix(t.matrix.a, t.matrix.b));
  let (dw, dh) = header_dims.unwrap_or((width, height));
  let (display_width, display_height) = meta::rotated_dims(dw, dh, rotation);
  let frame_rate = match (timing.sample_count, timing.stts_ticks, timing.timescale) {
    (Some(n), Some(ticks), Some(ts)) if n > 0 && ticks > 0 => {
      Some(n as f64 * f64::from(ts) / ticks as f64)
    }
    _ => None,
  };
  let bitrate = total_sample_bytes(track, timing.sample_count)
    .zip(timing.duration_sec)
    .and_then(|(bytes, d)| meta::bitrate(bytes, d));
  VideoTrackMeta {
    track_id: track.track_id.unwrap_or(track.id as u32),
    codec: video_codec(codec_type, tag.as_deref()),
    codec_tag: tag,
    width,
    height,
    display_width,
    display_height,
    rotation,
    duration_sec: timing.duration_sec,
    timescale: timing.timescale,
    sample_count: timing.sample_count,
    frame_rate,
    constant_frame_rate: timing.constant_delta,
    bitrate,
    language: trak.and_then(|t| t.language.clone()),
  }
}

fn audio_track(track: &Track, trak: Option<&Trak>) -> AudioTrackMeta {
  let timing = timing(track);
  let tag = tag_of(trak);
  let entry = track.stsd.as_ref().and_then(|s| s.descriptions.first());
  let (codec_type, channels, sample_rate, bits) = match entry {
    Some(SampleEntry::Audio(a)) => {
      // The legacy stsd fields lie for AAC (ffmpeg writes two channels for a
      // mono track); the esds AudioSpecificConfig is authoritative when present.
      let (esds_rate, esds_channels) = match &a.codec_specific {
        AudioCodecSpecific::ES_Descriptor(es) => (
          es.audio_sample_rate.filter(|r| *r > 0),
          es.audio_channel_count.filter(|c| *c > 0).map(u32::from),
        ),
        _ => (None, None),
      };
      (
        a.codec_type,
        esds_channels.unwrap_or(a.channelcount),
        esds_rate.unwrap_or(a.samplerate.round().max(0.0) as u32),
        (a.samplesize > 0).then_some(u32::from(a.samplesize)),
      )
    }
    _ => (CodecType::Unknown, 0, 0, None),
  };
  // A missing stsd rate falls back to the media timescale, which audio
  // tracks conventionally set to the sample rate.
  let sample_rate = if sample_rate > 0 {
    sample_rate
  } else {
    timing.timescale.unwrap_or(0)
  };
  let bitrate = total_sample_bytes(track, timing.sample_count)
    .zip(timing.duration_sec)
    .and_then(|(bytes, d)| meta::bitrate(bytes, d));
  AudioTrackMeta {
    track_id: track.track_id.unwrap_or(track.id as u32),
    codec: audio_codec(codec_type, tag.as_deref()),
    codec_tag: tag,
    channels,
    sample_rate,
    bits_per_sample: bits,
    duration_sec: timing.duration_sec,
    sample_count: timing.sample_count,
    bitrate,
    language: trak.and_then(|t| t.language.clone()),
  }
}

pub fn parse(b: &[u8], kind: VideoKind) -> Result<VideoMeta, String> {
  let layout = boxes::layout(b);
  let mut m = from_layout(kind, &layout);
  let Some((start, end)) = layout.moov else {
    // No whole moov in the buffer: a prefix, or a file without one.
    return Ok(m);
  };
  let context = mp4parse::read_mp4(&mut Cursor::new(&b[start..end]))
    .map_err(|e| format!("moov present but unparseable: {e}"))?;
  m.complete = true;
  m.fragmented = m.fragmented || context.mvex.is_some();
  if m.duration_sec.is_none()
    && let Some(mvex) = &context.mvex
    && let (Some(fragment), Some(ts)) = (mvex.fragment_duration, m.timescale)
    && fragment.0 > 0
  {
    m.duration_sec = Some(fragment.0 as f64 / f64::from(ts));
  }
  m.track_count = context.tracks.len() as u32;
  for (index, track) in context.tracks.iter().enumerate() {
    let trak = layout.traks.get(index);
    match track.track_type {
      TrackType::Video => {
        m.video_track_count += 1;
        if m.video.is_none() {
          let timing = timing(track);
          m.keyframes = keyframes(track, &timing, m.timescale);
          m.video = Some(video_track(track, trak));
        }
      }
      TrackType::Audio => m.audio.push(audio_track(track, trak)),
      _ => {}
    }
  }
  if m.duration_sec.is_none() {
    m.duration_sec = m.video.as_ref().and_then(|v| v.duration_sec).or_else(|| {
      m.audio
        .iter()
        .filter_map(|a| a.duration_sec)
        .reduce(f64::max)
    });
  }
  Ok(m)
}
