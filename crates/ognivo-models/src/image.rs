//! Images the model sees (docs/architecture.md §5.8): the user's, sent with a message, and
//! an image file a call reads. An image is its bytes as they came, kept in base64, the form
//! every request and the store take, with what its header says of its size.
//!
//! The formats are those llama.cpp's projector reads on every build: PNG, JPEG, GIF and BMP
//! (stb_image in `tools/mtmd/mtmd-helper.cpp`). WebP it reads only when built with FFmpeg,
//! so ognivo does not take it: an image the server cannot read fails every later step of
//! the conversation, which keeps it. The GUI turns other formats into PNG before it sends.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The largest image ognivo takes: what llama-server takes from a URL too.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// The most images one message of the user's may hold, and their bytes together.
pub const MAX_PER_MESSAGE: usize = 8;
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// The bytes of images one request carries at most: llama-server takes a request of up to
/// 100 MB (cpp-httplib's limit), and the images go in it in base64 with the rest of the
/// conversation. Older images than fit go as what they are.
pub const MAX_REQUEST_BYTES: usize = 48 * 1024 * 1024;
const _: () = assert!(MAX_MESSAGE_BYTES <= MAX_REQUEST_BYTES);
const _: () = assert!(MAX_REQUEST_BYTES.div_ceil(3) * 4 < 90 * 1024 * 1024);
/// The largest side an image may have, in pixels.
const MAX_SIDE: u32 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Bmp => "image/bmp",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("not base64")]
    Base64,
    #[error("a WebP image: the server reads PNG, JPEG, GIF and BMP")]
    WebP,
    #[error("not a PNG, JPEG, GIF or BMP image")]
    Format,
    #[error("a {0} image whose header is cut or damaged")]
    Damaged(&'static str),
    #[error("{width}×{height} pixels: an image may be at most {MAX_SIDE} on a side")]
    Size { width: u32, height: u32 },
    #[error("{}, over the {} an image may have", about_bytes(*.0), about_bytes(MAX_BYTES))]
    TooLarge(usize),
}

/// An image whose format and size ognivo knows. Cloning it is cheap.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    format: Format,
    width: u32,
    height: u32,
    /// The length of its bytes.
    size: usize,
    /// Its bytes in standard base64 with padding.
    data: Arc<str>,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({})", self.describe())
    }
}

impl Image {
    pub fn new(bytes: &[u8]) -> Result<Image, ImageError> {
        if bytes.len() > MAX_BYTES {
            return Err(ImageError::TooLarge(bytes.len()));
        }
        let (format, width, height) = header(bytes)?;
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(ImageError::Size { width, height });
        }
        Ok(Image {
            format,
            width,
            height,
            size: bytes.len(),
            data: STANDARD.encode(bytes).into(),
        })
    }

    /// From its bytes in base64, as the protocol and the store carry them; a data URL's
    /// prefix is taken off first.
    pub fn from_base64(data: &str) -> Result<Image, ImageError> {
        let data = match data.split_once(";base64,") {
            Some((head, rest)) if head.starts_with("data:") => rest,
            _ => data,
        };
        // the bytes of a larger image are not decoded at all
        if data.len() / 4 * 3 > MAX_BYTES + 3 {
            return Err(ImageError::TooLarge(data.len() / 4 * 3));
        }
        let bytes = STANDARD
            .decode(data.trim().as_bytes())
            .map_err(|_| ImageError::Base64)?;
        Image::new(&bytes)
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn mime(&self) -> &'static str {
        self.format.mime()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The length of its bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Its bytes in standard base64.
    pub fn base64(&self) -> &str {
        &self.data
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime(), self.data)
    }

    /// `PNG, 1280×720, 118 KB`.
    pub fn describe(&self) -> String {
        format!(
            "{}, {}×{}, {}",
            self.format.name(),
            self.width,
            self.height,
            about_bytes(self.size)
        )
    }

    /// About the tokens the model's projector makes of it: Gemma 4's take 70 to 1120,
    /// Qwen's one for each 32×32 pixels up to 4096 (llama.cpp `tools/mtmd/clip.cpp`). The
    /// count the server gives back is the real one; this one stands until it does.
    pub fn tokens(&self) -> u64 {
        (u64::from(self.width) * u64::from(self.height))
            .div_ceil(32 * 32)
            .clamp(256, 4096)
    }
}

/// `118 KB`, `2.4 MB`.
pub fn about_bytes(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;
    if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n.div_ceil(KB))
    } else {
        format!("{n} bytes")
    }
}

