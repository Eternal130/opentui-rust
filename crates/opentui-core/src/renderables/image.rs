//! `ImageRenderable` — `packages/core/src/renderables/Image.ts`
//! (0.5.14), contract-level port.
//!
//! Observability ruling: upstream's `renderSelf` (Image.ts:174-219)
//! delegates every grid effect to `buffer.drawImage`
//! (buffer.ts:383-432 → native `bufferDrawImage`), and the Rust
//! port's `OptimizedBuffer` has no image rasterizer (that is the
//! native-render lane's, GAPS-registered). What IS portable 1:1 is
//! everything the renderable itself decides: the source lifecycle
//! (retain/replace/dispose/destroy, Image.ts:66-100/221-255), the
//! fit geometry (`getFittedSize`/`cellAspectRatio`, Image.ts:132-159),
//! protocol resolution (`resolveImageRenderProtocol`, Image.ts:20-32),
//! and the exact `drawImage` argument tuple `renderSelf` computes —
//! exposed here as [`ImageRenderableState::draw_request`] and pinned
//! by unit tests (ports of tests/image-renderable.test.ts:208-234/
//! 324-358/509-540/602-640). Async loading is sync in the Rust port
//! (`NativeImage` decodes on construction): `loading` is therefore
//! observable only as "a source is set and unresolved" — the abort /
//! in-flight-request semantics (image-renderable.test.ts:636-785)
//! collapse to generation-counter no-ops, registered narrowing.
//!
//! Upstream line citations throughout refer to Image.ts.

use crate::image::{ImageError, NativeImage};
use crate::renderer::TerminalCapabilities;

/// `ImageFit` (Image.ts:7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFit {
    #[default]
    Fit,
    Cover,
    Fill,
}

/// `ImageRenderProtocol` (types.ts:108).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageRenderProtocol {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Blocks,
}

impl ImageRenderProtocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ImageRenderProtocol::Auto => "auto",
            ImageRenderProtocol::Kitty => "kitty",
            ImageRenderProtocol::Sixel => "sixel",
            ImageRenderProtocol::Blocks => "blocks",
        }
    }
}

/// `resolveImageRenderProtocol` (Image.ts:20-32):
/// 1. an explicit request wins, except sixel without a pixel
///    resolution falls back to blocks (:25);
/// 2. `auto` defers to the configured `capabilities.image_protocol`
///    with the same sixel gate (:26-27);
/// 3. configured-auto → tmux (or unknown caps) is blocks (:28),
///    kitty graphics wins (:29), sixel needs the resolution (:30),
///    else blocks (:31).
#[must_use]
pub fn resolve_image_render_protocol(
    requested: ImageRenderProtocol,
    capabilities: Option<&TerminalCapabilities>,
    has_resolution: bool,
) -> ImageRenderProtocol {
    use ImageRenderProtocol as P;
    let gate = |p: P| {
        if p == P::Sixel && !has_resolution {
            P::Blocks
        } else {
            p
        }
    };
    if requested != P::Auto {
        return gate(requested);
    }
    let configured = capabilities.map_or(P::Auto, |c| c.image_protocol);
    if configured != P::Auto {
        return gate(configured);
    }
    match capabilities {
        None => P::Blocks,
        Some(caps) => {
            if caps.multiplexer == crate::renderer::TerminalMultiplexer::Tmux {
                return P::Blocks;
            }
            if caps.kitty_graphics {
                return P::Kitty;
            }
            if caps.sixel && has_resolution {
                return P::Sixel;
            }
            P::Blocks
        }
    }
}

/// The render-context slice `ImageRenderable` reads
/// (`RenderContext`, types.ts): `terminalWidth`/`terminalHeight`/
/// `resolution` drive `pixelResolution` (Image.ts:34-39) and
/// `cellAspectRatio` (Image.ts:132-138). Legacy shapes with the
/// fields absent are `None` — the aspect falls back to 2
/// (image-renderable.test.ts:509-523).
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageRenderCtx<'a> {
    pub capabilities: Option<&'a TerminalCapabilities>,
    pub terminal_width: Option<u32>,
    pub terminal_height: Option<u32>,
    /// `(width, height)` in pixels.
    pub resolution: Option<(u32, u32)>,
}

