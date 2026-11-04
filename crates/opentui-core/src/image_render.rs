//! Native image rendering — the rasterize → protocol-emit lane.
//!
//! Upstream's `OptimizedBuffer.drawImage` (buffer.ts:383-432) validates
//! the geometry and hands it to native `bufferDrawImage`
//! (buffer.zig:2569-2644), which only *reserves* cells and records an
//! [`ImagePlacement`]; the renderer later resolves the protocol
//! (`renderer.zig:1912-1954`) and emits:
//!
//! * **kitty graphics** — transmit (`a=t`, raw `f=24`/`f=32` or PNG
//!   passthrough `f=100`), place (`a=p`) and delete (`a=d`) APCs
//!   (`terminal-image.zig:89-238`).
//! * **sixel** — Wu-moment median-cut quantizer + banded DCS payload
//!   (`terminal-image.zig:240-629`).
//! * **blocks** — the 2×2 quadrant-cell fallback (`buffer.zig:2648-2710` + `renderQuadrantBlock` :3069-3131); the cells themselves are the output.
//!
//! Everything is byte-deterministic and terminal-free; tests capture a
//! `Vec<u8>` or inspect the cell grid. The lane's oracle is the vendored
//! Zig implementation (inspectable at `third_party/opentui`), the
//! upstream buffer/renderer/terminal-image tests, and the oc-tui PTY
//! evidence for kitty placement framing.
//!
//! ## Bounded deviations (registered in PORT-MATRIX)
//!
//! * Encoded PNG → sixel / blocks requires materialized pixels. This
//!   port's compressed decode is C3-deferred (`image.rs`), so an
//!   encoded-only image renders through kitty's `f=100` passthrough or
//!   keeps its reservation markers; raw-rgba images (the upstream
//!   `createFromRgba` test shape) rasterize fully.
//! * Kitty zlib/file transports are not ported (`t=f`/`o=z`); raw and
//!   PNG passthrough are. tmux wrapping is byte-ported.
//! * Sixel opacity dimming toward covered cell backgrounds
//!   (`dimSixelPixels`, renderer.zig:2478+) is approximated by scaling
//!   the placement opacity into the encoder input alpha.
//! * The renderer's incremental image-dirty/cache machinery is not
//!   ported; [`ImageFrameEmitter`] recomputes and retransmits on each
//!   frame. The occlusion/placeholder ruling is ported: mixed
//!   protocols, overlapping placements, off-screen placements, or
//!   overwritten reservation cells all flatten to the blocks fallback
//!   (`renderer.zig:1404-1467`).
//! * A sixel placement that leaves the frame is not actively cleared
//!   (sixel has no delete command; upstream repaints the vacated rect
//!   through its image-dirty path, renderer.zig:2395-2470). Kitty
//!   removals DO emit `a=d` deletions.

use std::io::{self, Write};

use crate::buffer::{ImagePlacement, OptimizedBuffer};
use crate::color::Rgba;
use crate::image::{ImageError, NativeImage, PixelFormat};

/// The emitter's error surface: output-writer failures + the buffer's
/// deferred-decode/materialization failures.
#[derive(Debug)]
pub enum ImageEmitError {
    Io(io::Error),
    Image(ImageError),
}

impl std::fmt::Display for ImageEmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageEmitError::Io(error) => error.fmt(f),
            ImageEmitError::Image(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ImageEmitError {}

impl From<io::Error> for ImageEmitError {
    fn from(error: io::Error) -> Self {
        ImageEmitError::Io(error)
    }
}

impl From<ImageError> for ImageEmitError {
    fn from(error: ImageError) -> Self {
        ImageEmitError::Image(error)
    }
}
use crate::renderables::image::{
    resolve_image_render_protocol, ImageRenderCtx, ImageRenderProtocol,
};

// ---------------------------------------------------------------------------
// Pixel-resolution probe — `queryPixelResolution` (ansi.zig:249,
// renderer.ts:3959-3965) and `parsePixelResolution`
// (terminal-capability-detection.ts:89-99).
// ---------------------------------------------------------------------------

/// `CSI 1 4 t` — "report text-area size in pixels", the query the
/// renderer sends on startup and after every resize (renderer.ts:3343/
/// :4001; the native constant is `ansi.zig:249 queryPixelSize`).
pub const PIXEL_RESOLUTION_QUERY: &[u8] = b"\x1b[14t";

/// `PixelResolution` (renderer.ts:267) — the reply payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelResolution {
    pub width: u32,
    pub height: u32,
}

/// `isPixelResolutionResponse` (terminal-capability-detection.ts:81):
/// shape-only — an oversized value still *is* a pixel-resolution
/// response even though `parse` rejects it (the upstream test's
/// `\x1b[4;4294967295;4294967295t` case).
#[must_use]
pub fn is_pixel_resolution_response(sequence: &str) -> bool {
    let Some(rest) = sequence
        .find("\x1b[4;")
        .map(|i| &sequence[i + "\x1b[4;".len()..])
    else {
        return false;
    };
    let Some((height, rest)) = rest.split_once(';') else {
        return false;
    };
    if height.is_empty() || !height.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Some(width) = rest.split_once('t').map(|(w, _)| w) else {
        return false;
    };
    !width.is_empty() && width.bytes().all(|b| b.is_ascii_digit())
}

/// `parsePixelResolution` (terminal-capability-detection.ts:89-99):
/// `ESC [ 4 ; height ; width t`, width/height `<= 0x7fff_ffff`.
/// Zero dimensions parse (the TS test pins `{width:0,height:0}`); the
/// resolution consumer ([`ImageRenderCtx::pixel_resolution`]) then
/// treats them as absent.
#[must_use]
pub fn parse_pixel_resolution(sequence: &str) -> Option<PixelResolution> {
    let start = sequence.find("\x1b[4;")? + "\x1b[4;".len();
    let rest = &sequence[start..];
    let (height_text, rest) = rest.split_once(';')?;
    let (width_text, _) = rest.split_once('t')?;
    if height_text.is_empty()
        || width_text.is_empty()
        || !height_text.bytes().all(|b| b.is_ascii_digit())
        || !width_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let height: u32 = height_text.parse().ok()?;
    let width: u32 = width_text.parse().ok()?;
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    Some(PixelResolution { width, height })
}

/// Append the pixel-resolution query to `out` (renderer.ts:3965's
/// `lib.queryPixelResolution` writes the same single CSI).
pub fn write_pixel_resolution_query<W: Write + ?Sized>(out: &mut W) -> io::Result<()> {
    out.write_all(PIXEL_RESOLUTION_QUERY)
}

/// The cell size in pixels (`resolution / terminal grid`) — the
/// `cellAspectRatio` input (Image.ts:132-138) in its probe form.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellPx {
    pub width: f64,
    pub height: f64,
}

impl PixelResolution {
    /// Divide by a positive terminal grid. `None` when either grid
    /// dimension is zero (the legacy/headless context).
    #[must_use]
    pub fn cell_px(self, terminal_width: u32, terminal_height: u32) -> Option<CellPx> {
        if terminal_width == 0 || terminal_height == 0 {
            return None;
        }
        Some(CellPx {
            width: f64::from(self.width) / f64::from(terminal_width),
            height: f64::from(self.height) / f64::from(terminal_height),
        })
    }
}

// ---------------------------------------------------------------------------
// Protocol resolution — native `ImageProtocol` (renderer.zig:194) and
// `resolveImageRenderProtocol` (Image.ts:20-32) combined.
// ---------------------------------------------------------------------------

/// The emitter's three concrete destinations. `blocks` is native's
/// `fallback` (renderer.zig:194).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitProtocol {
    Kitty,
    Sixel,
    Blocks,
}

/// `Limits` (image.zig:125-131) — the sixel placement gate
/// (`dimensionsWithinLimits`, image.zig:297-300).
pub const MAX_IMAGE_WIDTH: u32 = 16_384;
pub const MAX_IMAGE_HEIGHT: u32 = 16_384;
pub const MAX_IMAGE_PIXELS: u64 = 25_000_000;
pub const MAX_IMAGE_DECODED_BYTES: u64 = 100 * 1024 * 1024;

/// `dimensionsWithinLimits` (image.zig:297-300 + checkedPixelBytes
/// :285-294).
#[must_use]
pub fn dimensions_within_limits(width: u32, height: u32) -> bool {
    if width == 0 || height == 0 || width > MAX_IMAGE_WIDTH || height > MAX_IMAGE_HEIGHT {
        return false;
    }
    let Some(pixels) = u64::from(width).checked_mul(u64::from(height)) else {
        return false;
    };
    if pixels > MAX_IMAGE_PIXELS {
        return false;
    }
    pixels
        .checked_mul(4)
        .is_some_and(|bytes| bytes <= MAX_IMAGE_DECODED_BYTES && bytes <= usize::MAX as u64)
}

