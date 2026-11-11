//! Touch Bar renderer selected via ~/.config/t1bridge/renderer.
//! t1bridge's launcher falls back to its own built-in renderer if this process exits.

use std::env;

use fnstrip::builtin_renderer;
use fnstrip::desktop_provider::{PROVIDER_ENVIRONMENT, resolve_provider_path};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The `t1bridge` release this is built against: the tag in `Cargo.toml` (a test keeps them in step).
const T1BRIDGE: &str = "v0.1.12";

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("fnstrip {VERSION} (for T1Bridge {T1BRIDGE})");
            return;
        }
        Some("--help" | "-h") => {
            println!(
                "fnstrip {VERSION}: a Touch Bar renderer for T1 MacBook Pros, on T1Bridge {T1BRIDGE}.\n\n\
                 T1Bridge starts it through ~/.config/t1bridge/renderer; it isn't run by hand.\n\
                 Settings: ~/.config/fnstrip/config.toml (see config/default.toml).\n\
                 Log: journalctl --user -u t1-touchbar.service\n\n\
                 Options:\n  -V, --version  print the version\n  -h, --help     print this"
            );
            return;
        }
        _ => {}
    }
    eprintln!("fnstrip {VERSION} starting (for T1Bridge {T1BRIDGE})");
    let provider_value = env::var_os(PROVIDER_ENVIRONMENT);
    let provider_path = resolve_provider_path(provider_value.as_deref());
    builtin_renderer::run_forever(provider_path.as_deref());
}

#[cfg(test)]
mod tests {
    #[test]
    fn t1bridge_version_matches_cargo_toml() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains(&format!("tag = \"{}\"", super::T1BRIDGE)),
            "update T1BRIDGE in src/main.rs to the tag in Cargo.toml"
        );
    }
}
