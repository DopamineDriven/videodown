# videodown design

Date: 2026-10-14

## Goal

`@d0paminedriven/videodown` is a napi-rs 3 native addon for Node that reads
MP4 / MOV / WebM / MKV container metadata, builds the keyframe index from the
container's own sync-sample table, and decodes the audio track to the same
exhaustive waveform envelope `@d0paminedriven/audiodown` produces. The first
consumer is a websocket server that persists generated clips (`VideoMetadata`
row, `keyframeTimes`, a poster seek time, a `waveformPeaks` column) and wants
to do so from a bounded prefix fetch whenever the file allows it.

The package mirrors audiodown (`~/rust/audiodown`) in layout, tooling and CI,
which in turn mirrors `@d0paminedriven/pdfdown` (`~/rust/napi-rs`), so the
three projects feel identical to work in.

## Non-goals

- Decoding video. No H.264 / HEVC / VP9 / AV1 decoder is viable in pure Rust,
  and linking libav would break the fourteen-target matrix and the wasm
  build. Posters, frame strips and the faststart remux belong to a
  feature-gated superset (below), never to this package.
- Opus. Symphonia 0.6 has no Opus decoder; the track is reported, not decoded.
- Writing or remuxing containers.
- Resampling, downmixing, normalization, loudness or true-peak metering.

## Layout

```
.cargo/config.toml            windows crt-static; --cfg tokio_unstable for wasm32-wasip1-threads
.github/workflows/CI.yml      template CI minus the non-threaded wasm target; clippy -D warnings + cargo test in lint
__test__/index.spec.ts        ava tests against local fixtures, truths from fixture-info.json
__test__/fixtures/            thirteen FFmpeg-generated clips + two f32 PCM references + fixture-info.json
benchmark/bench.ts            tinybench over fixtures
scripts/fixtures.py           regenerates fixtures with FFmpeg + ffprobe; stamps tkhd rotation matrices
scripts/dts-header.txt        Rm, UTR, the four *Props over VideoSpecsBase, VideoUnion, VideoSpecsObj, VideoSpecs
src/lib.rs                    napi functions, promise() helper, VideoService and VideoDown classes
src/types.rs                  #[napi(object)] boundary types + builders from core
src/core/mod.rs               audio_peaks (truncation gate → decode → reduce), analyze under rayon::join
src/core/sniff.rs             magic → VideoKind (ISOBMFF by leading box, EBML by DocType)
src/core/boxes.rs             bounds-checked ISOBMFF walker: box order, ftyp, mvhd, per-trak hdlr/mdhd/stsd fourcc, udta/meta/ilst (covr, ©nam, ©too), prefix detection
src/core/meta.rs              VideoMeta model + codec naming, rotation, ISO 8601, resolution/aspect labels
src/core/mp4.rs               mp4parse over the moov slice; tracks, timing, stsz bitrates, stts/ctts/elst keyframes
src/core/mkv.rs               purpose-built EBML reader: header, Info, Tracks, Cues, cluster block walk
src/core/audio.rs             Symphonia (isomp4 | mkv) → default audio track → interleaved f32 Pcm
src/core/waveform.rs          audiodown's reduction, verbatim
src/core/tests.rs             #[cfg(test)] tests over the fixtures, so plain `cargo test` works
```

`src/core` never imports napi.

## Public API

Emitted by napi-rs into `index.d.ts`; the only hand-written piece is
`scripts/dts-header.txt`, which carries `Rm` and `UTR` (the `@slipstream/types`
utilities), the four members `Mp4Props` / `MovProps` / `WebMProps` / `MkvProps`
(`Rm<VideoSpecsBase, …>` with `kind`, `format`, `ext` and the mime fields
pinned), their union `VideoUnion`, the record `VideoSpecsObj = UTR<VideoUnion,
'kind'>`, and `VideoSpecs = VideoUnion` for the return positions. The struct is emitted
under the `Base` name and every returning signature carries
`ts_return_type = "VideoSpecs"`, so consumers see the union and
`UTR<VideoUnion, 'kind'>` resolves instead of collapsing to `never` on a flat
interface; there are no hand-written per-kind aliases. `VideoService` has audiodown's shape (`sniffVideo`,
`parseVideo`, `keyframes`, `decodeAudio`, `audioPeaks`, `analyzeVideo`, each
with an `Async` twin, plus `open`); the standalone functions are the same
code; `VideoDown` is the per-buffer "open once, call many" handle.

