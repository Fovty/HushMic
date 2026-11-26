<!-- Thanks for contributing to HushMic! -->

## What & why

<!-- What does this change, and what problem does it solve? Link any issue: "Fixes #123". -->

## How it was tested

<!-- Distro / desktop you tested on, and what you checked. For audio, window or
     tray changes, note that you verified them in the running app. -->

## Checklist

- [ ] `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass, and `cargo test` for what the change touches
- [ ] Tested on a real desktop (window, tray and virtual mic behave), not just the build
- [ ] Docs / README updated if user-facing behavior changed
- [ ] New or changed interface text is in the English catalog (`crates/hushmic/i18n/en/hushmic.ftl`); other languages come through Weblate
