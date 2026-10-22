//! `TimeToFirstDraw` — port of `packages/solid/src/time-to-first-draw.tsx`.
//!
//! Upstream (time-to-first-draw.tsx:5-17): the module registers the
//! core `TimeToFirstDrawRenderable` as the custom intrinsic
//! `time_to_first_draw` via `extend(...)` AT IMPORT TIME, and exports
//! the `TimeToFirstDraw` component that spreads its props onto that
//! intrinsic.
//!
//! Rust has no import side effects, so registration is explicit:
//! [`register_time_to_first_draw`] (idempotent — later registrations
//! win, like upstream's `Object.assign` catalogue merge). The builder
//! [`time_to_first_draw`] is the component: a
//! `Tag::Custom("time_to_first_draw")` element whose props map onto
//! [`opentui_core::renderables::time_to_first_draw::TimeToFirstDrawOptions`]
//! (fg/label/precision + the renderable option set: width/height and
//! the layout props).

use crate::builder::{custom, ElementBuilder};
use crate::element::PropValue;
use crate::extend::{extend, CustomElement};
use crate::props::{apply_layout, prop_get, PropMap};
use opentui_core::color::parse_color;
use opentui_core::layout::Dimension;
use opentui_core::renderable::{Content, Renderable};
use opentui_core::renderables::time_to_first_draw::TimeToFirstDrawOptions;
use opentui_core::renderer::CliRenderer;

/// The registered intrinsic name
/// (`extend({ time_to_first_draw: ... })`, time-to-first-draw.tsx:11).
pub const TIME_TO_FIRST_DRAW_TAG: &str = "time_to_first_draw";

/// `extend({ time_to_first_draw: TimeToFirstDrawRenderable })`
/// (time-to-first-draw.tsx:11) — registers the build + patch surface.
/// Idempotent (the catalogue is process-global; re-registration wins).
pub fn register_time_to_first_draw() {
    extend(vec![CustomElement::new(
        TIME_TO_FIRST_DRAW_TAG,
        |renderer: &mut CliRenderer, props: &PropMap, id: &str| -> Renderable {
            renderer.make_time_to_first_draw(id, &options_from_props(props))
        },
    )
    .with_patch(
        |renderer: &mut CliRenderer, live_id: &str, new_props: &PropMap, _prev: &PropMap| {
            // upstream setProperty → the class setters (fg/textLabel/decimals,
            // TimeToFirstDraw.ts:64-90); each requests a render on change
            let Some(node) = renderer.root.inner.find_mut(live_id) else {
                return;
            };
            let Content::TimeToFirstDraw(state) = &mut node.content else {
                return;
            };
            if let Some(v) = prop_get(new_props, "fg") {
                if let Some(s) = str_of(v) {
                    state.set_fg(parse_color(s));
                }
            }
            if let Some(v) = prop_get(new_props, "label") {
                if let Some(s) = str_of(v) {
                    state.set_text_label(s);
                }
            }
            if let Some(v) = prop_get(new_props, "precision") {
                if let Some(n) = num_of(v) {
                    state.set_decimals(n);
                }
            }
        },
    )]);
}

/// The `TimeToFirstDraw` component (time-to-first-draw.tsx:15-17):
/// `<time_to_first_draw {...props} />` — a builder over the registered
/// intrinsic. Call [`register_time_to_first_draw`] first.
#[must_use]
pub fn time_to_first_draw() -> ElementBuilder {
    custom(TIME_TO_FIRST_DRAW_TAG)
}

fn str_of(v: &PropValue) -> Option<&str> {
    match v {
        PropValue::Str(s) | PropValue::Color(s) | PropValue::Dimension(s) => Some(s),
        PropValue::Text(s) => Some(s),
        _ => None,
    }
}

fn num_of(v: &PropValue) -> Option<f64> {
    match v {
        PropValue::Num(n) => Some(f64::from(*n)),
        PropValue::Str(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn dimension_of(v: &PropValue) -> Option<Dimension> {
    crate::props::parse_dimension(v)
}

/// The prop map → [`TimeToFirstDrawOptions`] (the JSX props are the
/// renderable's constructor options).
#[must_use]
pub fn options_from_props(props: &PropMap) -> TimeToFirstDrawOptions {
    let mut opts = TimeToFirstDrawOptions::default();
    for (name, v) in props {
        match name.as_str() {
            "fg" | "color" => opts.fg = str_of(v).map(parse_color),
            "label" => opts.label = str_of(v).map(str::to_owned),
            "precision" => opts.precision = num_of(v),
            "width" => opts.width = dimension_of(v),
            "height" => opts.height = dimension_of(v),
            n => apply_layout(&mut opts.layout, n, v),
        }
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::SolidRenderer;
    use crate::Signal;
    use opentui_core::renderer::CliRendererConfig;

    fn frame_of(sr: &SolidRenderer) -> String {
        sr.renderer.current_render_buffer.char_frame()
    }

    fn sr(w: u32, h: u32) -> SolidRenderer {
        SolidRenderer::new(CliRendererConfig {
            width: w,
            height: h,
            ..Default::default()
        })
    }

    /// The component mounts through the registered intrinsic and
    /// renders the timestamp line (time-to-first-draw.tsx:11,15-17 +
    /// TimeToFirstDraw.ts:102).
    #[test]
    fn mounts_and_renders_the_timestamp() {
        register_time_to_first_draw();
        let mut s = sr(24, 1);
        s.renderer.set_performance_now(Some(0.0));
        s.render_root(
            time_to_first_draw()
                .key("ttfd")
                .prop("label", "boot")
                .prop("precision", 0)
                .into_element(),
        );
        assert!(frame_of(&s).contains("boot: 0ms"));
    }

    /// Prop patches route through the class setters (fg/textLabel/
    /// decimals — TimeToFirstDraw.ts:64-90) and re-render.
    #[test]
    fn patches_label_and_precision() {
        register_time_to_first_draw();
        let mut s = sr(30, 2);
        s.renderer.set_performance_now(Some(3.0));
        let label = Signal::new(crate::element::PropValue::Str("t0".into()));
        s.render_root(
            time_to_first_draw()
                .key("ttfd")
                .prop("precision", 1)
                .dyn_prop("label", crate::builder::dyn_value(label.clone()))
                .into_element(),
        );
        assert!(frame_of(&s).contains("t0: 3.0ms"));

        label.set(crate::element::PropValue::Str("boot".into()));
        s.flush();
        assert!(frame_of(&s).contains("boot: 3.0ms"));
    }

    /// `getComponentCatalogue` lists the registered intrinsic
    /// (catalogue.ts:137-139).
    #[test]
    fn catalogue_lists_the_tag() {
        register_time_to_first_draw();
        assert!(
            crate::extend::get_component_catalogue().contains(&TIME_TO_FIRST_DRAW_TAG.to_owned())
        );
    }
}