`decodeAudio` and `audioPeaks` return `null` only for a container without an
audio track. A prefix buffer and an undecodable codec throw there, because a
partial envelope presented as whole is the one silent failure the consumer
cannot detect later. `analyzeVideo` reports the same cases as
`waveformStatus: 'truncated' | 'unsupported-codec'` so the specs still come
back from one call.

### `VideoSpecs`

One flat, nullable shape for every container. The asset-identity fields
(`type`, `kind`/`format`/`ext`, `mime`/`mimeType`/`contentType`, `byteSize`/
`fetchedBytes`/`size`, `source`) match audiodown and the image/doc specs. The
picture fields are taken from the first video track and shaped for Prisma
`VideoMetadata`: coded `width`/`height`, `displayWidth`/`displayHeight` after
the track matrix, `rotation` as 0/90/180/270, `aspectRatio` as a float plus a
`'16:9'` label, `orientation`, `resolution` as `'1080p'`/`'4K'` from the
shorter display edge, `frameRate`, `bitrate` in bps with `bitrateKbps`
beside it, `videoCodec` as the short name (`h264`, `hevc`, `vp9`, `av1`) with
the raw `videoCodecTag` (`avc1`, `hvc1`, `V_VP9`). The first audio track is
flattened the same way; every track is also present structurally in `video`
and `audioTracks`.

The layout fields are the header-first contract (below): `complete`,
`truncated`, `declaredSize`, `moovBeforeMdat`, `fragmented`, `boxes`.

## Core

### sniff

ISOBMFF is recognised by a known top-level box type at offset 4 (`ftyp`,
`moov`, `mdat`, `free`, `wide`, `moof`, …); the `ftyp` major brand `qt  `
makes it a `.mov`, anything else is `mp4`. Matroska is the EBML magic
`1A 45 DF A3`; the header's `DocType` string separates `webm` from `mkv`.

### boxes

`mp4parse` keeps `ftyp`, `mvhd` and the sample-entry fourcc private and
reads the whole stream, so a thin bounds-checked walker runs first. It
records the top-level box order (hence `moovBeforeMdat`), the brands, the
`mvhd` timescale, duration and times, and for each `trak` the `hdlr` type,
the `mdhd` language and the first `stsd` entry's fourcc, in `moov` order so
they align with `mp4parse`'s track numbering. It also decides `truncated`: a
box whose declared end runs past the buffer, or a declared file size larger
than the buffer. Size-1 (64-bit) and size-0 (to end of file) headers are
handled; a size smaller than its own header stops the walk instead of
looping.

### mp4

`mp4parse::read_mp4` is fed **only the `moov` slice** the walker located. A
cut `mdat` can then never fail the parse, and a buffer without a whole
`moov` is reported as `complete: false` rather than an error. HEVC is
`CodecType::Unknown` to `mp4parse`, so codec names fall back to the fourcc
(`hvc1`/`hev1` → `hevc`), and the `esds` descriptor's channel count and
sample rate override the legacy `stsd` fields, which FFmpeg writes as two
channels for a mono AAC track.

Frame rate is `samples × timescale / Σ(stts)`; `constantFrameRate` tolerates
one differently timed final run. Per-track bitrate is `Σ(stsz) × 8 /
duration`; whole-file bitrate uses `declaredSize`, so a header-only prefix
still reports the real number.

Keyframe times are presentation times: cumulative `stts` decode time at each
`stss` sample, plus the `ctts` composition offset, minus the edit list's
`media_time`, plus any empty edit, in seconds. Without `stss` every sample is
a sync sample (`source: 'all-samples'`). A `RunCursor` walks both run-length
tables once, in ascending sample order, so the index is linear in the table
size. The result lines up with `ffprobe -show_packets` `pts_time` on `K`
packets, which is what the fixtures assert.

