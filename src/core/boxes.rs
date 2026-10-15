//! Minimal ISOBMFF box walker for the things `mp4parse` does not expose:
//! top-level box order (`moov` before `mdat`), `ftyp` brands, `mvhd` times
//! and duration, the per-track `hdlr` type, `mdhd` language and `stsd`
//! sample-entry fourcc, and whether the buffer is a prefix of the file.
//! Every read is bounds-checked; arbitrary bytes never panic.

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
  b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
  b.get(at..at + 4)
    .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be_u64(b: &[u8], at: usize) -> Option<u64> {
  b.get(at..at + 8)
    .map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn fourcc(b: &[u8], at: usize) -> Option<[u8; 4]> {
  b.get(at..at + 4).map(|s| [s[0], s[1], s[2], s[3]])
}

pub fn fourcc_string(tag: &[u8; 4]) -> String {
  tag
    .iter()
    .map(|&c| {
      if (0x20..0x7f).contains(&c) {
        char::from(c)
      } else {
        '?'
      }
    })
    .collect()
}

/// One box header. `end` is the declared absolute end offset; `u64::MAX`
/// stands for "to the end of the file" (a zero size field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
  pub kind: [u8; 4],
  pub start: usize,
  pub body: usize,
  pub end: u64,
}

impl Head {
  pub fn fits(&self, limit: usize) -> bool {
    self.end <= limit as u64
  }

  pub fn body_end(&self, limit: usize) -> usize {
    usize::try_from(self.end.min(limit as u64)).unwrap_or(limit)
  }
}

pub fn read_head(b: &[u8], at: usize, limit: usize) -> Option<Head> {
  if at.checked_add(8)? > limit {
    return None;
  }
  let size32 = be_u32(b, at)?;
  let kind = fourcc(b, at + 4)?;
  let (body, size) = match size32 {
    0 => (at + 8, None),
    1 => {
      if at + 16 > limit {
        return None;
      }
      (at + 16, Some(be_u64(b, at + 8)?))
    }
    n => (at + 8, Some(u64::from(n))),
  };
  let end = match size {
    None => u64::MAX,
    Some(s) => {
      if s < (body - at) as u64 {
        return None;
      }
      (at as u64).checked_add(s)?
    }
  };
  Some(Head {
    kind,
    start: at,
    body,
    end,
  })
}

/// Iterates the boxes in `[at, limit)`. A box whose declared end runs past
/// `limit` is still yielded (callers check [`Head::fits`]) and ends the walk.
pub struct Children<'a> {
  b: &'a [u8],
  at: usize,
  limit: usize,
}

pub fn children(b: &[u8], at: usize, limit: usize) -> Children<'_> {
  Children {
    b,
    at,
    limit: limit.min(b.len()),
  }
}

impl Iterator for Children<'_> {
  type Item = Head;

  fn next(&mut self) -> Option<Head> {
    let head = read_head(self.b, self.at, self.limit)?;
    self.at = if head.fits(self.limit) {
      usize::try_from(head.end).unwrap_or(self.limit)
    } else {
      self.limit
    };
    Some(head)
  }
}

