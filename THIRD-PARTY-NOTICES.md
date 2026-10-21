# M6の追加ライブラリ

本体はkurbo 0.13.1とlyon_tessellation 1.0.22（lyon_path / lyon_geom）を使用します。比較用WASMにはvello 0.11.0（vello_encoding / vello_shaders）も含まれます。これらの上流ライセンスを `third-party/` に同梱しています。依存関係の正確なバージョンは `Cargo.lock` を参照してください。

- [kurbo](https://github.com/linebender/kurbo)
- [lyon](https://github.com/nical/lyon)
- [vello](https://github.com/linebender/vello)

# M7の追加ライブラリ

テキストのシェーピングと組版に rustybuzz 0.20.1（MIT）と、それが含む ttf-parser 0.25.1（MIT / Apache-2.0）、unicode-bidi 0.3.18・unicode-segmentation 1.13.3・unicode-script・unicode-properties・unicode-ccc・unicode-bidi-mirroring（MIT / Apache-2.0）、unicode-linebreak 0.1.5（Apache-2.0）、core_maths（MIT）を使用します。上流ライセンスを `third-party/` に同梱しています。

- [rustybuzz](https://github.com/harfbuzz/rustybuzz)
- [ttf-parser](https://github.com/harfbuzz/ttf-parser)
- [unicode-bidi](https://github.com/servo/unicode-bidi)
- [unicode-linebreak](https://github.com/axelf4/unicode-linebreak)
- [unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation)

# テスト用フォント

`tests/fonts/AltoTestJP.otf` は Noto Sans JP（Noto Sans CJK）、`tests/fonts/AltoTestLatin.ttf` は Noto Sans の Regular をサブセット化したものです。いずれも SIL Open Font License 1.1 で、ライセンス本文を `tests/fonts/OFL-NotoSansCJK.txt`・`tests/fonts/OFL-NotoSans.txt` に同梱しています。収録文字は `tests/fonts/subset-text.txt` と基本ラテン・かな・記号です。`verification/sample-m78.alto` はこれらを埋め込んでいます。

- [Noto CJK](https://github.com/notofonts/noto-cjk)
- [Noto Sans](https://github.com/notofonts/latin-greek-cyrillic)