impl ImageRenderCtx<'_> {
    /// `pixelResolution(ctx)` (Image.ts:34-39): the resolution counts
    /// only when both terminal dimensions are positive and the
    /// resolution itself is positive.
    #[must_use]
    pub fn pixel_resolution(&self) -> Option<(u32, u32)> {
        let (tw, th) = (
            self.terminal_width.unwrap_or(0),
            self.terminal_height.unwrap_or(0),
        );
        if tw == 0 || th == 0 {
            return None;
        }
        match self.resolution {
            Some((w, h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        }
    }

    /// Build the context from a probed capability snapshot (the
    /// capture now carries the pixel-resolution reply alongside the
    /// live terminal grid). Legacy callers keep constructing the
    /// fields directly.
    #[must_use]
    pub fn from_capabilities(
        capabilities: &TerminalCapabilities,
        terminal_width: u32,
        terminal_height: u32,
    ) -> ImageRenderCtx<'_> {
        ImageRenderCtx {
            capabilities: Some(capabilities),
            terminal_width: Some(terminal_width),
            terminal_height: Some(terminal_height),
            resolution: capabilities.pixel_resolution,
        }
    }

    /// The cell size derived from the resolution probe (`cellPx`).
    #[must_use]
    pub fn cell_px(&self) -> Option<crate::image_render::CellPx> {
        let (width, height) = self.pixel_resolution()?;
        crate::image_render::PixelResolution { width, height }
            .cell_px(self.terminal_width?, self.terminal_height?)
    }
}

/// The `onLoad` callback shape (Image.ts:16).
pub type ImageOnLoad = Box<dyn FnMut(&NativeImage)>;
/// The `onError` callback shape (Image.ts:17).
pub type ImageOnError = Box<dyn FnMut(&ImageError)>;

/// `ImageRenderableOptions` (Image.ts:12-18).
#[derive(Default)]
pub struct ImageRenderableOptions {
    pub source: Option<NativeImage>,
    pub fit: Option<ImageFit>,
    pub protocol: Option<ImageRenderProtocol>,
    pub on_load: Option<ImageOnLoad>,
    pub on_error: Option<ImageOnError>,
}

/// The `drawImage` argument tuple `renderSelf` computes
/// (Image.ts:205-218) — the paint contract this port pins; the
/// buffer-side rasterization (blocks glyphs / kitty APC / sixel DCS)
/// stays with the native-render lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawImageRequest {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub protocol: ImageRenderProtocol,
}

/// `ImageRenderable`'s state (Image.ts:41-255) — the port's
/// state-struct shape (see module docs for the sync-lane ruling).
pub struct ImageRenderableState {
    source: Option<NativeImage>,
    image: Option<NativeImage>,
    fit: ImageFit,
    protocol: ImageRenderProtocol,
    load_error: Option<ImageError>,
    /// The load-in-flight marker (`_loadController !== null`,
    /// Image.ts:161-163). Sync port: only a source that FAILED to
    /// retain/resolve keeps a transient pending marker; there is no
    /// await window.
    loading: bool,
    on_load: Option<ImageOnLoad>,
    on_error: Option<ImageOnError>,
    /// `requestRender()` observability (Image.ts:80/114/125/243).
    pub render_requested: bool,
    /// `destroySelf` ran (Image.ts:247-255).
    destroyed: bool,
}

impl Default for ImageRenderableState {
    fn default() -> Self {
        Self::new(ImageRenderableOptions::default())
    }
}

impl ImageRenderableState {
    /// The constructor (Image.ts:53-60): defaults fit=fit
    /// (:55), protocol=auto (:56), wire callbacks, apply `source`.
    pub fn new(options: ImageRenderableOptions) -> Self {
        let mut state = ImageRenderableState {
            source: None,
            image: None,
            fit: options.fit.unwrap_or_default(),
            protocol: options.protocol.unwrap_or_default(),
            load_error: None,
            loading: false,
            on_load: options.on_load,
            on_error: options.on_error,
            render_requested: false,
            destroyed: false,
        };
        if let Some(source) = options.source {
            state.set_source(Some(source));
        }
        state
    }