/// Resolve one placement to the emitter protocol.
///
/// The TS renderable gate ([`resolve_image_render_protocol`]) already
/// handles explicit/configured/auto + tmux + sixel-without-resolution.
/// Native adds the placement-size gate for sixel (`nextPlacementProtocol`,
/// renderer.zig:1948-1954).
#[must_use]
pub fn resolve_emit_protocol(
    requested: ImageRenderProtocol,
    ctx: &ImageRenderCtx<'_>,
    pixel_width: u32,
    pixel_height: u32,
) -> EmitProtocol {
    let resolved = resolve_image_render_protocol(
        requested,
        ctx.capabilities,
        ctx.pixel_resolution().is_some(),
    );
    match resolved {
        ImageRenderProtocol::Kitty => EmitProtocol::Kitty,
        ImageRenderProtocol::Sixel => {
            if dimensions_within_limits(pixel_width, pixel_height) {
                EmitProtocol::Sixel
            } else {
                EmitProtocol::Blocks
            }
        }
        ImageRenderProtocol::Blocks | ImageRenderProtocol::Auto => EmitProtocol::Blocks,
    }
}

// ---------------------------------------------------------------------------
// Blocks fallback raster — renderQuadrantBlock (buffer.zig:3069-3131)
// and quadrantChars (buffer.zig:2997-3015), ported exactly.
// ---------------------------------------------------------------------------

/// `quadrantChars` (buffer.zig:2997-3015): index = bit0 TL? No —
/// native packs quadrant bits as `[8, 4, 2, 1]` for `[TL, TR, BL, BR]`
/// (:3102), so index 1 = BR, 2 = BL, 4 = TR, 8 = TL.
pub const QUADRANT_CHARS: [u32; 16] = [
    32,     // 0000
    0x2597, // 0001 BR ▗
    0x2596, // 0010 BL ▖
    0x2584, // 0011 lower half ▄
    0x259d, // 0100 TR ▝
    0x2590, // 0101 right half ▐
    0x259e, // 0110 TR+BL ▞
    0x259f, // 0111 all but TL ▟
    0x2598, // 1000 TL ▘
    0x259a, // 1001 TL+BR ▚
    0x258c, // 1010 left half ▌
    0x2599, // 1011 all but TR ▙
    0x2580, // 1100 upper half ▀
    0x259c, // 1101 all but BL ▜
    0x259b, // 1110 all but BR ▛
    0x2588, // 1111 full █
];

/// `quadrantIndex` (buffer.zig:3017-3022): the table position of a
/// rendered glyph, 15 when not found (full block).
#[must_use]
pub fn quadrant_index(ch: u32) -> u8 {
    QUADRANT_CHARS
        .iter()
        .position(|candidate| *candidate == ch)
        .map_or(15, |index| index as u8)
}

/// `QuadrantResult` (buffer.zig:3061-3065).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadrantResult {
    pub ch: u32,
    pub fg: Rgba,
    pub bg: Rgba,
}

fn color_distance(a: Rgba, b: Rgba) -> f32 {
    let dr = f32::from(a.red()) - f32::from(b.red());
    let dg = f32::from(a.green()) - f32::from(b.green());
    let db = f32::from(a.blue()) - f32::from(b.blue());
    dr * dr + dg * dg + db * db
}

fn closest_color_index(pixel: Rgba, candidates: [Rgba; 2]) -> u8 {
    if color_distance(pixel, candidates[0]) <= color_distance(pixel, candidates[1]) {
        0
    } else {
        1
    }
}

fn average_color(pixels: &[Rgba]) -> Rgba {
    if pixels.is_empty() {
        return Rgba::from_ints(0, 0, 0, 0);
    }
    let mut sum = [0u32; 4];
    for pixel in pixels {
        sum[0] += u32::from(pixel.red());
        sum[1] += u32::from(pixel.green());
        sum[2] += u32::from(pixel.blue());
        sum[3] += u32::from(pixel.alpha());
    }
    let len = pixels.len() as u32;
    Rgba::from_ints(
        ((sum[0] + len / 2) / len) as u8,
        ((sum[1] + len / 2) / len) as u8,
        ((sum[2] + len / 2) / len) as u8,
        ((sum[3] + len / 2) / len) as u8,
    )
}

fn luminance(color: Rgba) -> f32 {
    // ansi.zig `redF`/`greenF`/`blueF` = component / 255.
    0.2126 * (f32::from(color.red()) / 255.0)
        + 0.7152 * (f32::from(color.green()) / 255.0)
        + 0.0722 * (f32::from(color.blue()) / 255.0)
}

/// `renderQuadrantBlock` (buffer.zig:3069-3131), exact: pick the most
/// distant pixel pair, orient it dark/light, classify each quadrant by
/// nearest of the two, then emit the block glyph. All-light and
/// all-dark are the two special cases (:3112-3123).
#[must_use]
pub fn render_quadrant_block(pixels: [Rgba; 4]) -> QuadrantResult {
    let mut idx_a = 0usize;
    let mut idx_b = 1usize;
    let mut max_dist = color_distance(pixels[0], pixels[1]);
    for i in 0..4 {
        for j in (i + 1)..4 {
            let dist = color_distance(pixels[i], pixels[j]);
            if dist > max_dist {
                idx_a = i;
                idx_b = j;
                max_dist = dist;
            }
        }
    }
    let (dark, light) = if luminance(pixels[idx_a]) <= luminance(pixels[idx_b]) {
        (pixels[idx_a], pixels[idx_b])
    } else {
        (pixels[idx_b], pixels[idx_a])
    };
    let bit_values = [8u32, 4, 2, 1];
    let mut quadrant_bits = 0u32;
    for (i, bit) in bit_values.iter().enumerate() {
        if closest_color_index(pixels[i], [dark, light]) == 0 {
            quadrant_bits |= bit;
        }
    }
    if quadrant_bits == 0 {
        QuadrantResult {
            ch: 32,
            fg: dark,
            bg: average_color(&pixels),
        }
    } else if quadrant_bits == 15 {
        QuadrantResult {
            ch: QUADRANT_CHARS[15],
            fg: average_color(&pixels),
            bg: light,
        }
    } else {
        QuadrantResult {
            ch: QUADRANT_CHARS[quadrant_bits as usize],
            fg: dark,
            bg: light,
        }
    }
}

// ---------------------------------------------------------------------------
// Base64 (RFC 4648 standard alphabet, padded) — the kitty payload encoder.
// ---------------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard padded base64. Chunk-boundary safe (3072-byte kitty chunks
/// are multiples of 3, so only a final chunk pads).
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(BASE64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(BASE64_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Kitty graphics — terminal-image.zig:1-238.
// ---------------------------------------------------------------------------

/// Payload chunk budget (`raw_chunk`, terminal-image.zig:94): 3072 raw
/// bytes → 4096 base64 characters.
const KITTY_CHUNK: usize = 3072;

fn kitty_wrap<W: Write + ?Sized>(
    writer: &mut W,
    tmux: bool,
    control: &str,
    payload: &str,
) -> io::Result<()> {
    if tmux {
        writer.write_all(b"\x1bPtmux;\x1b\x1b_G")?;
    } else {
        writer.write_all(b"\x1b_G")?;
    }
    writer.write_all(control.as_bytes())?;
    writer.write_all(payload.as_bytes())?;
    if tmux {
        writer.write_all(b"\x1b\x1b\\\x1b\\")?;
    } else {
        writer.write_all(b"\x1b\\")?;
    }
    Ok(())
}

/// `writeKittyTransmit` (terminal-image.zig:89-179): encoded PNGs pass
/// through verbatim as `f=100`; materialized pixels go as `f=24` (no
/// alpha) or `f=32` (alpha), chunked at 3072 bytes with `m=1`
/// continuations and `q=2` (quiet).
pub fn write_kitty_transmit<W: Write + ?Sized>(
    writer: &mut W,
    image: &NativeImage,
    id: u32,
    tmux: bool,
) -> io::Result<()> {
    let encoded = image.encoded().unwrap_or(&[]);
    if !encoded.is_empty() {
        let mut offset = 0;
        let mut first = true;
        while offset < encoded.len() {
            let end = (offset + KITTY_CHUNK).min(encoded.len());
            let more = end < encoded.len();
            let payload = base64_encode(&encoded[offset..end]);
            let options = if first {
                format!("a=t,f=100,i={id},m={},q=2;", u8::from(more))
            } else {
                format!("m={},q=2;", u8::from(more))
            };
            kitty_wrap(writer, tmux, &options, &payload)?;
            offset = end;
            first = false;
        }
        return Ok(());
    }
    let raw = image
        .raw(PixelFormat::Rgba8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "image pixels unavailable"))?;
    let count = (raw.width as u64 * raw.height as u64) as usize;
    if raw.data.len() < count * 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "image pixel storage truncated",
        ));
    }
    let has_alpha = image.info().map(|info| info.has_alpha).unwrap_or(true);
    if has_alpha {
        let rgba_len = count * 4;
        let mut offset = 0;
        let mut first = true;
        while offset < rgba_len {
            let end = (offset + KITTY_CHUNK).min(rgba_len);
            let more = end < rgba_len;
            let payload = base64_encode(&raw.data[offset..end]);
            let options = if first {
                format!(
                    "a=t,f=32,s={},v={},i={id},m={},q=2;",
                    raw.width,
                    raw.height,
                    u8::from(more)
                )
            } else {
                format!("m={},q=2;", u8::from(more))
            };
            kitty_wrap(writer, tmux, &options, &payload)?;
            offset = end;
            first = false;
        }
    } else {
        let mut rgb = Vec::with_capacity(KITTY_CHUNK);
        let mut offset = 0;
        let mut first = true;
        while offset < count {
            rgb.clear();
            let pixels = (KITTY_CHUNK / 3).min(count - offset);
            for index in 0..pixels {
                let source = (offset + index) * 4;
                rgb.extend_from_slice(&raw.data[source..source + 3]);
            }
            let more = offset + pixels < count;
            let payload = base64_encode(&rgb);
            let options = if first {
                format!(
                    "a=t,f=24,s={},v={},i={id},m={},q=2;",
                    raw.width,
                    raw.height,
                    u8::from(more)
                )
            } else {
                format!("m={},q=2;", u8::from(more))
            };
            kitty_wrap(writer, tmux, &options, &payload)?;
            offset += pixels;
            first = false;
        }
    }
    Ok(())
}

