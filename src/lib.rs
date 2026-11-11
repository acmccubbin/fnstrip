//! A customizable Touch Bar renderer, forked from t1bridge's `t1-touchbar` crate at v0.1.12
//! (MIT, <https://github.com/standardagents/t1bridge>, see LICENSES/t1bridge-MIT.txt).
//! The modules below started as byte-identical copies of upstream; changes are ours.

mod brightness;
pub mod builtin_renderer;
pub mod config;
#[cfg(test)]
mod demo_media;
pub mod desktop_provider;
pub mod extras;
pub mod framebuffer;
pub mod gesture;
mod intro;
mod live;
mod media;
mod meters;
mod overlay_labels;
pub mod overlay_state;
mod record;
mod screensaver;
mod stock_art;
// Generated upstream art; the slider row no longer draws the −/+ icons.
#[allow(dead_code)]
mod stock_icons;
mod stock_labels;
pub mod stock_slice;
pub mod theme;
pub mod touch_id_overlay;
mod value_glyphs;
mod wordmark;