/// Why the images of one message cannot go together: too many, or too large.
pub fn too_many(images: &[Image]) -> Option<String> {
    let bytes: usize = images.iter().map(Image::size).sum();
    if images.len() > MAX_PER_MESSAGE {
        Some(format!(
            "{} images: a message may hold at most {MAX_PER_MESSAGE}",
            images.len()
        ))
    } else if bytes > MAX_MESSAGE_BYTES {
        Some(format!(
            "the images have {} together, over the {} a message may hold",
            about_bytes(bytes),
            about_bytes(MAX_MESSAGE_BYTES)
        ))
    } else {
        None
    }
}

/// Whether `head`, the first 16 bytes of a file or fewer, begin as an image: of a format
/// ognivo takes, or WebP, which [`Image::new`] then refuses with why.
pub fn looks_like_image(head: &[u8]) -> bool {
    format_of(head).is_some() || is_webp(head)
}

fn format_of(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if bytes.len() >= 16
        && bytes.starts_with(b"BM")
        // the reserved words are zero, and the info header one of the known sizes
        && bytes[6..10] == [0; 4]
        && matches!(bytes[14], 12 | 40 | 52 | 56 | 64 | 108 | 124)
        && bytes[15] == 0
    {
        Some(Format::Bmp)
    } else {
        None
    }
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

/// The format and the size in pixels, from the header.
fn header(bytes: &[u8]) -> Result<(Format, u32, u32), ImageError> {
    let Some(format) = format_of(bytes) else {
        return Err(if is_webp(bytes) {
            ImageError::WebP
        } else {
            ImageError::Format
        });
    };
    let damaged = ImageError::Damaged(format.name());
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let le16 = |at: usize| -> Option<u32> {
        Some(u32::from(u16::from_le_bytes(
            bytes.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let le32 = |at: usize| -> Option<i32> {
        Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let size = match format {
        // the signature, then IHDR: length, type, width, height
        Format::Png => (bytes.get(12..16) == Some(b"IHDR".as_slice()))
            .then(|| Some((be32(16)?, be32(20)?)))
            .flatten(),
        // the logical screen
        Format::Gif => le16(6).zip(le16(8)),
        // the file header, then the info header: a 12-byte core header has 16-bit sides,
        // the others signed 32-bit ones, a negative height for rows top down
        Format::Bmp => match le32(14) {
            Some(12) => le16(18).zip(le16(20)),
            Some(_) => le32(18)
                .zip(le32(22))
                .map(|(w, h)| (w.unsigned_abs(), h.unsigned_abs())),
            None => None,
        },
        Format::Jpeg => jpeg_size(bytes),
    };
    size.map(|(w, h)| (format, w, h)).ok_or(damaged)
}

/// The size a JPEG's start of frame gives, past the segments before it.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        // markers may be padded with any number of 0xFF
        while *bytes.get(at)? == 0xFF && *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        at += 2;
        match marker {
            // markers that stand alone
            0x01 | 0xD0..=0xD7 => continue,
            // the scan, or the end, before any frame
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        if length < 2 {
            return None;
        }
        // a start of frame: every SOFn but the DHT, JPG and DAC markers among them
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes(bytes.get(at + 3..at + 5)?.try_into().ok()?);
            let width = u16::from_be_bytes(bytes.get(at + 5..at + 7)?.try_into().ok()?);
            return Some((u32::from(width), u32::from(height)));
        }
        at += length;
    }
}

/// As the store keeps it: `{"mime", "data"}`, the data in base64.
#[derive(Serialize, Deserialize)]
struct Stored<'a> {
    mime: &'a str,
    data: &'a str,
}

impl Serialize for Image {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Stored {
            mime: self.mime(),
            data: &self.data,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Owned {
            data: String,
        }
        let o = Owned::deserialize(d)?;
        Image::from_base64(&o.data).map_err(serde::de::Error::custom)
    }
}

/// Images as the session's log and traces keep them: what they are, without their bytes,
/// which the conversation keeps.
pub fn described<S: Serializer>(images: &[Image], s: S) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Described {
        mime: &'static str,
        width: u32,
        height: u32,
        bytes: usize,
    }
    s.collect_seq(images.iter().map(|i| Described {
        mime: i.mime(),
        width: i.width,
        height: i.height,
        bytes: i.size,
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG header of `width`×`height`, with a body that nothing reads.
    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend(width.to_be_bytes());
        b.extend(height.to_be_bytes());
        b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    /// A JPEG with an APP0 segment and a padded marker before its frame.
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        b.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        b.extend([0xFF, 0xFF, 0xC2, 0x00, 0x11, 0x08]);
        b.extend(height.to_be_bytes());
        b.extend(width.to_be_bytes());
        b.extend([0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        b.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn the_header_says_the_format_and_the_size() {
        let i = Image::new(&png(1280, 720)).unwrap();
        assert_eq!(
            (i.format(), i.width(), i.height(), i.mime()),
            (Format::Png, 1280, 720, "image/png")
        );
        let j = Image::new(&jpeg(640, 480)).unwrap();
        assert_eq!(
            (j.format(), j.width(), j.height()),
            (Format::Jpeg, 640, 480)
        );
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x20, 0x03, 0x58, 0x02, 0xF7, 0, 0]);
        let g = Image::new(&gif).unwrap();
        assert_eq!((g.width(), g.height()), (800, 600));
        // a BMP drawn top down has a negative height
        let mut bmp = b"BM".to_vec();
        bmp.extend([0; 12]);
        bmp.extend(40i32.to_le_bytes());
        bmp.extend(300i32.to_le_bytes());
        bmp.extend((-200i32).to_le_bytes());
        let b = Image::new(&bmp).unwrap();
        assert_eq!((b.format(), b.width(), b.height()), (Format::Bmp, 300, 200));
        assert_eq!(i.describe(), "PNG, 1280×720, 33 bytes");
        assert_eq!(format!("{i:?}"), "Image(PNG, 1280×720, 33 bytes)");
        assert_eq!(about_bytes(120_000), "118 KB");
        assert_eq!(about_bytes(2_500_000), "2.4 MB");
    }

    #[test]
    fn a_message_holds_so_many_images_of_so_many_bytes() {
        let one = Image::new(&png(2, 2)).unwrap();
        assert_eq!(too_many(&vec![one.clone(); MAX_PER_MESSAGE]), None);
        assert_eq!(
            too_many(&vec![one; MAX_PER_MESSAGE + 1]).as_deref(),
            Some("9 images: a message may hold at most 8")
        );
        let mut nine = png(10, 10);
        nine.resize(9 * 1024 * 1024, 0);
        let nine = Image::new(&nine).unwrap();
        assert_eq!(
            too_many(&vec![nine; 4]).as_deref(),
            Some("the images have 36.0 MB together, over the 32.0 MB a message may hold")
        );
    }

    #[test]
    fn a_file_is_an_image_by_its_first_bytes() {
        assert!(looks_like_image(&png(1, 1)[..16]));
        assert!(looks_like_image(b"RIFF\0\0\0\0WEBPVP8 "));
        // text that begins as a BMP's signature does
        assert!(!looks_like_image(b"BMW cars are fast."));
        assert!(!looks_like_image(b"GIF8"));
        assert!(!looks_like_image(b""));
    }

    #[test]
    fn what_the_server_cannot_read_is_not_taken() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend([0; 16]);
        assert_eq!(Image::new(&webp), Err(ImageError::WebP));
        assert_eq!(Image::new(b"%PDF-1.7"), Err(ImageError::Format));
        // a PNG cut in its header, and a JPEG whose scan comes before any frame
        assert_eq!(
            Image::new(&png(10, 10)[..20]),
            Err(ImageError::Damaged("PNG"))
        );
        assert_eq!(
            Image::new(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]),
            Err(ImageError::Damaged("JPEG"))
        );
        assert_eq!(
            Image::new(&png(0, 10)),
            Err(ImageError::Size {
                width: 0,
                height: 10
            })
        );
        assert_eq!(
            Image::new(&png(20_000, 10)),
            Err(ImageError::Size {
                width: 20_000,
                height: 10
            })
        );
        let mut big = png(10, 10);
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(Image::new(&big), Err(ImageError::TooLarge(MAX_BYTES + 1)));
        assert_eq!(
            ImageError::TooLarge(MAX_BYTES + 1).to_string(),
            "10.0 MB, over the 10.0 MB an image may have"
        );
    }

    #[test]
    fn base64_goes_both_ways_and_a_data_url_is_taken_too() {
        let i = Image::new(&png(4, 2)).unwrap();
        assert_eq!(Image::from_base64(i.base64()).unwrap(), i);
        assert_eq!(Image::from_base64(&i.data_url()).unwrap(), i);
        assert!(
            i.data_url()
                .starts_with("data:image/png;base64,iVBORw0KGgo")
        );
        assert_eq!(Image::from_base64("not base64!"), Err(ImageError::Base64));
        let stored = serde_json::to_value(&i).unwrap();
        assert_eq!(stored["mime"], "image/png");
        assert_eq!(serde_json::from_value::<Image>(stored).unwrap(), i);
        let shown = serde_json::to_value(Described(vec![i])).unwrap();
        assert_eq!(
            shown,
            serde_json::json!([{"mime": "image/png", "width": 4, "height": 2, "bytes": 33}])
        );
    }

    #[derive(Serialize)]
    struct Described(#[serde(serialize_with = "described")] Vec<Image>);

    #[test]
    fn tokens_go_by_the_pixels_within_the_projectors_limits() {
        let t = |w, h| Image::new(&png(w, h)).unwrap().tokens();
        assert_eq!(t(1280, 720), 900);
        assert_eq!(t(64, 64), 256);
        assert_eq!(t(3840, 2160), 4096);
    }
}
