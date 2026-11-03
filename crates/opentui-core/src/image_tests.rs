//! Ports of the upstream image-family tests —
//! `packages/core/src/tests/image.test.ts`,
//! `tests/image-pool.test.ts`, and `tests/image-renderable.test.ts`
//! (0.5.14) — the cases that pin the contract surface of
//! `crate::image` / `crate::renderables::image`. The grid-rendering
//! cases (blocks glyphs, kitty APC bytes, buffer dumps) stay with
//! the native `bufferDrawImage` lane — GAPS-registered; the
//! async-loading cases (AbortController/fetch) collapse to their
//! sync-lane equivalents (see renderables/image.rs module docs).

use crate::image::{
    image_info, ImageError, ImageFormat, ImageLoadErrorCode, ImageSource, NativeImage,
    NativeImagePool, NativeImagePoolOptions, PixelFormat, PixelImportOptions, ResizeOptions,
    MAX_ENCODED_BYTES,
};
use crate::renderables::image::{
    resolve_image_render_protocol, ImageFit, ImageRenderCtx, ImageRenderProtocol,
    ImageRenderableOptions, ImageRenderableState,
};
use crate::renderer::{TerminalCapabilities, TerminalMultiplexer};

#[cfg(feature = "terminal-backend")]
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---- fixtures (synthetic headers matching the sniffed formats) --

/// A minimal 1x1 PNG header: signature + IHDR (color type 6 = RGBA).
fn png_header(width: u32, height: u32, color_type: u8) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&width.to_be_bytes());
    v.extend_from_slice(&height.to_be_bytes());
    v.push(8); // bit depth
    v.push(color_type);
    v.extend_from_slice(&[0, 0, 0]); // compression/filter/interlace
    v
}

fn gif_header(width: u16, height: u16) -> Vec<u8> {
    let mut v = b"GIF89a".to_vec();
    v.extend_from_slice(&width.to_le_bytes());
    v.extend_from_slice(&height.to_le_bytes());
    v.extend_from_slice(&[0x80, 0x00, 0x00, 0x3B]); // flags + bg + aspect + trailer
    v
}

/// JPEG: SOI + one APP0 segment + SOF0.
fn jpeg_header(width: u16, height: u16) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, len 4
    v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8]); // SOF0, len 17, precision
    v.extend_from_slice(&height.to_be_bytes());
    v.extend_from_slice(&width.to_be_bytes());
    v.push(1); // components
    v
}

fn webp_header(width: u32, height: u32) -> Vec<u8> {
    // VP8L lossless: signature 0x2F + 14-bit minus-one dims in LE bits
    let bits: u32 = ((width - 1) & 0x3FFF) | (((height - 1) & 0x3FFF) << 14);
    let mut body = vec![0x2F];
    body.extend_from_slice(&bits.to_le_bytes());
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WEBP");
    v.extend_from_slice(b"VP8L");
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(&body);
    v
}

fn solid_rgba(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        v.extend_from_slice(&px);
    }
    v
}

// ---- image.test.ts: ImageError / tables -------------------------

/// STATUS_MESSAGES / STATUS_CODES (image.ts:125-169).
#[test]
fn image_error_native_status_tables() {
    for (status, (message, code)) in [
        (0u8, ("ok", crate::image::ImageErrorCode::InternalError)),
        (
            1,
            (
                "invalid image handle",
                crate::image::ImageErrorCode::InvalidHandle,
            ),
        ),
        (
            2,
            (
                "unsupported image format",
                crate::image::ImageErrorCode::UnsupportedFormat,
            ),
        ),
        (
            6,
            (
                "image memory limit exceeded",
                crate::image::ImageErrorCode::MemoryLimit,
            ),
        ),
        (
            7,
            (
                "invalid image argument",
                crate::image::ImageErrorCode::InvalidArgument,
            ),
        ),
        (
            11,
            (
                "unsupported image feature",
                crate::image::ImageErrorCode::UnsupportedFeature,
            ),
        ),
        (
            12,
            ("image is retained", crate::image::ImageErrorCode::Busy),
        ),
    ] {
        let err = ImageError::native_for_test(status);
        assert_eq!(
            err.native_message(),
            format!("Native image operation failed: {message}")
        );
        assert!(
            matches!(err, ImageError::Native { code: c, .. } if c == code),
            "status {status}"
        );
    }
    // unknown status fallback (image.ts:178)
    let err = ImageError::native_for_test(99);
    assert_eq!(
        err.native_message(),
        "Native image operation failed: unknown status 99"
    );
}

