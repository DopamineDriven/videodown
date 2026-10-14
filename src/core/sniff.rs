//! Magic sniffing. ISOBMFF is recognised by a known top-level box type at
//! offset 4; Matroska and WebM by the EBML header, told apart by `DocType`.

use super::mkv;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoKind {
  Mp4,
  Mov,
  Webm,
  Mkv,
}

impl VideoKind {
  pub fn as_str(self) -> &'static str {
    match self {
      VideoKind::Mp4 => "mp4",
      VideoKind::Mov => "mov",
      VideoKind::Webm => "webm",
      VideoKind::Mkv => "mkv",
    }
  }

  pub fn mime(self) -> &'static str {
    match self {
      VideoKind::Mp4 => "video/mp4",
      VideoKind::Mov => "video/quicktime",
      VideoKind::Webm => "video/webm",
      VideoKind::Mkv => "video/x-matroska",
    }
  }

  pub fn container(self) -> VideoContainer {
    match self {
      VideoKind::Mp4 | VideoKind::Mov => VideoContainer::Isobmff,
      VideoKind::Webm | VideoKind::Mkv => VideoContainer::Matroska,
    }
  }

  pub fn is_isobmff(self) -> bool {
    self.container() == VideoContainer::Isobmff
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoContainer {
  Isobmff,
  Matroska,
}

impl VideoContainer {
  pub fn as_str(self) -> &'static str {
    match self {
      VideoContainer::Isobmff => "isobmff",
      VideoContainer::Matroska => "matroska",
    }
  }
}

/// Box types that may legally open an ISOBMFF file. `ftyp` is the norm; the
/// rest cover QuickTime files without one and fragmented / streaming layouts.
const ISOBMFF_LEADING_BOXES: [&[u8; 4]; 13] = [
  b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid", b"styp", b"sidx",
  b"moof", b"meta", b"pdin",
];

pub const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

pub fn kind_for_brand(brand: &[u8]) -> VideoKind {
  if brand == b"qt  " {
    VideoKind::Mov
  } else {
    VideoKind::Mp4
  }
}

pub fn sniff(b: &[u8]) -> Option<VideoKind> {
  if b.len() >= 8 && ISOBMFF_LEADING_BOXES.iter().any(|t| &b[4..8] == *t) {
    if &b[4..8] == b"ftyp" && b.len() >= 12 {
      return Some(kind_for_brand(&b[8..12]));
    }
    return Some(VideoKind::Mp4);
  }
  if b.len() >= 4 && b[..4] == EBML_MAGIC {
    return Some(match mkv::doc_type(b).as_deref() {
      Some("webm") => VideoKind::Webm,
      _ => VideoKind::Mkv,
    });
  }
  None
}
