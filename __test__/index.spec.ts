import test from 'ava'
import type { ExecutionContext } from 'ava'
import { readFileSync } from 'node:fs'

import {
  VideoDown,
  VideoService,
  analyzeVideo,
  analyzeVideoAsync,
  audioPeaks,
  audioPeaksAsync,
  coverArt,
  coverArtAsync,
  decodeAudio,
  decodeAudioAsync,
  keyframes,
  keyframesAsync,
  parseVideo,
  parseVideoAsync,
  sniffVideo,
} from '../index'
import type { VideoSpecs, VideoSpecsObj, WaveformPeaks } from '../index'

interface FixtureVideo {
  codec: string
  codecTag: string | null
  width: number
  height: number
  rFrameRate: string
  avgFrameRate: string
  nbFrames: number | null
  duration: number | null
  bitRate: number | null
  rotate: string | null
  sideData: Array<{ rotation?: number }> | null
}

interface FixtureAudio {
  codec: string
  channels: number
  sampleRate: number
  duration: number | null
  bitRate: number | null
}

interface FixtureInfo {
  formatName: string
  size: number
  duration: number | null
  bitRate: number | null
  creationTime: string | null
  streams: number
  video: FixtureVideo | null
  audio: FixtureAudio[]
  keyframes: number[]
  videoPackets: number
  attachedPics: number
  title: string | null
}

const fixture = (name: string) => readFileSync(new URL(`./fixtures/${name}`, import.meta.url))
const info: Record<string, FixtureInfo> = JSON.parse(
  readFileSync(new URL('./fixtures/fixture-info.json', import.meta.url), 'utf8'),
)
const close = (t: ExecutionContext, a: number | null, b: number | null, tol: number, label = '') =>
  t.true(a !== null && b !== null && Math.abs(a - b) <= tol, `${label} ${a} vs ${b}`)
const ratio = (text: string) => {
  const [n, d] = text.split('/').map(Number)
  return n / d
}

const MP4_LIKE = [
  'h264-aac-faststart.mp4',
  'h264-aac-mdat-first.mp4',
  'hevc-aac.mp4',
  'av1-aac.mp4',
  'h264-aac.mov',
  'rotated-90.mp4',
  'rotated-270.mp4',
  'silent.mp4',
  'two-audio.mp4',
  'h264-aac-cover.mp4',
]
const MATROSKA = ['h264-aac.mkv', 'vp9-opus.webm', 'vp9-vorbis.webm', 'h264-aac-cover.mkv']

// ── sniffing ────────────────────────────────────────────────────

test('sniffVideo classifies by magic', (t) => {
  t.is(sniffVideo(fixture('h264-aac-faststart.mp4')), 'mp4')
  t.is(sniffVideo(fixture('h264-aac.mov')), 'mov')
  t.is(sniffVideo(fixture('h264-aac.mkv')), 'mkv')
  t.is(sniffVideo(fixture('vp9-opus.webm')), 'webm')
  t.is(sniffVideo(Uint8Array.of(0x1a, 0x45, 0xdf, 0xa3)), 'mkv')
  t.is(sniffVideo(Uint8Array.of(0xff, 0xfb, 0x90, 0x00)), null)
  t.is(sniffVideo(new Uint8Array(0)), null)
})

// ── VideoSpecs shape ────────────────────────────────────────────

function assertShape(t: ExecutionContext, specs: VideoSpecs, length: number) {
  t.is(specs.type, 'VIDEO')
  t.is(specs.format, specs.kind)
  t.is(specs.ext, specs.kind)
  t.is(specs.mimeType, specs.mime)
  t.is(specs.contentType, specs.mime)
  t.is(specs.byteSize, length)
  t.is(specs.fetchedBytes, length)
  t.is(specs.size, length)
  t.is(specs.hasAudio, specs.audioTrackCount > 0)
  t.is(specs.audioTracks.length, specs.audioTrackCount)
}