/// image.test.ts:211 "inspects and decodes PNG data" +
/// :223 (opaque RGBA has_alpha=false) + :441 (every format).
#[test]
fn image_info_sniffs_every_format() {
    let png = image_info(&png_header(7, 5, 6)).unwrap();
    assert_eq!(png.format, ImageFormat::Png);
    assert_eq!((png.width, png.height), (7, 5));
    assert_eq!((png.source_width, png.source_height), (7, 5));
    assert!(png.has_alpha, "color type 6 is RGBA");
    let opaque = image_info(&png_header(7, 5, 2)).unwrap();
    assert!(!opaque.has_alpha, "color type 2 is RGB (test:223)");

    let gif = image_info(&gif_header(10, 4)).unwrap();
    assert_eq!(gif.format, ImageFormat::Gif);
    assert_eq!((gif.width, gif.height), (10, 4));

    let jpeg = image_info(&jpeg_header(320, 200)).unwrap();
    assert_eq!(jpeg.format, ImageFormat::Jpeg);
    assert_eq!((jpeg.width, jpeg.height), (320, 200));
    assert!(!jpeg.has_alpha);

    let webp = image_info(&webp_header(16, 9)).unwrap();
    assert_eq!(webp.format, ImageFormat::Webp);
    assert_eq!((webp.width, webp.height), (16, 9));
}

/// image.test.ts:234 "rejects malformed PNG data" + :428 (unsupported
/// formats) + :469 (malformed for every recognized format) + the
/// TypeError on empty data (image.ts:377).
#[test]
fn image_info_rejects_empty_unknown_and_malformed() {
    assert!(matches!(image_info(&[]), Err(ImageError::Invalid(_))));
    // unknown magic → unsupported-format (status 2)
    assert!(matches!(
        image_info(&[1, 2, 3, 4]),
        Err(ImageError::Native { status: 2, .. })
    ));
    // truncated PNG → malformed-data (status 4)
    assert!(matches!(
        image_info(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
        Err(ImageError::Native { status: 4, .. })
    ));
    // truncated JPEG (no SOF)
    assert!(matches!(
        image_info(&[0xFF, 0xD8, 0x00]),
        Err(ImageError::Native { status: 4, .. })
    ));
}

/// image.test.ts:457 "rejects animated WebP".
#[test]
fn image_info_rejects_animated_webp() {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&[0, 0, 0, 0]);
    v.extend_from_slice(b"WEBP");
    v.extend_from_slice(b"VP8X");
    v.extend_from_slice(&10u32.to_le_bytes());
    let mut body = vec![0x02]; // ANIM flag
    body.extend_from_slice(&[0, 0, 0]); // reserved
    body.extend_from_slice(&[9, 0, 0, 8, 0, 0]); // 10x9 canvas minus one
    v.extend_from_slice(&body);
    assert!(matches!(
        image_info(&v),
        Err(ImageError::Native { status: 2, .. })
    ));
}

// ---- image.test.ts: NativeImage --------------------------------

/// image.test.ts:532 "constructs and exports immutable RGBA images".
#[test]
fn from_rgba_constructs_and_raw_exports() {
    let image = NativeImage::from_rgba(&solid_rgba(2, 1, [12, 34, 56, 255]), 2, 1).unwrap();
    let info = image.info().unwrap();
    assert_eq!(info.format, ImageFormat::RawRgba);
    assert_eq!((info.width, info.height), (2, 1));
    assert!(!info.has_alpha);
    let raw = image.raw(PixelFormat::Rgba8).unwrap();
    assert_eq!(raw.data, solid_rgba(2, 1, [12, 34, 56, 255]));
    // bgra export swaps R and B (image.ts:606-618)
    let bgra = image.raw(PixelFormat::Bgra8).unwrap();
    assert_eq!(bgra.data[..4], [56, 34, 12, 255]);
}

