"""Reproducible fixtures. FFmpeg and ffprobe are development-only dependencies."""
import json, pathlib, subprocess
root = pathlib.Path(__file__).resolve().parent.parent
out = root / '__test__' / 'fixtures'
out.mkdir(parents=True, exist_ok=True)

# Four seconds, 320x180 at 30 fps, one keyframe per second. Stereo audio in
# opposite phase (naive mono averaging cancels it), 48 kHz.
VIDEO = ['-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=30:duration=4']
AUDIO = ['-f', 'lavfi', '-i', 'aevalsrc=0.55*sin(2*PI*440*t)|-0.55*sin(2*PI*440*t):s=48000:d=4']
AUDIO2 = ['-f', 'lavfi', '-i', 'aevalsrc=0.3*sin(2*PI*880*t):s=44100:d=4']
H264 = ['-c:v', 'libx264', '-preset', 'veryfast', '-pix_fmt', 'yuv420p', '-g', '30', '-keyint_min', '30', '-sc_threshold', '0']
AAC = ['-c:a', 'aac', '-b:a', '128k']
META = ['-metadata', 'creation_time=2026-10-14T12:00:00Z']
FASTSTART = ['-movflags', '+faststart']

def ffmpeg(*args):
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', *map(str, args)], check=True)

ffmpeg(*VIDEO, *AUDIO, *H264, *AAC, *META, *FASTSTART, out / 'h264-aac-faststart.mp4')
ffmpeg(*VIDEO, *AUDIO, *H264, *AAC, *META, out / 'h264-aac-mdat-first.mp4')
ffmpeg(*VIDEO, *AUDIO, '-c:v', 'libx265', '-preset', 'ultrafast', '-tag:v', 'hvc1',
       '-x265-params', 'log-level=error:keyint=30:min-keyint=30:scenecut=0', *AAC, *META, *FASTSTART, out / 'hevc-aac.mp4')
ffmpeg(*VIDEO, *AUDIO, '-c:v', 'libaom-av1', '-cpu-used', '8', '-row-mt', '1', '-g', '30', *AAC, *META, *FASTSTART, out / 'av1-aac.mp4')
ffmpeg(*VIDEO, *AUDIO, '-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-cpu-used', '8', '-g', '30', '-c:a', 'libopus', '-b:a', '96k', out / 'vp9-opus.webm')
ffmpeg(*VIDEO, *AUDIO, '-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-cpu-used', '8', '-g', '30', '-c:a', 'libvorbis', '-q:a', '4', out / 'vp9-vorbis.webm')
ffmpeg(*VIDEO, *AUDIO, *H264, *AAC, out / 'h264-aac.mkv')
# What MediaRecorder and `-live 1` produce: unknown-size Segment and Clusters, no Cues, no Duration.
ffmpeg(*VIDEO, *AUDIO, '-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-cpu-used', '8', '-g', '30', '-c:a', 'libopus', '-b:a', '96k', '-live', '1', out / 'vp9-opus-live.webm')
ffmpeg(*VIDEO, *AUDIO, *H264, *AAC, '-f', 'mov', *FASTSTART, out / 'h264-aac.mov')

# FFmpeg 4.x writes a tkhd matrix only from display-matrix side data, never
# from a `rotate` tag, so rotation is stamped into the video trak's tkhd by
# hand: the same values movenc writes for 90 and 270 (ISO 14496-12 § 8.3.2).
import struct

def boxes(buf, start, end):
    at = start
    while at + 8 <= end:
        size, kind = struct.unpack('>I4s', buf[at:at + 8]); body = at + 8
        if size == 1:
            size = struct.unpack('>Q', buf[at + 8:at + 16])[0]; body = at + 16
        elif size == 0:
            size = end - at
        yield kind, at, body, at + size
        at += size

def find(buf, start, end, kind):
    return next(((b, e) for k, _, b, e in boxes(buf, start, end) if k == kind), None)