    /// `source` setter (Image.ts:66-100): same-source no-op (:68),
    /// abort the previous load + drop its pending image (:70-73),
    /// `None` clears the error/image (:75-82), a `NativeImage`
    /// source is retained then loaded (:88-95, :97).
    pub fn set_source(&mut self, source: Option<NativeImage>) {
        if self.destroyed {
            return;
        }
        if let (Some(a), Some(b)) = (&source, &self.source) {
            if same_body(a, b) {
                return;
            }
        }
        // abort the in-flight load: dispose the pending retain
        // (Image.ts:70-73). In the sync port a live pending cannot
        // exist here — the previous load resolved inside its own
        // set_source.
        self.loading = false;
        self.source = source;
        let Some(source) = self.source.clone() else {
            // Image.ts:75-82 — clear everything, request a render
            self.load_error = None;
            self.image = None;
            self.render_requested = true;
            return;
        };
        self.load_error = None;
        // retain (Image.ts:90-92) — a disposed source rejects here
        let retained = source.retain();
        match retained {
            Ok(pending) => {
                // the load resolves immediately in the sync port
                // (Image.ts:221-245): pending graduates, previous
                // disposes (:242), render + onLoad fire.
                if let Some(previous) = self.image.as_mut() {
                    previous.dispose();
                }
                self.image = Some(pending);
                self.render_requested = true;
                if let Some(cb) = self.on_load.as_mut() {
                    if let Some(image) = self.image.as_ref() {
                        cb(image);
                    }
                }
            }
            Err(error) => {
                // the async reject path (Image.ts:225-231): keep the
                // previous image, record the error, fire onError.
                self.loading = false;
                self.load_error = Some(error.clone());
                if let Some(cb) = self.on_error.as_mut() {
                    cb(&error);
                }
            }
        }
    }

    /// `image` getter (Image.ts:102-104).
    #[must_use]
    pub fn image(&self) -> Option<&NativeImage> {
        self.image.as_ref()
    }

    pub fn fit(&self) -> ImageFit {
        self.fit
    }

    /// `fit` setter (Image.ts:110-115): default `fit`, no-op when
    /// equal, requestRender on change.
    pub fn set_fit(&mut self, value: Option<ImageFit>) {
        let next = value.unwrap_or_default();
        if self.fit == next {
            return;
        }
        self.fit = next;
        self.render_requested = true;
    }

    pub fn protocol(&self) -> ImageRenderProtocol {
        self.protocol
    }

    /// `protocol` setter (Image.ts:121-126).
    pub fn set_protocol(&mut self, value: Option<ImageRenderProtocol>) {
        let next = value.unwrap_or_default();
        if self.protocol == next {
            return;
        }
        self.protocol = next;
        self.render_requested = true;
    }

    /// `effectiveProtocol` (Image.ts:128-130).
    #[must_use]
    pub fn effective_protocol(&self, ctx: &ImageRenderCtx<'_>) -> ImageRenderProtocol {
        resolve_image_render_protocol(
            self.protocol,
            ctx.capabilities,
            ctx.pixel_resolution().is_some(),
        )
    }

    /// `cellAspectRatio` (Image.ts:132-138): fallback 2 when the
    /// pixel resolution is unknown; else
    /// `cellHeight / cellWidth` guarded to positive.
    #[must_use]
    pub fn cell_aspect_ratio(&self, ctx: &ImageRenderCtx<'_>) -> f64 {
        let Some((rw, rh)) = ctx.pixel_resolution() else {
            return 2.0;
        };
        let (tw, th) = (
            ctx.terminal_width.expect("pixel_resolution guarantees >0") as f64,
            ctx.terminal_height.expect("pixel_resolution guarantees >0") as f64,
        );
        let cell_w = rw as f64 / tw;
        let cell_h = rh as f64 / th;
        if cell_w > 0.0 && cell_h > 0.0 {
            cell_h / cell_w
        } else {
            2.0
        }
    }