Rotation follows the `rotate` tag convention: clockwise degrees to display
upright, from `atan2(b, a)` of the `tkhd` matrix snapped to a quarter turn, so
`[0 1; -1 0]` is 90 and `[-1 0; 0 -1]` is 180. `ffprobe` prints the same
matrix as `rotation: -90` (its sign is counter-clockwise); the fixtures carry
both so the convention is pinned. Display dimensions come from `tkhd` width
and height (falling back to the coded size) and are swapped for 90 and 270.

### mkv

A purpose-built EBML reader rather than the `matroska` crate, which exposed
`Info` and `Tracks` but neither `Cues` nor the clusters, followed `SeekHead`
positions a prefix cannot satisfy, and `assert!`ed on a lying seek head. The
reader handles both vint forms (ids keep their marker bits, sizes strip them,
the all-ones "unknown size" is honoured), walks the Segment's children in
order, and treats an unknown-size Segment or Cluster the way the spec says:
it ends at the first segment-level id. That covers MediaRecorder output and
`ffmpeg -live 1`.

From `Info`: `TimecodeScale`, `Duration`, `DateUTC`, title and writing app.
From `Tracks`: number, type, codec id, `DefaultDuration`, language, pixel and
display dimensions with `DisplayUnit` (pixels, or a display aspect ratio),
sampling rate, channels and bit depth. `complete` is set only when the whole
`Tracks` element lies inside the buffer; a Vorbis `CodecPrivate` alone is
about 3 KiB, so a 4 KiB prefix is not whole and says so.

Every cluster is walked, block headers only: track vint, `i16` timecode
relative to the cluster `Timecode`, flags. Payloads are skipped by size, so
the walk is linear in the block count and costs microseconds per thousand
blocks. It yields per-track block counts and bytes (frame count, bitrates),
first and last block times (the duration when `Info` has none: span plus one
frame), and the sync blocks of the video track (`SimpleBlock` flag `0x80`, or
a `BlockGroup` without `ReferenceBlock`). Keyframes come from `Cues` when
present (`source: 'cues'`), else from those sync blocks (`'blocks'`); both are
`CueTime` / block ticks × `TimecodeScale`, which is exactly the `pts_time`
`ffprobe` prints for `K` packets. An index read from the blocks of a truncated
buffer is marked `complete: false`.

### cover art

Some muxers embed a poster. ISOBMFF carries it as the iTunes-style `covr`
item under `moov/udta/meta/ilst` (a `data` atom typed 13 JPEG / 14 PNG /
27 BMP); ffmpeg writes it from an `attached_pic` stream and xAI's Grok Imagine
Video output ships one. Matroska carries it as an attachment (`AttachedFile`
with `FileName`, `FileMimeType`, `FileData`), `cover.jpg` by convention. The
walker records the byte range, the text items `©nam` and `©too` fill `title`
and `writingApp` for parity with Matroska, and the mime is sniffed from the
magic bytes with the declared type as fallback. `width` and `height` come
from the image header (JPEG SOF, PNG IHDR, WebP VP8/VP8L/VP8X, GIF, BMP), so
no image decoder enters the crate. `VideoSpecs.coverArt` describes it;
`coverArt()` and `analyzeVideo().coverArt` copy the bytes out. For mp4 the
atom is inside `moov`, so the header-first prefix still yields the poster.

### audio

A port of audiodown's decoder over Symphonia's `isomp4` and `mkv` demuxers:
probe with the sniffed extension and mime as hints, take the default audio
track, build the decoder (a missing codec, Opus today, is reported as
`Unsupported`, not an error), decode packets sequentially, copy each buffer
interleaved as `f32`, reject non-finite samples and midstream spec changes,
and cap the total at `maxDecodedSamples`. `gapless` and `strict` behave as in
audiodown.

### waveform

`audiodown/src/core/waveform.rs`, verbatim, with the `Pcm` import pointed at
`audio.rs`. Moving it to a shared crate both packages depend on is the right
end state; it is deferred so this package lands without touching audiodown
mid-build.

## The header-first contract

The extractor fetches a bounded prefix of a CDN object and asks for specs.
Three fields make that safe:

- `complete`: the movie header was whole inside the bytes. `false` means
  fetch more (or, with `truncated: false`, the file has no header at all).