def rotate_tkhd(src, dst, degrees):
    buf = bytearray(src.read_bytes())
    moov = find(buf, 0, len(buf), b'moov')
    for kind, _, body, end in boxes(buf, *moov):
        if kind != b'trak':
            continue
        mdia = find(buf, body, end, b'mdia')
        hdlr = find(buf, *mdia, b'hdlr')
        if buf[hdlr[0] + 8:hdlr[0] + 12] != b'vide':
            continue
        tkhd = find(buf, body, end, b'tkhd')
        version = buf[tkhd[0]]
        at = tkhd[0] + 4 + (32 if version == 1 else 20) + 16
        width, height = struct.unpack('>II', buf[at + 36:at + 44])
        one, w = 1 << 16, 1 << 30
        matrix = {90: (0, one, 0, -one, 0, 0, height, 0, w), 270: (0, -one, 0, one, 0, 0, 0, width, w)}[degrees]
        buf[at:at + 36] = struct.pack('>9i', *matrix)
    dst.write_bytes(buf)

ffmpeg(*VIDEO, *AUDIO, *H264, *AAC, *FASTSTART, out / 'rotated-90.mp4')
rotate_tkhd(out / 'rotated-90.mp4', out / 'rotated-270.mp4', 270)
rotate_tkhd(out / 'rotated-90.mp4', out / 'rotated-90.mp4', 90)
ffmpeg(*VIDEO, *H264, '-an', *FASTSTART, out / 'silent.mp4')
ffmpeg(*VIDEO, *AUDIO, *AUDIO2, '-map', '0:v', '-map', '1:a', '-map', '2:a', *H264, *AAC, *FASTSTART, out / 'two-audio.mp4')

# Independent PCM references for the decoder comparison.
for name in ['h264-aac-faststart.mp4', 'vp9-vorbis.webm']:
    ffmpeg('-i', out / name, '-vn', '-f', 'f32le', '-c:a', 'pcm_f32le', out / (name + '.reference.f32'))

def ffprobe(*args):
    return subprocess.check_output(['ffprobe', '-v', 'error', *map(str, args)]).decode()

def packets(path):
    times, total = [], 0
    for line in ffprobe('-select_streams', 'v:0', '-show_packets', '-show_entries', 'packet=pts_time,flags', '-of', 'csv=p=0', path).splitlines():
        parts = line.split(',')
        if len(parts) < 2:
            continue
        total += 1
        if 'K' in parts[1] and parts[0] not in ('', 'N/A'):
            times.append(float(parts[0]))
    return sorted(times), total

def number(value, cast):
    try:
        return cast(value)
    except (TypeError, ValueError):
        return None

def probe(path):
    data = json.loads(ffprobe('-print_format', 'json', '-show_format', '-show_streams', path))
    fmt = data['format']
    video = next((s for s in data['streams'] if s['codec_type'] == 'video'), None)
    audio = [s for s in data['streams'] if s['codec_type'] == 'audio']
    return {
        'formatName': fmt['format_name'],
        'size': int(fmt['size']),
        'duration': number(fmt.get('duration'), float),
        'bitRate': number(fmt.get('bit_rate'), int),
        'creationTime': (fmt.get('tags') or {}).get('creation_time'),
        'streams': len(data['streams']),
        'video': video and {
            'codec': video['codec_name'],
            'codecTag': video.get('codec_tag_string'),
            'width': video['width'],
            'height': video['height'],
            'rFrameRate': video['r_frame_rate'],
            'avgFrameRate': video['avg_frame_rate'],
            'nbFrames': number(video.get('nb_frames'), int),
            'duration': number(video.get('duration'), float),
            'bitRate': number(video.get('bit_rate'), int),
            'rotate': (video.get('tags') or {}).get('rotate'),
            'sideData': video.get('side_data_list'),
            'displayAspectRatio': video.get('display_aspect_ratio'),
        },
        'audio': [{
            'codec': s['codec_name'],
            'codecTag': s.get('codec_tag_string'),
            'channels': s['channels'],
            'sampleRate': int(s['sample_rate']),
            'duration': number(s.get('duration'), float),
            'bitRate': number(s.get('bit_rate'), int),
        } for s in audio],
        'keyframes': packets(path)[0],
        'videoPackets': packets(path)[1],
    }

info = {p.name: probe(p) for p in sorted(out.iterdir()) if p.suffix in ('.mp4', '.mov', '.webm', '.mkv')}
(out / 'fixture-info.json').write_text(json.dumps(info, indent=2) + '\n')
print(f'Generated {len(info)} video fixtures, two FFmpeg PCM references and fixture-info.json.')
