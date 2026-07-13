# Font Attribution

- Upstream repository: `ryanoasis/nerd-fonts`
- Upstream URL: https://github.com/ryanoasis/nerd-fonts
- Source archive: `NerdFontsSymbolsOnly.zip`
- Source release: `v3.4.0`
- Retrieved date: 2026-07-13
- Retrieved by: vendor step for the embedded PUA-icon fallback face (session-sidebar NFR-5 revision)
- Archive SHA-256: `8e617904b980fe3648a4b116808788fe50c99d2d495376cb7c0badbd8a564c47`
- Vendored file SHA-256 (`SymbolsNerdFontMono-Regular.ttf`): `f0f624d9b474bea1662cf7e862d44aebe1ae1f6c7f9cb7a0ca5d0e5ac9561c60`
- License: MIT (Nerd Fonts project); individual glyph sets retain their upstream licenses (Font Awesome, Devicons, Octicons, etc.) per the Nerd Fonts project.
- License file: `crates/noa-font/vendor/LICENSE`
- Upstream license URL: https://github.com/ryanoasis/nerd-fonts/blob/master/LICENSE

## Notes

`SymbolsNerdFontMono-Regular.ttf` is the symbols-only, monospace-advance Nerd Fonts
face ("Symbols Nerd Font Mono"). It carries no Latin/CJK letterforms — only the
Nerd Fonts icon codepoints (private-use area plus a small set of shared symbols) —
so it is safe to keep permanently in the fallback stack: it can only ever resolve
codepoints the primary/emoji faces miss. It is embedded via `include_bytes!` so
Nerd Font icons (e.g. the sidebar status dot `U+F111` and project icons
`U+E5FF`/`U+E7A8`/…) render on machines with no Nerd Font installed, mirroring
Ghostty's guarantee of icon coverage through its bundled fonts.
