//! Vertical text orientation (UAX #50 classes, compact table for the common blocks)
//! and the fallback placement used when a font has no vertical alternates.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// U: upright in vertical text.
    Upright,
    /// R: rotated 90° clockwise with the horizontal run.
    Rotated,
    /// Tu: vertical alternate glyph, otherwise upright.
    TransformedUpright,
    /// Tr: vertical alternate glyph, otherwise rotated.
    TransformedRotated,
}

pub fn orientation(c: char) -> Orientation {
    use Orientation::*;
    let u = c as u32;
    match u {
        // Tr: brackets, dashes and lines that turn with the column.
        0x2014
        | 0x2025
        | 0x2026
        | 0x2190..=0x2193
        | 0x3008..=0x3011
        | 0x3014..=0x301C
        | 0x30A0
        | 0x30FC
        | 0xFE59..=0xFE5E
        | 0xFF08
        | 0xFF09
        | 0xFF0D
        | 0xFF1C
        | 0xFF1E
        | 0xFF3B
        | 0xFF3D
        | 0xFF3F
        | 0xFF5B..=0xFF60
        | 0xFFE3 => TransformedRotated,
        // Tu: ideographic punctuation and small kana use dedicated positions.
        0x3001
        | 0x3002
        | 0x3041
        | 0x3043
        | 0x3045
        | 0x3047
        | 0x3049
        | 0x3063
        | 0x3083
        | 0x3085
        | 0x3087
        | 0x308E
        | 0x3095
        | 0x3096
        | 0x30A1
        | 0x30A3
        | 0x30A5
        | 0x30A7
        | 0x30A9
        | 0x30C3
        | 0x30E3
        | 0x30E5
        | 0x30E7
        | 0x30EE
        | 0x30F5
        | 0x30F6
        | 0x31F0..=0x31FF
        | 0xFF01
        | 0xFF0C
        | 0xFF0E
        | 0xFF1F
        | 0x3099..=0x309C => TransformedUpright,
        // U: CJK, kana, Hangul, fullwidth forms, symbols and pictographs.
        0x00A7
        | 0x00A9
        | 0x00AE
        | 0x00B1
        | 0x00BC..=0x00BE
        | 0x00D7
        | 0x00F7
        | 0x02EA
        | 0x02EB
        | 0x1100..=0x11FF
        | 0x1401..=0x167F
        | 0x18B0..=0x18FF
        | 0x2016
        | 0x2020
        | 0x2021
        | 0x2030
        | 0x2031
        | 0x203B
        | 0x203C
        | 0x2042
        | 0x2047..=0x2049
        | 0x2051
        | 0x2065
        | 0x20DD..=0x20E0
        | 0x20E2..=0x20E4
        | 0x2100..=0x2101
        | 0x2103..=0x2109
        | 0x210F
        | 0x2113..=0x2114
        | 0x2116..=0x2117
        | 0x211E..=0x2123
        | 0x2125
        | 0x2127
        | 0x2129
        | 0x212E
        | 0x2135..=0x213F
        | 0x2145..=0x214A
        | 0x214C..=0x214D
        | 0x214F..=0x2189
        | 0x2460..=0x24FF
        | 0x25A0..=0x2619
        | 0x2620..=0x2767
        | 0x2776..=0x2793
        | 0x2B12..=0x2B2F
        | 0x2B50..=0x2B59
        | 0x2E80..=0x3007
        | 0x3012
        | 0x3013
        | 0x301D..=0x3040
        | 0x3042..=0x3098
        | 0x309D..=0x30FB
        | 0x30FD..=0xA4CF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF
        | 0xE000..=0xFAFF
        | 0xFE10..=0xFE1F
        | 0xFE30..=0xFE48
        | 0xFE50..=0xFE57
        | 0xFE5F..=0xFE6F
        | 0xFF02..=0xFF07
        | 0xFF0A..=0xFF0B
        | 0xFF0F..=0xFF1B
        | 0xFF1D
        | 0xFF20..=0xFF3A
        | 0xFF3C
        | 0xFF3E
        | 0xFF40..=0xFF5A
        | 0xFFE0..=0xFFE2
        | 0xFFE4..=0xFFE7
        | 0xFFF0..=0xFFF8
        | 0xFFFC..=0xFFFD
        | 0x1B000..=0x1B2FF
        | 0x1F000..=0x1FAFF
        | 0x20000..=0x3FFFD
        | 0xF0000..=0x10FFFD => Upright,
        _ => Rotated,
    }
}

/// How a glyph is placed in a vertical cell when the font has no `vert` substitution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VerticalFallback {
    None,
    /// Ideographic comma/full stop: move from the lower-left to the upper-right quadrant.
    Punctuation,
    /// Small kana: nudge toward the upper right like typeset vertical small kana.
    SmallKana,
}
pub fn vertical_fallback(c: char) -> VerticalFallback {
    match c as u32 {
        0x3001 | 0x3002 | 0xFF0C | 0xFF0E => VerticalFallback::Punctuation,
        _ if orientation(c) == Orientation::TransformedUpright
            && !matches!(c as u32, 0xFF01 | 0xFF1F | 0x3099..=0x309C) =>
        {
            VerticalFallback::SmallKana
        }
        _ => VerticalFallback::None,
    }
}

/// Characters that may be composed into automatic tate-chu-yoko.
pub fn auto_tcy_char(c: char) -> bool {
    c.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn common_japanese_classes() {
        assert_eq!(orientation('あ'), Orientation::Upright);
        assert_eq!(orientation('漢'), Orientation::Upright);
        assert_eq!(orientation('𠮷'), Orientation::Upright);
        assert_eq!(orientation('「'), Orientation::TransformedRotated);
        assert_eq!(orientation('ー'), Orientation::TransformedRotated);
        assert_eq!(orientation('。'), Orientation::TransformedUpright);
        assert_eq!(orientation('ゃ'), Orientation::TransformedUpright);
        assert_eq!(orientation('A'), Orientation::Rotated);
        assert_eq!(orientation('1'), Orientation::Rotated);
        assert_eq!(orientation('１'), Orientation::Upright);
        assert_eq!(vertical_fallback('、'), VerticalFallback::Punctuation);
        assert_eq!(vertical_fallback('ッ'), VerticalFallback::SmallKana);
        assert_eq!(vertical_fallback('！'), VerticalFallback::None);
    }
}
