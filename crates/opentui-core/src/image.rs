//! The image family — `packages/core/src/image.ts` (0.5.14).
//!
//! Observability ruling (same shape as the audio lane note): image
//! transport (kitty APC / sixel DCS bytes) and blocks rasterization
//! are NOT cell-grid observable at this layer — upstream's
//! `OptimizedBuffer.drawImage` (buffer.ts:383-432) delegates to the
//! native `bufferDrawImage` — so this port pins the *contract*:
//! format sniffing, geometry (`ImageInfo`), the error-code surface,
//! the refcount/dispose lifecycle, the pool slot machine, and the
//! `ImageRenderable` geometry/protocol decisions. The compressed
//! pixel decode itself (upstream's stb-image `imageDecode`,
//! zig-structs `NativeImageInfo`) is narrowed: encoded formats get
//! REAL header geometry + format + encoded-byte retention (the kitty
//! `a=t,f=100` passthrough payload), but pixel materialization
//! reports `unsupported-feature` until the native-render lane lands.
//! `raw-rgba` construction (`from_rgba`/`from_pixels`) is fully real
//! — pixels, validation, bgra swap, opaque-alpha forcing — and so
//! are the materialized-pixel ops (resize/extract/extend/transform/
//! composite). Pinned by `image_tests.rs` (ports of
//! tests/image.test.ts, tests/image-pool.test.ts, and
//! tests/image-renderable.test.ts key cases).
//!
//! Upstream line citations throughout refer to image.ts.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

/// `ImageFormat` (image.ts:7). `raw-rgba` only exists as a
/// construction format (`from_rgba`/`from_pixels`); encoded sniffing
/// covers png/jpeg/webp/gif.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    RawRgba,
    Jpeg,
    Webp,
    Gif,
}

impl ImageFormat {
    /// The wire name (upstream's string union).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::RawRgba => "raw-rgba",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
        }
    }
}

/// `ImageColorStatus` (image.ts:8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageColorStatus {
    AssumedSrgb,
    ExplicitSrgb,
}

/// `ImageErrorCode` (image.ts:141-153).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageErrorCode {
    InvalidHandle,
    UnsupportedFormat,
    UnsupportedColorSpace,
    MalformedData,
    DimensionLimit,
    MemoryLimit,
    InvalidArgument,
    OutOfMemory,
    OutputTooSmall,
    InternalError,
    UnsupportedFeature,
    Busy,
}

/// `STATUS_MESSAGES` (image.ts:125-139).
const STATUS_MESSAGES: [&str; 13] = [
    "ok",
    "invalid image handle",
    "unsupported image format",
    "unsupported image color space",
    "malformed image data",
    "image dimensions exceed limits",
    "image memory limit exceeded",
    "invalid image argument",
    "out of memory",
    "image output buffer is too small",
    "internal image error",
    "unsupported image feature",
    "image is retained",
];

/// `STATUS_CODES` (image.ts:155-169) — status → code.
const STATUS_CODES: [ImageErrorCode; 13] = [
    ImageErrorCode::InternalError,         // 0
    ImageErrorCode::InvalidHandle,         // 1
    ImageErrorCode::UnsupportedFormat,     // 2
    ImageErrorCode::UnsupportedColorSpace, // 3
    ImageErrorCode::MalformedData,         // 4
    ImageErrorCode::DimensionLimit,        // 5
    ImageErrorCode::MemoryLimit,           // 6
    ImageErrorCode::InvalidArgument,       // 7
    ImageErrorCode::OutOfMemory,           // 8
    ImageErrorCode::OutputTooSmall,        // 9
    ImageErrorCode::InternalError,         // 10
    ImageErrorCode::UnsupportedFeature,    // 11
    ImageErrorCode::Busy,                  // 12
];

/// The native status behind [`ImageErrorCode::MemoryLimit`]
/// (`MAX_ENCODED_BYTES` overflow paths, image.ts:208/281/297).
const MEMORY_LIMIT_STATUS: u8 = 6;
/// The native status behind [`ImageErrorCode::UnsupportedFormat`].
const UNSUPPORTED_FORMAT_STATUS: u8 = 2;
/// The native status behind [`ImageErrorCode::MalformedData`].
const MALFORMED_DATA_STATUS: u8 = 4;
/// The native status behind [`ImageErrorCode::UnsupportedFeature`]
/// (the deferred-decode narrowing of this port).
const UNSUPPORTED_FEATURE_STATUS: u8 = 11;
const BUSY_STATUS: u8 = 12;

/// The unified error surface: upstream's `TypeError` validation
/// throws plus the native-status `ImageError` (image.ts:173-183),
/// and the plain `Error("NativeImage is disposed")` guard
/// (image.ts:491).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// TypeError paths — e.g. empty data (image.ts:377/396), wrong
    /// pixel-array type (image.ts:453), unsupported option values.
    Invalid(String),
    /// The native status payload (image.ts:177-182): message is
    /// `Native image operation failed: {STATUS_MESSAGES[status]}`.
    Native { status: u8, code: ImageErrorCode },
    /// The disposed guard (image.ts:490-492).
    Disposed,
    /// `takeRaw`'s pinned-by-native-buffers refusal
    /// (image.ts:623-628).
    RetainedByBuffers,
}

impl ImageError {
    /// Test seam for the STATUS tables (image_tests.rs).
    #[cfg(test)]
    pub(crate) fn native_for_test(status: u8) -> Self {
        Self::native(status)
    }

    fn native(status: u8) -> Self {
        let code = STATUS_CODES
            .get(status as usize)
            .copied()
            .unwrap_or(ImageErrorCode::InternalError);
        ImageError::Native { status, code }
    }

    /// The message `ImageError`'s constructor builds
    /// (image.ts:178) — `STATUS_MESSAGES[status]`, unknown status
    /// falls back to `unknown status N`.
    #[must_use]
    pub fn native_message(&self) -> String {
        match self {
            ImageError::Invalid(m) => m.clone(),
            ImageError::Disposed => "NativeImage is disposed".to_owned(),
            ImageError::RetainedByBuffers => {
                "Cannot transfer image pixels while native buffers retain the image".to_owned()
            }
            ImageError::Native { status, .. } => match STATUS_MESSAGES.get(*status as usize) {
                Some(m) => format!("Native image operation failed: {m}"),
                None => format!("Native image operation failed: unknown status {status}"),
            },
        }
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.native_message())
    }
}

