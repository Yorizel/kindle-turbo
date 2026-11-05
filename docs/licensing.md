# Licensing and attribution

The Rust repository is distributed under GPL-3.0-or-later; see `LICENSE`. Adapted portions in the AsuraScans integration and associated fixtures derive from Darkgoatie/TachiKindle, commit [`a268fbdc0ded6dc55877e3be19cd84f8958c4798`](https://github.com/Darkgoatie/TachiKindle/commit/a268fbdc0ded6dc55877e3be19cd84f8958c4798), whose relevant upstream files are `tests/test_asurascans_selectors.lua` and `koplugin/tachikindle.koplugin/sources/en/asurascans.lua`. Those portions retain their Apache-2.0 licensing; see `third-party/TachiKindle-LICENSE`.

The plugin installer copies both license notices and provenance into the installed plugin directory.

The WeebCentral adapters were implemented against live HTML from `https://weebcentral.com`, with endpoint/selector reference from the upstream `extensions/sources/en/weebcentral.tkext.json` at the same commit. Compact synthetic fixtures and their live verification URLs are documented in `tests/fixtures/weebcentral/README.md`.
