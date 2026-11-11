# Contributing

Thanks for looking. fnstrip is small and young, so the most useful help right now is
**trying it on your Mac and saying how it went.**

## Reports

Open an issue with:

- your model: `cat /sys/class/dmi/id/product_name` (for example `MacBookPro13,3`);
- your fnstrip version: `fnstrip --version` (or `~/.local/bin/fnstrip --version`);
- your T1Bridge version and Linux distribution;
- what you did, what you expected, and what happened;
- the log: `journalctl --user -u t1-touchbar.service -b` (look it over for anything
  private first).

"It works on my MacBookPro14,2" is a welcome report too.

Security problems: see [SECURITY.md](SECURITY.md), not a public issue.

## Changes

- Open an issue first for anything bigger than a fix, so we can agree on the shape.
- Keep it std-only and free of `unsafe`, like the rest of the crate.
- Before a pull request: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test`. CI runs the same.
- The project page draws its bar with fnstrip's own code compiled to WebAssembly (`web/`):
  after a drawing change, `tools/build_web.sh` rebuilds it (needs the `wasm32-unknown-unknown`
  Rust target).
- Say how you tested it, and on which model. Drawing and input changes need a run on a real
  Touch Bar; the tests can't see the panel.
- AI-assisted changes are fine (fnstrip itself is written that way): say so in the pull
  request, and make sure you understand and have tested what you submit.

By contributing, you agree your work is released under the project's MIT license.