impl Error for ImageError {}

/// `ImageLoadErrorCode` (image.ts:14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLoadErrorCode {
    FileRead,
    Network,
    HttpStatusCode,
    UnsupportedUrlScheme,
}

impl ImageLoadErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ImageLoadErrorCode::FileRead => "file-read",
            ImageLoadErrorCode::Network => "network",
            ImageLoadErrorCode::HttpStatusCode => "http-status",
            ImageLoadErrorCode::UnsupportedUrlScheme => "unsupported-url-scheme",
        }
    }
}

/// `ImageLoadError` (image.ts:16-33) — code + source + optional
/// HTTP status + cause.
#[derive(Debug)]
pub struct ImageLoadError {
    pub code: ImageLoadErrorCode,
    pub source: String,
    pub status: Option<u16>,
    pub cause: Option<Box<dyn Error>>,
}

impl ImageLoadError {
    fn new(
        code: ImageLoadErrorCode,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ImageLoadError {
            code,
            source: source.into(),
            status: None,
            cause: Some(Box::new(LoadMessage(message.into()))),
        }
    }

    /// The message shape of each constructor site
    /// (image.ts:360/371/430/439/447).
    #[must_use]
    pub fn message(&self) -> String {
        self.cause
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| format!("Failed to load image: {}", self.source))
    }
}

#[derive(Debug)]
struct LoadMessage(String);

impl fmt::Display for LoadMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for LoadMessage {}

impl fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl Error for ImageLoadError {}

/// `ImageInfo` (image.ts:40-49).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub format: ImageFormat,
    pub color_status: ImageColorStatus,
    pub orientation: u16,
    pub has_alpha: bool,
}

/// `ResizeKernel` (image.ts:9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeKernel {
    #[default]
    Default,
    Area,
    Triangle,
    CubicBspline,
    CatmullRom,
    Mitchell,
    Nearest,
}

/// `BlendMode` (image.ts:10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    SourceOver,
    Source,
    DestinationOver,
}

/// `PixelFormat` (image.ts:11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelFormat {
    #[default]
    Rgba8,
    Bgra8,
}

/// `MAX_ENCODED_BYTES` (image.ts:208).
pub const MAX_ENCODED_BYTES: u64 = 64 * 1024 * 1024;

/// `imageInfo(data)` (image.ts:375-381): sniff the encoded format and
/// its geometry. Empty data is upstream's TypeError (image.ts:377);
/// unrecognized/truncated data maps to the native statuses
/// (`unsupported-format` 2 / `malformed-data` 4).
pub fn image_info(data: &[u8]) -> Result<ImageInfo, ImageError> {
    if data.is_empty() {
        return Err(ImageError::Invalid(
            "image data must not be empty".to_owned(),
        ));
    }
    sniff(data)
}

/// The header sniffer — the geometry half of native `imageInfo`
/// (zig-structs `NativeImageInfo`): real dimensions/format/alpha for
/// PNG/GIF/JPEG/WebP. Narrowed: ICC/cICP color-status policy is
/// `assumed-srgb` (upstream's explicit-srgb classification rides the
/// deferred decode lane), and JPEG EXIF orientation stays 1.
fn sniff(data: &[u8]) -> Result<ImageInfo, ImageError> {
    if data.len() >= 8 && data[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        // IHDR must be the first chunk: 8 signature + 4 len + 4 type +
        // 13 data bytes (PNG spec 11.2.2).
        if data.len() < 29 || &data[12..16] != b"IHDR" {
            return Err(ImageError::native(MALFORMED_DATA_STATUS));
        }
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        let color_type = data[25];
        if width == 0 || height == 0 {
            return Err(ImageError::native(MALFORMED_DATA_STATUS));
        }
        let has_alpha = color_type == 4 || color_type == 6;
        return Ok(ImageInfo {
            width,
            height,
            source_width: width,
            source_height: height,
            format: ImageFormat::Png,
            color_status: ImageColorStatus::AssumedSrgb,
            orientation: 1,
            has_alpha,
        });
    }
    if data.len() >= 13 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        if width == 0 || height == 0 {
            return Err(ImageError::native(MALFORMED_DATA_STATUS));
        }
        return Ok(ImageInfo {
            width,
            height,
            source_width: width,
            source_height: height,
            format: ImageFormat::Gif,
            color_status: ImageColorStatus::AssumedSrgb,
            orientation: 1,
            // transparent logical-screen backgrounds stay transparent
            // (image.test.ts:496) — the family default is alpha-capable
            has_alpha: true,
        });
    }
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
        // SOF segment scan (JPEG spec: any C0-CF except C4/C8/CC).
        let mut i = 2usize;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC
            {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                if width == 0 || height == 0 {
                    return Err(ImageError::native(MALFORMED_DATA_STATUS));
                }
                return Ok(ImageInfo {
                    width,
                    height,
                    source_width: width,
                    source_height: height,
                    format: ImageFormat::Jpeg,
                    color_status: ImageColorStatus::AssumedSrgb,
                    orientation: 1, // EXIF scan deferred (narrowing)
                    has_alpha: false,
                });
            }
            if i + 4 > data.len() {
                break;
            }
            let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            i += 2 + len;
        }
        return Err(ImageError::native(MALFORMED_DATA_STATUS));
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return sniff_webp(data);
    }
    Err(ImageError::native(UNSUPPORTED_FORMAT_STATUS))
}