- `truncated`: some declared box or segment runs past the buffer.
- `declaredSize`: the end of the last top-level box or the Segment; the file
  length for a whole file, a lower bound for a prefix cut inside a box.

`audio_peaks` checks `truncated` with the cheap walk before decoding and
refuses, because Symphonia stops quietly at EOF and the partial envelope
would look finished. `analyze` turns that refusal into
`waveformStatus: 'truncated'`.

## Async

As audiodown: one `promise()` helper snapshots the input on the JS thread,
runs the work under `spawn_blocking` on napi's tokio runtime, and maps the
result back on the JS thread where typed arrays are created. On
`wasm32-wasip1-threads` the work runs inline on the runtime worker, since
`spawn_blocking` there would relay thread creation through the JS thread and
deadlock under load; `.cargo/config.toml` sets `--cfg tokio_unstable` for that
target so napi compiles its wasm tokio path at all.

The WASI loader files (`videodown.wasi.cjs`, `videodown.wasi-browser.js`,
`videodown.wasi.d.cts`, `wasi-worker*.mjs`) are generated by a local
`yarn build --target wasm32-wasip1-threads` and tracked, as audiodown's are;
the `.wasm` binaries are not. The suite is run strictly on that binding with
`NAPI_RS_FORCE_WASI=error yarn test`, which needs `@oxc-node/core-wasm32-wasi`
because the flag is global and ava's TypeScript loader is a napi package too.
The three `emnapi` packages are pinned to exactly `2.0.0-alpha.5`, as in
audiodown: with `@emnapi/core` 2.0.0-beta.1 next to `@napi-rs/wasm-runtime`
1.2.5 the instantiate step dies at `napiModule.initWorker` on an undefined
shared object, before any binding code runs.

## Errors

Unrecognised magic, a `moov` that is present but unparseable, a malformed
Matroska structure, invalid options and a prefix handed to `decodeAudio` /
`audioPeaks` reject with a plain `Error`. Nothing panics across the binding;
the one third-party `assert!` is caught.

## Testing

`scripts/fixtures.py` generates every fixture with FFmpeg 4.4: `testsrc2`
320×180 at 30 fps for four seconds with a one-second GOP, a 0.55 full-scale
sine in opposite phase on two channels at 48 kHz. Variants: H.264 + AAC with
`moov` first (`+faststart`) and last, HEVC (`hvc1`), AV1, VP9 + Opus and
VP9 + Vorbis WebM, a live-muxed VP9 + Opus WebM (`-live 1`: unknown-size
Segment and Clusters, no `Cues`, no `Duration`), an MKV, a `.mov` (`qt  `
brand), a silent clip, a clip with two audio tracks (the second mono at
44.1 kHz), and rotation at 90 and 270.
FFmpeg 4.x only writes a `tkhd` matrix from display-matrix side data, never
from a `rotate` tag, so the script stamps the matrices movenc would write into
the video `trak` by hand and lets `ffprobe` confirm them.

`fixture-info.json` holds the `ffprobe` truths: format, size, duration,
bitrate, creation time, per-stream codec, tag, dimensions, frame rate, frame
count, rotation, channels and sample rate, and the `pts_time` of every `K`
packet and the video packet count. The ava suite compares specs and keyframes against it field by
field; the Rust suite pins the same values plus the walker, the run cursor,
the geometry and date helpers, and decodes the AAC and Vorbis tracks against
FFmpeg's `f32le` output after aligning on the lag that minimises error (the
two sides disagree on priming-sample trimming), asserting a maximum error
under 5e-3.

## Deferred

- **Shared waveform crate** across audiodown and videodown.
- **Opus** when a Symphonia decoder exists.
- **`@d0paminedriven/videodown-frames`**: the same crate with a `frames`
  Cargo feature depending on `ffmpeg-the-third`, exporting `posterAsync`,
  `framesAsync` and `remuxFaststartAsync` behind `#[cfg(feature = "frames")]`,
  returning raw RGBA so the consumer's `sharp` encodes. Built like
  `pdfdown-ocr`: `napi build --platform --release --no-const-enum --features
  frames --package-json-path frames/package.json --output-dir frames`, four
  targets (linux x64/arm64, darwin x64/arm64), `os: ["darwin", "linux"]`, the
  host providing the libav runtime libraries.