/// `kittyPlacementZ` (terminal-image.zig:181-193): stay in
/// `[INT32_MIN/2, 0)` so placements sit above cell backgrounds.
#[must_use]
pub fn kitty_placement_z(placement_id: u32) -> i32 {
    i32::MIN / 2 + placement_id as i32
}

/// `writeKittyPlacement` (terminal-image.zig:195-216): CUP to the
/// placement rect, then an `a=p` APC carrying the crop/size and z.
#[allow(clippy::too_many_arguments)]
pub fn write_kitty_placement<W: Write + ?Sized>(
    writer: &mut W,
    id: u32,
    placement_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    z: i32,
    tmux: bool,
) -> io::Result<()> {
    write!(writer, "\x1b[{};{}H", y + 1, x + 1)?;
    let options = format!(
        "a=p,i={id},p={placement_id},c={width},r={height},x={source_x},y={source_y},w={source_width},h={source_height},C=1,z={z},q=2"
    );
    kitty_wrap(writer, tmux, &options, "")
}

/// `writeKittyDelete` (terminal-image.zig:232-238): `a=d,d=i` (drop
/// placements) or `d=I` (free the image too).
pub fn write_kitty_delete<W: Write + ?Sized>(
    writer: &mut W,
    id: u32,
    placement_id: Option<u32>,
    free_image: bool,
    tmux: bool,
) -> io::Result<()> {
    let mut options = format!("a=d,d={},i={id}", if free_image { 'I' } else { 'i' });
    if let Some(p) = placement_id {
        options.push_str(&format!(",p={p}"));
    }
    options.push_str(",q=2");
    kitty_wrap(writer, tmux, &options, "")
}

/// Emit the kitty deletes for a list of image ids (the
/// `image_deletes` set the oc-tui frame loop keeps). Uses `d=I` so the
/// upstream image is freed as well.
pub fn emit_image_deletes<W: Write + ?Sized>(writer: &mut W, ids: &[u32]) -> io::Result<()> {
    for id in ids {
        write_kitty_delete(writer, *id, None, true, false)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sixel — terminal-image.zig:240-629. Wu's fixed RGB moment cube gives
// a deterministic adaptive palette; boxes are tagged so pixels map
// without a nearest-color search.
// ---------------------------------------------------------------------------

const SIXEL_HISTOGRAM_SIDE: usize = 33;
const SIXEL_HISTOGRAM_LEN: usize =
    SIXEL_HISTOGRAM_SIDE * SIXEL_HISTOGRAM_SIDE * SIXEL_HISTOGRAM_SIDE;
const SIXEL_MAX_COLORS: usize = 255;
const SIXEL_BAND: usize = 6;

#[derive(Clone, Copy, Default)]
struct SixelMoment {
    count: u64,
    r: u64,
    g: u64,
    b: u64,
    square: u64,
}

#[derive(Clone, Copy)]
struct SixelBox {
    r0: u8,
    r1: u8,
    g0: u8,
    g1: u8,
    b0: u8,
    b1: u8,
}

impl Default for SixelBox {
    fn default() -> Self {
        SixelBox {
            r0: 0,
            r1: 32,
            g0: 0,
            g1: 32,
            b0: 0,
            b1: 32,
        }
    }
}

#[derive(Clone, Copy)]
enum SixelAxis {
    R,
    G,
    B,
}

#[derive(Clone, Copy)]
struct SixelCut {
    axis: SixelAxis,
    at: u8,
    score: f64,
}

#[derive(Clone, Copy, Default)]
struct BoxStats {
    count: u64,
    r: u64,
    g: u64,
    b: u64,
}

/// `QuantizedSixel` (terminal-image.zig:248-258): per-pixel palette
/// index (`255` = transparent/unpainted) + the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantizedSixel {
    pub palette: Vec<[u8; 3]>,
    pub indices: Vec<u8>,
}

fn moment_index(r: usize, g: usize, b: usize) -> usize {
    (r * SIXEL_HISTOGRAM_SIDE + g) * SIXEL_HISTOGRAM_SIDE + b
}

fn add_moment(target: &mut SixelMoment, value: SixelMoment) {
    target.count += value.count;
    target.r += value.r;
    target.g += value.g;
    target.b += value.b;
    target.square += value.square;
}

#[allow(clippy::similar_names)]
fn volume_value(field: fn(&SixelMoment) -> u64, moments: &[SixelMoment], b: SixelBox) -> i128 {
    let corner = |r: u8, g: u8, bb: u8| {
        i128::from(field(
            &moments[moment_index(usize::from(r), usize::from(g), usize::from(bb))],
        ))
    };
    corner(b.r1, b.g1, b.b1) - corner(b.r1, b.g1, b.b0) - corner(b.r1, b.g0, b.b1)
        + corner(b.r1, b.g0, b.b0)
        - corner(b.r0, b.g1, b.b1)
        + corner(b.r0, b.g1, b.b0)
        + corner(b.r0, b.g0, b.b1)
        - corner(b.r0, b.g0, b.b0)
}

fn volume_stats(moments: &[SixelMoment], b: SixelBox) -> BoxStats {
    let count = volume_value(|m| m.count, moments, b);
    let r = volume_value(|m| m.r, moments, b);
    let g = volume_value(|m| m.g, moments, b);
    let blue = volume_value(|m| m.b, moments, b);
    BoxStats {
        count: u64::try_from(count.max(0)).unwrap_or(u64::MAX),
        r: u64::try_from(r.max(0)).unwrap_or(u64::MAX),
        g: u64::try_from(g.max(0)).unwrap_or(u64::MAX),
        b: u64::try_from(blue.max(0)).unwrap_or(u64::MAX),
    }
}

fn stats_energy(stats: BoxStats) -> f64 {
    if stats.count == 0 {
        return 0.0;
    }
    let r = stats.r as f64;
    let g = stats.g as f64;
    let b = stats.b as f64;
    (2.0 * r * r + 4.0 * g * g + 3.0 * b * b) / (8.0 * stats.count as f64)
}

fn box_variance(moments: &[SixelMoment], b: SixelBox) -> f64 {
    let square = volume_value(|m| m.square, moments, b);
    (square as f64 - stats_energy(volume_stats(moments, b))).max(0.0)
}

fn best_box_cut(moments: &[SixelMoment], b: SixelBox) -> Option<SixelCut> {
    let mut best: Option<SixelCut> = None;
    for axis in [SixelAxis::R, SixelAxis::G, SixelAxis::B] {
        let (low, high) = match axis {
            SixelAxis::R => (b.r0, b.r1),
            SixelAxis::G => (b.g0, b.g1),
            SixelAxis::B => (b.b0, b.b1),
        };
        let mut at = low + 1;
        while at < high {
            let mut first = b;
            let mut second = b;
            match axis {
                SixelAxis::R => {
                    first.r1 = at;
                    second.r0 = at;
                }
                SixelAxis::G => {
                    first.g1 = at;
                    second.g0 = at;
                }
                SixelAxis::B => {
                    first.b1 = at;
                    second.b0 = at;
                }
            }
            let first_stats = volume_stats(moments, first);
            let second_stats = volume_stats(moments, second);
            if first_stats.count == 0 || second_stats.count == 0 {
                at += 1;
                continue;
            }
            let score = stats_energy(first_stats) + stats_energy(second_stats);
            if best.is_none_or(|cut| score > cut.score) {
                best = Some(SixelCut { axis, at, score });
            }
            at += 1;
        }
    }
    best
}

/// `quantizeSixel` (terminal-image.zig:358-479).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn quantize_sixel(raw: &crate::image::RawImage, max_colors: usize) -> Option<QuantizedSixel> {
    if max_colors == 0 || max_colors > SIXEL_MAX_COLORS {
        return None;
    }
    if raw.width == 0 || raw.height == 0 {
        return None;
    }
    let width = raw.width as usize;
    let height = raw.height as usize;
    let pixel_count = width.checked_mul(height)?;
    if raw.data.len() < pixel_count * 4 {
        return None;
    }
    let mut moments = vec![SixelMoment::default(); SIXEL_HISTOGRAM_LEN];
    let mut active_bins = 0usize;
    for pixel in 0..pixel_count {
        let offset = pixel * 4;
        if raw.data[offset + 3] < 128 {
            continue;
        }
        let r = raw.data[offset];
        let g = raw.data[offset + 1];
        let b = raw.data[offset + 2];
        let bin = &mut moments[moment_index(
            usize::from(r >> 3) + 1,
            usize::from(g >> 3) + 1,
            usize::from(b >> 3) + 1,
        )];
        if bin.count == 0 {
            active_bins += 1;
        }
        bin.count += 1;
        bin.r += u64::from(r);
        bin.g += u64::from(g);
        bin.b += u64::from(b);
        bin.square += (2 * u64::from(r) * u64::from(r)
            + 4 * u64::from(g) * u64::from(g)
            + 3 * u64::from(b) * u64::from(b)
            + 4)
            >> 3;
    }
    for r in 1..SIXEL_HISTOGRAM_SIDE {
        let mut area = vec![SixelMoment::default(); SIXEL_HISTOGRAM_SIDE];
        for g in 1..SIXEL_HISTOGRAM_SIDE {
            let mut line = SixelMoment::default();
            for b in 1..SIXEL_HISTOGRAM_SIDE {
                add_moment(&mut line, moments[moment_index(r, g, b)]);
                add_moment(&mut area[b], line);
                let mut cumulative = moments[moment_index(r - 1, g, b)];
                add_moment(&mut cumulative, area[b]);
                moments[moment_index(r, g, b)] = cumulative;
            }
        }
    }
    let mut indices = vec![255u8; pixel_count];
    if active_bins == 0 {
        return Some(QuantizedSixel {
            palette: Vec::new(),
            indices,
        });
    }
    let mut boxes = [SixelBox::default(); SIXEL_MAX_COLORS];
    let mut variances = [0f64; SIXEL_MAX_COLORS];
    variances[0] = box_variance(&moments, boxes[0]);
    let mut box_count = 1usize;
    while box_count < active_bins.min(max_colors) {
        let mut split_index: Option<usize> = None;
        let mut highest = 0f64;
        for (index, variance) in variances[..box_count].iter().enumerate() {
            if *variance > highest {
                highest = *variance;
                split_index = Some(index);
            }
        }
        let Some(split) = split_index else { break };
        let Some(cut) = best_box_cut(&moments, boxes[split]) else {
            variances[split] = 0.0;
            continue;
        };
        let mut second = boxes[split];
        match cut.axis {
            SixelAxis::R => {
                boxes[split].r1 = cut.at;
                second.r0 = cut.at;
            }
            SixelAxis::G => {
                boxes[split].g1 = cut.at;
                second.g0 = cut.at;
            }
            SixelAxis::B => {
                boxes[split].b1 = cut.at;
                second.b0 = cut.at;
            }
        }
        boxes[box_count] = second;
        variances[split] = box_variance(&moments, boxes[split]);
        variances[box_count] = box_variance(&moments, second);
        box_count += 1;
    }
    // Common colors get shorter selectors: stable insertion by count.
    let mut box_counts = [0u64; SIXEL_MAX_COLORS];
    for (index, b) in boxes[..box_count].iter().enumerate() {
        box_counts[index] = volume_stats(&moments, *b).count;
    }
    for index in 1..box_count {
        let b = boxes[index];
        let count = box_counts[index];
        let mut destination = index;
        while destination > 0 && box_counts[destination - 1] < count {
            boxes[destination] = boxes[destination - 1];
            box_counts[destination] = box_counts[destination - 1];
            destination -= 1;
        }
        boxes[destination] = b;
        box_counts[destination] = count;
    }
    let mut tags = [0u8; 32 * 32 * 32];
    let mut palette = Vec::with_capacity(box_count);
    for (palette_index, b) in boxes[..box_count].iter().enumerate() {
        let stats = volume_stats(&moments, *b);
        if stats.count == 0 {
            palette.push([0, 0, 0]);
            continue;
        }
        palette.push([
            ((stats.r + stats.count / 2) / stats.count) as u8,
            ((stats.g + stats.count / 2) / stats.count) as u8,
            ((stats.b + stats.count / 2) / stats.count) as u8,
        ]);
        for r in b.r0..b.r1 {
            for g in b.g0..b.g1 {
                for blue in b.b0..b.b1 {
                    tags[(usize::from(r) << 10) | (usize::from(g) << 5) | usize::from(blue)] =
                        palette_index as u8;
                }
            }
        }
    }
    for y in 0..height {
        for x in 0..width {
            let pixel = y * width + x;
            let offset = pixel * 4;
            indices[pixel] = if raw.data[offset + 3] < 128 {
                255
            } else {
                tags[(usize::from(raw.data[offset] >> 3) << 10)
                    | (usize::from(raw.data[offset + 1] >> 3) << 5)
                    | usize::from(raw.data[offset + 2] >> 3)]
            };
        }
    }
    Some(QuantizedSixel { palette, indices })
}