fn sniff_webp(data: &[u8]) -> Result<ImageInfo, ImageError> {
    let mut i = 12usize;
    while i + 8 <= data.len() {
        let tag = &data[i..i + 4];
        let size =
            data[i + 4] as usize | ((data[i + 5] as usize) << 8) | ((data[i + 6] as usize) << 16);
        let body = match data.get(i + 8..i + 8 + size.max(1)) {
            Some(b) => b,
            None => return Err(ImageError::native(MALFORMED_DATA_STATUS)),
        };
        let info = |width, height, has_alpha| {
            Ok(ImageInfo {
                width,
                height,
                source_width: width,
                source_height: height,
                format: ImageFormat::Webp,
                color_status: ImageColorStatus::AssumedSrgb,
                orientation: 1,
                has_alpha,
            })
        };
        if tag == b"VP8X" {
            if body.len() < 10 {
                return Err(ImageError::native(MALFORMED_DATA_STATUS));
            }
            // ANIM flag (bit 1 of the flags byte) — animated WebP is
            // rejected (image.test.ts:457).
            if body[0] & 0x02 != 0 {
                return Err(ImageError::native(UNSUPPORTED_FORMAT_STATUS));
            }
            let w = 1 + (body[4] as u32 | (body[5] as u32) << 8 | (body[6] as u32) << 16);
            let h = 1 + (body[7] as u32 | (body[8] as u32) << 8 | (body[9] as u32) << 16);
            return info(w, h, body[0] & 0x10 != 0);
        }
        if tag == b"VP8 " {
            // lossy: 3-byte frame tag, 3-byte start code 0x9D012A,
            // then 14-bit dims (WebP container spec).
            if body.len() < 10 {
                return Err(ImageError::native(MALFORMED_DATA_STATUS));
            }
            let w = (u16::from_le_bytes([body[6], body[7]]) & 0x3FFF) as u32;
            let h = (u16::from_le_bytes([body[8], body[9]]) & 0x3FFF) as u32;
            return info(w, h, false);
        }
        if tag == b"VP8L" {
            // lossless: signature 0x2F then 14-bit minus-one dims in a
            // 32-bit LE bitstream.
            if body.len() < 5 || body[0] != 0x2F {
                return Err(ImageError::native(MALFORMED_DATA_STATUS));
            }
            let bits = u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
            let w = 1 + (bits & 0x3FFF);
            let h = 1 + ((bits >> 14) & 0x3FFF);
            return info(w, h, true);
        }
        i += 8 + size + (size & 1); // chunks are padded to even
    }
    Err(ImageError::native(MALFORMED_DATA_STATUS))
}

/// The materialized pixel store — what upstream's native handle
/// owns after decode/materialize. `stride` is bytes per row.
#[derive(Debug, Clone)]
pub(crate) struct Pixels {
    data: Vec<u8>,
    stride: usize,
}

/// The shared handle body: geometry always, pixels when
/// materialized, the retained encoded payload when constructed from
/// encoded bytes (the kitty `a=t,f=100` passthrough source,
/// image-renderable.test.ts:476-507).
struct ImageInner {
    info: ImageInfo,
    pixels: Option<Pixels>,
    encoded: Option<Vec<u8>>,
}

/// `NativeImage` (image.ts:383-651). A cheap wrapper over a shared
/// [`Rc`] body: `retain`/`clone` add references (upstream's native
/// refcount), `dispose` drops THIS reference — the body frees when
/// the last wrapper goes away (upstream leaks instead if you never
/// dispose; Rust's RAII close is documented behavior, not a
/// divergence in any observable).
#[derive(Clone)]
pub struct NativeImage {
    inner: Option<Rc<ImageInner>>,
    disposed: bool,
}

impl fmt::Debug for NativeImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeImage")
            .field("disposed", &self.disposed)
            .field("info", &self.inner.as_ref().map(|i| &i.info))
            .finish_non_exhaustive()
    }
}

/// `RawImage` (image.ts:79-87).
pub struct RawImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
}

/// `OwnedRawImage` (image.ts:96-123) — `take_raw`'s transfer result;
/// `dispose` is the port's no-op free (Vec's Drop).
pub struct OwnedRawImage {
    image: RawImage,
}

impl OwnedRawImage {
    pub fn dispose(self) {}
}

impl std::ops::Deref for OwnedRawImage {
    type Target = RawImage;
    fn deref(&self) -> &RawImage {
        &self.image
    }
}

/// `PixelImportOptions` (image.ts:89-94).
#[derive(Debug, Clone, Copy, Default)]
pub struct PixelImportOptions {
    pub stride: Option<u32>,
    pub format: Option<PixelFormat>,
    pub opaque_alpha: bool,
}

/// `ResizeOptions` (image.ts:51-55).
#[derive(Debug, Clone, Copy, Default)]
pub struct ResizeOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kernel: Option<ResizeKernel>,
}

/// `ExtractOptions` (image.ts:57-62).
#[derive(Debug, Clone, Copy)]
pub struct ExtractOptions {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// `ExtendOptions` (image.ts:64-70).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtendOptions {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
    /// RGBA channels (image.ts:69), default `[0,0,0,0]`
    /// (image.ts:557).
    pub background: Option<[u8; 4]>,
}

/// `CompositeOptions` (image.ts:72-77).
#[derive(Debug, Clone, Copy, Default)]
pub struct CompositeOptions {
    pub left: i32,
    pub top: i32,
    pub blend: Option<BlendMode>,
    pub opacity: Option<f64>,
}