test('ISOBMFF specs agree with ffprobe', (t) => {
  for (const name of MP4_LIKE) {
    const buf = fixture(name)
    const truth = info[name]
    const specs = parseVideo(buf, `cdn://${name}`)
    assertShape(t, specs, buf.length)
    t.is(specs.kind, name.endsWith('.mov') ? 'mov' : 'mp4', name)
    t.is(specs.container, 'isobmff', name)
    t.is(specs.source, `cdn://${name}`)
    t.true(specs.complete && !specs.truncated && !specs.fragmented, name)
    t.is(specs.declaredSize, truth.size, name)
    t.is(specs.trackCount, truth.streams - truth.attachedPics, name)
    t.is(specs.title, truth.title, name)
    close(t, specs.durationSec, truth.duration, 1e-3, name)
    t.is(specs.durationMs, Math.round((specs.durationSec ?? 0) * 1000), name)
    t.is(specs.bitrateKbps, Math.floor((specs.bitrate ?? 0) / 1000), name)
    close(t, specs.bitrate, truth.bitRate, (truth.bitRate ?? 0) * 0.01, `${name} bitrate`)

    const video = truth.video
    t.truthy(video)
    if (!video) continue
    t.is(specs.videoCodec, video.codec, name)
    t.is(specs.videoCodecTag, video.codecTag, name)
    t.is(specs.width, video.width, name)
    t.is(specs.height, video.height, name)
    t.is(specs.frameCount, video.nbFrames, name)
    t.is(specs.frameCount, truth.videoPackets, name)
    close(t, specs.frameRate, ratio(video.rFrameRate), 1e-6, `${name} fps`)
    t.is(specs.constantFrameRate, true, name)
    const rotation = Number(video.rotate ?? 0)
    t.true(specs.rotation === rotation, `${name} rotation ${specs.rotation}`)
    const swapped = rotation === 90 || rotation === 270
    t.is(specs.displayWidth, swapped ? video.height : video.width, name)
    t.is(specs.displayHeight, swapped ? video.width : video.height, name)
    t.is(specs.orientation, swapped ? 'portrait' : 'landscape', name)
    t.is(specs.aspectRatioLabel, swapped ? '9:16' : '16:9', name)
    t.is(specs.resolution, '180p', name)
    close(t, specs.aspectRatio, (specs.displayWidth ?? 0) / (specs.displayHeight ?? 1), 1e-12, name)
    if (specs.video) {
      t.true(specs.video.codec === specs.videoCodec)
      t.true(specs.video.rotation === specs.rotation)
      t.is(specs.video.frameRate, specs.frameRate)
    } else t.fail(`${name}: structured video track missing`)

    t.is(specs.audioTrackCount, truth.audio.length, name)
    truth.audio.forEach((track, i) => {
      const ours = specs.audioTracks[i]
      t.is(ours.codec, track.codec, `${name} audio ${i}`)
      t.is(ours.channels, track.channels, `${name} audio ${i}`)
      t.is(ours.sampleRate, track.sampleRate, `${name} audio ${i}`)
      close(t, ours.bitrate, track.bitRate, (track.bitRate ?? 0) * 0.02, `${name} audio ${i} bitrate`)
    })
    if (truth.audio.length > 0) {
      t.is(specs.audioCodec, truth.audio[0].codec)
      t.is(specs.audioCodecTag, 'mp4a')
      t.is(specs.audioChannels, truth.audio[0].channels)
      t.is(specs.audioSampleRate, truth.audio[0].sampleRate)
    } else {
      t.is(specs.audioCodec, null)
      t.is(specs.audioChannels, null)
    }

    t.is(specs.keyframeSource, 'stss', name)
    t.is(specs.keyframeCount, truth.keyframes.length, name)
    const index = keyframes(buf)
    t.is(index.count, truth.keyframes.length, name)
    t.deepEqual(
      Array.from(index.timesSec, (v) => Math.round(v * 1e6) / 1e6),
      truth.keyframes,
      `${name} keyframes`,
    )
    t.deepEqual(
      Array.from(index.timesMs),
      truth.keyframes.map((v) => Math.round(v * 1000)),
    )
    close(t, index.meanIntervalSec, 1, 1e-9, `${name} gop`)
    t.is(index.complete, true, name)
  }
})

const isMov = (specs: VideoSpecs): specs is VideoSpecsObj['mov'] => specs.kind === 'mov'

