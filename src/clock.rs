//! The time, for code shared with the browser build: the system clock on Linux; in the
//! browser (WebAssembly, which has no clock of its own), the page's `performance.now()`,
//! which `web/` registers with [`set_source`].

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

#[cfg(target_arch = "wasm32")]
pub use page::{Instant, set_source};

#[cfg(target_arch = "wasm32")]
mod page {
    use std::sync::OnceLock;
    use std::time::Duration;

    static SOURCE: OnceLock<fn() -> f64> = OnceLock::new();

    /// Where the time comes from: milliseconds since some fixed start.
    pub fn set_source(milliseconds: fn() -> f64) {
        let _ = SOURCE.set(milliseconds);
    }

    /// A moment on the page's clock, with the parts of `std::time::Instant` fnstrip uses.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct Instant(Duration);

    impl Instant {
        #[must_use]
        pub fn now() -> Self {
            let milliseconds = SOURCE.get().map_or(0.0, |source| source());
            Self(Duration::from_secs_f64(milliseconds.max(0.0) / 1000.0))
        }

        #[must_use]
        pub fn elapsed(&self) -> Duration {
            Self::now().0.saturating_sub(self.0)
        }

        #[must_use]
        pub fn duration_since(&self, earlier: Self) -> Duration {
            self.0.saturating_sub(earlier.0)
        }
    }
}
