# Bundled application fonts

Bokheim embeds these fonts so desktop and AppImage builds do not depend on the
ignored `reference/plato` development checkout or on fonts installed by the
host system.

- Noto Sans Regular and Bold are from the Noto Fonts project and are licensed
  under the SIL Open Font License 1.1. See `LICENSE-NOTO.txt`.
- Bodoni Moda Regular and SemiBold (version 2.005, static instances) are from
  https://github.com/indestructible-type/Bodoni and are licensed under the
  SIL Open Font License 1.1. See `LICENSE-BODONI-MODA.txt`. The radial graph
  uses it for prominent headings and graph labels with the serif UI choice.
- NV Garamond and NV Jost Regular, Bold, Italic and BoldItalic are from
  https://github.com/nicoverbruggen/ebook-fonts at revision
  `f875aca16d3e871a01af52c854b67bc7f7abce9d` (`fonts/core`). These reader fonts
  are variants of EB Garamond and Jost, licensed under the SIL Open Font License
  1.1. See `LICENSE-NV-GARAMOND.txt` and `LICENSE-NV-JOST.txt`. NV Garamond
  is used by the interface and reader; NV Jost is retained for comparison previews.
- Raleway Regular, Bold, Italic and BoldItalic (4.101, upstream static TTFs)
  are from https://github.com/theleagueof/raleway/releases/tag/4.101 and are
  licensed under the SIL Open Font License 1.1. See `LICENSE-RALEWAY.txt`.
  The interface and reader use this sans-serif family on every platform.

The shared `ui-components` font registration installs NV Garamond, Raleway and
Bodoni Moda once per application, before either the browser or reader uses them.
The sans-serif UI choice uses Raleway for both body text and prominent headings.

Libertinus is no longer bundled by the application. Historical comparison
assets and their license are in `apps/ui/design-tokens/assets/fonts/libertinus-comparison`.

The files are redistributed without modification.

Droid Sans Fallback Regular is the compact bundled CJK fallback (3,451,900
bytes), from the Android Open Source Project's `android13-qpr2-s10-release`
`data/fonts/DroidSansFallback.ttf` (Git blob
`1099b177c881c96bf298989dcdca5fda7841133d`). It is redistributed without
modification under Apache License 2.0; see `LICENSE-DROID.txt`.
Source: https://android.googlesource.com/platform/frameworks/base/+/android13-qpr2-s10-release/data/fonts/
SHA-256: `21b96a0377f067833a93af3082eb28d4ffab7a8cd46bfd513286f1d64b7b0949`.

It provides 33,275 Unicode mappings, covering common Chinese, Japanese kana,
Korean Hangul and Bopomofo. It lacks CJK Extensions A/B and separate regional
Han glyph forms. Only regular weight is included; bold and oblique depend on
platform synthesis. An installed regional font can be chosen in the reader.
The reader registers this face once per application when the first book is
opened, before constructing the text shaper. It supplies ordinary glyph
fallback for every book, including books without language metadata. Structured
BCP 47 book language metadata enables separate Latin and CJK font controls. The
CJK choices are Book (the default) and Custom. Book
preserves the publisher's CJK family even when a different Latin font is chosen,
and uses the embedded face for missing glyphs. The Custom picker checks the
actual face's glyph coverage and excludes Latin-only fonts. Without
CJK metadata those controls stay hidden and the usual book font choices apply.
Chapter text is never scanned to guess a language.
CJK typeface overrides and installed font names are device-local preferences,
separate from Latin book settings.