test('mov carries the QuickTime brand and mime', (t) => {
  const specs = parseVideo(fixture('h264-aac.mov'))
  t.true(isMov(specs))
  if (!isMov(specs)) return
  // Narrowed: `mime` is the literal, not the four-way union.
  const mime: 'video/quicktime' = specs.mime
  const contentType: 'video/quicktime' = specs.contentType
  t.is(mime, 'video/quicktime')
  t.is(contentType, 'video/quicktime')
  t.is(specs.kind, 'mov')
  t.is(specs.brands?.major, 'qt  ')
  const mp4 = parseVideo(fixture('h264-aac-faststart.mp4'))
  t.is(mp4.mime, 'video/mp4')
  t.is(mp4.brands?.major, 'isom')
  t.true(mp4.brands?.compatible.includes('avc1'))
  t.is(mp4.creationTime, '2026-10-14T12:00:00Z')
  t.true(info['h264-aac-faststart.mp4'].creationTime?.startsWith('2026-10-14T12:00:00'))
  t.is(mp4.modificationTime, '2026-10-14T12:00:00Z')
  t.is(mp4.timescale, 1000)
  t.is(mp4.title, null)
  t.regex(mp4.writingApp ?? '', /^Lavf/)
  t.is(mp4.coverArt, null)
})

test('moov placement is reported and prefixes are flagged', (t) => {
  const fast = fixture('h264-aac-faststart.mp4')
  const slow = fixture('h264-aac-mdat-first.mp4')
  t.is(parseVideo(fast).moovBeforeMdat, true)
  t.is(parseVideo(slow).moovBeforeMdat, false)
  t.deepEqual(parseVideo(fast).boxes.slice(0, 2), ['ftyp', 'moov'])

  // A prefix of a progressive file cut before mdat: whole header, no payload.
  const headerOnly = fast.subarray(0, fast.indexOf('mdat') + 4)
  const probe = parseVideo(headerOnly)
  t.is(probe.complete, true)
  t.is(probe.truncated, true)
  t.is(probe.moovBeforeMdat, true)
  t.is(probe.declaredSize, fast.length)
  t.is(probe.keyframeCount, 4)
  t.is(probe.width, 320)
  t.is(probe.byteSize, headerOnly.length)

  // A prefix of an mdat-first file: the header is still to come.
  const payloadOnly = slow.subarray(0, 65_536)
  const blind = parseVideo(payloadOnly)
  t.is(blind.complete, false)
  t.is(blind.truncated, true)
  t.is(blind.moovBeforeMdat, null)
  t.is(blind.width, null)
  t.is(blind.videoCodec, null)
  t.is(blind.keyframeSource, 'none')
  t.is(blind.brands?.major, 'isom')
  t.true((blind.declaredSize ?? 0) > payloadOnly.length && (blind.declaredSize ?? 0) < slow.length)
})

test('Matroska and WebM specs agree with ffprobe', (t) => {
  for (const name of MATROSKA) {
    const buf = fixture(name)
    const truth = info[name]
    const specs = parseVideo(buf)
    assertShape(t, specs, buf.length)
    t.is(specs.kind, name.endsWith('.webm') ? 'webm' : 'mkv', name)
    t.is(specs.container, 'matroska', name)
    t.is(specs.mime, name.endsWith('.webm') ? 'video/webm' : 'video/x-matroska')
    t.true(specs.complete && !specs.truncated, name)
    t.is(specs.declaredSize, truth.size, name)
    t.is(specs.moovBeforeMdat, null, name)
    t.deepEqual(specs.boxes, [], name)
    t.is(specs.timescale, null, name)
    close(t, specs.durationSec, truth.duration, 1e-9, name)
    const video = truth.video
    if (!video) {
      t.fail(name)
      continue
    }
    t.is(specs.videoCodec, video.codec, name)
    t.true(specs.videoCodecTag?.startsWith('V_'), name)
    t.is(specs.width, video.width, name)
    t.is(specs.height, video.height, name)
    close(t, specs.frameRate, ratio(video.rFrameRate), 1e-3, `${name} fps`)
    t.is(specs.rotation, 0, name)
    t.is(specs.frameCount, truth.videoPackets, name)
    t.true((specs.videoBitrate ?? 0) > 50_000, name)
    t.is(specs.audioTrackCount, truth.audio.length, name)
    t.is(specs.audioCodec, truth.audio[0].codec, name)
    t.is(specs.audioChannels, truth.audio[0].channels, name)
    t.is(specs.audioSampleRate, truth.audio[0].sampleRate, name)
    t.true(specs.audioCodecTag?.startsWith('A_'), name)
    t.true(specs.writingApp?.includes('Lavf'), name)
    // Cues, in the container's millisecond ticks: exactly ffprobe's K-packet pts.
    t.is(specs.keyframeSource, 'cues', name)
    t.is(specs.keyframeCount, truth.keyframes.length, name)
    const index = keyframes(buf)
    t.is(index.source, 'cues')
    t.is(index.complete, true)
    t.deepEqual(
      Array.from(index.timesSec, (v) => Math.round(v * 1e6) / 1e6),
      truth.keyframes,
      `${name} keyframes`,
    )
    t.deepEqual(
      Array.from(index.timesMs),
      truth.keyframes.map((v) => Math.round(v * 1000)),
    )
    t.is(index.sampleCount, truth.videoPackets)
  }
})