    /// `getFittedSize` (Image.ts:140-159): zero when any input is
    /// non-positive (:147), `fill` returns the target (:148), else
    /// scale by `min` (fit) / `max` (cover) against the
    /// aspect-corrected display width, each side clamped to ≥1
    /// (:150-158).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn get_fitted_size(
        &self,
        target_width: u32,
        target_height: u32,
        cell_aspect_ratio: f64,
        source_width: u32,
        source_height: u32,
    ) -> (u32, u32) {
        if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
            return (0, 0);
        }
        if self.fit == ImageFit::Fill {
            return (target_width, target_height);
        }
        let display_aspect =
            (f64::from(source_width) / f64::from(source_height)) * cell_aspect_ratio;
        let scale = match self.fit {
            ImageFit::Fit => {
                (f64::from(target_width) / display_aspect).min(f64::from(target_height))
            }
            ImageFit::Cover => {
                (f64::from(target_width) / display_aspect).max(f64::from(target_height))
            }
            ImageFit::Fill => unreachable!(),
        };
        (
            (display_aspect * scale).round().max(1.0) as u32,
            scale.round().max(1.0) as u32,
        )
    }

    /// `loading` getter (Image.ts:161-163).
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// `loadError` getter (Image.ts:165-167).
    #[must_use]
    pub fn load_error(&self) -> Option<&ImageError> {
        self.load_error.as_ref()
    }

    /// The `renderSelf` geometry (Image.ts:174-219) as the exact
    /// `drawImage` request tuple: guard on image + positive box
    /// (:175), cover short-circuits the fitted size to the box then
    /// crops the source around the target aspect (:177, :194-204),
    /// center placement (:181-182), pixel dims from the resolution
    /// (:183-189), the requested protocol rides through (:217).
    #[must_use]
    pub fn draw_request(
        &self,
        x_origin: i32,
        y_origin: i32,
        width: u32,
        height: u32,
        ctx: &ImageRenderCtx<'_>,
    ) -> Option<DrawImageRequest> {
        let image = self.image.as_ref()?;
        if width == 0 || height == 0 {
            return None;
        }
        let (fitted_w, fitted_h) = if self.fit == ImageFit::Cover {
            (width, height)
        } else {
            let (sw, sh) = (image.width().ok()?, image.height().ok()?);
            self.get_fitted_size(width, height, self.cell_aspect_ratio(ctx), sw, sh)
        };
        if fitted_w == 0 || fitted_h == 0 {
            return None;
        }
        let x = x_origin + i32::try_from((width - fitted_w) / 2).unwrap_or(i32::MAX / 2);
        let y = y_origin + i32::try_from((height - fitted_h) / 2).unwrap_or(i32::MAX / 2);
        let (pixel_width, pixel_height) = match ctx.pixel_resolution() {
            Some((rw, rh)) => {
                let tw = ctx.terminal_width.expect("pixel_resolution guarantees >0");
                let th = ctx.terminal_height.expect("pixel_resolution guarantees >0");
                (
                    (((fitted_w as u64) * rw as u64) / tw as u64)
                        .max(1)
                        .min(u32::MAX as u64) as u32,
                    (((fitted_h as u64) * rh as u64) / th as u64)
                        .max(1)
                        .min(u32::MAX as u64) as u32,
                )
            }
            None => (0, 0),
        };
        let (iw, ih) = (image.width().ok()?, image.height().ok()?);
        let (mut source_x, mut source_y) = (0u32, 0u32);
        let (mut source_w, mut source_h) = (iw, ih);
        if self.fit == ImageFit::Cover {
            // Image.ts:194-204 — crop the larger axis around the
            // target aspect, centered.
            let target_aspect =
                f64::from(width) / (f64::from(height) * self.cell_aspect_ratio(ctx));
            let source_aspect = f64::from(source_w) / f64::from(source_h);
            if source_aspect > target_aspect {
                source_w = (((f64::from(source_h)) * target_aspect).round() as u32).max(1);
                source_x = (iw - source_w) / 2;
            } else {
                source_h = (((f64::from(source_w)) / target_aspect).round() as u32).max(1);
                source_y = (ih - source_h) / 2;
            }
        }
        Some(DrawImageRequest {
            x,
            y,
            width: fitted_w,
            height: fitted_h,
            pixel_width,
            pixel_height,
            source_x,
            source_y,
            source_width: source_w,
            source_height: source_h,
            protocol: self.protocol,
        })
    }

    /// `destroySelf` (Image.ts:247-255): abort the load, dispose the
    /// retained image, clear state.
    pub fn destroy_self(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        self.loading = false;
        if let Some(image) = self.image.as_mut() {
            image.dispose();
        }
        self.image = None;
        self.source = None;
        self.load_error = None;
    }

    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }
}

/// Body identity for the same-source no-op (Image.ts:68 — upstream
/// compares the wrapper reference; the port compares the shared Rc
/// body).
fn same_body(a: &NativeImage, b: &NativeImage) -> bool {
    a.ptr_eq(b)
}
