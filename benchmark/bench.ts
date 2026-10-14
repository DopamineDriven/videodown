import { readFileSync } from 'node:fs'
import { Bench } from 'tinybench'

import {
  VideoService,
  analyzeVideo,
  analyzeVideoAsync,
  audioPeaks,
  audioPeaksAsync,
  decodeAudio,
  keyframes,
  parseVideo,
  parseVideoAsync,
} from '../index.js'

const fixture = (name: string) => readFileSync(new URL(`../__test__/fixtures/${name}`, import.meta.url))

const mp4 = fixture('h264-aac-faststart.mp4')
const webm = fixture('vp9-vorbis.webm')
// Header-only prefix: what the extractor fetches first.
const header = mp4.subarray(0, mp4.indexOf('mdat') + 4)

const service = new VideoService({ peakCount: 1024 })
const handle = service.open(mp4)

const b = new Bench()

b.add('parseVideo mp4 (sync)', () => parseVideo(mp4))
b.add('parseVideoAsync mp4', async () => await parseVideoAsync(mp4))
b.add('parseVideo mp4 header prefix (sync)', () => parseVideo(header))
b.add('parseVideo webm (sync)', () => parseVideo(webm))
b.add('keyframes mp4 (sync)', () => keyframes(mp4))

b.add('decodeAudio mp4 4s aac (sync)', () => decodeAudio(mp4))
b.add('audioPeaks mp4 4s aac, 1024 peaks (sync)', () => audioPeaks(mp4, { peakCount: 1024 }))
b.add('audioPeaksAsync mp4 4s aac, 1024 peaks', async () => await audioPeaksAsync(mp4, { peakCount: 1024 }))
b.add('audioPeaks webm 4s vorbis, 1024 peaks (sync)', () => audioPeaks(webm, { peakCount: 1024 }))

b.add('analyzeVideo mp4 (sync)', () => analyzeVideo(mp4))
b.add('analyzeVideoAsync mp4', async () => await analyzeVideoAsync(mp4))
b.add('VideoDown metadata + keyframes (1 snapshot)', () => {
  handle.metadata()
  handle.keyframes()
})

await b.run()

console.table(b.table())