test('live WebM without Cues or Duration is indexed from its blocks', (t) => {
  const name = 'vp9-opus-live.webm'
  const buf = fixture(name)
  const truth = info[name]
  t.is(truth.duration, null, 'ffprobe sees no duration either')
  const specs = parseVideo(buf)
  t.is(specs.kind, 'webm')
  t.true(specs.complete && !specs.truncated)
  t.is(specs.declaredSize, null)
  close(t, specs.durationSec, 4, 0.15)
  t.is(specs.frameCount, truth.videoPackets)
  close(t, specs.frameRate, 30, 0.5)
  t.is(specs.videoCodec, 'vp9')
  t.is(specs.audioCodec, 'opus')
  t.is(specs.keyframeSource, 'blocks')
  const index = keyframes(buf)
  t.is(index.complete, true)
  t.deepEqual(
    Array.from(index.timesSec, (v) => Math.round(v * 1e6) / 1e6),
    truth.keyframes,
  )
  const analysis = analyzeVideo(buf)
  t.is(analysis.waveformStatus, 'unsupported-codec')
  t.is(analysis.keyframes.count, truth.keyframes.length)
})

test('a WebM prefix keeps the header and marks the block index partial', (t) => {
  const full = fixture('vp9-vorbis.webm')
  // 4 KiB cuts the Tracks element itself (the Vorbis setup header is ~3 KiB).
  const short = parseVideo(full.subarray(0, 4096))
  t.is(short.complete, false)
  t.is(short.truncated, true)
  const prefix = full.subarray(0, 16384)
  const specs = parseVideo(prefix)
  t.is(specs.complete, true)
  t.is(specs.truncated, true)
  t.is(specs.declaredSize, info['vp9-vorbis.webm'].size)
  t.is(specs.videoCodec, 'vp9')
  t.is(specs.audioCodec, 'vorbis')
  const index = keyframes(prefix)
  t.is(index.source, 'blocks')
  t.is(index.complete, false)
  t.is(index.count, 1)
  t.throws(() => audioPeaks(prefix), { message: /prefix/ })
  t.is(analyzeVideo(prefix).waveformStatus, 'truncated')
})

test('cover art comes out of the mp4 covr atom and the mkv attachment', async (t) => {
  const jpeg = fixture('cover.jpg')
  for (const name of ['h264-aac-cover.mp4', 'h264-aac-cover.mkv']) {
    const buf = fixture(name)
    const specs = parseVideo(buf)
    t.deepEqual(specs.coverArt, { mime: 'image/jpeg', byteLength: jpeg.length, width: 320, height: 180 }, name)
    const art = coverArt(buf)
    t.truthy(art, name)
    if (!art) continue
    t.is(art.mime, 'image/jpeg')
    t.is(art.byteLength, jpeg.length)
    t.is(art.width, 320)
    t.is(art.height, 180)
    t.true(art.data instanceof Uint8Array)
    t.true(Buffer.from(art.data).equals(jpeg), `${name} bytes`)
    t.deepEqual(await coverArtAsync(buf), art, name)
    const analysis = analyzeVideo(buf, { peakCount: 8 })
    t.true(Buffer.from(analysis.coverArt?.data ?? []).equals(jpeg), `${name} analysis`)
    t.is(analysis.specs.coverArt?.byteLength, jpeg.length)
  }
  t.is(parseVideo(fixture('h264-aac-cover.mp4')).title, 'Cover fixture')
  // The covr atom sits inside moov: a header-only prefix still yields it.
  const mp4 = fixture('h264-aac-cover.mp4')
  const header = mp4.subarray(0, mp4.indexOf('mdat') + 4)
  t.true(Buffer.from(coverArt(header)?.data ?? []).equals(jpeg))
  t.is(parseVideo(header).complete, true)
  for (const name of ['h264-aac-faststart.mp4', 'h264-aac.mkv', 'vp9-vorbis.webm', 'silent.mp4']) {
    t.is(coverArt(fixture(name)), null, name)
    t.is(parseVideo(fixture(name)).coverArt, null, name)
    t.is(analyzeVideo(fixture(name), { peakCount: 4 }).coverArt, null, name)
  }
})