fn find(b: &[u8], at: usize, limit: usize, kind: &[u8; 4]) -> Option<Head> {
  children(b, at, limit).find(|h| &h.kind == kind && h.fits(limit))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ftyp {
  pub major: String,
  pub minor_version: u32,
  pub compatible: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mvhd {
  pub timescale: u32,
  pub duration: u64,
  /// Seconds since 1904-01-01T00:00:00Z; zero when unset.
  pub creation: u64,
  pub modification: u64,
}

/// What the walker learns about one `trak`, in `moov` order (the same order
/// `mp4parse` numbers its tracks).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trak {
  pub handler: Option<[u8; 4]>,
  pub sample_entry: Option<[u8; 4]>,
  /// ISO 639-2/T code from `mdhd`; `None` for `und` or absent.
  pub language: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
  /// Top-level box types in file order.
  pub order: Vec<String>,
  pub ftyp: Option<Ftyp>,
  /// `[start, end)` of the `moov` box when it lies entirely within the buffer.
  pub moov: Option<(usize, usize)>,
  /// A `moov` header was seen but the box runs past the buffer.
  pub moov_partial: bool,
  pub mdat_seen: bool,
  pub moov_before_mdat: Option<bool>,
  pub fragmented: bool,
  /// End offset of the last top-level box, when every box declares a size.
  pub declared_size: Option<u64>,
  /// Some top-level box runs past the buffer: the buffer is a prefix.
  pub truncated: bool,
  pub mvhd: Option<Mvhd>,
  pub traks: Vec<Trak>,
  pub ilst: Ilst,
}

fn read_ftyp(b: &[u8], head: &Head, limit: usize) -> Option<Ftyp> {
  let end = head.body_end(limit);
  let major = fourcc(b, head.body)?;
  let minor_version = be_u32(b, head.body + 4)?;
  let mut compatible = Vec::new();
  let mut at = head.body + 8;
  while at + 4 <= end {
    if let Some(tag) = fourcc(b, at) {
      compatible.push(fourcc_string(&tag));
    }
    at += 4;
  }
  Some(Ftyp {
    major: fourcc_string(&major),
    minor_version,
    compatible,
  })
}

fn read_mvhd(b: &[u8], head: &Head) -> Option<Mvhd> {
  let version = *b.get(head.body)?;
  let at = head.body + 4;
  Some(match version {
    0 => Mvhd {
      creation: u64::from(be_u32(b, at)?),
      modification: u64::from(be_u32(b, at + 4)?),
      timescale: be_u32(b, at + 8)?,
      duration: u64::from(be_u32(b, at + 12)?),
    },
    1 => Mvhd {
      creation: be_u64(b, at)?,
      modification: be_u64(b, at + 8)?,
      timescale: be_u32(b, at + 16)?,
      duration: be_u64(b, at + 20)?,
    },
    _ => return None,
  })
}

/// `mdhd` language: three 5-bit fields, each `+ 0x60`, packed into 16 bits.
fn read_mdhd_language(b: &[u8], head: &Head) -> Option<String> {
  let version = *b.get(head.body)?;
  let at = head.body + 4 + if version == 1 { 28 } else { 16 };
  let packed = be_u16(b, at)?;
  let code: String = [10u16, 5, 0]
    .iter()
    .map(|shift| char::from(((packed >> shift) & 0x1f) as u8 + 0x60))
    .collect();
  if code == "und" || !code.bytes().all(|c| c.is_ascii_lowercase()) {
    None
  } else {
    Some(code)
  }
}

fn read_trak(b: &[u8], head: &Head, limit: usize) -> Trak {
  let mut trak = Trak::default();
  let end = head.body_end(limit);
  let Some(mdia) = find(b, head.body, end, b"mdia") else {
    return trak;
  };
  let mdia_end = mdia.body_end(limit);
  if let Some(hdlr) = find(b, mdia.body, mdia_end, b"hdlr") {
    trak.handler = fourcc(b, hdlr.body + 8);
  }
  if let Some(mdhd) = find(b, mdia.body, mdia_end, b"mdhd") {
    trak.language = read_mdhd_language(b, &mdhd);
  }
  let stsd = find(b, mdia.body, mdia_end, b"minf")
    .and_then(|minf| find(b, minf.body, minf.body_end(limit), b"stbl"))
    .and_then(|stbl| find(b, stbl.body, stbl.body_end(limit), b"stsd"));
  if let Some(stsd) = stsd {
    // fullbox (4) + entry_count (4), then the first sample entry's header.
    if be_u32(b, stsd.body + 4).is_some_and(|n| n > 0) {
      trak.sample_entry = read_head(b, stsd.body + 8, stsd.body_end(limit)).map(|h| h.kind);
    }
  }
  trak
}

/// iTunes-style metadata under `moov/udta/meta/ilst`: the cover art and
/// the two text items a video pipeline cares about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ilst {
  /// `covr` payload: offset, length and the `data` atom's type code
  /// (13 JPEG, 14 PNG, 27 BMP, 0 implicit).
  pub cover: Option<(usize, usize, u32)>,
  /// `©too`, the muxing tool.
  pub encoder: Option<String>,
  /// `©nam`.
  pub title: Option<String>,
}

/// The payload of an item's first `data` atom: its type code and byte range.
fn ilst_data(b: &[u8], item: &Head, limit: usize) -> Option<(u32, usize, usize)> {
  let data = find(b, item.body, item.body_end(limit), b"data")?;
  // 4 bytes of type indicator (1 version + 3 well-known type), 4 of locale.
  let kind = be_u32(b, data.body)? & 0x00ff_ffff;
  let payload = data.body + 8;
  let end = data.body_end(limit);
  (payload <= end).then_some((kind, payload, end - payload))
}

