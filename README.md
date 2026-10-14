# `@d0paminedriven/videodown`

![CI](https://github.com/DopamineDriven/videodown/workflows/CI/badge.svg)

Rust-powered MP4 / MOV / WebM / MKV inspection for Node.js via [napi-rs](https://napi.rs). Reads container metadata from the headers (tracks, codecs, coded and display dimensions, rotation, frame rate, duration, bitrates, brands, creation time, progressive layout), builds the keyframe index from the container's own sync-sample table (`stss`, Matroska `Cues`, or a walk of the cluster block headers when a WebM has no `Cues`), and decodes the audio track with [Symphonia 0.6](https://github.com/pdeljanov/Symphonia) into the same exhaustive waveform envelope `@d0paminedriven/audiodown` produces, on a [rayon](https://github.com/rayon-rs/rayon) pool. It never decodes video, so the crate is pure Rust and ships for every target including `wasm32-wasip1-threads`.

The output is shaped to drop straight into an `ImageSpecs` / `AudioSpecs`-style pipeline (`type`, `format`, `contentType`, `byteSize`, `fetchedBytes`, `source`, …) and a Prisma `VideoMetadata` row (`format`, `width`, `height`, `aspectRatio`, `duration` in ms, `frameRate`, `bitrate` in kbps, `codec`, `hasAudio`, `resolution`, `rotation`, `keyframeTimes`), with `Array.from(waveform.envelope)` feeding the same `waveformPeaks` column audiodown fills.

## Install

```bash
npm install @d0paminedriven/videodown
# or
yarn add @d0paminedriven/videodown
# or
pnpm add @d0paminedriven/videodown
```

No FFmpeg, system codec or C toolchain is needed at runtime. Prebuilt binaries ship for Linux (gnu/musl, x64/arm64/armv7), macOS (x64/arm64), Windows (x64/x86/arm64), FreeBSD, Android and `wasm32-wasip1-threads`.

## Quick start

```typescript
import { readFile } from 'fs/promises'
import { VideoService } from '@d0paminedriven/videodown'

const video = new VideoService()
const buffer = await readFile('clip.mp4')

// Headers only; tens of microseconds. No decoding.
const specs = video.parseVideo(buffer, 'https://cdn.example/clip.mp4')
specs.kind // 'mp4' | 'mov' | 'webm' | 'mkv'
specs.width // 1920   (coded)
specs.displayHeight // 1920   (after the rotation matrix)
specs.rotation // 90
specs.videoCodec // 'h264' | 'hevc' | 'vp9' | 'av1' | ...
specs.frameRate // 29.97
specs.durationMs // 8008
specs.moovBeforeMdat // true: progressive layout, seekable from the first byte
specs.complete // true: the movie header was inside the bytes supplied

// The container's own sync-sample index, in ms, ready for `keyframeTimes Int[]`.
const index = video.keyframes(buffer)
Array.from(index.timesMs) // [0, 2002, 4004, 6006]

// Specs + keyframes + audio-track envelope from one snapshot, off the event loop.
const { specs: meta, keyframes, waveform, waveformStatus } = await video.analyzeVideoAsync(buffer, { peakCount: 1024 })
waveformStatus // 'ok' | 'no-audio' | 'unsupported-codec' | 'truncated'
waveform?.envelope // Float32Array(1024): max |peak| across channels per bucket
```

## API

### `VideoService` class

Stateless, construct once. Every method is also exported as a standalone function with the same signature (minus `this`). Constructor defaults are merged under per-call options; a call that names either resolution selector (`peakCount` or `samplesPerPeak`) replaces both defaults.

```typescript
export declare class VideoService {
  constructor(defaults?: WaveformOptions)
  get defaults(): WaveformOptions

  sniffVideo(data: Uint8Array): VideoKind | null

  parseVideo(data: Uint8Array, source?: string): VideoSpecs
  parseVideoAsync(data: Uint8Array, source?: string): Promise<VideoSpecs>

  keyframes(data: Uint8Array): KeyframeIndex
  keyframesAsync(data: Uint8Array): Promise<KeyframeIndex>

  decodeAudio(data: Uint8Array, options?: DecodeOptions): DecodedAudio | null
  decodeAudioAsync(data: Uint8Array, options?: DecodeOptions): Promise<DecodedAudio | null>

  audioPeaks(data: Uint8Array, options?: WaveformOptions): WaveformPeaks | null
  audioPeaksAsync(data: Uint8Array, options?: WaveformOptions): Promise<WaveformPeaks | null>

  analyzeVideo(data: Uint8Array, options?: WaveformOptions, source?: string): VideoAnalysis
  analyzeVideoAsync(data: Uint8Array, options?: WaveformOptions, source?: string): Promise<VideoAnalysis>

  /** Snapshot a buffer once and reuse it; inherits this service's defaults. */
  open(data: Uint8Array, source?: string): VideoDown
}
```

Inputs are `Uint8Array`, so `Buffer` and sliced views work and `byteOffset` is honoured. Sync methods borrow the bytes with no copy. Async methods copy once, synchronously, before anything is queued, so mutating the caller's buffer afterwards cannot race the worker; the CPU work then runs on the tokio blocking pool that napi-rs manages (`tokio_rt`), and the result is converted to JS values when the promise resolves. The event loop and Node's libuv pool are never blocked.

`decodeAudio` and `audioPeaks` return `null` only when the container has **no audio track**. A buffer that is a prefix of the file, or an audio codec no decoder is compiled in for, throws; `analyzeVideo` reports the same situations through `waveformStatus` instead, so one call always yields the specs.

### `VideoDown` class

Open once, call many. The buffer is copied exactly once into shared memory; sync methods borrow it and async methods share it with the worker through `Arc`, so there is no per-call re-copy.

```typescript
export declare class VideoDown {
  constructor(data: Uint8Array, source?: string)
  get kind(): VideoKind
  get byteSize(): number
  get source(): string | null
  metadata(): VideoSpecs
  metadataAsync(): Promise<VideoSpecs>
  keyframes(): KeyframeIndex
  keyframesAsync(): Promise<KeyframeIndex>
  decodeAudio(options?: DecodeOptions): DecodedAudio | null
  decodeAudioAsync(options?: DecodeOptions): Promise<DecodedAudio | null>
  audioPeaks(options?: WaveformOptions): WaveformPeaks | null
  audioPeaksAsync(options?: WaveformOptions): Promise<WaveformPeaks | null>
  analyze(options?: WaveformOptions): VideoAnalysis
  analyzeAsync(options?: WaveformOptions): Promise<VideoAnalysis>
}
```

## Types

### `VideoSpecs`

The binding emits one flat, nullable interface, `VideoSpecsBase`. The typings narrow it by `kind` into four members, `Mp4Props`, `MovProps`, `WebMProps` and `MkvProps` (each `Rm<VideoSpecsBase, …>` with `kind`, `format`, `ext` and the three mime fields pinned to literals), their union `VideoUnion`, and the by-kind record `VideoSpecsObj = UTR<VideoUnion, 'kind'>`. `VideoSpecs`, what every parse call is typed to return, is `VideoUnion`. `Rm` and `UTR` ship with the `@slipstream/types` semantics.

| Field                                                                                         | Value                                                                                 | Parity                                       |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- | -------------------------------------------- |
| `type`                                                                                        | `'VIDEO'`                                                                             | `ImageSpecs.type`, `AudioSpecs.type`         |
| `kind`, `format`, `ext`                                                                       | `'mp4' \| 'mov' \| 'webm' \| 'mkv'`                                                   | `VideoMetadata.format`, asset `ext`          |
| `mime`, `mimeType`, `contentType`                                                             | `'video/mp4' \| 'video/quicktime' \| 'video/webm' \| 'video/x-matroska'`              | `DocSpecs.mimeType`, `Expanded*.contentType` |
| `container`                                                                                   | `'isobmff' \| 'matroska'`                                                             |                                              |
| `byteSize`, `fetchedBytes`, `size`                                                            | bytes parsed                                                                          | `Expanded*.byteSize`, `fetchedBytes`         |
| `source`                                                                                      | passed through (e.g. the CDN URL)                                                     | `ImageSpecs.source`                          |
| `complete`                                                                                    | the movie header was whole inside the buffer                                          | header-first fetch contract                  |
| `truncated`                                                                                   | a declared box / segment runs past the buffer                                         |                                              |
| `declaredSize`                                                                                | end of the last top-level box or Segment (file length, or a lower bound for a prefix) |                                              |
| `fragmented`, `boxes`, `moovBeforeMdat`                                                       | fMP4 flag, top-level box order, progressive layout (`null` for Matroska)              | remux decision                               |
| `brands`                                                                                      | `{ major, minorVersion, compatible }` from `ftyp`                                     |                                              |
| `creationTime`, `modificationTime`                                                            | ISO 8601 UTC from `mvhd` / `DateUTC`                                                  |                                              |
| `title`, `writingApp`, `timescale`                                                            | Matroska title and muxer; `mvhd` ticks per second                                     |                                              |
| `durationSec`, `durationMs`                                                                   | movie duration                                                                        | `VideoMetadata.duration` (ms)                |
| `bitrate`, `bitrateKbps`                                                                      | whole-file average bps / floored kbps                                                 | `VideoMetadata.bitrate` (kbps)               |
| `trackCount`, `videoTrackCount`, `audioTrackCount`                                            |                                                                                       |                                              |
| `width`, `height`                                                                             | coded dimensions of the first video track                                             | `VideoMetadata.width`, `.height`             |
| `displayWidth`, `displayHeight`, `rotation`                                                   | presentation size after the track matrix; `0 \| 90 \| 180 \| 270`                     | `VideoMetadata.rotation`                     |
| `aspectRatio`, `aspectRatioLabel`, `orientation`                                              | `displayWidth / displayHeight`, `'16:9'`, `'landscape' \| 'portrait' \| 'square'`     | `VideoMetadata.aspectRatio`, `.orientation`  |
| `resolution`                                                                                  | `'1080p'`, `'720p'`, `'4K'` from the shorter display edge                             | `VideoMetadata.resolution`                   |
| `frameRate`, `constantFrameRate`, `frameCount`                                                | from `stts` (MP4) or `DefaultDuration` (Matroska)                                     | `VideoMetadata.frameRate`                    |
| `videoCodec`, `videoCodecTag`, `videoBitrate`                                                 | `'h264'` + `'avc1'`, `'hevc'` + `'hvc1'`, `'vp9'` + `'V_VP9'`, …                      | `VideoMetadata.codec`                        |
| `hasAudio`, `audioCodec`, `audioCodecTag`, `audioChannels`, `audioSampleRate`, `audioBitrate` | first audio track                                                                     | `VideoMetadata.hasAudio`                     |
| `video`, `audioTracks`                                                                        | the structured `VideoTrack` and every `AudioTrack`                                    |                                              |
| `keyframeCount`, `keyframeSource` | `'stss' \| 'all-samples' \| 'cues' \| 'blocks' \| 'none'` | | 'cues' \| 'none'`                                         |                                              |

### `KeyframeIndex`

```typescript
interface KeyframeIndex {
  source: 'stss' | 'all-samples' | 'cues' | 'blocks' | 'none'
  complete: boolean // the index covers the whole file; false when read from the blocks of a prefix
  count: number
  timesMs: Uint32Array // presentation times, whole ms, ascending → VideoMetadata.keyframeTimes
  timesSec: Float64Array
  sampleCount: number | null // video samples in the indexed track
  durationSec: number | null
  meanIntervalSec: number | null // durationSec / count: the average GOP
}
```

Times are presentation times: decode time from `stts`, plus the `ctts` composition offset, minus the edit list's media time, so they line up with what `ffprobe -show_packets` reports as `pts_time` for `K` packets. When a file has no `stss` every sample is a sync sample per ISOBMFF and `source` is `'all-samples'`. Matroska and WebM read `Cues` (`'cues'`); a file without them, which is what MediaRecorder and `ffmpeg -live 1` write, is indexed from the keyframe flag of every `SimpleBlock` and from `BlockGroup`s without a `ReferenceBlock` (`'blocks'`), the same walk that supplies the frame count, per-track bitrates and the duration those files also lack.

### `VideoAnalysis`, `WaveformPeaks`, `DecodedAudio`, options

`VideoAnalysis` is `{ specs, keyframes, waveform, waveformStatus, waveformReason }`. `waveform` is `null` whenever `waveformStatus` is not `'ok'`; `waveformReason` carries the decoder's message for `'unsupported-codec'`.

`WaveformPeaks`, `DecodedAudio`, `WaveformOptions` and `DecodeOptions` are audiodown's shapes, field for field: `envelope` is `max(channel.peak[i])` across channels, `boundaries` has `peakCount + 1` frame offsets, every sample belongs to exactly one bucket, and serial and parallel reduction are bit-identical. See [audiodown](https://github.com/DopamineDriven/audiodown) for the reduction's guarantees.

## Usage

### Persist a generated clip (metadata, keyframes and waveform in one pass)

```typescript
const { specs, keyframes, waveform, waveformStatus } = await video.analyzeVideoAsync(bytes, { peakCount: 1024 }, cdnUrl)

await prisma.videoMetadata.create({
  data: {
    attachmentId,
    format: specs.format,
    width: specs.width ?? 0,
    height: specs.height ?? 0,
    aspectRatio: specs.aspectRatio,
    duration: specs.durationMs ?? 0,
    frameRate: specs.frameRate,
    bitrate: specs.bitrateKbps,
    codec: specs.videoCodec,
    hasAudio: specs.hasAudio,
    resolution: specs.resolution,
    rotation: specs.rotation,
    keyframeTimes: Array.from(keyframes.timesMs),
  },
})
if (waveformStatus === 'ok' && waveform) {
  const peaks = Array.from(waveform.envelope, (v) => Math.round(Math.min(1, v) * 100))
  // → the same 1024 × 0..100 column audiodown fills for generated tracks
}
```

### Probe a remote file header-first

Fetch a bounded prefix, parse it, and let `complete` decide whether the rest is needed. A progressive file (`moov` before `mdat`) yields every spec and the full keyframe index from its first few hundred kilobytes; a file muxed with `moov` at the end reports `complete: false` and the parse is repeated on the whole object.

```typescript
const head = await fetch(url, { headers: { range: 'bytes=0-262143' } }).then((r) => r.arrayBuffer())
let specs = video.parseVideo(new Uint8Array(head), url)
if (!specs.complete) {
  const whole = await fetch(url).then((r) => r.arrayBuffer())
  specs = video.parseVideo(new Uint8Array(whole), url)
}
specs.moovBeforeMdat // false here: worth a faststart remux before serving
```

`truncated` is independent of `complete`: a header-only prefix of a progressive file is `complete: true, truncated: true`, which is exactly the state in which `audioPeaks` would decode a partial track and present it as whole. It throws instead, and `analyzeVideo` reports `waveformStatus: 'truncated'`.

### Discriminate on `kind`

```typescript
import type { VideoSpecs, VideoSpecsObj } from '@d0paminedriven/videodown'

// VideoSpecsObj = UTR<VideoUnion, 'kind'>: { mp4: Mp4Props; mov: MovProps; webm: WebMProps; mkv: MkvProps }
const isMatroska = (s: VideoSpecs): s is VideoSpecsObj['webm'] | VideoSpecsObj['mkv'] => s.container === 'matroska'

const specs = video.parseVideo(bytes)
if (isMatroska(specs)) {
  specs.mime // 'video/webm' | 'video/x-matroska'
  specs.writingApp // 'Lavf58.76.100'
} else {
  specs.brands?.major // 'isom', 'mp42', 'qt  ', ...
}
```

## Errors

Unrecognised magic, a malformed movie header and invalid options reject synchronously (or reject the returned promise) with a plain `Error`; nothing panics across the binding. Option validation is strict: `peakCount` and `samplesPerPeak` are exclusive, must be positive integers (`1.5`, `NaN` and `Infinity` are rejected rather than truncated), and `maxDecodedSamples` caps the interleaved sample count held in memory (default 64,000,000).

## Limitations in 0.1

- **Opus is not decoded.** Symphonia 0.6 ships no Opus decoder, so a VP9 + Opus WebM parses fully but `audioPeaks` throws and `analyzeVideo` reports `'unsupported-codec'`. Vorbis, AAC, FLAC, ALAC and PCM tracks decode.
- **No frames.** Posters, frame strips and a faststart remux need a decoder; they belong to the planned `@d0paminedriven/videodown-frames` superset (the same crate behind a Cargo feature linking libav, built for the four server targets like `@d0paminedriven/pdfdown-ocr`), so the base stays wasm-capable.
- **Laced video blocks count once.** `frameCount` for Matroska is the block count; lacing (rare for video, common for audio) packs several frames into one block.

## Performance

`yarn bench` on a 4 s, 320×180, 30 fps H.264 + AAC fixture (WSL2, x64, release build, medians):

| Task                                           | Latency |
| ---------------------------------------------- | ------: |
| `parseVideo` mp4 (whole file or header prefix) |   42 µs |
| `parseVideo` webm                              |   23 µs |
| `keyframes` mp4                                |   29 µs |
| `VideoDown` metadata + keyframes               |   73 µs |
| `decodeAudio` 4 s AAC                          |  3.8 ms |
| `audioPeaks` 4 s AAC, 1024 peaks               |  4.4 ms |
| `audioPeaks` 4 s Vorbis, 1024 peaks            |  6.6 ms |
| `analyzeVideo` (specs ‖ decode + reduce)       |  5.0 ms |

Header parsing touches a few kilobytes and is independent of the audio decode, so `analyzeVideo` runs the two under `rayon::join` and costs what the decode costs.

## Development

```bash
yarn install
yarn fixtures      # regenerate __test__/fixtures with FFmpeg + ffprobe (dev-only dependency)
yarn build         # napi build --platform --release --no-const-enum
yarn test          # ava against the built binding, truths from fixture-info.json
yarn test:rs       # cargo test (pure-Rust core over the same fixtures)
yarn lint:rs       # cargo fmt --check + clippy -D warnings
yarn bench

# WASI build and a strict run of the suite on the wasm binding
yarn build --target wasm32-wasip1-threads
NAPI_RS_FORCE_WASI=error yarn test
```

`NAPI_RS_FORCE_WASI=error` matters: with `=1` the loader silently falls back to a native `.node` sitting next to the wasm files, so the run would not prove anything. The generated WASI loader files (`videodown.wasi.cjs`, `videodown.wasi-browser.js`, `videodown.wasi.d.cts`, `wasi-worker*.mjs`) are tracked, as in audiodown; the `.wasm` binaries are not. The wasm target is built with `--cfg tokio_unstable` (see `.cargo/config.toml`); without it napi compiles the threadless async path and every `*Async` call rejects, and on wasm the async work runs inline on the tokio worker instead of through `spawn_blocking` (see the ADR).

Fixtures are generated by `scripts/fixtures.py`: H.264 + AAC with `moov` first and last, HEVC, AV1, VP9 + Opus and VP9 + Vorbis WebM, an MKV, a `.mov`, a silent clip, a two-audio-track clip, two rotated clips whose `tkhd` matrices are stamped by hand (FFmpeg 4.x only writes a matrix from side data), and a live-muxed WebM (`-live 1`: unknown-size Segment and Clusters, no `Cues`, no `Duration`). `fixture-info.json` records the `ffprobe` truths the ava suite compares against, including the `pts_time` of every `K` packet.

## License

MIT
