//! Colors: the card a table is drawn on, and nushell styles turned into RGBA.

use nu_ansi_term::{Color, Style};

/// A color with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// An opaque color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba([r, g, b, 255])
    }

    /// A color drawn at `alpha` over whatever is below it.
    pub const fn overlay(r: u8, g: u8, b: u8, alpha: u8) -> Self {
        Rgba([r, g, b, alpha])
    }
}

/// The colors of the card a table sits on.
///
/// The card is opaque, so text colors don't depend on the terminal's own
/// background, which many terminals won't report.
pub struct Theme {
    /// The top-level card.
    pub card: Rgba,
    /// A nested table's card, drawn over its parent.
    pub nested_card: Rgba,
    /// The band behind the header row.
    pub header_band: Rgba,
    /// Every other row.
    pub zebra: Rgba,
    /// The line under the header.
    pub rule: Rgba,
    /// Text with no style of its own.
    pub text: Rgba,
    /// The index column.
    pub muted: Rgba,
    /// The 16 ANSI colors, normal then bright, tuned for the card.
    pub ansi: [Rgba; 16],
}

impl Theme {
    /// A dark card (Catppuccin Mocha).
    pub fn dark() -> Self {
        Theme {
            card: Rgba::rgb(0x1e, 0x1e, 0x2e),
            nested_card: Rgba::overlay(255, 255, 255, 12),
            header_band: Rgba::rgb(0x31, 0x32, 0x44),
            zebra: Rgba::overlay(255, 255, 255, 7),
            rule: Rgba::rgb(0x45, 0x47, 0x5a),
            text: Rgba::rgb(0xcd, 0xd6, 0xf4),
            muted: Rgba::rgb(0x7f, 0x84, 0x9c),
            ansi: [
                Rgba::rgb(0x6c, 0x70, 0x86),
                Rgba::rgb(0xf3, 0x8b, 0xa8),
                Rgba::rgb(0xa6, 0xe3, 0xa1),
                Rgba::rgb(0xf9, 0xe2, 0xaf),
                Rgba::rgb(0x89, 0xb4, 0xfa),
                Rgba::rgb(0xf5, 0xc2, 0xe7),
                Rgba::rgb(0x94, 0xe2, 0xd5),
                Rgba::rgb(0xba, 0xc2, 0xde),
                Rgba::rgb(0x7f, 0x84, 0x9c),
                Rgba::rgb(0xf3, 0x8b, 0xa8),
                Rgba::rgb(0xa6, 0xe3, 0xa1),
                Rgba::rgb(0xf9, 0xe2, 0xaf),
                Rgba::rgb(0x89, 0xb4, 0xfa),
                Rgba::rgb(0xf5, 0xc2, 0xe7),
                Rgba::rgb(0x94, 0xe2, 0xd5),
                Rgba::rgb(0xa6, 0xad, 0xc8),
            ],
        }
    }

    /// A light card (Catppuccin Latte).
    pub fn light() -> Self {
        Theme {
            card: Rgba::rgb(0xef, 0xf1, 0xf5),
            nested_card: Rgba::overlay(0, 0, 0, 10),
            header_band: Rgba::rgb(0xdc, 0xe0, 0xe8),
            zebra: Rgba::overlay(0, 0, 0, 8),
            rule: Rgba::rgb(0xbc, 0xc0, 0xcc),
            text: Rgba::rgb(0x4c, 0x4f, 0x69),
            muted: Rgba::rgb(0x8c, 0x8f, 0xa1),
            ansi: [
                Rgba::rgb(0x5c, 0x5f, 0x77),
                Rgba::rgb(0xd2, 0x0f, 0x39),
                Rgba::rgb(0x40, 0xa0, 0x2b),
                Rgba::rgb(0xdf, 0x8e, 0x1d),
                Rgba::rgb(0x1e, 0x66, 0xf5),
                Rgba::rgb(0xea, 0x76, 0xcb),
                Rgba::rgb(0x17, 0x92, 0x99),
                Rgba::rgb(0x8c, 0x8f, 0xa1),
                Rgba::rgb(0x6c, 0x6f, 0x85),
                Rgba::rgb(0xd2, 0x0f, 0x39),
                Rgba::rgb(0x40, 0xa0, 0x2b),
                Rgba::rgb(0xdf, 0x8e, 0x1d),
                Rgba::rgb(0x1e, 0x66, 0xf5),
                Rgba::rgb(0xea, 0x76, 0xcb),
                Rgba::rgb(0x17, 0x92, 0x99),
                Rgba::rgb(0x7c, 0x7f, 0x93),
            ],
        }
    }

    /// The card that suits a terminal background, dark when it's unknown.
    pub fn for_background(background: Option<[u8; 3]>) -> Self {
        match background {
            Some([r, g, b]) if luminance(r, g, b) > 0.5 => Theme::light(),
            _ => Theme::dark(),
        }
    }

    /// `style`'s foreground on this card, or `fallback` when it has none.
    pub fn foreground(&self, style: &Style, fallback: Rgba) -> Rgba {
        style
            .foreground
            .map_or(fallback, |color| self.color(color, fallback))
    }

    /// An ANSI color on this card. `Default` is `fallback`.
    pub fn color(&self, color: Color, fallback: Rgba) -> Rgba {
        match color {
            Color::Black => self.ansi[0],
            Color::Red => self.ansi[1],
            Color::Green => self.ansi[2],
            Color::Yellow => self.ansi[3],
            Color::Blue => self.ansi[4],
            Color::Purple | Color::Magenta => self.ansi[5],
            Color::Cyan => self.ansi[6],
            Color::White => self.ansi[7],
            Color::DarkGray => self.ansi[8],
            Color::LightRed => self.ansi[9],
            Color::LightGreen => self.ansi[10],
            Color::LightYellow => self.ansi[11],
            Color::LightBlue => self.ansi[12],
            Color::LightPurple | Color::LightMagenta => self.ansi[13],
            Color::LightCyan => self.ansi[14],
            Color::LightGray => self.ansi[15],
            Color::Fixed(n) => self.fixed(n),
            Color::Rgb(r, g, b) => Rgba::rgb(r, g, b),
            Color::Default => fallback,
        }
    }

    /// A color from the xterm 256-color table.
    fn fixed(&self, n: u8) -> Rgba {
        match n {
            0..=15 => self.ansi[n as usize],
            16..=231 => {
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                let n = n - 16;
                Rgba::rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            _ => {
                let gray = 8 + (n - 232) * 10;
                Rgba::rgb(gray, gray, gray)
            }
        }
    }
}

/// Relative luminance, 0 for black and 1 for white.
fn luminance(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}
