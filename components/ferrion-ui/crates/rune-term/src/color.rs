#![forbid(unsafe_code)]

use rune::Color;
use rune::color::srgb_decode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TermColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub ansi: [[u8; 3]; 16],
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
}

impl Palette {
    pub const XTERM_DARK: Self = Self {
        ansi: [
            [0, 0, 0],
            [205, 0, 0],
            [0, 205, 0],
            [205, 205, 0],
            [0, 0, 238],
            [205, 0, 205],
            [0, 205, 205],
            [229, 229, 229],
            [127, 127, 127],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [92, 92, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ],
        default_fg: [229, 229, 229],
        default_bg: [18, 20, 26],
    };

    pub fn resolve_indexed(&self, idx: u8) -> [u8; 3] {
        if idx < 16 {
            return self.ansi[usize::from(idx)];
        }
        if idx < 232 {
            let n = idx - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            return [level(r), level(g), level(b)];
        }
        let gray = 8 + 10 * (idx - 232);
        [gray, gray, gray]
    }

    pub fn resolve_fg(&self, color: TermColor) -> [u8; 3] {
        match color {
            TermColor::Default => self.default_fg,
            TermColor::Indexed(idx) => self.resolve_indexed(idx),
            TermColor::Rgb(r, g, b) => [r, g, b],
        }
    }

    pub fn resolve_bg(&self, color: TermColor) -> [u8; 3] {
        match color {
            TermColor::Default => self.default_bg,
            TermColor::Indexed(idx) => self.resolve_indexed(idx),
            TermColor::Rgb(r, g, b) => [r, g, b],
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::XTERM_DARK
    }
}

pub fn srgb_to_rune(rgb: [u8; 3]) -> Color {
    Color {
        r: srgb_decode(f32::from(rgb[0]) / 255.0),
        g: srgb_decode(f32::from(rgb[1]) / 255.0),
        b: srgb_decode(f32::from(rgb[2]) / 255.0),
        a: 1.0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs(u16);

impl Attrs {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 4);
    pub const INVERSE: Self = Self(1 << 5);
    pub const HIDDEN: Self = Self(1 << 6);
    pub const STRIKETHROUGH: Self = Self(1 << 7);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.contains(Self::BOLD) {
            out.push("bold");
        }
        if self.contains(Self::DIM) {
            out.push("dim");
        }
        if self.contains(Self::ITALIC) {
            out.push("italic");
        }
        if self.contains(Self::UNDERLINE) {
            out.push("underline");
        }
        if self.contains(Self::DOUBLE_UNDERLINE) {
            out.push("double_underline");
        }
        if self.contains(Self::INVERSE) {
            out.push("inverse");
        }
        if self.contains(Self::HIDDEN) {
            out.push("hidden");
        }
        if self.contains(Self::STRIKETHROUGH) {
            out.push("strikethrough");
        }
        out
    }
}