test('Buffer and sliced Uint8Array inputs respect byteOffset', (t) => {
  const b = fixture('h264-aac-faststart.mp4')
  const large = Buffer.concat([Buffer.alloc(31), b, Buffer.alloc(21)])
  const view = new Uint8Array(large.buffer, large.byteOffset + 31, b.length)
  t.deepEqual(parseVideo(view), parseVideo(b))
  t.deepEqual(keyframes(view), keyframes(b))
})

test('async output equals sync output', async (t) => {
  for (const name of ['h264-aac-faststart.mp4', 'vp9-vorbis.webm', 'rotated-90.mp4']) {
    t.deepEqual(await parseVideoAsync(fixture(name), 'src'), parseVideo(fixture(name), 'src'), name)
    t.deepEqual(await keyframesAsync(fixture(name)), keyframes(fixture(name)), name)
  }
})

// ── audio track ─────────────────────────────────────────────────

test('decodeAudio returns the track PCM, null without one', async (t) => {
  const pcm = decodeAudio(fixture('h264-aac-faststart.mp4'))
  t.truthy(pcm)
  if (!pcm) return
  t.true(pcm.samples instanceof Float32Array)
  t.is(pcm.sampleRate, 48000)
  t.is(pcm.channels, 2)
  t.true(Math.abs(pcm.frameCount - 192_000) < 2048, `${pcm.frameCount}`)
  t.true(Math.abs(pcm.durationSec - 4) < 0.05)
  t.is(pcm.gaplessEnabled, true)
  t.is(pcm.skippedPackets, 0)
  t.is(decodeAudio(fixture('silent.mp4')), null)
  t.is(await decodeAudioAsync(fixture('silent.mp4')), null)
  const vorbis = await decodeAudioAsync(fixture('vp9-vorbis.webm'))
  t.is(vorbis?.channels, 2)
  t.is(vorbis?.sampleRate, 48000)
})

function assertEnvelope(t: ExecutionContext, w: WaveformPeaks, peakCount: number) {
  t.is(w.sampleRate, 48000)
  t.is(w.channelCount, 2)
  t.is(w.peakCount, peakCount)
  t.is(w.boundaries.length, peakCount + 1)
  t.is(w.envelope.length, peakCount)
  t.is(w.channels.length, 2)
  t.true(w.channels[0].rms instanceof Float64Array)
  // 0.55 full-scale sine on both channels.
  const max = Math.max(...w.envelope)
  t.true(Math.abs(max - 0.55) < 0.03, `${max}`)
  for (let i = 0; i < w.peakCount; i++) {
    t.is(w.envelope[i], Math.max(w.channels[0].peak[i], w.channels[1].peak[i]))
  }
  const column = Array.from(w.envelope, (v) => Math.round(Math.min(1, v) * 100))
  t.true(column.every((v) => Number.isInteger(v) && v >= 0 && v <= 100))
}

test('audioPeaks reduces the audio track, null without one, throws when undecodable', async (t) => {
  const w = audioPeaks(fixture('h264-aac-faststart.mp4'))
  t.truthy(w)
  if (w) assertEnvelope(t, w, 1024)
  const small = await audioPeaksAsync(fixture('vp9-vorbis.webm'), { peakCount: 100 })
  t.truthy(small)
  if (small) assertEnvelope(t, small, 100)
  t.is(audioPeaks(fixture('silent.mp4')), null)
  t.is(await audioPeaksAsync(fixture('silent.mp4')), null)
  t.throws(() => audioPeaks(fixture('vp9-opus.webm')), { message: /unsupported/ })
  await t.throwsAsync(audioPeaksAsync(fixture('vp9-opus.webm')), { message: /unsupported/ })
  const fast = fixture('h264-aac-faststart.mp4')
  t.throws(() => audioPeaks(fast.subarray(0, fast.indexOf('mdat') + 4)), { message: /prefix/ })
  t.throws(() => decodeAudio(fast.subarray(0, 4096)), { message: /prefix/ })
})

