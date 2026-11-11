//! A customizable Touch Bar renderer, forked from t1bridge's `t1-touchbar` crate at v0.1.12
//! (MIT, <https://github.com/standardagents/t1bridge>, see LICENSES/t1bridge-MIT.txt).
//! The modules below started as byte-identical copies of upstream; changes are ours.
//!
//! The hardware-facing modules are left out of WebAssembly builds: there, `web/` uses the
//! drawing code (rows, icons, themes, meters, startup animation) for the project page.

// The browser build uses only the drawing code; the rest of the hardware side goes unused there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(not(target_arch = "wasm32"))]
mod brightness;
#[cfg(not(target_arch = "wasm32"))]
pub mod builtin_renderer;
pub mod clock;
pub mod config;
#[cfg(test)]
mod demo_media;
#[cfg(not(target_arch = "wasm32"))]
pub mod desktop_provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod extras;
pub mod framebuffer;
pub mod gesture;
pub mod intro;
#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
mod media;
pub mod meters;
#[cfg(not(target_arch = "wasm32"))]
mod overlay_labels;
#[cfg(not(target_arch = "wasm32"))]
pub mod overlay_state;
#[cfg(target_arch = "wasm32")]
pub mod page_feeds;
#[cfg(not(target_arch = "wasm32"))]
mod record;
pub mod screensaver;
mod stock_art;
// Generated upstream art; the slider row no longer draws the −/+ icons.
pub mod spectrum;
#[allow(dead_code)]
mod stock_icons;
mod stock_labels;
pub mod stock_slice;
pub mod theme;
#[cfg(not(target_arch = "wasm32"))]
pub mod touch_id_overlay;
mod value_glyphs;
mod wordmark;