/// `ImageSource` for [`NativeImage::load`] (image.ts:12, 404-450):
/// encoded bytes or a filesystem path. `http(s)`/`blob:` fetch and
/// `Response`/`Blob` streaming stay with the network lane
/// (registered narrowing — they surface as `unsupported-url-scheme`
/// errors, the same code upstream gives non-http schemes);
/// `data:image/...;base64` URLs are real (image.test.ts:895).
pub enum ImageSource<'a> {
    Bytes(&'a [u8]),
    Path(&'a Path),
    Url(&'a str),
}

fn check_u32(value: u32, name: &str, allow_zero: bool) -> Result<u32, ImageError> {
    if !allow_zero && value == 0 {
        return Err(ImageError::Invalid(format!(
            "{name} must be a positive u32 integer"
        )));
    }
    Ok(value)
}

impl NativeImage {
    /// `decode(data)` (image.ts:394-402) — sniff + retain. The
    /// compressed pixel decode is the registered narrowing: geometry
    /// is real; `raw()` on an encoded-only image reports
    /// `unsupported-feature` (upstream defers decode too —
    /// image.test.ts:713).
    pub fn decode(data: &[u8]) -> Result<Self, ImageError> {
        if data.is_empty() {
            return Err(ImageError::Invalid(
                "image data must not be empty".to_owned(),
            ));
        }
        if data.len() as u64 > MAX_ENCODED_BYTES {
            return Err(ImageError::native(MEMORY_LIMIT_STATUS));
        }
        let info = sniff(data)?;
        Ok(NativeImage {
            inner: Some(Rc::new(ImageInner {
                info,
                pixels: None,
                encoded: Some(data.to_vec()),
            })),
            disposed: false,
        })
    }

    /// `load(source)` (image.ts:404-450) — the sync port: bytes and
    /// filesystem paths load for real; `data:` URLs decode; network
    /// schemes report `unsupported-url-scheme` (the registered
    /// network-fetch narrowing).
    pub fn load(source: ImageSource<'_>) -> Result<Self, ImageLoadError> {
        match source {
            ImageSource::Bytes(bytes) => NativeImage::decode(bytes).map_err(|e| ImageLoadError {
                code: ImageLoadErrorCode::FileRead,
                source: "<bytes>".to_owned(),
                status: None,
                cause: Some(Box::new(e)),
            }),
            ImageSource::Path(path) => NativeImage::load_path(path),
            ImageSource::Url(url) => NativeImage::load_url(url),
        }
    }

    fn load_path(path: &Path) -> Result<Self, ImageLoadError> {
        let source = path.display().to_string();
        let failure = |cause: String| ImageLoadError {
            code: ImageLoadErrorCode::FileRead,
            source: source.clone(),
            status: None,
            cause: Some(Box::new(LoadMessage(cause))),
        };
        let meta = std::fs::metadata(path).map_err(|e| failure(e.to_string()))?;
        // oversized local files are rejected before reading
        // (image.ts:323, image.test.ts:1015)
        if meta.len() as u64 > MAX_ENCODED_BYTES {
            return Err(ImageLoadError {
                code: ImageLoadErrorCode::FileRead,
                source,
                status: None,
                cause: Some(Box::new(ImageError::native(MEMORY_LIMIT_STATUS))),
            });
        }
        let data = std::fs::read(path).map_err(|e| failure(e.to_string()))?;
        NativeImage::decode(&data).map_err(|e| ImageLoadError {
            code: ImageLoadErrorCode::FileRead,
            source,
            status: None,
            cause: Some(Box::new(e)),
        })
    }

    fn load_url(url: &str) -> Result<Self, ImageLoadError> {
        let scheme_fail = |scheme: &str| {
            ImageLoadError::new(
                ImageLoadErrorCode::UnsupportedUrlScheme,
                url,
                format!("Unsupported image URL scheme: {scheme}"),
            )
        };
        let Some((scheme, rest)) = url.split_once(':') else {
            return Err(scheme_fail("(none)"));
        };
        let scheme_l = scheme.to_ascii_lowercase();
        if scheme_l == "data" {
            // data:[mediatype][;base64],payload (RFC 2397). Only the
            // base64 arm is real — the percent-encoded arm stays with
            // the network lane.
            let (meta, payload) = rest.split_once(',').ok_or_else(|| {
                ImageLoadError::new(ImageLoadErrorCode::Network, url, "Malformed data URL")
            })?;
            if !meta.to_ascii_lowercase().ends_with(";base64") {
                return Err(scheme_fail("data"));
            }
            let bytes = base64_decode(payload.as_bytes()).ok_or_else(|| {
                ImageLoadError::new(
                    ImageLoadErrorCode::Network,
                    url,
                    "Failed to read image response: invalid base64",
                )
            })?;
            return NativeImage::decode(&bytes).map_err(|e| ImageLoadError {
                code: ImageLoadErrorCode::Network,
                source: url.to_owned(),
                status: None,
                cause: Some(Box::new(e)),
            });
        }
        // http/https/blob would fetch (image.ts:444) — registered
        // network narrowing; file: falls to the path arm upstream,
        // keep that real via the caller using ImageSource::Path.
        Err(scheme_fail(&scheme_l))
    }

    /// `fromRgba` (image.ts:452-462).
    pub fn from_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Self, ImageError> {
        Self::from_pixels(pixels, width, height, &PixelImportOptions::default())
    }

    /// `fromPixels` (image.ts:464-479) — real import: stride/bgra
    /// validation, straight or forced-opaque alpha
    /// (image.test.ts:544-633).
    pub fn from_pixels(
        pixels: &[u8],
        width: u32,
        height: u32,
        options: &PixelImportOptions,
    ) -> Result<Self, ImageError> {
        check_u32(width, "width", false)?;
        check_u32(height, "height", false)?;
        let stride = match options.stride {
            Some(s) => check_u32(s, "stride", false)? as usize,
            None => width as usize * 4,
        };
        let format = options.format.unwrap_or(PixelFormat::Rgba8);
        let required = stride
            .checked_mul(height as usize)
            .ok_or_else(|| ImageError::native(MEMORY_LIMIT_STATUS))?;
        if pixels.len() < required {
            return Err(ImageError::Invalid(format!(
                "pixels must cover {required} bytes (stride {stride} x height {height})"
            )));
        }
        let mut data = vec![0u8; width as usize * 4 * height as usize];
        let dst_stride = width as usize * 4;
        let mut has_alpha = false;
        for row in 0..height as usize {
            for col in 0..width as usize {
                let src = row * stride + col * 4;
                let dst = row * dst_stride + col * 4;
                let px = [
                    pixels[src],
                    pixels[src + 1],
                    pixels[src + 2],
                    pixels[src + 3],
                ];
                let px = if options.opaque_alpha {
                    [px[0], px[1], px[2], 255]
                } else {
                    if px[3] != 0xFF {
                        has_alpha = true;
                    }
                    px
                };
                // bgra8 → rgba8 (image.ts:201-204 + the FFI's swap)
                let px = match format {
                    PixelFormat::Rgba8 => px,
                    PixelFormat::Bgra8 => [px[2], px[1], px[0], px[3]],
                };
                data[dst..dst + 4].copy_from_slice(&px);
            }
        }
        Ok(Self::materialized(
            width,
            height,
            has_alpha,
            Pixels {
                data,
                stride: dst_stride,
            },
        ))
    }

    fn materialized(width: u32, height: u32, has_alpha: bool, pixels: Pixels) -> Self {
        NativeImage {
            inner: Some(Rc::new(ImageInner {
                info: ImageInfo {
                    width,
                    height,
                    source_width: width,
                    source_height: height,
                    format: ImageFormat::RawRgba,
                    color_status: ImageColorStatus::AssumedSrgb,
                    orientation: 1,
                    has_alpha,
                },
                pixels: Some(pixels),
                encoded: None,
            })),
            disposed: false,
        }
    }

    fn guard(&self) -> Result<&ImageInner, ImageError> {
        if self.disposed || self.inner.is_none() {
            return Err(ImageError::Disposed);
        }
        Ok(self.inner.as_ref().expect("checked above"))
    }

    /// `ptr` (image.ts:495) — the guard's Result shape in Rust.
    pub(crate) fn ptr_eq(&self, other: &NativeImage) -> bool {
        match (&self.inner, &other.inner) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn pixels_of(&self) -> Result<&Pixels, ImageError> {
        self.guard()?
            .pixels
            .as_ref()
            .ok_or_else(|| ImageError::native(UNSUPPORTED_FEATURE_STATUS))
    }

    /// `info()` (image.ts:505-508).
    pub fn info(&self) -> Result<ImageInfo, ImageError> {
        Ok(self.guard()?.info)
    }

    pub fn width(&self) -> Result<u32, ImageError> {
        Ok(self.guard()?.info.width)
    }

    pub fn height(&self) -> Result<u32, ImageError> {
        Ok(self.guard()?.info.height)
    }

    /// The retained encoded payload (`ensureEncodedPng`'s source,
    /// image.ts:602-604): the original bytes for encoded images —
    /// the kitty `a=t,f=100` passthrough (image-renderable.test.ts:
    /// 476-507 keeps a corrupt-IDAT PNG byte-identical).
    pub fn encoded(&self) -> Result<&[u8], ImageError> {
        Ok(self.guard()?.encoded.as_deref().unwrap_or(&[]))
    }

    /// `retain()` (image.ts:524-526) — a new reference to the same
    /// body (the native refcount is the Rc count).
    pub fn retain(&self) -> Result<Self, ImageError> {
        self.guard()?;
        Ok(NativeImage {
            inner: self.inner.clone(),
            disposed: false,
        })
    }

    /// Whether another wrapper still holds this body (the pool's
    /// BUSY probe — `imageUpdatePixels` status 12, image.ts:685-686).
    pub fn is_retained(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| Rc::strong_count(inner) > 1)
    }

    /// `dispose()` (image.ts:646-650) — drops this reference;
    /// idempotent, later ops report [`ImageError::Disposed`]
    /// (image.test.ts:864).
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.inner = None;
    }

    /// `resize(options)` (image.ts:528-542) — one-sided sizes derive
    /// the other by aspect (image.test.ts:796), derived dimensions
    /// are validated before use (image.test.ts:807).
    pub fn resize(&self, options: &ResizeOptions) -> Result<Self, ImageError> {
        if options.width.is_none() && options.height.is_none() {
            return Err(ImageError::Invalid(
                "resize requires width, height, or both".to_owned(),
            ));
        }
        let (sw, sh) = (self.width()?, self.height()?);
        let width = options
            .width
            .map(|w| check_u32(w, "width", false))
            .transpose()?;
        let height = options
            .height
            .map(|h| check_u32(h, "height", false))
            .transpose()?;
        let width = match width {
            Some(w) => w,
            None => {
                let h = height.expect("one of the two is set");
                derive_dimension(sw, sh, h)?
            }
        };
        let height = match height {
            Some(h) => h,
            None => derive_dimension(sh, sw, width)?,
        };
        check_u32(width, "width", false)?;
        check_u32(height, "height", false)?;
        let kernel = options.kernel.unwrap_or(ResizeKernel::Area);
        let src = self.pixels_of()?;
        let data = resample(src, sw, sh, width, height, kernel);
        let has_alpha = scan_alpha(&data);
        Ok(Self::materialized(
            width,
            height,
            has_alpha,
            Pixels {
                data,
                stride: width as usize * 4,
            },
        ))
    }

    /// `extract(options)` (image.ts:544-554).
    pub fn extract(&self, options: &ExtractOptions) -> Result<Self, ImageError> {
        check_u32(options.left, "left", true)?;
        check_u32(options.top, "top", true)?;
        check_u32(options.width, "width", false)?;
        check_u32(options.height, "height", false)?;
        let (sw, sh) = (self.width()? as u64, self.height()? as u64);
        if options.left as u64 + options.width as u64 > sw
            || options.top as u64 + options.height as u64 > sh
        {
            return Err(ImageError::native(MALFORMED_DATA_STATUS));
        }
        let src = self.pixels_of()?;
        let stride = src.stride;
        let mut data = Vec::with_capacity(options.width as usize * options.height as usize * 4);
        for row in 0..options.height as usize {
            let y = options.top as usize + row;
            let start = y * stride + options.left as usize * 4;
            data.extend_from_slice(&src.data[start..start + options.width as usize * 4]);
        }
        let has_alpha = scan_alpha(&data); // image.test.ts:772
        Ok(Self::materialized(
            options.width,
            options.height,
            has_alpha,
            Pixels {
                data,
                stride: options.width as usize * 4,
            },
        ))
    }

    /// `extend(options)` (image.ts:556-570).
    pub fn extend(&self, options: &ExtendOptions) -> Result<Self, ImageError> {
        let bg = options.background.unwrap_or([0, 0, 0, 0]);
        let (w, h) = (self.width()?, self.height()?);
        let nw = w
            .checked_add(options.left)
            .and_then(|v| v.checked_add(options.right))
            .ok_or_else(|| ImageError::native(5))?; // dimension-limit
        let nh = h
            .checked_add(options.top)
            .and_then(|v| v.checked_add(options.bottom))
            .ok_or_else(|| ImageError::native(5))?;
        let src = self.pixels_of()?;
        let nstride = nw as usize * 4;
        let mut data = vec![0u8; nstride * nh as usize];
        // background fill (only where the source doesn't land)
        let (pxs, _) = data.as_chunks_mut::<4>();
        for px in pxs {
            px.copy_from_slice(&bg);
        }
        for row in 0..h as usize {
            let dst = (options.top as usize + row) * nstride + options.left as usize * 4;
            data[dst..dst + w as usize * 4]
                .copy_from_slice(&src.data[row * src.stride..row * src.stride + w as usize * 4]);
        }
        let has_alpha = bg[3] != 0xFF || scan_alpha(&data); // image.test.ts:784
        Ok(Self::materialized(
            nw,
            nh,
            has_alpha,
            Pixels {
                data,
                stride: nstride,
            },
        ))
    }

    /// `rotate(angle)` (image.ts:572-576).
    pub fn rotate(&self, angle: u16) -> Result<Self, ImageError> {
        let op = match angle {
            90 => 0,
            180 => 1,
            270 => 2,
            _ => {
                return Err(ImageError::Invalid(
                    "angle must be 90, 180, or 270".to_owned(),
                ))
            }
        };
        self.transform(op)
    }

    /// `flip()` (image.ts:578-580) — vertical.
    pub fn flip(&self) -> Result<Self, ImageError> {
        self.transform(3)
    }

    /// `flop()` (image.ts:582-584) — horizontal.
    pub fn flop(&self) -> Result<Self, ImageError> {
        self.transform(4)
    }

    fn transform(&self, operation: i32) -> Result<Self, ImageError> {
        let src = self.pixels_of()?;
        let (w, h) = (self.width()? as usize, self.height()? as usize);
        let (nw, nh) = if operation == 0 || operation == 2 {
            (h, w)
        } else {
            (w, h)
        };
        let nstride = nw * 4;
        let mut data = vec![0u8; nstride * nh];
        for y in 0..h {
            for x in 0..w {
                let s = y * src.stride + x * 4;
                let (dx, dy) = match operation {
                    0 => (h - 1 - y, x),         // 90° CW
                    1 => (w - 1 - x, h - 1 - y), // 180°
                    2 => (y, w - 1 - x),         // 270°
                    3 => (x, h - 1 - y),         // flip vertical
                    _ => (w - 1 - x, y),         // flop horizontal
                };
                let d = dy * nstride + dx * 4;
                data[d..d + 4].copy_from_slice(&src.data[s..s + 4]);
            }
        }
        let has_alpha = scan_alpha(&data);
        Ok(Self::materialized(
            nw as u32,
            nh as u32,
            has_alpha,
            Pixels {
                data,
                stride: nstride,
            },
        ))
    }

    /// `composite(overlay, options)` (image.ts:586-600) — linear
    /// light blending (image.test.ts:816) with straight-alpha
    /// source-over / source / destination-over modes.
    pub fn composite(
        &self,
        overlay: &NativeImage,
        options: &CompositeOptions,
    ) -> Result<Self, ImageError> {
        let opacity = options.opacity.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&opacity) || opacity.is_nan() {
            return Err(ImageError::Invalid(
                "opacity must be between 0 and 1".to_owned(),
            ));
        }
        let opacity = opacity.clamp(0.0, 1.0) as f32;
        let blend = options.blend.unwrap_or_default();
        let (w, h) = (self.width()?, self.height()?);
        let (ow, oh) = (overlay.width()?, overlay.height()?);
        let dst = self.pixels_of()?;
        let src = overlay.pixels_of()?;
        let stride = w as usize * 4;
        let mut data = dst.data.clone();
        for sy in 0..oh as i64 {
            let dy = sy + i64::from(options.top);
            if dy < 0 || dy >= h as i64 {
                continue;
            }
            for sx in 0..ow as i64 {
                let dx = sx + i64::from(options.left);
                if dx < 0 || dx >= w as i64 {
                    continue;
                }
                let s = sy as usize * src.stride + sx as usize * 4;
                let d = dy as usize * stride + dx as usize * 4;
                let out = blend_px(&data[d..d + 4], &src.data[s..s + 4], blend, opacity);
                data[d..d + 4].copy_from_slice(&out);
            }
        }
        let has_alpha = scan_alpha(&data); // image.test.ts:832
        Ok(Self::materialized(w, h, has_alpha, Pixels { data, stride }))
    }

    /// `ensureEncodedPng()` (image.ts:602-604): encoded sources keep
    /// their original payload (real); raw sources would need a PNG
    /// ENCODER — registered narrowing reports
    /// `unsupported-feature` (upstream encodes lazily).
    pub fn ensure_encoded_png(&self) -> Result<(), ImageError> {
        let inner = self.guard()?;
        if inner.encoded.is_some() {
            return Ok(());
        }
        Err(ImageError::native(UNSUPPORTED_FEATURE_STATUS))
    }

    /// `raw(format)` (image.ts:606-618).
    pub fn raw(&self, format: PixelFormat) -> Result<RawImage, ImageError> {
        let (w, h) = (self.width()?, self.height()?);
        let src = self.pixels_of()?;
        let stride = w as usize * 4;
        let mut data = src.data[..stride * h as usize].to_vec();
        if format == PixelFormat::Bgra8 {
            let (pxs, _) = data.as_chunks_mut::<4>();
            for px in pxs {
                px.swap(0, 2);
            }
        }
        Ok(RawImage {
            data,
            width: w,
            height: h,
            stride,
            format,
        })
    }

    /// `takeRaw()` (image.ts:620-636) — transfers the pixels out.
    /// Refused while other wrappers (or the port's future native
    /// buffers) still retain the body (image.ts:623-628);
    /// encoded-only images report the deferred-decode failure shape.
    pub fn take_raw(&mut self) -> Result<OwnedRawImage, ImageError> {
        self.guard()?;
        if self.is_retained() {
            return Err(ImageError::RetainedByBuffers);
        }
        let raw = self.raw(PixelFormat::Rgba8)?;
        // the wrapper loses its handle (image.ts:634)
        self.disposed = true;
        self.inner = None;
        Ok(OwnedRawImage { image: raw })
    }

    /// `copyTo(destination, options)` (image.ts:638-644).
    pub fn copy_to(
        &self,
        destination: &mut [u8],
        stride: u32,
        format: PixelFormat,
    ) -> Result<(), ImageError> {
        let (w, h) = (self.width()? as usize, self.height()? as usize);
        let src = self.pixels_of()?;
        let stride = stride as usize;
        if destination.len() < stride.saturating_mul(h) {
            return Err(ImageError::Invalid(
                "destination must cover stride x height bytes".to_owned(),
            ));
        }
        for row in 0..h {
            let s = row * src.stride;
            let d = row * stride;
            for x in 0..w {
                let mut px = [
                    src.data[s + x * 4],
                    src.data[s + x * 4 + 1],
                    src.data[s + x * 4 + 2],
                    src.data[s + x * 4 + 3],
                ];
                if format == PixelFormat::Bgra8 {
                    px.swap(0, 2);
                }
                destination[d + x * 4..d + x * 4 + 4].copy_from_slice(&px);
            }
        }
        Ok(())
    }

    /// In-place pixel refresh — the pool's `imageUpdatePixels`
    /// (image.ts:685). Only valid on a non-retained body.
    pub(crate) fn update_pixels(
        &mut self,
        pixels: &Pixels,
        opaque_alpha: bool,
    ) -> Result<(), ImageError> {
        if self.disposed {
            return Err(ImageError::Disposed);
        }
        let inner = self
            .inner
            .as_mut()
            .and_then(Rc::get_mut)
            .ok_or_else(|| ImageError::native(BUSY_STATUS))?;
        let slot = inner
            .pixels
            .as_mut()
            .ok_or_else(|| ImageError::native(UNSUPPORTED_FEATURE_STATUS))?;
        if pixels.data.len() != slot.data.len() {
            return Err(ImageError::native(MEMORY_LIMIT_STATUS));
        }
        slot.data.copy_from_slice(&pixels.data);
        if opaque_alpha {
            let (pxs, _) = slot.data.as_chunks_mut::<4>();
            for px in pxs {
                px[3] = 0xFF;
            }
            // opaque forcing keeps has-alpha only if any channel stays
            // transparent — it cannot: alpha is 255 everywhere
        } else {
            inner.info.has_alpha = scan_alpha(&slot.data); // image-pool.test.ts:162
        }
        if opaque_alpha {
            inner.info.has_alpha = false;
        }
        Ok(())
    }

    /// Build a pool slot image sharing nothing (from_pixels with the
    /// pool's fixed geometry).
    pub(crate) fn from_pool_pixels(pixels: &Pixels, w: u32, h: u32) -> Result<Self, ImageError> {
        Self::from_pixels(
            &pixels.data,
            w,
            h,
            &PixelImportOptions {
                stride: Some(pixels.stride as u32),
                ..Default::default()
            },
        )
    }
}