fn write_unsigned<W: Write + ?Sized>(writer: &mut W, value: usize) -> io::Result<()> {
    write!(writer, "{value}")
}

/// `writeSixelIndexedPayload` (terminal-image.zig:538-616): raster
/// attributes, register definitions, then banded masks with `$`
/// carriage returns and `!` run-lengths.
pub fn write_sixel_indexed_payload<W: Write + ?Sized>(
    writer: &mut W,
    indices: &[u8],
    palette: &[[u8; 3]],
    width: u32,
    height: u32,
) -> io::Result<()> {
    if palette.is_empty() || palette.len() > SIXEL_MAX_COLORS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sixel palette must be 1..=255 colors",
        ));
    }
    let width_usize = width as usize;
    let height_usize = height as usize;
    let pixel_count = width_usize
        .checked_mul(height_usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel dimensions overflow"))?;
    if indices.len() < pixel_count {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sixel index buffer truncated",
        ));
    }
    writer.write_all(b"0;1;0q\"1;1;")?;
    write_unsigned(writer, width_usize)?;
    writer.write_all(b";")?;
    write_unsigned(writer, height_usize)?;
    for (index, color) in palette.iter().enumerate() {
        writer.write_all(b"#")?;
        write_unsigned(writer, index)?;
        writer.write_all(b";2;")?;
        for (channel_index, channel) in color.iter().enumerate() {
            write_unsigned(writer, (u16::from(*channel) * 100 + 127) as usize / 255)?;
            if channel_index < 2 {
                writer.write_all(b";")?;
            }
        }
    }
    let mut masks = vec![0u8; palette.len() * width_usize];
    let mut generations = [0u32; 256];
    let mut last_nonzero = [0usize; 256];
    let mut band_y = 0usize;
    let mut generation = 0u32;
    while band_y < height_usize {
        generation += 1;
        for bit in 0..SIXEL_BAND {
            let y = band_y + bit;
            if y >= height_usize {
                continue;
            }
            for x in 0..width_usize {
                let palette_index = indices[y * width_usize + x];
                if palette_index == 255 {
                    continue;
                }
                if usize::from(palette_index) >= palette.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "sixel palette index out of range",
                    ));
                }
                let index = usize::from(palette_index);
                if generations[index] != generation {
                    generations[index] = generation;
                    masks[index * width_usize..(index + 1) * width_usize].fill(0);
                    last_nonzero[index] = 0;
                }
                masks[index * width_usize + x] |= 1 << bit;
                last_nonzero[index] = last_nonzero[index].max(x + 1);
            }
        }
        let mut first_plane = true;
        for (palette_index, _) in palette.iter().enumerate() {
            if generations[palette_index] != generation {
                continue;
            }
            let plane = &masks[palette_index * width_usize
                ..palette_index * width_usize + last_nonzero[palette_index]];
            if !first_plane {
                writer.write_all(b"$")?;
            }
            first_plane = false;
            writer.write_all(b"#")?;
            write_unsigned(writer, palette_index)?;
            let mut x = 0usize;
            while x < plane.len() {
                let mask = plane[x];
                let ch = b'?' + mask;
                let mut run = 1usize;
                while x + run < plane.len() && plane[x + run] == mask {
                    run += 1;
                }
                if run >= 4 {
                    writer.write_all(b"!")?;
                    write_unsigned(writer, run)?;
                    writer.write_all(&[ch])?;
                } else {
                    for _ in 0..run {
                        writer.write_all(&[ch])?;
                    }
                }
                x += run;
            }
        }
        if band_y + SIXEL_BAND < height_usize {
            writer.write_all(b"-")?;
        }
        band_y += SIXEL_BAND;
    }
    Ok(())
}

