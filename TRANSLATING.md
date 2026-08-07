# Translating HushMic

HushMic's interface (tray menu, mic-test window, About dialog, desktop
notifications) is translatable. Translations live in this repository as
[Fluent](https://projectfluent.org/) files under
`crates/hushmic/i18n/<language>/hushmic.ftl` and are embedded into the binary
at build time. English (`en`) is the complete reference catalog; anything a
translation does not cover falls back to English.

## How to contribute

The preferred way is [Hosted Weblate](https://hosted.weblate.org/) — edit
translations in the browser, no tooling needed. Weblate contributions arrive
here as pull requests with your authorship.

Editing the `.ftl` file directly and opening a pull request works just as
well. The format is plain text:

```ftl
tray-quit = Quit
about-version = Version { $version }
```

Keep placeables like `{ $version }` unchanged; translate everything around
them. Multi-line values continue on indented lines.

## What stays English

Diagnostic output (`hushmic --doctor`), CLI/log messages, and technical error
details relayed inside notifications or window toasts are deliberately not
translated: they end up in bug reports, which the maintainer has to be able
to read.

## Trying your translation

`HUSHMIC_LANG=<code> hushmic --tray` forces a language regardless of the
system locale. Otherwise HushMic follows `LANG`/`LC_MESSAGES`.

## Fonts

Latin, Cyrillic and Greek scripts render out of the box. Scripts that need
extra fonts in the mic-test window (CJK, Arabic, ...) are not wired up yet —
if you want to translate into one, open an issue first so font embedding can
be sorted out with you.