// ── analysis ────────────────────────────────────────────────────

test('analyzeVideo folds specs, keyframes and the envelope with a status', async (t) => {
  const a = await analyzeVideoAsync(fixture('h264-aac-faststart.mp4'), { peakCount: 321 }, 'cdn://fast')
  t.is(a.specs.kind, 'mp4')
  t.is(a.specs.source, 'cdn://fast')
  t.is(a.keyframes.count, 4)
  t.is(a.waveformStatus, 'ok')
  t.is(a.waveformReason, null)
  t.truthy(a.waveform)
  if (a.waveform) assertEnvelope(t, a.waveform, 321)
  t.deepEqual(a, analyzeVideo(fixture('h264-aac-faststart.mp4'), { peakCount: 321 }, 'cdn://fast'))
  t.deepEqual(a.keyframes, keyframes(fixture('h264-aac-faststart.mp4')))

  const silent = analyzeVideo(fixture('silent.mp4'))
  t.is(silent.waveformStatus, 'no-audio')
  t.is(silent.waveform, null)
  t.is(silent.specs.hasAudio, false)

  const opus = analyzeVideo(fixture('vp9-opus.webm'))
  t.is(opus.waveformStatus, 'unsupported-codec')
  t.is(opus.waveform, null)
  t.regex(opus.waveformReason ?? '', /unsupported/)
  t.is(opus.specs.audioCodec, 'opus')

  const fast = fixture('h264-aac-faststart.mp4')
  const prefix = analyzeVideo(fast.subarray(0, fast.indexOf('mdat') + 4))
  t.is(prefix.waveformStatus, 'truncated')
  t.is(prefix.waveform, null)
  t.is(prefix.specs.complete, true)
  t.is(prefix.keyframes.count, 4)
})

test('async methods snapshot input before JS can mutate it', async (t) => {
  const b = fixture('h264-aac-faststart.mp4')
  const pending = analyzeVideoAsync(b, { peakCount: 10 })
  b.fill(0)
  const a = await pending
  t.is(a.specs.width, 320)
  t.is(a.waveform?.peakCount, 10)
})

test('output typed arrays remain valid across garbage collection', async (t) => {
  const w = await audioPeaksAsync(fixture('h264-aac-faststart.mp4'))
  const expected = Array.from(w?.envelope ?? [])
  if (globalThis.gc) for (let i = 0; i < 3; i++) globalThis.gc()
  t.deepEqual(Array.from(w?.envelope ?? []), expected)
})

// ── validation ──────────────────────────────────────────────────

test('rejects ambiguous resolutions, fractions and invalid inputs', async (t) => {
  const b = fixture('h264-aac-faststart.mp4')
  for (const o of [
    { peakCount: 0 },
    { peakCount: -1 },
    { peakCount: 1.5 },
    { peakCount: NaN },
    { peakCount: Infinity },
    { samplesPerPeak: 0 },
    { peakCount: 10, samplesPerPeak: 5 },
    { peakCount: 1_000_001 },
  ]) {
    t.throws(() => audioPeaks(b, o), undefined, JSON.stringify(o))
  }
  t.throws(() => decodeAudio(b, { maxDecodedSamples: 0 }))
  t.throws(() => decodeAudio(b, { maxDecodedSamples: 100 }), { message: /maxDecodedSamples/ })
  t.throws(() => parseVideo(Uint8Array.of(0, 1, 2, 3)), { message: /unrecognized/ })
  t.throws(() => keyframes(Uint8Array.of(0, 1, 2, 3)), { message: /unrecognized/ })
  await t.throwsAsync(parseVideoAsync(Uint8Array.of(0, 1, 2, 3)), { message: /unrecognized/ })
  await t.throwsAsync(audioPeaksAsync(Uint8Array.of(0, 1, 2, 3)), { message: /unrecognized/ })
  t.throws(() => new VideoDown(Uint8Array.of(0, 1, 2, 3)), { message: /unrecognized/ })
})

// ── VideoService ────────────────────────────────────────────────