fn ilst_text(b: &[u8], item: &Head, limit: usize) -> Option<String> {
  let (kind, at, len) = ilst_data(b, item, limit)?;
  if kind != 1 && kind != 0 {
    return None;
  }
  let text = String::from_utf8_lossy(b.get(at..at + len)?)
    .trim_end_matches('\0')
    .to_owned();
  (!text.is_empty()).then_some(text)
}

fn read_ilst(b: &[u8], moov: &Head, limit: usize) -> Ilst {
  let mut out = Ilst::default();
  let end = moov.body_end(limit);
  let Some(udta) = find(b, moov.body, end, b"udta") else {
    return out;
  };
  let Some(meta) = find(b, udta.body, udta.body_end(limit), b"meta") else {
    return out;
  };
  // ISOBMFF `meta` is a full box (4 bytes of version/flags); QuickTime
  // writes it as a plain box. Look for a child header at both offsets.
  let meta_end = meta.body_end(limit);
  let start = if read_head(b, meta.body + 4, meta_end)
    .is_some_and(|h| matches!(&h.kind, b"hdlr" | b"ilst" | b"keys"))
  {
    meta.body + 4
  } else {
    meta.body
  };
  let Some(ilst) = find(b, start, meta_end, b"ilst") else {
    return out;
  };
  for item in children(b, ilst.body, ilst.body_end(limit)) {
    if !item.fits(limit) {
      break;
    }
    match &item.kind {
      b"covr" => {
        if let Some((kind, at, len)) = ilst_data(b, &item, limit)
          && len > 0
        {
          out.cover = Some((at, len, kind));
        }
      }
      [0xA9, b't', b'o', b'o'] => out.encoder = ilst_text(b, &item, limit),
      [0xA9, b'n', b'a', b'm'] => out.title = ilst_text(b, &item, limit),
      _ => {}
    }
  }
  out
}

fn read_moov(b: &[u8], head: &Head, limit: usize, layout: &mut Layout) {
  let end = head.body_end(limit);
  for child in children(b, head.body, end) {
    if !child.fits(end) {
      break;
    }
    match &child.kind {
      b"mvhd" => layout.mvhd = read_mvhd(b, &child),
      b"trak" => layout.traks.push(read_trak(b, &child, limit)),
      b"mvex" => layout.fragmented = true,
      _ => {}
    }
  }
  layout.ilst = read_ilst(b, head, limit);
}

pub fn layout(b: &[u8]) -> Layout {
  let limit = b.len();
  let mut layout = Layout::default();
  let mut moov_index = None;
  let mut mdat_index = None;
  let mut declared: Option<u64> = Some(0);
  for (index, head) in children(b, 0, limit).enumerate() {
    layout.order.push(fourcc_string(&head.kind));
    declared = match (declared, head.end) {
      (Some(_), u64::MAX) => None,
      (Some(d), end) => Some(d.max(end)),
      (None, _) => None,
    };
    if !head.fits(limit) {
      layout.truncated = true;
      if &head.kind == b"moov" {
        layout.moov_partial = true;
        moov_index.get_or_insert(index);
      }
      if &head.kind == b"mdat" {
        layout.mdat_seen = true;
        mdat_index.get_or_insert(index);
      }
      break;
    }
    match &head.kind {
      b"ftyp" => layout.ftyp = read_ftyp(b, &head, limit),
      b"moov" => {
        moov_index.get_or_insert(index);
        if layout.moov.is_none() {
          layout.moov = Some((head.start, head.body_end(limit)));
          read_moov(b, &head, limit, &mut layout);
        }
      }
      b"mdat" => {
        layout.mdat_seen = true;
        mdat_index.get_or_insert(index);
      }
      b"moof" => layout.fragmented = true,
      _ => {}
    }
  }
  layout.declared_size = declared.filter(|d| *d > 0);
  if let Some(d) = layout.declared_size
    && d > limit as u64
  {
    layout.truncated = true;
  }
  layout.moov_before_mdat = match (moov_index, mdat_index) {
    (Some(m), Some(d)) => Some(m < d),
    // A complete moov with the mdat cut off: the remainder follows it.
    (Some(_), None) if layout.moov.is_some() && layout.truncated => Some(true),
    _ => None,
  };
  layout
}