/// `writeSixelFramedPayload` (terminal-image.zig:618-622).
pub fn write_sixel_framed_payload<W: Write + ?Sized>(
    writer: &mut W,
    payload: &[u8],
    tmux: bool,
) -> io::Result<()> {
    if tmux {
        writer.write_all(b"\x1bPtmux;\x1bP")?;
    } else {
        writer.write_all(b"\x1bP")?;
    }
    writer.write_all(payload)?;
    if tmux {
        writer.write_all(b"\x1b\x1b\\\x1b\\")?;
    } else {
        writer.write_all(b"\x1b\\")?;
    }
    Ok(())
}

/// `writeSixel` (terminal-image.zig:624-629) for a materialized image.
pub fn write_sixel<W: Write + ?Sized>(
    writer: &mut W,
    raw: &crate::image::RawImage,
    tmux: bool,
) -> io::Result<()> {
    let quantized = quantize_sixel(raw, SIXEL_MAX_COLORS)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sixel quantization failed"))?;
    if quantized.palette.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sixel image is fully transparent",
        ));
    }
    let mut payload = Vec::new();
    write_sixel_indexed_payload(
        &mut payload,
        &quantized.indices,
        &quantized.palette,
        raw.width,
        raw.height,
    )?;
    write_sixel_framed_payload(writer, &payload, tmux)
}

// ---------------------------------------------------------------------------
// Placement transforms — source crop + area resize before sixel encoding
// (renderer.zig:2423-2448 uses `native_image.extract` + `resize(.area)`).
// ---------------------------------------------------------------------------

fn crop_rgba(
    raw: &crate::image::RawImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Option<crate::image::RawImage> {
    if width == 0 || height == 0 || x + width > raw.width || y + height > raw.height {
        return None;
    }
    let stride = width as usize * 4;
    let mut data = vec![0u8; stride * height as usize];
    for row in 0..height as usize {
        let source = (y as usize + row) * raw.stride + x as usize * 4;
        let destination = row * stride;
        data[destination..destination + stride].copy_from_slice(&raw.data[source..source + stride]);
    }
    Some(crate::image::RawImage {
        data,
        width,
        height,
        stride,
        format: PixelFormat::Rgba8,
    })
}

fn resize_rgba_area(
    raw: &crate::image::RawImage,
    width: u32,
    height: u32,
) -> Option<crate::image::RawImage> {
    if width == 0 || height == 0 {
        return None;
    }
    if raw.width == width && raw.height == height {
        return Some(crate::image::RawImage {
            data: raw.data.clone(),
            width: raw.width,
            height: raw.height,
            stride: raw.stride,
            format: raw.format,
        });
    }
    let stride = width as usize * 4;
    let mut data = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        let y0 = y * raw.height as usize / height as usize;
        let y1 = (((y + 1) * raw.height as usize / height as usize).max(y0 + 1))
            .min(raw.height as usize);
        for x in 0..width as usize {
            let x0 = x * raw.width as usize / width as usize;
            let x1 = (((x + 1) * raw.width as usize / width as usize).max(x0 + 1))
                .min(raw.width as usize);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let source = sy * raw.stride + sx * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u32::from(raw.data[source + channel]);
                    }
                    count += 1;
                }
            }
            let destination = y * stride + x * 4;
            for channel in 0..4 {
                data[destination + channel] = ((sums[channel] + count / 2) / count) as u8;
            }
        }
    }
    Some(crate::image::RawImage {
        data,
        width,
        height,
        stride,
        format: PixelFormat::Rgba8,
    })
}

fn placement_sixel_payload(placement: &ImagePlacement) -> Option<Vec<u8>> {
    let raw = placement.image.raw(PixelFormat::Rgba8).ok()?;
    let cropped = crop_rgba(
        &raw,
        placement.source_x,
        placement.source_y,
        placement.source_width,
        placement.source_height,
    )?;
    let width = if placement.pixel_width == 0 {
        cropped.width
    } else {
        placement.pixel_width
    };
    let height = if placement.pixel_height == 0 {
        cropped.height
    } else {
        placement.pixel_height
    };
    let resized = resize_rgba_area(&cropped, width, height)?;
    let quantized = quantize_sixel(&resized, SIXEL_MAX_COLORS)?;
    if quantized.palette.is_empty() {
        return None;
    }
    let mut payload = Vec::new();
    write_sixel_indexed_payload(
        &mut payload,
        &quantized.indices,
        &quantized.palette,
        resized.width,
        resized.height,
    )
    .ok()?;
    Some(payload)
}

// ---------------------------------------------------------------------------
// Frame emitter — the occlusion/placeholder ruling and protocol dispatch.
// ---------------------------------------------------------------------------

fn placements_overlap(a: &ImagePlacement, b: &ImagePlacement) -> bool {
    let ax1 = i64::from(a.x) + i64::from(a.width);
    let ay1 = i64::from(a.y) + i64::from(a.height);
    let bx1 = i64::from(b.x) + i64::from(b.width);
    let by1 = i64::from(b.y) + i64::from(b.height);
    i64::from(a.x) < bx1 && ax1 > i64::from(b.x) && i64::from(a.y) < by1 && ay1 > i64::from(b.y)
}

fn placement_addressable(buffer: &OptimizedBuffer, placement: &ImagePlacement) -> bool {
    placement.x >= 0
        && placement.y >= 0
        && u64::from(placement.x as u32) + u64::from(placement.width) <= u64::from(buffer.width)
        && u64::from(placement.y as u32) + u64::from(placement.height) <= u64::from(buffer.height)
}

/// The reservation scan (`placementReservation`/`snapshotPlacementUncovered`,
/// renderer.zig:1332-1398): every cell in the rect still carries this
/// placement's image marker. A text draw or a higher placement that
/// overwrote any cell invalidates native graphics and forces blocks.
fn placement_uncovered(buffer: &OptimizedBuffer, placement: &ImagePlacement) -> bool {
    if !placement_addressable(buffer, placement) {
        return false;
    }
    for dy in 0..placement.height {
        for dx in 0..placement.width {
            let x = placement.x as u32 + dx;
            let y = placement.y as u32 + dy;
            let Some(cell) = buffer.get(x, y) else {
                return false;
            };
            if !crate::buffer::is_image_char(cell.char)
                || crate::buffer::image_id_from_char(cell.char) != placement.placement_id
            {
                return false;
            }
        }
    }
    true
}

/// `EmitReport` — what one [`ImageFrameEmitter::render_frame`] emitted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmitReport {
    pub protocol: Option<EmitProtocol>,
    /// Kitty image ids transmitted/placed (or sixel placements drawn).
    pub drawn: Vec<u32>,
    /// Kitty image ids deleted because their placement left the frame.
    pub deleted: Vec<u32>,
    /// The blocks fallback materialized the placements into cells.
    pub materialized: bool,
}

/// A frame-scoped image emitter. It owns the committed kitty image-id
/// set (the `image_deletes` bookkeeping) and dispatches one buffer's
/// placements under the occlusion ruling:
///
/// * empty placements → delete every committed kitty id;
/// * mixed protocols, overlapping placements, off-screen placements,
///   or any overwritten reservation cell → blocks fallback
///   (`materialize_image_fallbacks`);
/// * kitty → transmit new ids, place all, delete ids that left;
/// * sixel → re-encode each placement and emit CUP + DCS.
///
/// The platform/multiplexer toggle is the caller's (`ctx.capabilities`
/// carries `multiplexer`; `tmux` selects the DCS/APC passthrough
/// wrappers).
#[derive(Debug, Default)]
pub struct ImageFrameEmitter {
    committed: Vec<u32>,
}