fn derive_dimension(from: u32, to: u32, value: u32) -> Result<u32, ImageError> {
    // Math.max(1, Math.round((from * value) / to)) — image.ts:536-537
    let v = ((from as u64) * (value as u64)).div_ceil(to as u64);
    let v = v.max(1);
    u32::try_from(v).map_err(|_| {
        ImageError::Invalid("derived dimension must be a positive u32 integer".to_owned())
    })
}

fn scan_alpha(data: &[u8]) -> bool {
    data.as_chunks::<4>().0.iter().any(|px| px[3] != 0xFF)
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Linear-light blend (image.test.ts:816 "composites in linear
/// light"): straight-alpha compositing over the linearized channels.
fn blend_px(dst: &[u8], src: &[u8], mode: BlendMode, opacity: f32) -> [u8; 4] {
    let sa = srgb_to_linear(src[3]) * opacity;
    let (sc, da) = (
        [
            srgb_to_linear(src[0]),
            srgb_to_linear(src[1]),
            srgb_to_linear(src[2]),
        ],
        [
            srgb_to_linear(dst[0]),
            srgb_to_linear(dst[1]),
            srgb_to_linear(dst[2]),
        ],
    );
    let da_c = srgb_to_linear(dst[3]);
    let (out_a, premul) = match mode {
        BlendMode::SourceOver => {
            let a = sa + da_c * (1.0 - sa);
            (
                a,
                [
                    sc[0] * sa + da[0] * da_c * (1.0 - sa),
                    sc[1] * sa + da[1] * da_c * (1.0 - sa),
                    sc[2] * sa + da[2] * da_c * (1.0 - sa),
                ],
            )
        }
        BlendMode::DestinationOver => {
            let a = da_c + sa * (1.0 - da_c);
            (
                a,
                [
                    da[0] * da_c + sc[0] * sa * (1.0 - da_c),
                    da[1] * da_c + sc[1] * sa * (1.0 - da_c),
                    da[2] * da_c + sc[2] * sa * (1.0 - da_c),
                ],
            )
        }
        BlendMode::Source => (sa, [sc[0] * sa, sc[1] * sa, sc[2] * sa]),
    };
    if out_a <= 0.0 {
        return [0, 0, 0, linear_to_srgb(out_a)];
    }
    [
        linear_to_srgb(premul[0] / out_a),
        linear_to_srgb(premul[1] / out_a),
        linear_to_srgb(premul[2] / out_a),
        linear_to_srgb(out_a),
    ]
}

/// Separable resampler covering the kernel table (image.ts:185-193).
/// `Default` maps to triangle (upstream's zig default filter id 0 is
/// stb's CHR default — registered approximation; `Area` is the
/// upstream default KERNEL argument, image.ts:540).
fn resample(src: &Pixels, sw: u32, sh: u32, dw: u32, dh: u32, kernel: ResizeKernel) -> Vec<u8> {
    if kernel == ResizeKernel::Nearest {
        let mut out = vec![0u8; dw as usize * 4 * dh as usize];
        for y in 0..dh as usize {
            let sy =
                (((y as f32 + 0.5) * sh as f32 / dh.max(1) as f32) as usize).min(sh as usize - 1);
            for x in 0..dw as usize {
                let sx = (((x as f32 + 0.5) * sw as f32 / dw as f32) as usize).min(sw as usize - 1);
                let s = sy * src.stride + sx * 4;
                out[y * dw as usize * 4 + x * 4..y * dw as usize * 4 + x * 4 + 4]
                    .copy_from_slice(&src.data[s..s + 4]);
            }
        }
        return out;
    }
    // horizontal pass then vertical pass, f32 accumulation
    let mid_w = dw as usize;
    let mut mid = vec![0f32; mid_w * sh as usize * 4];
    for y in 0..sh as usize {
        for x in 0..mid_w {
            let center = (x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            let (lo, weights) = kernel_weights(kernel, center, sw as f32);
            let mut acc = [0f32; 4];
            let mut total = 0f32;
            for (i, wgt) in weights {
                let sx = (lo + i).clamp(0, sw as usize - 1);
                let s = y * src.stride + sx * 4;
                let src_px: [f32; 4] = src.data[s..s + 4]
                    .iter()
                    .map(|&b| b as f32)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap();
                for (a, v) in acc.iter_mut().zip(src_px) {
                    *a += v * wgt;
                }
                total += wgt;
            }
            let base = (y * mid_w + x) * 4;
            for c in 0..4 {
                mid[base + c] = acc[c] / total.max(1e-6);
            }
        }
    }
    let mut out = vec![0u8; dw as usize * 4 * dh as usize];
    for y in 0..dh as usize {
        for x in 0..mid_w {
            let center = (y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
            let (lo, weights) = kernel_weights(kernel, center, sh as f32);
            let mut acc = [0f32; 4];
            let mut total = 0f32;
            for (i, wgt) in weights {
                let sy = (lo + i).clamp(0, sh as usize - 1);
                let base = (sy * mid_w + x) * 4;
                let mid_px: [f32; 4] = mid[base..base + 4].try_into().unwrap();
                for (a, v) in acc.iter_mut().zip(mid_px) {
                    *a += v * wgt;
                }
                total += wgt;
            }
            let d = (y * dw as usize + x) * 4;
            for c in 0..4 {
                out[d + c] = (acc[c] / total.max(1e-6)).clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    out
}

fn kernel_weights(kernel: ResizeKernel, center: f32, extent: f32) -> (usize, Vec<(usize, f32)>) {
    let (support, weight): (f32, Box<dyn Fn(f32) -> f32>) = match kernel {
        ResizeKernel::Area => (0.5, Box::new(|_t: f32| 1.0)), // box
        ResizeKernel::Triangle | ResizeKernel::Default => {
            (1.0, Box::new(|t: f32| (1.0 - t.abs()).max(0.0)))
        }
        ResizeKernel::CubicBspline | ResizeKernel::CatmullRom | ResizeKernel::Mitchell => {
            (2.0, Box::new(cubic_weight(kernel)))
        }
        ResizeKernel::Nearest => unreachable!(),
    };
    let scale = 1.0f32;
    let lo = ((center - support * scale).ceil().max(0.0)) as usize;
    let hi = ((center + support * scale).floor().min(extent - 1.0)) as usize;
    let weights: Vec<(usize, f32)> = (lo..=hi)
        .map(|i| {
            let t = (i as f32 - center) / scale;
            (i, weight(t))
        })
        .collect();
    (lo, weights)
}

fn cubic_weight(kernel: ResizeKernel) -> impl Fn(f32) -> f32 {
    // Mitchell-Netravali (b, c): bspline (1,0), catmull-rom (0,0.5),
    // mitchell (1/3,1/3)
    let (b, c) = match kernel {
        ResizeKernel::CubicBspline => (1.0, 0.0),
        ResizeKernel::CatmullRom => (0.0, 0.5),
        _ => (1.0 / 3.0, 1.0 / 3.0),
    };
    move |t: f32| {
        let t = t.abs();
        let t2 = t * t;
        if t < 1.0 {
            ((12.0 - 9.0 * b - 6.0 * c) * t2 * t
                + (-18.0 + 12.0 * b + 6.0 * c) * t2
                + (6.0 - 2.0 * b))
                / 6.0
        } else if t < 2.0 {
            ((-b - 6.0 * c) * t2 * t
                + (6.0 * b + 30.0 * c) * t2
                + (-12.0 * b - 48.0 * c) * t
                + (8.0 * b + 24.0 * c))
                / 6.0
        } else {
            0.0
        }
    }
}

/// RFC 4648 base64 (standard alphabet, padding tolerated) — the
/// `data:...;base64` arm of `load` (image.test.ts:895).
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some((b - b'A') as u32),
            b'a'..=b'z' => Some((b - b'a' + 26) as u32),
            b'0'..=b'9' => Some((b - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = input
        .iter()
        .copied()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    for chunk in cleaned.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut acc = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= val(b)? << (18 - 6 * i);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// `NativeImagePoolOptions` (image.ts:653-657).
#[derive(Debug, Clone, Copy)]
pub struct NativeImagePoolOptions {
    pub width: u32,
    pub height: u32,
    pub capacity: Option<u32>,
}

/// `NativeImagePool` (image.ts:659-705) — the fixed-geometry slot
/// machine: publish into a FREE slot (busy = outstanding retained
/// frames, status 12 image.ts:685-686), else grow to capacity, else
/// `None` (frame drop). `dispose` cancels future publications but
/// leaves outstanding frames alive (image-pool.test.ts:187).
pub struct NativeImagePool {
    slots: Vec<NativeImage>,
    pub width: u32,
    pub height: u32,
    pub capacity: usize,
    disposed: bool,
}

impl NativeImagePool {
    pub fn new(options: &NativeImagePoolOptions) -> Result<Self, ImageError> {
        let width = check_u32(options.width, "width", false)?;
        let height = check_u32(options.height, "height", false)?;
        let capacity = check_u32(options.capacity.unwrap_or(3), "capacity", false)?;
        if capacity > 8 {
            return Err(ImageError::Invalid(
                "capacity must be between 1 and 8".to_owned(),
            ));
        }
        Ok(NativeImagePool {
            slots: Vec::new(),
            width,
            height,
            capacity: capacity as usize,
            disposed: false,
        })
    }

    /// `publishRgba` (image.ts:675-677).
    pub fn publish_rgba(&mut self, pixels: &[u8]) -> Result<Option<NativeImage>, ImageError> {
        self.publish_pixels(pixels, &PixelImportOptions::default())
    }

    /// `publishPixels` (image.ts:679-697). Validation happens BEFORE
    /// the busy scan (image-pool.test.ts:89).
    pub fn publish_pixels(
        &mut self,
        pixels: &[u8],
        options: &PixelImportOptions,
    ) -> Result<Option<NativeImage>, ImageError> {
        if self.disposed {
            return Err(ImageError::Invalid(
                "NativeImagePool is disposed".to_owned(),
            ));
        }
        // validate against the pool geometry first
        NativeImage::from_pixels(pixels, self.width, self.height, options)?;
        let stored = Pixels {
            data: pixels[..self.width as usize * 4 * self.height as usize].to_vec(),
            stride: self.width as usize * 4,
        };
        for slot in &mut self.slots {
            match slot.update_pixels(&stored, options.opaque_alpha) {
                Ok(()) => {
                    // the returned retain is a FRESH wrapper — the
                    // ImageRenderable source-invalidation contract
                    // (image.ts:688-690)
                    return Ok(Some(slot.retain()?));
                }
                Err(ImageError::Native {
                    status: BUSY_STATUS,
                    ..
                }) => continue,
                Err(e) => return Err(e),
            }
        }
        if self.slots.len() == self.capacity {
            return Ok(None);
        }
        let image = NativeImage::from_pool_pixels(&stored, self.width, self.height)?;
        self.slots.push(image);
        Ok(Some(self.slots.last().expect("just pushed").retain()?))
    }

    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        for slot in &mut self.slots {
            slot.dispose();
        }
        self.slots.clear();
    }
}

impl Drop for NativeImagePool {
    fn drop(&mut self) {
        self.dispose();
    }
}