test('VideoService mirrors the standalone functions', async (t) => {
  const service = new VideoService()
  const mp4 = fixture('h264-aac-faststart.mp4')
  const webm = fixture('vp9-vorbis.webm')
  t.is(service.sniffVideo(mp4), 'mp4')
  t.deepEqual(service.parseVideo(mp4, 'x'), parseVideo(mp4, 'x'))
  t.deepEqual(await service.parseVideoAsync(webm), parseVideo(webm))
  t.deepEqual(service.keyframes(mp4), keyframes(mp4))
  t.deepEqual(await service.keyframesAsync(mp4), keyframes(mp4))
  const cover = fixture('h264-aac-cover.mp4')
  t.deepEqual(service.coverArt(cover), coverArt(cover))
  t.deepEqual(await service.coverArtAsync(cover), coverArt(cover))
  t.is(service.coverArt(mp4), null)
  t.deepEqual(service.decodeAudio(mp4), decodeAudio(mp4))
  t.deepEqual(await service.decodeAudioAsync(webm), decodeAudio(webm))
  t.deepEqual(service.audioPeaks(mp4, { peakCount: 50 }), audioPeaks(mp4, { peakCount: 50 }))
  t.deepEqual(await service.audioPeaksAsync(mp4, { peakCount: 50 }), audioPeaks(mp4, { peakCount: 50 }))
  t.deepEqual(service.analyzeVideo(mp4, { peakCount: 9 }), analyzeVideo(mp4, { peakCount: 9 }))
  t.deepEqual(await service.analyzeVideoAsync(mp4, { peakCount: 9 }), analyzeVideo(mp4, { peakCount: 9 }))
})

test('VideoService defaults merge under per-call options', (t) => {
  const service = new VideoService({ peakCount: 64, gapless: false })
  t.deepEqual(service.defaults, { peakCount: 64, gapless: false })
  const mp4 = fixture('h264-aac-faststart.mp4')
  t.is(service.audioPeaks(mp4)?.peakCount, 64)
  // One bucket per second of decoded audio; AAC priming makes it a hair over four.
  const frames = decodeAudio(mp4)?.frameCount ?? 0
  t.is(service.audioPeaks(mp4, { samplesPerPeak: 48000 })?.peakCount, Math.ceil(frames / 48000))
  t.is(service.audioPeaks(mp4, { peakCount: 5 })?.peakCount, 5)
  t.is(service.analyzeVideo(mp4).waveform?.peakCount, 64)
  t.is(service.decodeAudio(mp4)?.gaplessEnabled, false)
  t.throws(() => new VideoService({ peakCount: 0 }))
})

// ── VideoDown ───────────────────────────────────────────────────

test('VideoDown snapshots once and serves every call', async (t) => {
  const buf = fixture('h264-aac-faststart.mp4')
  const down = new VideoService({ peakCount: 32 }).open(buf, 'cdn://down')
  t.is(down.kind, 'mp4')
  t.is(down.byteSize, buf.length)
  t.is(down.source, 'cdn://down')
  t.deepEqual(down.metadata(), parseVideo(buf, 'cdn://down'))
  t.deepEqual(await down.metadataAsync(), parseVideo(buf, 'cdn://down'))
  t.deepEqual(down.keyframes(), keyframes(buf))
  t.deepEqual(await down.keyframesAsync(), keyframes(buf))
  t.is(down.coverArt(), null)
  t.is(await down.coverArtAsync(), null)
  const withCover = new VideoDown(fixture('h264-aac-cover.mp4'))
  t.deepEqual(withCover.coverArt(), coverArt(fixture('h264-aac-cover.mp4')))
  t.deepEqual(await withCover.coverArtAsync(), coverArt(fixture('h264-aac-cover.mp4')))
  t.is(down.audioPeaks()?.peakCount, 32)
  t.is((await down.audioPeaksAsync({ peakCount: 7 }))?.peakCount, 7)
  t.deepEqual(down.decodeAudio(), decodeAudio(buf))
  t.is((await down.decodeAudioAsync())?.frameCount, decodeAudio(buf)?.frameCount)
  t.deepEqual(down.analyze(), analyzeVideo(buf, { peakCount: 32 }, 'cdn://down'))
  t.deepEqual(await down.analyzeAsync({ peakCount: 3 }), analyzeVideo(buf, { peakCount: 3 }, 'cdn://down'))
  // The handle owns its copy.
  buf.fill(0)
  t.is(down.metadata().width, 320)
  const direct = new VideoDown(fixture('silent.mp4'))
  t.is(direct.source, null)
  t.is(direct.audioPeaks(), null)
  t.is(direct.analyze().waveformStatus, 'no-audio')
})
