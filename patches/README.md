# Patched crates

## epaint 0.36.2 (right-to-left text)

egui's text layout has no bidi support: Hebrew and Arabic came out with
words (and runs mixed with Latin) in the wrong order, and a right-to-left
run broke the one-glyph-per-char invariant cursor code relies on (the
shaper returns it in visual order, which emitted extra "continuation"
glyphs).

This copy of `epaint` 0.36.2 (MIT OR Apache-2.0, from crates.io, used via
`[patch.crates-io]` in the workspace `Cargo.toml`) changes
`src/text/text_layout.rs` and `src/text/text_layout_types.rs`:

- `layout_shaped_run` walks a right-to-left run backwards, so glyphs stay
  in logical order and are laid out as if left to right.
- After wrapping, elision and alignment (all done in logical order), rows
  holding right-to-left text are reordered with `unicode-bidi` (UAX #9,
  paragraph direction from its first strong char): glyphs keep their
  logical order and their room, their x positions become visual.
- `Glyph::rtl` marks right-to-left glyphs; `Row::x_offset` puts the cursor
  at a char's leading edge and `Row::char_at` takes the nearest cursor
  position, so clicking and drawing the cursor work in mixed text.

Rows with no right-to-left char take none of the new paths. Tests:
`test_bidi_*` in `text_layout.rs` (`cargo test --lib` in this folder).
Drop the patch once egui lays out bidi text itself.