impl ImageFrameEmitter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The kitty ids currently committed to the terminal.
    #[must_use]
    pub fn committed(&self) -> &[u32] {
        &self.committed
    }

    fn resolve(buffer: &OptimizedBuffer, ctx: &ImageRenderCtx<'_>) -> Option<EmitProtocol> {
        let placements = buffer.image_placements();
        let first = placements.first()?;
        let protocol =
            resolve_emit_protocol(first.protocol, ctx, first.pixel_width, first.pixel_height);
        for (index, placement) in placements.iter().enumerate() {
            if resolve_emit_protocol(
                placement.protocol,
                ctx,
                placement.pixel_width,
                placement.pixel_height,
            ) != protocol
            {
                return Some(EmitProtocol::Blocks);
            }
            for previous in placements.iter().take(index) {
                if placements_overlap(previous, placement) {
                    return Some(EmitProtocol::Blocks);
                }
            }
        }
        match protocol {
            EmitProtocol::Kitty | EmitProtocol::Sixel => {
                if !placements.iter().all(|p| placement_uncovered(buffer, p)) {
                    Some(EmitProtocol::Blocks)
                } else {
                    Some(protocol)
                }
            }
            EmitProtocol::Blocks => Some(EmitProtocol::Blocks),
        }
    }

    /// Resolve + emit one frame. `tmux` mirrors `terminal.isInTmux()`
    /// (the DCS/APC passthrough wrappers).
    pub fn render_frame<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        buffer: &mut OptimizedBuffer,
        ctx: &ImageRenderCtx<'_>,
    ) -> Result<EmitReport, ImageEmitError> {
        let protocol = Self::resolve(buffer, ctx);
        let tmux = ctx
            .capabilities
            .is_some_and(|caps| caps.multiplexer == crate::renderer::TerminalMultiplexer::Tmux);
        let mut report = EmitReport {
            protocol,
            ..EmitReport::default()
        };
        match protocol {
            None => {
                for id in std::mem::take(&mut self.committed) {
                    write_kitty_delete(writer, id, None, true, tmux)?;
                    report.deleted.push(id);
                }
            }
            Some(EmitProtocol::Blocks) => {
                // Placeholder ruling: the reservation cells become owned
                // block cells; the placements (and their refcounts) go.
                buffer.materialize_image_fallbacks()?;
                report.materialized = true;
                for id in std::mem::take(&mut self.committed) {
                    write_kitty_delete(writer, id, None, true, tmux)?;
                    report.deleted.push(id);
                }
            }
            Some(EmitProtocol::Kitty) => {
                let placements: Vec<ImagePlacement> = buffer.image_placements().to_vec();
                let current: Vec<u32> = placements.iter().map(|p| p.placement_id).collect();
                for id in self.committed.clone() {
                    if !current.contains(&id) {
                        write_kitty_delete(writer, id, None, true, tmux)?;
                        report.deleted.push(id);
                    }
                }
                for placement in &placements {
                    if !self.committed.contains(&placement.placement_id) {
                        write_kitty_transmit(
                            writer,
                            &placement.image,
                            placement.placement_id,
                            tmux,
                        )?;
                    }
                    write_kitty_placement(
                        writer,
                        placement.placement_id,
                        placement.placement_id,
                        placement.x as u32,
                        placement.y as u32,
                        placement.width,
                        placement.height,
                        placement.source_x,
                        placement.source_y,
                        placement.source_width,
                        placement.source_height,
                        kitty_placement_z(placement.placement_id),
                        tmux,
                    )?;
                    report.drawn.push(placement.placement_id);
                }
                self.committed = current;
            }
            Some(EmitProtocol::Sixel) => {
                let placements: Vec<ImagePlacement> = buffer.image_placements().to_vec();
                for placement in &placements {
                    let Some(payload) = placement_sixel_payload(placement) else {
                        continue;
                    };
                    write!(writer, "\x1b[{};{}H", placement.y + 1, placement.x + 1)?;
                    write_sixel_framed_payload(writer, &payload, tmux)?;
                    report.drawn.push(placement.placement_id);
                }
                for id in std::mem::take(&mut self.committed) {
                    write_kitty_delete(writer, id, None, true, tmux)?;
                    report.deleted.push(id);
                }
            }
        }
        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// drawImage contract shim — buffer.ts:383-432's `guard()` +
// `requireInteger` ladder. Rust's integer types make the TypeError arm
// unrepresentable; the range arms stay real.
// ---------------------------------------------------------------------------

/// The `requireInteger` / range failures `drawImage` throws before it
/// reaches native (buffer.ts:384-406). The TS arm is `RangeError`; the
/// TypeError arm (non-integer/NaN) cannot exist in the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawImageError {
    /// `requireInteger(name, min, max)` failed.
    Range(&'static str),
    /// `x + width` or `y + height` exceeded i32 (buffer.ts:402-405).
    Bounds,
}

impl std::fmt::Display for DrawImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrawImageError::Range(name) => write!(f, "{name} is outside the drawImage i32 range"),
            DrawImageError::Bounds => {
                f.write_str("image destination coordinates and dimensions exceed i32 bounds")
            }
        }
    }
}

impl std::error::Error for DrawImageError {}

/// The exact range ladder from buffer.ts:384-406.
#[allow(clippy::too_many_arguments)]
pub fn validate_draw_image_args(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixel_width: u32,
    pixel_height: u32,
    source_width: u32,
    source_height: u32,
) -> Result<(), DrawImageError> {
    let _ = (x, y); // i32 by construction: requireInteger(x/y, -2^31, 2^31-1)
    if !(1..=i32::MAX as u32).contains(&width) {
        return Err(DrawImageError::Range("width"));
    }
    if !(1..=i32::MAX as u32).contains(&height) {
        return Err(DrawImageError::Range("height"));
    }
    if pixel_width > i32::MAX as u32 {
        return Err(DrawImageError::Range("pixelWidth"));
    }
    if pixel_height > i32::MAX as u32 {
        return Err(DrawImageError::Range("pixelHeight"));
    }
    if source_width == 0 {
        return Err(DrawImageError::Range("sourceWidth"));
    }
    if source_height == 0 {
        return Err(DrawImageError::Range("sourceHeight"));
    }
    if i64::from(x) + i64::from(width) > i64::from(i32::MAX)
        || i64::from(y) + i64::from(height) > i64::from(i32::MAX)
    {
        return Err(DrawImageError::Bounds);
    }
    Ok(())
}