/// image.test.ts:544 "fromPixels imports strided subarrays in both
/// formats and alpha modes" (the key arms) + :635 rejects invalid
/// geometry.
#[test]
fn from_pixels_strided_bgra_and_opaque() {
    // 2x2 stored with stride 12 (one padding pixel per row)
    let mut pixels = Vec::new();
    pixels.extend_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255, 0, 0, 0, 0]);
    pixels.extend_from_slice(&[7, 8, 9, 255, 10, 11, 12, 255, 0, 0, 0, 0]);
    let image = NativeImage::from_pixels(
        &pixels,
        2,
        2,
        &PixelImportOptions {
            stride: Some(12),
            ..Default::default()
        },
    )
    .unwrap();
    let raw = image.raw(PixelFormat::Rgba8).unwrap();
    assert_eq!(raw.stride, 8);
    assert_eq!(raw.data[..8], [1, 2, 3, 255, 4, 5, 6, 255]);

    // bgra8 import swaps to rgba (image.test.ts:544's both-formats)
    let bgra_src = [255, 0, 0, 255]; // stored B=255,G=0,R=0 → red
    let image = NativeImage::from_pixels(
        &bgra_src,
        1,
        1,
        &PixelImportOptions {
            format: Some(PixelFormat::Bgra8),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        image.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [0, 0, 255, 255]
    );

    // opaque forcing (image.test.ts:622)
    let image = NativeImage::from_pixels(
        &[9, 9, 9, 0],
        1,
        1,
        &PixelImportOptions {
            opaque_alpha: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!image.info().unwrap().has_alpha);
    assert_eq!(image.raw(PixelFormat::Rgba8).unwrap().data[3], 255);

    // geometry validation (image.test.ts:635)
    assert!(NativeImage::from_rgba(&[1, 2, 3, 4], 0, 1).is_err());
    assert!(NativeImage::from_rgba(&[1, 2, 3], 1, 1).is_err());
}

/// image.test.ts:672 "retains independently disposable references
/// without copying" + :864 dispose is idempotent and rejects later
/// operations.
#[test]
fn retain_dispose_lifecycle() {
    let mut image = NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap();
    let mut retained = image.retain().unwrap();
    assert_eq!(
        retained.raw(PixelFormat::Rgba8).unwrap().data,
        image.raw(PixelFormat::Rgba8).unwrap().data
    );
    image.dispose();
    image.dispose(); // idempotent
    assert!(matches!(image.info(), Err(ImageError::Disposed)));
    assert!(matches!(
        image.raw(PixelFormat::Rgba8),
        Err(ImageError::Disposed)
    ));
    // the retained reference outlives (image.test.ts:672)
    assert!(retained.info().is_ok());
    retained.dispose();
}

/// image.test.ts:684 "transfers native RGBA pixels without copying" —
/// the transfer is REFUSED while another wrapper retains the body
/// (image.ts:623-628).
#[test]
fn take_raw_refused_while_retained() {
    let mut image = NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap();
    let _retained = image.retain().unwrap();
    assert!(matches!(
        image.take_raw(),
        Err(ImageError::RetainedByBuffers)
    ));
    drop(_retained);
    let owned = image.take_raw().unwrap();
    assert_eq!(owned.data[..4], [1, 2, 3, 255]);
    owned.dispose();
    assert!(matches!(image.info(), Err(ImageError::Disposed)));
}

/// image.test.ts:713 "takeRaw reports deferred PNG decode errors" —
/// encoded-only bodies fail materialization with the
/// unsupported-feature status (the port's registered narrowing of
/// the compressed-decode lane).
#[test]
fn encoded_only_images_defer_pixel_materialization() {
    let image = NativeImage::decode(&png_header(1, 1, 6)).unwrap();
    assert_eq!(image.info().unwrap().width, 1);
    // the retained payload is byte-identical (the kitty a=t,f=100
    // passthrough, image-renderable.test.ts:476-507)
    assert_eq!(image.encoded().unwrap(), &png_header(1, 1, 6)[..]);
    image.ensure_encoded_png().unwrap();
    assert!(matches!(
        image.raw(PixelFormat::Rgba8),
        Err(ImageError::Native { status: 11, .. })
    ));
    // raw sources cannot re-encode (the encoder is the same lane)
    let raw_image = NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap();
    assert!(matches!(
        raw_image.ensure_encoded_png(),
        Err(ImageError::Native { status: 11, .. })
    ));
}

/// image.test.ts:722 "supports exact transforms, extraction, and
/// extension" (key arms) + :772/:784 metadata updates.
#[test]
fn extract_extend_and_transforms() {
    let image = NativeImage::from_rgba(
        &[1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255],
        2,
        2,
    )
    .unwrap();
    let crop = image
        .extract(&crate::image::ExtractOptions {
            left: 1,
            top: 0,
            width: 1,
            height: 1,
        })
        .unwrap();
    assert_eq!(
        crop.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [2, 2, 2, 255]
    );
    assert!(!crop.info().unwrap().has_alpha, "opaque crop (test:772)");

    let extended = image
        .extend(&crate::image::ExtendOptions {
            top: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        (extended.width().unwrap(), extended.height().unwrap()),
        (2, 3)
    );
    assert!(
        extended.info().unwrap().has_alpha,
        "transparent padding (test:784 family)"
    );

    let rotated = image.rotate(90).unwrap();
    assert_eq!(
        (rotated.width().unwrap(), rotated.height().unwrap()),
        (2, 2)
    );
    assert_eq!(
        rotated.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [3, 3, 3, 255],
        "90° CW moves the bottom-left to the top-left"
    );
    assert!(image.rotate(45).is_err());
    let flipped = image.flip().unwrap();
    assert_eq!(
        flipped.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [3, 3, 3, 255]
    );
    let flopped = image.flop().unwrap();
    assert_eq!(
        flopped.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [2, 2, 2, 255]
    );
}

/// image.test.ts:796 "preserves aspect ratio when one resize
/// dimension is omitted" + :807 validates derived dimensions.
#[test]
fn resize_aspect_derivation() {
    let image = NativeImage::from_rgba(&solid_rgba(4, 2, [9, 9, 9, 255]), 4, 2).unwrap();
    let wide = image
        .resize(&ResizeOptions {
            width: Some(8),
            kernel: None,
            height: None,
        })
        .unwrap();
    assert_eq!((wide.width().unwrap(), wide.height().unwrap()), (8, 4));
    let tall = image
        .resize(&ResizeOptions {
            height: Some(6),
            ..Default::default()
        })
        .unwrap();
    assert_eq!((tall.width().unwrap(), tall.height().unwrap()), (12, 6));
    assert!(image.resize(&ResizeOptions::default()).is_err());
    // nearest keeps pixels exact (image.test.ts:760)
    let nearest = image
        .resize(&ResizeOptions {
            width: Some(8),
            height: Some(4),
            kernel: Some(crate::image::ResizeKernel::Nearest),
        })
        .unwrap();
    assert_eq!(
        nearest.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [9, 9, 9, 255]
    );
}

/// image.test.ts:816 "composites in linear light" (the identity arm:
/// opacity 1 source-over replaces) + :846 destination-over keeps the
/// destination where the source is transparent.
#[test]
fn composite_blend_modes() {
    let dst = NativeImage::from_rgba(&[0, 0, 255, 255], 1, 1).unwrap();
    let src = NativeImage::from_rgba(&[255, 0, 0, 255], 1, 1).unwrap();
    let out = dst.composite(&src, &Default::default()).unwrap();
    let px = out.raw(PixelFormat::Rgba8).unwrap().data;
    assert_eq!(&px[..3], &[255, 0, 0]);
    assert!(!out.info().unwrap().has_alpha);

    // destination-over under a transparent source keeps the dst
    let transparent_src = NativeImage::from_rgba(&[0, 0, 0, 0], 1, 1).unwrap();
    let out = dst
        .composite(
            &transparent_src,
            &crate::image::CompositeOptions {
                blend: Some(crate::image::BlendMode::DestinationOver),
                ..Default::default()
            },
        )
        .unwrap();
    let px = out.raw(PixelFormat::Rgba8).unwrap().data;
    assert_eq!(&px[..4], &[0, 0, 255, 255]);

    // opacity bounds (image.ts:589)
    assert!(dst
        .composite(
            &src,
            &crate::image::CompositeOptions {
                opacity: Some(1.5),
                ..Default::default()
            }
        )
        .is_err());
}

/// image.test.ts:882 "loads encoded bytes and ArrayBuffer sources" +
/// :999 (local paths) + :1060 (unsupported schemes) + :1015
/// (oversized files).
#[test]
fn load_bytes_paths_and_schemes() {
    let png = png_header(1, 1, 6);
    let via_bytes = NativeImage::load(ImageSource::Bytes(&png)).unwrap();
    assert_eq!(via_bytes.info().unwrap().format, ImageFormat::Png);

    let dir = std::env::temp_dir().join("opentui-image-tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("t.png");
    std::fs::write(&path, &png).unwrap();
    let via_path = NativeImage::load(ImageSource::Path(&path)).unwrap();
    assert_eq!(via_path.info().unwrap().width, 1);
    assert!(matches!(
        NativeImage::load(ImageSource::Path(&dir.join("missing.png"))),
        Err(e) if e.code == ImageLoadErrorCode::FileRead
    ));

    // data: URL (image.test.ts:895) — hand-encode the PNG header
    let mut b64 = String::new();
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in png.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        b64.push(T[(n >> 18) as usize & 63] as char);
        b64.push(T[(n >> 12) as usize & 63] as char);
        b64.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        b64.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    let url = format!("data:image/png;base64,{b64}");
    let via_data = NativeImage::load(ImageSource::Url(&url)).unwrap();
    assert_eq!(via_data.info().unwrap().format, ImageFormat::Png);

    // unsupported scheme (image.test.ts:1060) — ftp never loads
    assert!(matches!(
        NativeImage::load(ImageSource::Url("ftp://example.com/x.png")),
        Err(e) if e.code == ImageLoadErrorCode::UnsupportedUrlScheme
    ));

    // oversized local file rejected before reading (image.test.ts:1015)
    let big = dir.join("big.png");
    let _ = std::fs::write(&big, b"x");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // sparse file larger than MAX_ENCODED_BYTES
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_ENCODED_BYTES + 1).unwrap();
        drop(f);
        let _ = std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o644));
        assert!(matches!(
            NativeImage::load(ImageSource::Path(&big)),
            Err(e) if e.code == ImageLoadErrorCode::FileRead
                && e.cause.as_ref().unwrap().to_string().contains("memory limit")
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- image-pool.test.ts ----------------------------------------

/// image-pool.test.ts:7 "publishes shared pixel views through busy
/// and reused slots".
#[test]
fn pool_busy_and_reused_slots() {
    let mut pool = NativeImagePool::new(&NativeImagePoolOptions {
        width: 1,
        height: 1,
        capacity: Some(2),
    })
    .unwrap();
    // capacity+1 publications with every frame held: the first two
    // fill the slots, the third finds both busy → drop (image.ts:692)
    let mut outstanding = Vec::new();
    for _ in 0..3 {
        outstanding.push(pool.publish_rgba(&[10, 20, 30, 255]).unwrap());
    }
    assert!(outstanding[0].is_some() && outstanding[1].is_some());
    assert!(
        outstanding[2].is_none(),
        "every slot busy at capacity → dropped"
    );
    // dispose all outstanding; the pool must accept publishes again —
    // freed slots are reused (test:7)
    for frame in outstanding.iter_mut().flatten() {
        frame.dispose();
    }
    let frame = pool.publish_rgba(&[9, 9, 9, 255]).unwrap();
    assert!(frame.is_some(), "freed slots are reused (test:7)");
    // refresh updates the slot's pixels in place (imageUpdatePixels)
    let refreshed = pool.publish_rgba(&[1, 2, 3, 255]).unwrap().unwrap();
    assert_eq!(
        refreshed.raw(PixelFormat::Rgba8).unwrap().data[..4],
        [1, 2, 3, 255]
    );
}

/// image-pool.test.ts:187 "dispose cancels future publications but
/// leaves outstanding frames alive".
#[test]
fn pool_dispose_leaves_outstanding_frames_alive() {
    let mut pool = NativeImagePool::new(&NativeImagePoolOptions {
        width: 1,
        height: 1,
        capacity: Some(1),
    })
    .unwrap();
    let mut frame = pool.publish_rgba(&[5, 6, 7, 255]).unwrap().unwrap();
    pool.dispose();
    assert!(matches!(
        pool.publish_rgba(&[1, 2, 3, 255]),
        Err(ImageError::Invalid(_))
    ));
    assert!(frame.info().is_ok(), "the outstanding frame stays alive");
    frame.dispose();
}

/// image-pool.test.ts:202 "validates pool bounds and native limits".
#[test]
fn pool_validates_bounds() {
    assert!(NativeImagePool::new(&NativeImagePoolOptions {
        width: 0,
        height: 1,
        capacity: None
    })
    .is_err());
    assert!(NativeImagePool::new(&NativeImagePoolOptions {
        width: 1,
        height: 1,
        capacity: Some(9)
    })
    .is_err());
    assert!(NativeImagePool::new(&NativeImagePoolOptions {
        width: 1,
        height: 1,
        capacity: Some(0)
    })
    .is_err());
}

/// image-pool.test.ts:89 "publishPixels validates input before
/// reporting a busy pool".
#[test]
fn pool_validates_pixels_before_busy_scan() {
    let mut pool = NativeImagePool::new(&NativeImagePoolOptions {
        width: 2,
        height: 1,
        capacity: Some(1),
    })
    .unwrap();
    // occupy the only slot
    let _held = pool
        .publish_rgba(&[1, 2, 3, 255, 4, 5, 6, 255])
        .unwrap()
        .unwrap();
    // invalid pixel length errors even though the pool is busy
    assert!(pool.publish_rgba(&[1, 2, 3]).is_err());
}

// ---- image-renderable.test.ts ----------------------------------

fn caps(
    kitty: bool,
    sixel: bool,
    tmux: bool,
    protocol: ImageRenderProtocol,
) -> TerminalCapabilities {
    TerminalCapabilities {
        kitty_graphics: kitty,
        sixel,
        multiplexer: if tmux {
            TerminalMultiplexer::Tmux
        } else {
            TerminalMultiplexer::None
        },
        image_protocol: protocol,
        ..TerminalCapabilities::default()
    }
}

/// image-renderable.test.ts:342-358 "resolves automatic and
/// configured image protocols" (all five assertions).
#[test]
fn resolve_image_render_protocol_matrix() {
    use ImageRenderProtocol as P;
    assert_eq!(
        resolve_image_render_protocol(P::Sixel, None, false),
        P::Blocks
    );
    assert_eq!(
        resolve_image_render_protocol(P::Auto, Some(&caps(false, false, false, P::Sixel)), false),
        P::Blocks
    );
    assert_eq!(
        resolve_image_render_protocol(P::Auto, Some(&caps(true, false, false, P::Auto)), false),
        P::Kitty
    );
    assert_eq!(
        resolve_image_render_protocol(P::Auto, Some(&caps(false, true, false, P::Auto)), true),
        P::Sixel
    );
    assert_eq!(
        resolve_image_render_protocol(P::Auto, Some(&caps(true, false, true, P::Auto)), true),
        P::Blocks
    );
}

/// image-renderable.test.ts:208-234 "defaults to aspect-preserving
/// fit" + "calculates fit, cover, and fill using terminal cell
/// aspect".
#[test]
fn fitted_size_fit_cover_fill() {
    let mut state = ImageRenderableState::new(ImageRenderableOptions::default());
    assert_eq!(state.fit(), ImageFit::Fit);
    // square source, cell aspect 2 → display aspect 2 (test:215):
    // fit → min(60/2, 40) = 30 rows
    assert_eq!(state.get_fitted_size(60, 40, 2.0, 4, 4), (60, 30));

    state.set_fit(Some(ImageFit::Cover));
    // cover: max scale → width hits 80 at height 40 (test:228)
    assert_eq!(state.get_fitted_size(60, 40, 2.0, 4, 4), (80, 40));
    state.set_fit(Some(ImageFit::Fill));
    assert_eq!(state.get_fitted_size(60, 40, 2.0, 4, 4), (60, 40));
    // non-positive inputs collapse to zero (Image.ts:147)
    assert_eq!(state.get_fitted_size(0, 40, 2.0, 4, 4), (0, 0));
    assert_eq!(state.get_fitted_size(60, 40, 2.0, 0, 4), (0, 0));
}

/// image-renderable.test.ts:509-523 "accepts legacy capability and
/// render context shapes" — absent resolution fields → aspect 2.
#[test]
fn cell_aspect_ratio_legacy_default_two() {
    let state = ImageRenderableState::new(ImageRenderableOptions::default());
    assert_eq!(state.cell_aspect_ratio(&ImageRenderCtx::default()), 2.0);
    let ctx = ImageRenderCtx {
        terminal_width: Some(80),
        terminal_height: Some(24),
        resolution: Some((640, 480)),
        capabilities: None,
    };
    // 8x20 px cells → aspect 20/8 = 2.5
    assert_eq!(state.cell_aspect_ratio(&ctx), 2.5);
    // zero resolution falls back (Image.ts:37-38)
    let bad = ImageRenderCtx {
        resolution: Some((0, 0)),
        ..ctx
    };
    assert_eq!(state.cell_aspect_ratio(&bad), 2.0);
}

/// image-renderable.test.ts:324-340 "exposes requested and effective
/// image protocols" (the no-caps arm: auto → blocks).
#[test]
fn effective_protocol_tracks_requested_and_caps() {
    let mut state = ImageRenderableState::new(ImageRenderableOptions {
        protocol: Some(ImageRenderProtocol::Blocks),
        ..Default::default()
    });
    let ctx = ImageRenderCtx::default();
    assert_eq!(state.protocol(), ImageRenderProtocol::Blocks);
    assert_eq!(state.effective_protocol(&ctx), ImageRenderProtocol::Blocks);
    state.set_protocol(Some(ImageRenderProtocol::Kitty));
    assert!(
        state.render_requested,
        "protocol change requests a render (Image.ts:125)"
    );
    assert_eq!(state.effective_protocol(&ctx), ImageRenderProtocol::Kitty);
    state.set_protocol(None);
    assert_eq!(state.protocol(), ImageRenderProtocol::Auto);
    assert_eq!(state.effective_protocol(&ctx), ImageRenderProtocol::Blocks);
}

/// image-renderable.test.ts:104-121 "loads encoded bytes and retains
/// the image" (sync shape) + :145-158 replacement disposes the
/// previous retain.
#[test]
fn renderable_loads_and_replaces_atomically() {
    let loaded_formats = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = loaded_formats.clone();
    let mut state = ImageRenderableState::new(ImageRenderableOptions {
        source: Some(NativeImage::decode(&png_header(2, 2, 6)).unwrap()),
        on_load: Some(Box::new(move |img| {
            sink.borrow_mut().push(img.info().unwrap().format);
        })),
        ..Default::default()
    });
    assert!(!state.loading());
    assert!(state.load_error().is_none());
    assert_eq!(*loaded_formats.borrow(), [ImageFormat::Png]);

    // replacement swaps atomically (test:602-615). Upstream's
    // `previous` is the SAME JS wrapper the renderable disposed; the
    // Rust port's clone is a RETAIN (image.test.ts:672's independent
    // reference) — it must stay alive after the state dropped its
    // own reference (the JS disposed-wrapper arm is not expressible
    // through a clone).
    let previous = state.image().unwrap().clone();
    state.set_source(Some(NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap()));
    assert!(
        previous.info().is_ok(),
        "an independent retain outlives the swap"
    );
    assert_eq!(
        state.image().unwrap().info().unwrap().format,
        ImageFormat::RawRgba
    );

    // clearing the source disposes the retained image (test:636-682)
    state.set_source(None);
    assert!(state.image().is_none());
    assert!(state.load_error().is_none());
    assert!(state.render_requested);

    // destroy releases the state's reference (test:145-158, :247-255):
    // the independent retain stays alive (image.test.ts:672) — the
    // state no longer holds the image.
    state.set_source(Some(NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap()));
    let held = state.image().unwrap().clone();
    state.destroy_self();
    assert!(state.image().is_none());
    assert!(held.info().is_ok());
    assert!(state.is_destroyed());
}

/// image-renderable.test.ts:525-540 "reports decode failures without
/// installing an image" + :617-634 "keeps the previous image when a
/// replacement fails" (the disposed-source arm of Image.ts:93-95).
#[test]
fn renderable_reports_failures_and_keeps_previous() {
    // a disposed source: retain fails → error, no image (test:160-175)
    let mut disposed = NativeImage::from_rgba(&[1, 2, 3, 255], 1, 1).unwrap();
    disposed.dispose();
    let errors = std::rc::Rc::new(std::cell::RefCell::new(0));
    let sink = errors.clone();
    let state = ImageRenderableState::new(ImageRenderableOptions {
        source: Some(disposed),
        on_error: Some(Box::new(move |_| {
            *sink.borrow_mut() += 1;
        })),
        ..Default::default()
    });
    assert_eq!(*errors.borrow(), 1);
    assert!(state.image().is_none());
    assert!(state.load_error().is_some());
    assert!(!state.loading());
}

/// The `renderSelf` draw-request geometry (Image.ts:174-219): center
/// placement (:181-182), pixel dims (:183-189), cover crop
/// (:194-204).
#[test]
fn draw_request_geometry() {
    // fit: square source, cell aspect 2, box 60x40 → fitted 60x30
    // → centered at y+5 (test:215 shape)
    let state = ImageRenderableState::new(ImageRenderableOptions {
        source: Some(NativeImage::from_rgba(&solid_rgba(2, 2, [0, 0, 0, 255]), 2, 2).unwrap()),
        ..Default::default()
    });
    let ctx = ImageRenderCtx {
        terminal_width: Some(80),
        terminal_height: Some(24),
        resolution: Some((640, 480)),
        capabilities: None,
    };
    // cell aspect 640/80 x 480/24 → 8x20 px cells → aspect 2.5;
    // square source → display aspect 2.5 → fit min(60/2.5, 40) = 24
    let req = state.draw_request(0, 0, 60, 40, &ctx).unwrap();
    assert_eq!((req.x, req.y), (0, 8));
    assert_eq!((req.width, req.height), (60, 24));
    // pixel dims: 60/80 of 640 = 480 px, 24/24 of 480 = 480 px
    assert_eq!((req.pixel_width, req.pixel_height), (480, 480));
    assert_eq!(
        (
            req.source_x,
            req.source_y,
            req.source_width,
            req.source_height
        ),
        (0, 0, 2, 2)
    );
    assert_eq!(req.protocol, ImageRenderProtocol::Auto);
    // no resolution → pixel dims zero (Image.ts:184-189)
    let req = state
        .draw_request(0, 0, 60, 40, &ImageRenderCtx::default())
        .unwrap();
    assert_eq!((req.pixel_width, req.pixel_height), (0, 0));

    // cover: source wider than the target aspect crops horizontally,
    // centered (Image.ts:197-199)
    let cover = ImageRenderableState::new(ImageRenderableOptions {
        source: Some(NativeImage::from_rgba(&solid_rgba(4, 1, [0, 0, 0, 255]), 4, 1).unwrap()),
        fit: Some(ImageFit::Cover),
        ..Default::default()
    });
    // legacy ctx (aspect 2): target aspect = 2/(1*2) = 1, source
    // aspect 4 → crop to source_h * 1 = 1 wide? no: crop WIDTH to
    // round(source_h * target_aspect) = 1, x offset (4-1)/2 = 1
    let req = cover
        .draw_request(10, 20, 2, 1, &ImageRenderCtx::default())
        .unwrap();
    assert_eq!((req.width, req.height), (2, 1));
    assert_eq!(
        (
            req.source_x,
            req.source_y,
            req.source_width,
            req.source_height
        ),
        (1, 0, 1, 1)
    );

    // no image / zero box → no request (Image.ts:175)
    let empty = ImageRenderableState::new(ImageRenderableOptions::default());
    assert!(empty.draw_request(0, 0, 10, 10, &ctx).is_none());
    assert!(state.draw_request(0, 0, 0, 10, &ctx).is_none());
}

// ---- terminal teardown gate (session.rs, image lane C3) ---------

/// `note_kitty_graphics` arms the delete-all prologue
/// (`\x1b_Ga=d,d=a\x1b\\`), and a default session never emits it —
/// the A1 teardown byte stream is otherwise unchanged.
#[cfg(feature = "terminal-backend")]
#[test]
fn teardown_kitty_graphics_delete_all_gate() {
    use crate::terminal::session::SetupOptions;
    use crate::terminal::tty::tests::MockTty;
    use crate::terminal::Terminal;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct SharedBuf(Rc<RefCell<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // default: no image session → epilogue starts with the mode
    // resets (renderer.zig:642-645 family), NOT the graphics delete
    let out = SharedBuf::default();
    {
        let _t = Terminal::new(MockTty::new(), out.clone(), &SetupOptions::default()).unwrap();
    }
    assert!(
        !contains_bytes(&out.0.borrow(), b"\x1b_Ga=d,d=a"),
        "no image session → no graphics teardown bytes"
    );

    // armed: the delete-all leads the epilogue (renderer.zig:602-618
    // ordering — deletes precede resetState)
    let out = SharedBuf::default();
    {
        let mut t = Terminal::new(MockTty::new(), out.clone(), &SetupOptions::default()).unwrap();
        t.note_kitty_graphics();
    } // Drop runs teardown
    let stream = out.0.borrow();
    // delete-all leads the EPILOGUE — immediately before resetState's
    // showCursor (renderer.zig:602-618 ordering)
    let needle = b"\x1b_Ga=d,d=a\x1b\\\x1b[?25h";
    let idx = stream
        .windows(needle.len())
        .position(|w| w == &needle[..])
        .expect("delete-all then the epilogue");
    assert!(idx > 0, "the setup burst came first");
}
