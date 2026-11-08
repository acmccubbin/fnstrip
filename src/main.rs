//! Touch Bar renderer selected via ~/.config/t1bridge/renderer.
//! t1bridge's launcher falls back to its own built-in renderer if this process exits.

use std::env;

use fnstrip::builtin_renderer;
use fnstrip::desktop_provider::{PROVIDER_ENVIRONMENT, resolve_provider_path};

fn main() {
    let provider_value = env::var_os(PROVIDER_ENVIRONMENT);
    let provider_path = resolve_provider_path(provider_value.as_deref());
    builtin_renderer::run_forever(provider_path.as_deref());
}