/// [`OptimizedBuffer::draw_image`] over a renderable's
/// [`DrawImageRequest`](crate::renderables::image::DrawImageRequest) —
/// the C3 tuple becomes the native call without re-deriving geometry.
pub fn draw_image_request(
    buffer: &mut OptimizedBuffer,
    image: &NativeImage,
    request: &crate::renderables::image::DrawImageRequest,
) -> Result<bool, DrawImageError> {
    buffer.draw_image(
        image,
        request.x,
        request.y,
        request.width,
        request.height,
        request.pixel_width,
        request.pixel_height,
        request.source_x,
        request.source_y,
        request.source_width,
        request.source_height,
        request.protocol,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{TerminalCapabilities, TerminalMultiplexer};

    fn raw(width: u32, height: u32, pixels: &[u8]) -> crate::image::RawImage {
        crate::image::RawImage {
            data: pixels.to_vec(),
            width,
            height,
            stride: width as usize * 4,
            format: PixelFormat::Rgba8,
        }
    }

    fn image(width: u32, height: u32, pixels: &[u8]) -> NativeImage {
        NativeImage::from_rgba(pixels, width, height).unwrap()
    }

    #[test]
    fn pixel_resolution_parse_matches_the_ts_ladder() {
        // terminal-capability-detection.test.ts:107-128.
        assert!(is_pixel_resolution_response("\x1b[4;720;1280t"));
        assert!(is_pixel_resolution_response("\x1b[4;0;0t"));
        assert!(!is_pixel_resolution_response("a"));
        assert!(!is_pixel_resolution_response("\x1b[A"));
        assert!(!is_pixel_resolution_response("\x1b[?1016;2$y"));
        assert_eq!(
            parse_pixel_resolution("\x1b[4;720;1280t"),
            Some(PixelResolution {
                width: 1280,
                height: 720
            })
        );
        assert_eq!(
            parse_pixel_resolution("\x1b[4;0;0t"),
            Some(PixelResolution {
                width: 0,
                height: 0
            })
        );
        assert_eq!(
            parse_pixel_resolution("\x1b[4;4294967295;4294967295t"),
            None
        );
        let giant = format!("\x1b[4;{};80t", "9".repeat(400));
        assert_eq!(parse_pixel_resolution(&giant), None);
        // Shape-only detection still says yes (the probe consumes it and
        // leaves the resolution absent).
        assert!(is_pixel_resolution_response(&giant));
    }

    #[test]
    fn cell_px_is_resolution_over_the_grid() {
        let resolution = PixelResolution {
            width: 800,
            height: 480,
        };
        let cell = resolution.cell_px(80, 24).unwrap();
        assert!((cell.width - 10.0).abs() < f64::EPSILON);
        assert!((cell.height - 20.0).abs() < f64::EPSILON);
        assert_eq!(resolution.cell_px(0, 24), None);
        assert_eq!(PIXEL_RESOLUTION_QUERY, b"\x1b[14t");
        let mut query = Vec::new();
        write_pixel_resolution_query(&mut query).unwrap();
        assert_eq!(query, b"\x1b[14t");
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(&[0xFF, 0x00, 0xFF]), "/wD/");
    }

    #[test]
    fn quadrant_block_matches_the_zig_raster() {
        // Red TL/TR, blue BL/BR → upper-half block, dark (blue) fg,
        // light (red) bg per renderQuadrantBlock's dark/light rule.
        let red = Rgba::from_ints(255, 0, 0, 255);
        let blue = Rgba::from_ints(0, 0, 255, 255);
        let result = render_quadrant_block([red, red, blue, blue]);
        // Dark set (blue, BL/BR) paints the half-block: index 3 ▄.
        assert_eq!(result.ch, 0x2584, "lower half block (dark fg set)");
        assert_eq!(result.fg, blue);
        assert_eq!(result.bg, red);

        let all = render_quadrant_block([red; 4]);
        assert_eq!(all.ch, 0x2588);
        assert_eq!(all.fg, red);
        assert_eq!(all.bg, red);
    }

    #[test]
    fn kitty_transmit_raw_rgb_and_rgba() {
        // terminal-image_test.zig:260-277.
        let opaque = image(1, 1, &[1, 2, 3, 255]);
        let mut out = Vec::new();
        write_kitty_transmit(&mut out, &opaque, 1, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("a=t,f=24,s=1,v=1,i=1,m=0,q=2;"));
        assert!(text.ends_with(";AQID\x1b\\"));

        let transparent = image(1, 1, &[1, 2, 3, 4]);
        let mut out = Vec::new();
        write_kitty_transmit(&mut out, &transparent, 1, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1,m=0,q=2;"));
        assert!(text.ends_with(";AQIDBA==\x1b\\"));
    }

    #[test]
    fn kitty_transmit_chunks_and_prefers_png_passthrough() {
        // 1025 RGBA pixels → 4100 bytes ⇒ two chunks (3072 + 1028).
        let pixels = vec![42u8; 1025 * 4];
        let value = image(1025, 1, &pixels);
        let mut out = Vec::new();
        write_kitty_transmit(&mut out, &value, 7, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("i=7,m=1,q=2;"));
        assert!(text.contains("\x1b_Gm=0,q=2;"));
        assert_eq!(text.matches("\x1b_G").count(), 2);

        // Encoded PNG passes through unchanged as f=100. A minimal valid
        // PNG header (signature + 1×1 RGBA IHDR + zero CRC) is enough
        // for `sniff`; the payload is retained verbatim.
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x00\x00\x00\x00";
        let png = PNG;
        let decoded = NativeImage::decode(png).unwrap();
        let mut out = Vec::new();
        write_kitty_transmit(&mut out, &decoded, 9, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("a=t,f=100,i=9,m=0,q=2;"));
        assert_eq!(text.matches("\x1b_G").count(), 1);
        // The payload is the exact base64 of the retained bytes.
        assert!(text.contains(&base64_encode(png)));
    }

    #[test]
    fn kitty_placement_and_delete_framing() {
        // renderer_test.zig:138-140 / terminal-image_test.zig:251-256.
        let mut out = Vec::new();
        write_kitty_placement(&mut out, 7, 8, 2, 3, 4, 5, 0, 0, 1, 1, -99, false).unwrap();
        assert_eq!(
            out,
            b"\x1b[4;3H\x1b_Ga=p,i=7,p=8,c=4,r=5,x=0,y=0,w=1,h=1,C=1,z=-99,q=2\x1b\\"
        );
        let mut out = Vec::new();
        write_kitty_delete(&mut out, 7, None, true, false).unwrap();
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        assert_eq!(kitty_placement_z(1), i32::MIN / 2 + 1);
        assert!(kitty_placement_z(u32::from(u16::MAX)) < 0);
    }

    #[test]
    fn kitty_tmux_wrapping_doubles_inner_escapes() {
        let value = image(1, 1, &[1, 2, 3, 4]);
        let mut out = Vec::new();
        write_kitty_transmit(&mut out, &value, 1, true).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(text.ends_with("\x1b\x1b\\\x1b\\"));
    }

    #[test]
    fn sixel_encoding_writes_palette_raster_and_terminator() {
        // terminal-image_test.zig:385-395.
        let value = raw(1, 1, &[255, 0, 0, 255]);
        let mut out = Vec::new();
        write_sixel(&mut out, &value, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1bP0;1;0q\"1;1;1;1"));
        assert!(text.ends_with("\x1b\\"));
        assert!(text.contains(";2;100;0;0"));
    }

    #[test]
    fn sixel_quantizer_is_deterministic_and_handles_transparency() {
        let value = raw(
            2,
            2,
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        );
        let first = quantize_sixel(&value, 4).unwrap();
        let second = quantize_sixel(&value, 4).unwrap();
        assert_eq!(first, second);
        assert!(!first.palette.is_empty());
        assert_eq!(first.indices.len(), 4);

        let transparent = raw(2, 2, &[0; 16]);
        let quantized = quantize_sixel(&transparent, 4).unwrap();
        assert!(quantized.palette.is_empty());
        assert!(quantized.indices.iter().all(|index| *index == 255));
    }

    #[test]
    fn sixel_framed_payload_wraps_dcs() {
        let mut out = Vec::new();
        write_sixel_framed_payload(&mut out, b"0;1;0q", false).unwrap();
        assert_eq!(out, b"\x1bP0;1;0q\x1b\\");
        let mut tmux = Vec::new();
        write_sixel_framed_payload(&mut tmux, b"0;1;0q", true).unwrap();
        assert_eq!(tmux, b"\x1bPtmux;\x1bP0;1;0q\x1b\x1b\\\x1b\\");
    }

    #[test]
    fn dimensions_gate_matches_image_limits() {
        assert!(dimensions_within_limits(1, 1));
        assert!(dimensions_within_limits(16_384, 1));
        assert!(!dimensions_within_limits(0, 1));
        assert!(!dimensions_within_limits(16_385, 1));
        assert!(!dimensions_within_limits(5000, 5001)); // 25,005,000 > 25M
    }

    fn rainbow() -> NativeImage {
        image(
            2,
            2,
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        )
    }

    fn ctx_default<'a>(caps: &'a TerminalCapabilities) -> ImageRenderCtx<'a> {
        ImageRenderCtx {
            capabilities: Some(caps),
            terminal_width: Some(4),
            terminal_height: Some(2),
            resolution: None,
        }
    }

    #[test]
    fn draw_image_reserves_markers_and_preserves_cell_colors() {
        // buffer_test.zig:17-36.
        let mut buffer = OptimizedBuffer::new(2, 2);
        buffer.set(
            0,
            0,
            crate::buffer::Cell {
                char: 'x' as u32,
                fg: Rgba::from_ints(1, 2, 3, 255),
                bg: Rgba::from_ints(4, 5, 6, 255),
                attributes: 7,
            },
        );
        let before = buffer.get(0, 0).unwrap();
        assert!(buffer
            .draw_image(
                &rainbow(),
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let marker = buffer.get(0, 0).unwrap();
        assert!(crate::buffer::is_image_char(marker.char));
        assert_eq!(crate::buffer::image_fallback_from_char(marker.char), 0);
        assert_eq!(marker.fg, before.fg);
        assert_eq!(marker.bg, before.bg);
        assert_eq!(marker.attributes, before.attributes);
        assert_eq!(buffer.image_placements().len(), 1);
        assert_eq!(buffer.image_placements()[0].placement_id, 1);
    }

    #[test]
    fn draw_image_contract_ladder() {
        let mut buffer = OptimizedBuffer::new(2, 2);
        let source = rainbow();
        let base = || (0, 0, 1, 1, 0, 0, 0, 0, 2, 2, ImageRenderProtocol::Auto);
        let draw = |buffer: &mut OptimizedBuffer,
                    width: u32,
                    height: u32,
                    pixel_width: u32,
                    source_width: u32| {
            buffer.draw_image(
                &source,
                0,
                0,
                width,
                height,
                pixel_width,
                0,
                0,
                0,
                source_width,
                2,
                ImageRenderProtocol::Auto,
            )
        };
        let (..) = base();
        assert_eq!(
            draw(&mut buffer, 0, 1, 0, 2),
            Err(DrawImageError::Range("width"))
        );
        assert_eq!(
            draw(&mut buffer, 1, 0, 0, 2),
            Err(DrawImageError::Range("height"))
        );
        assert_eq!(
            draw(&mut buffer, 1, 1, i32::MAX as u32 + 1, 2),
            Err(DrawImageError::Range("pixelWidth"))
        );
        assert_eq!(
            draw(&mut buffer, 1, 1, 0, 0),
            Err(DrawImageError::Range("sourceWidth"))
        );
        assert_eq!(
            buffer.draw_image(
                &source,
                i32::MAX,
                i32::MAX,
                2,
                2,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            ),
            Err(DrawImageError::Bounds)
        );
        // Native non-throwing refusals: source out of range.
        assert!(!buffer
            .draw_image(
                &source,
                0,
                0,
                1,
                1,
                0,
                0,
                1,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        // Fully clipped by the buffer bounds.
        assert!(!buffer
            .draw_image(
                &source,
                8,
                8,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
    }

    #[test]
    fn draw_image_honours_opacity_and_scissor_clip() {
        // buffer_test.zig:83-101 / :115-137.
        let mut buffer = OptimizedBuffer::new(4, 2);
        let source = image(4, 4, &[255, 0, 0, 255].repeat(16));
        buffer.push_scissor_rect(1, 0, 2, 2);
        assert!(buffer
            .draw_image(
                &source,
                -1,
                0,
                4,
                2,
                40,
                20,
                0,
                0,
                4,
                4,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let placement = buffer.image_placements()[0].clone();
        assert_eq!(placement.x, 1);
        assert_eq!(placement.width, 2);
        assert_eq!(placement.source_x, 2);
        assert_eq!(placement.source_width, 2);
        assert_eq!(placement.pixel_width, 20);

        buffer.clear_image_placements();
        buffer.clear_opacity();
        buffer.push_opacity(0.0);
        assert!(!buffer
            .draw_image(
                &rainbow(),
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
    }

    #[test]
    fn materialize_fallback_on_demand_and_flatten() {
        // buffer_test.zig:38-79.
        let mut buffer = OptimizedBuffer::new(1, 1);
        let source = rainbow();
        assert!(buffer
            .draw_image(
                &source,
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        buffer.materialize_image_fallback(1).unwrap();
        let cell = buffer.get(0, 0).unwrap();
        assert!(crate::buffer::is_image_char(cell.char));
        assert_ne!(crate::buffer::image_fallback_from_char(cell.char), 0);

        let mut buffer = OptimizedBuffer::new(1, 1);
        assert!(buffer
            .draw_image(
                &source,
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Kitty,
            )
            .unwrap());
        buffer.materialize_image_fallbacks().unwrap();
        let cell = buffer.get(0, 0).unwrap();
        assert!(!crate::buffer::is_image_char(cell.char));
        assert!(QUADRANT_CHARS.contains(&cell.char));
        assert!(buffer.image_placements().is_empty());
    }

    #[test]
    fn placement_holds_a_retained_image_handle() {
        // buffer_test.zig:102-113.
        let source = rainbow();
        let mut buffer = OptimizedBuffer::new(1, 1);
        assert!(buffer
            .draw_image(
                &source,
                0,
                0,
                1,
                1,
                1,
                1,
                0,
                0,
                1,
                1,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let mut disposed = source;
        disposed.dispose();
        assert_eq!(
            buffer.image_placements()[0]
                .image
                .raw(PixelFormat::Rgba8)
                .unwrap()
                .data[0],
            255
        );
    }

    #[test]
    fn frame_emitter_flattens_to_blocks_without_a_protocol() {
        let mut buffer = OptimizedBuffer::new(2, 1);
        let caps = TerminalCapabilities::default();
        let ctx = ctx_default(&caps);
        assert!(buffer
            .draw_image(
                &rainbow(),
                0,
                0,
                2,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let mut emitter = ImageFrameEmitter::new();
        let mut out = Vec::new();
        let report = emitter.render_frame(&mut out, &mut buffer, &ctx).unwrap();
        assert_eq!(report.protocol, Some(EmitProtocol::Blocks));
        assert!(report.materialized);
        assert!(out.is_empty());
        assert!(buffer.image_placements().is_empty());
        assert!(!crate::buffer::is_image_char(
            buffer.get(0, 0).unwrap().char
        ));
    }

    #[test]
    fn frame_emitter_sixel_probe_to_bytes() {
        let mut buffer = OptimizedBuffer::new(2, 1);
        let caps = TerminalCapabilities {
            sixel: true,
            ..TerminalCapabilities::default()
        };
        let ctx = ImageRenderCtx {
            capabilities: Some(&caps),
            terminal_width: Some(80),
            terminal_height: Some(24),
            resolution: Some((80, 80)),
        };
        assert!(buffer
            .draw_image(
                &image(1, 1, &[255, 0, 0, 255]),
                0,
                0,
                1,
                1,
                20,
                20,
                0,
                0,
                1,
                1,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let mut emitter = ImageFrameEmitter::new();
        let mut out = Vec::new();
        let report = emitter.render_frame(&mut out, &mut buffer, &ctx).unwrap();
        assert_eq!(report.protocol, Some(EmitProtocol::Sixel));
        assert_eq!(report.drawn, vec![1]);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[1;1H\x1bP0;1;0q\"1;1;20;20"));
        assert!(text.ends_with("\x1b\\"));
    }

    #[test]
    fn frame_emitter_kitty_places_and_deletes() {
        let mut buffer = OptimizedBuffer::new(2, 1);
        let caps = TerminalCapabilities {
            kitty_graphics: true,
            ..TerminalCapabilities::default()
        };
        let ctx = ImageRenderCtx {
            capabilities: Some(&caps),
            terminal_width: Some(80),
            terminal_height: Some(24),
            resolution: Some((800, 480)),
        };
        assert!(buffer
            .draw_image(
                &image(1, 1, &[1, 2, 3, 255]),
                0,
                0,
                1,
                1,
                2,
                2,
                0,
                0,
                1,
                1,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let mut emitter = ImageFrameEmitter::new();
        let mut first = Vec::new();
        let report = emitter.render_frame(&mut first, &mut buffer, &ctx).unwrap();
        assert_eq!(report.protocol, Some(EmitProtocol::Kitty));
        assert_eq!(report.drawn, vec![1]);
        let text = String::from_utf8(first).unwrap();
        assert!(text.contains("a=t,f=24"));
        assert!(text.contains("a=p,i=1,p=1"));
        assert_eq!(&emitter.committed(), &[1]);

        buffer.clear(Rgba::from_ints(0, 0, 0, 0));
        let mut second = Vec::new();
        let report = emitter
            .render_frame(&mut second, &mut buffer, &ctx)
            .unwrap();
        assert_eq!(report.protocol, None);
        assert_eq!(report.deleted, vec![1]);
        let text = String::from_utf8(second).unwrap();
        assert!(text.contains("a=d,d=I,i=1"));
        assert!(emitter.committed().is_empty());
    }

    #[test]
    fn frame_emitter_occlusion_and_mixed_protocols_fall_back() {
        let caps = TerminalCapabilities::default();
        let ctx = ctx_default(&caps);
        // Overlap: lower + upper at the same cell.
        let mut buffer = OptimizedBuffer::new(1, 1);
        assert!(buffer
            .draw_image(
                &image(1, 1, &[0, 0, 255, 255]),
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                1,
                1,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        assert!(buffer
            .draw_image(
                &image(1, 1, &[255, 0, 0, 0]),
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                1,
                1,
                ImageRenderProtocol::Auto,
            )
            .unwrap());
        let mut emitter = ImageFrameEmitter::new();
        let mut out = Vec::new();
        let report = emitter.render_frame(&mut out, &mut buffer, &ctx).unwrap();
        assert_eq!(report.protocol, Some(EmitProtocol::Blocks));
        assert!(report.materialized);

        // Mixed explicit protocols.
        let mut buffer = OptimizedBuffer::new(2, 1);
        assert!(buffer
            .draw_image(
                &rainbow(),
                0,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Kitty,
            )
            .unwrap());
        assert!(buffer
            .draw_image(
                &rainbow(),
                1,
                0,
                1,
                1,
                0,
                0,
                0,
                0,
                2,
                2,
                ImageRenderProtocol::Sixel,
            )
            .unwrap());
        let mut emitter = ImageFrameEmitter::new();
        let report = emitter.render_frame(&mut out, &mut buffer, &ctx).unwrap();
        assert_eq!(report.protocol, Some(EmitProtocol::Blocks));
    }

    #[test]
    fn draw_image_request_bridges_the_renderable_tuple() {
        use crate::renderables::image::{ImageRenderableOptions, ImageRenderableState};
        let source = image(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        let state = ImageRenderableState::new(ImageRenderableOptions {
            source: Some(source.clone()),
            ..ImageRenderableOptions::default()
        });
        let ctx = ImageRenderCtx::default();
        let request = state.draw_request(0, 0, 2, 1, &ctx).unwrap();
        let mut buffer = OptimizedBuffer::new(2, 1);
        assert!(buffer.draw_image_from_request(&source, &request).unwrap());
        assert_eq!(buffer.image_placements().len(), 1);
    }

    #[test]
    fn resolve_emit_protocol_auto_ladder() {
        let resolve = |caps: &TerminalCapabilities, resolution, pixel: (u32, u32)| {
            let ctx = ImageRenderCtx {
                capabilities: Some(caps),
                terminal_width: Some(80),
                terminal_height: Some(24),
                resolution,
            };
            resolve_emit_protocol(ImageRenderProtocol::Auto, &ctx, pixel.0, pixel.1)
        };
        let mut caps = TerminalCapabilities::default();
        assert_eq!(
            resolve(&caps, Some((800, 480)), (10, 10)),
            EmitProtocol::Blocks
        );
        caps.sixel = true;
        assert_eq!(
            resolve(&caps, Some((800, 480)), (10, 10)),
            EmitProtocol::Sixel
        );
        // Sixel without a pixel resolution → blocks (Image.ts:25/30).
        assert_eq!(resolve(&caps, None, (10, 10)), EmitProtocol::Blocks);
        // Placement dims over the native limit → blocks.
        let ctx = ImageRenderCtx {
            capabilities: Some(&caps),
            terminal_width: Some(80),
            terminal_height: Some(24),
            resolution: Some((800, 480)),
        };
        assert_eq!(
            resolve_emit_protocol(ImageRenderProtocol::Sixel, &ctx, 20_000, 20_000),
            EmitProtocol::Blocks
        );
        caps.kitty_graphics = true;
        assert_eq!(
            resolve(&caps, Some((800, 480)), (10, 10)),
            EmitProtocol::Kitty
        );
        // tmux forces blocks for auto (Image.ts:28).
        caps.multiplexer = TerminalMultiplexer::Tmux;
        assert_eq!(
            resolve(&caps, Some((800, 480)), (10, 10)),
            EmitProtocol::Blocks
        );
    }
}
