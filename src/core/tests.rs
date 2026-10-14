use std::sync::Arc;

use super::audio::{self, AudioOutcome, DecodeConfig, Pcm};
use super::boxes;
use super::meta::{self, KeyframeSource, VideoMeta};
use super::mp4::RunCursor;
use super::sniff::{self, VideoKind};
use super::waveform::{Resolution, WaveformConfig};
use super::{WaveformOutcome, audio_peaks};

fn fixture(name: &str) -> Vec<u8> {
  std::fs::read(format!(
    "{}/__test__/fixtures/{name}",
    env!("CARGO_MANIFEST_DIR")
  ))
  .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn parse(name: &str) -> VideoMeta {
  meta::parse_video(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
  (a - b).abs() <= tolerance
}

fn peaks_of(name: &str) -> WaveformOutcome {
  audio_peaks(
    Arc::new(fixture(name)),
    DecodeConfig::default(),
    WaveformConfig {
      resolution: Resolution::PeakCount(64),
      parallel: false,
    },
  )
  .unwrap()
}

// ── sniffing ────────────────────────────────────────────────────

#[test]
fn sniff_classifies_every_fixture() {
  let cases = [
    ("h264-aac-faststart.mp4", VideoKind::Mp4),
    ("h264-aac-mdat-first.mp4", VideoKind::Mp4),
    ("hevc-aac.mp4", VideoKind::Mp4),
    ("av1-aac.mp4", VideoKind::Mp4),
    ("rotated-90.mp4", VideoKind::Mp4),
    ("silent.mp4", VideoKind::Mp4),
    ("two-audio.mp4", VideoKind::Mp4),
    ("h264-aac.mov", VideoKind::Mov),
    ("h264-aac.mkv", VideoKind::Mkv),
    ("vp9-opus.webm", VideoKind::Webm),
    ("vp9-vorbis.webm", VideoKind::Webm),
    ("vp9-opus-live.webm", VideoKind::Webm),
  ];
  for (name, kind) in cases {
    assert_eq!(sniff::sniff(&fixture(name)), Some(kind), "{name}");
  }
  assert_eq!(sniff::sniff(&[]), None);
  assert_eq!(sniff::sniff(b"RIFF....WAVEfmt "), None);
  assert_eq!(sniff::sniff(&[0xff, 0xfb, 0x90, 0x00]), None);
  // A bare EBML magic with no DocType is Matroska, not WebM.
  assert_eq!(sniff::sniff(&sniff::EBML_MAGIC), Some(VideoKind::Mkv));
  // moov-first QuickTime without an ftyp still reads as ISOBMFF.
  assert_eq!(sniff::sniff(b"\0\0\0\x08moov"), Some(VideoKind::Mp4));
}

// ── ISOBMFF layout ──────────────────────────────────────────────

#[test]
fn box_walker_reads_every_header_form() {
  let mut b = Vec::new();
  b.extend_from_slice(&16u32.to_be_bytes());
  b.extend_from_slice(b"ftypisom");
  b.extend_from_slice(&0u32.to_be_bytes());
  b.extend_from_slice(&1u32.to_be_bytes());
  b.extend_from_slice(b"mdat");
  b.extend_from_slice(&40u64.to_be_bytes());
  b.extend_from_slice(&[0; 24]);
  b.extend_from_slice(&0u32.to_be_bytes());
  b.extend_from_slice(b"free");
  let heads: Vec<_> = boxes::children(&b, 0, b.len()).collect();
  assert_eq!(heads.len(), 3);
  assert_eq!(
    (&heads[0].kind, heads[0].body, heads[0].end),
    (b"ftyp", 8, 16)
  );
  assert_eq!(
    (&heads[1].kind, heads[1].body, heads[1].end),
    (b"mdat", 32, 56)
  );
  assert_eq!((&heads[2].kind, heads[2].end), (b"free", u64::MAX));
  // A size smaller than its own header is corrupt, not an infinite loop.
  assert!(boxes::read_head(&[0, 0, 0, 4, b'f', b'r', b'e', b'e'], 0, 8).is_none());
}

#[test]
fn layout_flags_a_cut_moov_as_partial() {
  let full = fixture("h264-aac-faststart.mp4");
  let (moov_start, moov_end) = boxes::layout(&full).moov.expect("moov");
  let cut = &full[..moov_start + (moov_end - moov_start) / 2];
  let layout = boxes::layout(cut);
  assert!(layout.moov.is_none());
  assert!(layout.moov_partial);
  assert!(layout.truncated);
  assert_eq!(layout.moov_before_mdat, None);
}

// ── MP4 specs ───────────────────────────────────────────────────

#[test]
fn faststart_mp4_specs_match_ffprobe() {
  let data = fixture("h264-aac-faststart.mp4");
  let m = meta::parse_video(&data).unwrap();
  assert_eq!(m.kind, VideoKind::Mp4);
  assert!(m.complete && !m.truncated && !m.fragmented);
  assert_eq!(m.declared_size, Some(data.len() as u64));
  assert_eq!(m.moov_before_mdat, Some(true));
  assert_eq!(&m.boxes[..2], ["ftyp", "moov"]);
  assert!(m.boxes.contains(&"mdat".to_owned()));
  let brands = m.brands.as_ref().expect("brands");
  assert_eq!(brands.major, "isom");
  assert!(brands.compatible.iter().any(|b| b == "avc1"));
  assert_eq!(m.timescale, Some(1000));
  assert!(
    close(m.duration_sec.unwrap(), 4.0, 1e-9),
    "{:?}",
    m.duration_sec
  );
  assert_eq!(m.creation_time.as_deref(), Some("2026-10-14T12:00:00Z"));
  assert_eq!(
    (m.track_count, m.video_track_count, m.audio.len()),
    (2, 1, 1)
  );

  let v = m.video.as_ref().expect("video track");
  assert_eq!(
    (v.codec.as_str(), v.codec_tag.as_deref()),
    ("h264", Some("avc1"))
  );
  assert_eq!(
    (v.width, v.height, v.display_width, v.display_height),
    (320, 180, 320, 180)
  );
  assert_eq!(v.rotation, 0);
  assert_eq!(v.sample_count, Some(120));
  assert_eq!(v.timescale, Some(15_360));
  assert!(
    close(v.frame_rate.unwrap(), 30.0, 1e-9),
    "{:?}",
    v.frame_rate
  );
  assert_eq!(v.constant_frame_rate, Some(true));
  assert!(close(v.duration_sec.unwrap(), 4.0, 1e-9));
  assert!(
    v.bitrate.is_some_and(|b| b > 50_000 && b < 2_000_000),
    "{:?}",
    v.bitrate
  );
  assert_eq!(v.language, None);

  let a = &m.audio[0];
  assert_eq!(
    (a.codec.as_str(), a.codec_tag.as_deref()),
    ("aac", Some("mp4a"))
  );
  assert_eq!(
    (a.channels, a.sample_rate, a.bits_per_sample),
    (2, 48_000, Some(16))
  );
  assert!(
    close(a.duration_sec.unwrap(), 4.0, 0.1),
    "{:?}",
    a.duration_sec
  );
  assert!(
    a.bitrate.is_some_and(|b| b > 32_000 && b < 200_000),
    "{:?}",
    a.bitrate
  );

  assert_eq!(m.keyframes.source, KeyframeSource::Stss);
  assert_eq!(m.keyframes.sample_count, Some(120));
  let times = &m.keyframes.times_sec;
  assert_eq!(times.len(), 4, "{times:?}");
  for (t, expected) in times.iter().zip([0.0, 1.0, 2.0, 3.0]) {
    assert!(close(*t, expected, 1e-6), "{times:?}");
  }
  let whole = m.bitrate_bps(data.len()).unwrap();
  assert_eq!(whole, (data.len() as f64 * 8.0 / 4.0).round() as u32);
}

#[test]
fn mdat_first_mp4_shares_specs_but_not_layout() {
  let fast = parse("h264-aac-faststart.mp4");
  let slow = parse("h264-aac-mdat-first.mp4");
  assert_eq!(slow.moov_before_mdat, Some(false));
  assert_eq!(slow.video, fast.video);
  assert_eq!(slow.audio, fast.audio);
  assert_eq!(slow.keyframes, fast.keyframes);
  assert_eq!(slow.duration_sec, fast.duration_sec);
}

#[test]
fn prefix_without_moov_is_incomplete_and_truncated() {
  let full = fixture("h264-aac-mdat-first.mp4");
  let m = meta::parse_video(&full[..65_536]).unwrap();
  assert!(!m.complete);
  assert!(m.truncated);
  // Only the cut mdat's end is known: a lower bound on the file, not its size.
  let declared = m.declared_size.unwrap();
  assert!(
    declared > 65_536 && declared < full.len() as u64,
    "{declared}"
  );
  assert!(m.video.is_none() && m.audio.is_empty());
  assert_eq!(m.moov_before_mdat, None);
  assert_eq!(m.keyframes.source, KeyframeSource::None);
  assert!(m.brands.is_some(), "ftyp still parsed from the prefix");
}

#[test]
fn header_prefix_is_complete_but_truncated() {
  let full = fixture("h264-aac-faststart.mp4");
  let (_, moov_end) = boxes::layout(&full).moov.unwrap();
  let prefix = &full[..moov_end + 16];
  let m = meta::parse_video(prefix).unwrap();
  assert!(m.complete);
  assert!(m.truncated);
  assert_eq!(m.moov_before_mdat, Some(true));
  assert_eq!(m.keyframes.times_sec.len(), 4);
  assert_eq!(m.audio.len(), 1);
  assert_eq!(m.declared_size, Some(full.len() as u64));
  assert!(meta::is_truncated(prefix).unwrap());
  assert!(!meta::is_truncated(&full).unwrap());
  let outcome = audio_peaks(
    Arc::new(prefix.to_vec()),
    DecodeConfig::default(),
    WaveformConfig::default(),
  )
  .unwrap();
  assert!(matches!(outcome, WaveformOutcome::Truncated), "{outcome:?}");
}

#[test]
fn hevc_av1_and_mov_are_named_from_their_tags() {
  let hevc = parse("hevc-aac.mp4");
  let v = hevc.video.as_ref().unwrap();
  assert_eq!(
    (v.codec.as_str(), v.codec_tag.as_deref()),
    ("hevc", Some("hvc1"))
  );
  assert_eq!((v.width, v.height, v.sample_count), (320, 180, Some(120)));
  assert_eq!(hevc.keyframes.times_sec.len(), 4);

  let av1 = parse("av1-aac.mp4");
  let v = av1.video.as_ref().unwrap();
  assert_eq!(
    (v.codec.as_str(), v.codec_tag.as_deref()),
    ("av1", Some("av01"))
  );
  assert_eq!((v.width, v.height), (320, 180));

  let mov = parse("h264-aac.mov");
  assert_eq!(mov.kind, VideoKind::Mov);
  assert_eq!(mov.kind.mime(), "video/quicktime");
  assert_eq!(mov.brands.as_ref().unwrap().major, "qt  ");
  assert_eq!(mov.video.as_ref().unwrap().codec, "h264");
}

#[test]
fn rotation_comes_from_the_track_matrix() {
  let m = parse("rotated-90.mp4");
  let v = m.video.as_ref().unwrap();
  assert_eq!(v.rotation, 90);
  assert_eq!((v.width, v.height), (320, 180));
  assert_eq!((v.display_width, v.display_height), (180, 320));
  let m = parse("rotated-270.mp4");
  let v = m.video.as_ref().unwrap();
  assert_eq!(v.rotation, 270);
  assert_eq!((v.display_width, v.display_height), (180, 320));
  assert_eq!(parse("h264-aac-faststart.mp4").video.unwrap().rotation, 0);
}

#[test]
fn silent_and_multi_track_audio() {
  let silent = parse("silent.mp4");
  assert!(silent.audio.is_empty());
  assert_eq!((silent.track_count, silent.video_track_count), (1, 1));
  assert!(matches!(peaks_of("silent.mp4"), WaveformOutcome::NoAudio));

  let two = parse("two-audio.mp4");
  assert_eq!(two.audio.len(), 2);
  assert_eq!(
    (two.audio[0].channels, two.audio[0].sample_rate),
    (2, 48_000)
  );
  assert_eq!(
    (two.audio[1].channels, two.audio[1].sample_rate),
    (1, 44_100)
  );
  assert_eq!(two.track_count, 3);
}

// ── Matroska / WebM ─────────────────────────────────────────────

#[test]
fn matroska_and_webm_specs() {
  let mkv = parse("h264-aac.mkv");
  assert_eq!(mkv.kind, VideoKind::Mkv);
  assert!(mkv.complete && !mkv.truncated);
  assert_eq!(mkv.moov_before_mdat, None);
  assert!(mkv.boxes.is_empty());
  assert!(
    close(mkv.duration_sec.unwrap(), 4.021, 0.01),
    "{:?}",
    mkv.duration_sec
  );
  assert_eq!(
    mkv.declared_size,
    Some(fixture("h264-aac.mkv").len() as u64)
  );
  let v = mkv.video.as_ref().unwrap();
  assert_eq!(
    (v.codec.as_str(), v.codec_tag.as_deref()),
    ("h264", Some("V_MPEG4/ISO/AVC"))
  );
  assert_eq!(
    (v.width, v.height, v.display_width, v.display_height),
    (320, 180, 320, 180)
  );
  assert!(
    close(v.frame_rate.unwrap(), 30.0, 1e-3),
    "{:?}",
    v.frame_rate
  );
  assert_eq!(v.sample_count, Some(120));
  assert!(v.bitrate.is_some_and(|b| b > 50_000), "{:?}", v.bitrate);
  assert_eq!(mkv.audio.len(), 1);
  assert_eq!(
    (
      mkv.audio[0].codec.as_str(),
      mkv.audio[0].channels,
      mkv.audio[0].sample_rate
    ),
    ("aac", 2, 48_000)
  );
  assert!(mkv.audio[0].sample_count.is_some_and(|n| n > 100));
  assert!(
    mkv
      .writing_app
      .as_deref()
      .is_some_and(|w| w.contains("Lavf"))
  );
  // Cues, in the container's own millisecond ticks: ffprobe's K-packet pts.
  assert_eq!(mkv.keyframes.source, KeyframeSource::Cues);
  assert!(mkv.keyframes.complete);
  assert_eq!(mkv.keyframes.sample_count, Some(120));
  let times = &mkv.keyframes.times_sec;
  assert_eq!(times.len(), 4, "{times:?}");
  for (t, expected) in times.iter().zip([0.0, 1.021, 2.021, 3.021]) {
    assert!(close(*t, expected, 1e-9), "{times:?}");
  }

  let opus = parse("vp9-opus.webm");
  assert_eq!(opus.kind, VideoKind::Webm);
  assert_eq!(opus.kind.mime(), "video/webm");
  assert_eq!(opus.video.as_ref().unwrap().codec, "vp9");
  assert_eq!(opus.audio[0].codec, "opus");
  assert_eq!(opus.keyframes.source, KeyframeSource::Cues);
  for (t, expected) in opus
    .keyframes
    .times_sec
    .iter()
    .zip([0.007, 1.007, 2.007, 3.007])
  {
    assert!(close(*t, expected, 1e-9), "{:?}", opus.keyframes.times_sec);
  }
  let vorbis = parse("vp9-vorbis.webm");
  assert_eq!(vorbis.audio[0].codec, "vorbis");
  assert_eq!(vorbis.audio[0].sample_rate, 48_000);
}

#[test]
fn live_webm_without_cues_or_duration() {
  let data = fixture("vp9-opus-live.webm");
  let m = meta::parse_video(&data).unwrap();
  assert_eq!(m.kind, VideoKind::Webm);
  assert!(m.complete, "Tracks were read");
  assert!(!m.truncated, "unknown-size segment, every element whole");
  assert_eq!(m.declared_size, None);
  // No Info.Duration: the span of the blocks plus one frame.
  assert!(
    close(m.duration_sec.unwrap(), 4.0, 0.15),
    "{:?}",
    m.duration_sec
  );
  let v = m.video.as_ref().unwrap();
  assert_eq!((v.codec.as_str(), v.width, v.height), ("vp9", 320, 180));
  assert_eq!(v.sample_count, Some(120));
  assert!(
    close(v.frame_rate.unwrap(), 30.0, 0.5),
    "{:?}",
    v.frame_rate
  );
  assert_eq!(m.audio[0].codec, "opus");
  // No Cues: sync blocks from the cluster walk, the same four instants.
  assert_eq!(m.keyframes.source, KeyframeSource::Blocks);
  assert!(m.keyframes.complete);
  let times = &m.keyframes.times_sec;
  assert_eq!(times.len(), 4, "{times:?}");
  for (t, expected) in times.iter().zip([0.007, 1.007, 2.007, 3.007]) {
    assert!(close(*t, expected, 1e-9), "{times:?}");
  }
}

#[test]
fn webm_prefix_keeps_the_header_and_marks_the_index_partial() {
  let full = fixture("vp9-vorbis.webm");
  // 4 KiB cuts Tracks itself (the Vorbis setup header is ~3 KiB): not whole.
  let short = meta::parse_video(&full[..4_096]).unwrap();
  assert!(!short.complete && short.truncated);
  assert_eq!(short.declared_size, Some(full.len() as u64));
  // 16 KiB holds the header and the first cluster's sync block.
  let prefix = &full[..16_384];
  let m = meta::parse_video(prefix).unwrap();
  assert!(m.complete, "Tracks whole inside the prefix");
  assert!(m.truncated);
  assert_eq!(m.declared_size, Some(full.len() as u64));
  assert_eq!(m.video.as_ref().unwrap().codec, "vp9");
  assert_eq!(m.audio[0].codec, "vorbis");
  // Cues sit at the end of the file; only the first sync block is in reach.
  assert_eq!(m.keyframes.source, KeyframeSource::Blocks);
  assert!(!m.keyframes.complete);
  assert_eq!(
    m.keyframes.times_sec.len(),
    1,
    "{:?}",
    m.keyframes.times_sec
  );
  assert!(meta::is_truncated(prefix).unwrap());
  assert!(!meta::is_truncated(&full).unwrap());
  let outcome = audio_peaks(
    Arc::new(prefix.to_vec()),
    DecodeConfig::default(),
    WaveformConfig::default(),
  )
  .unwrap();
  assert!(matches!(outcome, WaveformOutcome::Truncated), "{outcome:?}");
  // A cut inside the live file (no declared sizes) is still caught by the block walk.
  let live = fixture("vp9-opus-live.webm");
  let cut = meta::parse_video(&live[..live.len() / 2]).unwrap();
  assert!(cut.complete && cut.truncated);
  assert_eq!(cut.declared_size, None);
  assert!(!cut.keyframes.complete);
  assert!(cut.keyframes.times_sec.len() < 4);
}

// ── Audio track decode + peaks ──────────────────────────────────

fn reference(name: &str) -> Vec<f32> {
  fixture(name)
    .as_chunks::<4>()
    .0
    .iter()
    .map(|c| f32::from_le_bytes(*c))
    .collect()
}

/// Max absolute error between two interleaved stereo streams after aligning
/// them on the lag (in frames) that minimises error over a mid-stream window,
/// since encoders and decoders disagree on priming-sample trimming.
fn aligned_max_error(ours: &[f32], theirs: &[f32], channels: usize) -> (i64, f32) {
  let window = 4_800 * channels;
  let start = 96_000 * channels;
  let mut best = (0i64, f32::INFINITY);
  for lag in -3_072i64..=3_072 {
    let shift = lag * channels as i64;
    let mut max = 0f32;
    for i in 0..window {
      let a = ours[start + i];
      let j = (start + i) as i64 + shift;
      let b = theirs[j as usize];
      max = max.max((a - b).abs());
      if max >= best.1 {
        break;
      }
    }
    if max < best.1 {
      best = (lag, max);
    }
  }
  best
}

fn decode_fixture(name: &str) -> Pcm {
  match audio::decode(Arc::new(fixture(name)), DecodeConfig::default()).unwrap() {
    AudioOutcome::Pcm(p) => p,
    other => panic!("{name}: {other:?}"),
  }
}

#[test]
fn aac_track_decodes_to_the_ffmpeg_reference() {
  let pcm = decode_fixture("h264-aac-faststart.mp4");
  assert_eq!((pcm.sample_rate, pcm.channels), (48_000, 2));
  let frames = pcm.frames();
  assert!((189_000..=194_000).contains(&frames), "frames {frames}");
  let reference = reference("h264-aac-faststart.mp4.reference.f32");
  let (lag, error) = aligned_max_error(&pcm.samples, &reference, 2);
  assert!(error < 5e-3, "lag {lag} frames, max error {error}");
  assert!(lag.abs() <= 2_048, "lag {lag}");
}

#[test]
fn vorbis_track_decodes_to_the_ffmpeg_reference() {
  let pcm = decode_fixture("vp9-vorbis.webm");
  assert_eq!((pcm.sample_rate, pcm.channels), (48_000, 2));
  let reference = reference("vp9-vorbis.webm.reference.f32");
  let (lag, error) = aligned_max_error(&pcm.samples, &reference, 2);
  assert!(error < 5e-3, "lag {lag} frames, max error {error}");
}

#[test]
fn peaks_outcomes_per_container() {
  match peaks_of("h264-aac-faststart.mp4") {
    WaveformOutcome::Peaks(w) => {
      assert_eq!((w.sample_rate, w.channel_count), (48_000, 2));
      assert_eq!(w.boundaries.len(), 65);
      assert!(close(w.duration_sec, 4.0, 0.05), "{}", w.duration_sec);
      // 0.55 full-scale sine in opposite phase on both channels.
      for channel in &w.channels {
        let peak = channel.peak.iter().cloned().fold(0f32, f32::max);
        assert!(close(f64::from(peak), 0.55, 0.03), "peak {peak}");
      }
    }
    other => panic!("{other:?}"),
  }
  assert!(matches!(
    peaks_of("vp9-vorbis.webm"),
    WaveformOutcome::Peaks(_)
  ));
  match peaks_of("vp9-opus.webm") {
    WaveformOutcome::Unsupported(reason) => assert!(reason.contains("unsupported"), "{reason}"),
    other => panic!("{other:?}"),
  }
  assert!(matches!(
    peaks_of("two-audio.mp4"),
    WaveformOutcome::Peaks(_)
  ));
  assert!(matches!(
    peaks_of("vp9-opus-live.webm"),
    WaveformOutcome::Unsupported(_)
  ));
}

#[test]
fn decode_rejects_bad_config_and_magic() {
  let err = audio::decode(
    Arc::new(fixture("silent.mp4")),
    DecodeConfig {
      max_decoded_samples: 0,
      ..DecodeConfig::default()
    },
  )
  .unwrap_err();
  assert!(err.contains("maxDecodedSamples"));
  assert!(audio::decode(Arc::new(vec![0, 1, 2, 3]), DecodeConfig::default()).is_err());
  assert!(meta::parse_video(&[0, 1, 2, 3]).is_err());
  let err = audio::decode(
    Arc::new(fixture("h264-aac-faststart.mp4")),
    DecodeConfig {
      max_decoded_samples: 1_000,
      ..DecodeConfig::default()
    },
  )
  .unwrap_err();
  assert!(err.contains("maxDecodedSamples"), "{err}");
}

// ── Pure helpers ────────────────────────────────────────────────

#[test]
fn run_cursor_accumulates_run_length_tables() {
  let runs = [(3u64, 10i64), (2, 20), (1, 5)];
  let mut c = RunCursor::new(&runs);
  assert_eq!(c.advance(0, |r| r.0, |r| r.1), Some(0));
  assert_eq!(c.advance(3, |r| r.0, |r| r.1), Some(30));
  assert_eq!(c.current(|r| r.1), Some(20));
  assert_eq!(c.advance(4, |r| r.0, |r| r.1), Some(50));
  assert_eq!(c.advance(5, |r| r.0, |r| r.1), Some(70));
  assert_eq!(c.current(|r| r.1), Some(5));
  assert_eq!(c.advance(6, |r| r.0, |r| r.1), Some(75));
  assert_eq!(c.advance(7, |r| r.0, |r| r.1), None);
}

#[test]
fn rotation_snaps_to_quarter_turns() {
  let one = 1 << 16;
  assert_eq!(meta::rotation_from_matrix(one, 0), 0);
  assert_eq!(meta::rotation_from_matrix(0, one), 90);
  assert_eq!(meta::rotation_from_matrix(-one, 0), 180);
  assert_eq!(meta::rotation_from_matrix(0, -one), 270);
  assert_eq!(meta::rotation_from_matrix(0, 0), 0);
  assert_eq!(meta::rotated_dims(320, 180, 90), (180, 320));
  assert_eq!(meta::rotated_dims(320, 180, 180), (320, 180));
}

#[test]
fn labels_and_dates() {
  assert_eq!(meta::aspect_label(1920, 1080).as_deref(), Some("16:9"));
  assert_eq!(meta::aspect_label(1080, 1920).as_deref(), Some("9:16"));
  assert_eq!(meta::aspect_label(1366, 768).as_deref(), Some("16:9"));
  assert_eq!(meta::aspect_label(320, 180).as_deref(), Some("16:9"));
  assert_eq!(meta::aspect_label(1000, 1000).as_deref(), Some("1:1"));
  assert_eq!(meta::aspect_label(0, 10), None);
  assert_eq!(meta::resolution_label(1920, 1080).as_deref(), Some("1080p"));
  assert_eq!(meta::resolution_label(1080, 1920).as_deref(), Some("1080p"));
  assert_eq!(meta::resolution_label(3840, 2160).as_deref(), Some("4K"));
  assert_eq!(meta::resolution_label(320, 180).as_deref(), Some("180p"));
  assert_eq!(
    meta::Orientation::of(320, 180),
    Some(meta::Orientation::Landscape)
  );
  assert_eq!(
    meta::Orientation::of(180, 320),
    Some(meta::Orientation::Portrait)
  );
  assert_eq!(meta::Orientation::of(7, 7), Some(meta::Orientation::Square));
  assert_eq!(meta::iso8601(0), "1970-01-01T00:00:00Z");
  assert_eq!(meta::iso8601(951_782_400), "2000-02-29T00:00:00Z");
  assert_eq!(meta::iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
  assert_eq!(meta::mp4_time(0), None);
  assert_eq!(
    meta::mp4_time(2_082_844_800).as_deref(),
    Some("1970-01-01T00:00:00Z")
  );
  assert_eq!(meta::mkv_time(0), "2001-01-01T00:00:00Z");
  assert_eq!(meta::video_codec_from_tag("hev1"), "hevc");
  assert_eq!(meta::audio_codec_from_tag("ec-3"), "eac3");
  assert_eq!(meta::codec_from_matroska_id("A_AAC/MPEG4/LC"), "aac");
  assert_eq!(meta::bitrate(1_000, 2.0), Some(4_000));
  assert_eq!(meta::bitrate(0, 2.0), None);
}
