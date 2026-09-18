# Localization

V2.6 ([issue #74](https://github.com/jm2/balun/issues/74)) remains open. The first
slice provides the shared catalog, startup selection, application menu, its
tooltip/accessibility label, and the About description. A follow-on slice adds
navigation titles and player-control copy. The remaining interface, CLI, errors,
desktop metadata, pluralization, and layout evidence remain pending.

## Catalog and startup contract

Balun uses [rust-i18n 4](https://docs.rs/rust-i18n/4.2.2/rust_i18n/) and
[sys-locale](https://docs.rs/sys-locale/0.3.2/sys_locale/), matching Tributary's
framework and initial locale set:

| Catalog | Language |
| --- | --- |
| `de` | German |
| `en` | English |
| `es` | Spanish |
| `fr` | French |
| `it` | Italian |
| `ja` | Japanese |
| `ko` | Korean |
| `nl` | Dutch |
| `pl` | Polish |
| `pt-BR` | Brazilian Portuguese |
| `ru` | Russian |
| `zh-CN` | Simplified Chinese |
| `zh-TW` | Traditional Chinese |

`locales/<locale>.yml` files compile into one GTK-free library backend. There is no
runtime catalog file loading, profile override, download, or live language switch.
`build.rs` watches the catalog directory so a translation-only edit rebuilds the
embedded tables. The YAML parser is used at build time and by catalog tests.

Desktop startup initializes the backend before creating widgets. Like Tributary,
one named, joined thread gives the generated initializer an explicit 8 MiB stack
as the catalogs grow. Subsequent initialization calls retain the first result and
selected locale. Platform packaging probes return before localization startup.

Selection uses the first OS preference returned by `sys-locale`, normalizes
underscores and ASCII case, and removes POSIX encoding/modifier suffixes. It
prefers an exact catalog, maps `zh-Hans[-…]` to `zh-CN` and `zh-Hant[-…]` to
`zh-TW`, then tries a shipped base language. For example, `de_DE.UTF-8` selects
`de`; `fr-CA` selects `fr`. Unsupported, missing, or overlong identifiers and
identifiers with empty or non-alphanumeric components quietly select English.
Regional-only catalogs do not imply support
for their whole language: `pt-PT` and bare `zh` currently select English.

On Linux, `sys-locale` checks `LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, then `LANG`.
For a deterministic manual check with a display available:

```sh
env -u LANGUAGE -u LC_ALL -u LC_MESSAGES LANG=de_DE.UTF-8 \
  cargo run --locked --features desktop --bin balun
```

The application menu, About description, navigation titles, and player controls
are translated. Status messages, dialogs, and other window copy still contain
English. The native GTK/libadwaita
controls also depend on the platform's own translations and locale setup.

## Adding presentation text

Use a literal key in a `rust_i18n::t!` call in the shared presentation module and
add it to **every** catalog. Keep translated text in presentation helpers so the
desktop and later CLI integration can share the backend without duplicating it.
Retain mnemonic underscores only in menu action labels; tooltips and accessible
names share their plain-text key. Brands, channel names, guide content, URLs,
trace fields, and domain diagnostics do not become translation keys.

English is the runtime fallback for a missing translation. Tests inspect raw
catalog keys before fallback and compare each value with the compiled backend;
missing or extra keys, missing locales, and empty translations fail `cargo test`.
Locale selection and explicit lookups have portable tests. Linux child-process
tests verify actual `LANG` startup, absent/unsupported fallback, and repeated
initialization without changing the parent test process's global locale.

```sh
cargo test --locked --lib localization::
```

Translation quality, mnemonic usability, long-string layout, screen-reader
announcements, and native menus still need review across the platform matrix.
Catalog parity alone does not establish those outcomes. Complete V2.6 alongside
the H4.3 accessibility evidence, including plural/count rules and the remaining
discovery, playback, settings, and failure copy.

## Navigation and player controls

The device header and navigation page share their title key. Channel, live-TV,
and combined navigation pages use the same catalog. Player-control labels cover
volume, mute/unmute, stop, enter/exit fullscreen, the video accessible name,
playback-status tooltip, and the desktop's idle-inhibition reason. Status values,
buffering percentages, and playback errors are still pending.

Tooltips and accessible names share translated text. The mute toggle retains a
stable translated name while its checked state conveys muting; its tooltip
switches to the translated unmute action. Fullscreen label/tooltip updates still
follow the compositor-confirmed state. `F11`, `Escape`, and GTK shortcut syntax
remain language-independent.

Portable tests check distinct action names and navigation titles in all thirteen
catalogs. The display-backed lifecycle script repeats its production player
binding test in an isolated German-locale process, including fullscreen
transitions and translated tooltip checks. This native CI check supplements the
English control/session smoke; it does not establish screen-reader output or
long-string layout quality on every platform.
