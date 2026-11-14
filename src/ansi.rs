//! ANSI escape codes in text: SGR colors and attributes are kept, every
//! other escape sequence and control character is dropped.

use nu_ansi_term::Color;

use crate::style::{Rgba, Theme};

/// The SGR state a span of text was written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sgr {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// A stretch of text in one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Sgr,
}

/// A span's colors on a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// `None` keeps the default text color of wherever the span is drawn.
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
}

impl Sgr {
    /// This style's colors on `theme`'s card, with reverse and dim applied.
    pub fn colors(&self, theme: &Theme, default_fg: Rgba) -> Colors {
        let fg = self.fg.map(|color| match (color, self.bg) {
            // Black is softened to show on a dark card, but over a
            // background of its own it should stay black.
            (Color::Black, Some(_)) => Rgba::rgb(0x11, 0x11, 0x1b),
            _ => theme.color(color, default_fg),
        });
        let bg = self.bg.map(|color| theme.color(color, theme.card));
        let (fg, bg) = if self.reverse {
            (
                Some(bg.unwrap_or(theme.card)),
                Some(fg.unwrap_or(default_fg)),
            )
        } else {
            (fg, bg)
        };
        let fg = if self.dim {
            let Rgba([r, g, b, _]) = fg.unwrap_or(default_fg);
            Some(Rgba([r, g, b, 150]))
        } else {
            fg
        };
        Colors { fg, bg }
    }
}

/// Whether `text` has anything for [`parse`] to do.
pub fn has_escapes(text: &str) -> bool {
    text.contains('\x1b')
}

/// Split `text` into lines of styled spans. A style carries on across lines,
/// as it does in a terminal.
pub fn parse(text: &str) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = vec![Vec::new()];
    let mut style = Sgr::default();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut last = None;
                    for next in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&next) {
                            last = Some(next);
                            break;
                        }
                        params.push(next);
                    }
                    if last == Some('m') {
                        flush(&mut lines, &mut current, style);
                        apply(&mut style, &params);
                    }
                }
                // OSC, DCS, APC, PM and SOS run to BEL or ST.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other escape is two characters long.
                _ => {}
            },
            '\n' => {
                flush(&mut lines, &mut current, style);
                lines.push(Vec::new());
            }
            '\t' => current.push('\t'),
            c if c.is_control() => {}
            c => current.push(c),
        }
    }
    flush(&mut lines, &mut current, style);
    lines
}

/// The text of a line of spans, without styles.
pub fn plain(line: &[Span]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

fn flush(lines: &mut [Vec<Span>], current: &mut String, style: Sgr) {
    if current.is_empty() {
        return;
    }
    if let Some(line) = lines.last_mut() {
        line.push(Span {
            text: std::mem::take(current),
            style,
        });
    }
}

/// Apply the parameters of one SGR sequence.
fn apply(style: &mut Sgr, params: &str) {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|code| code.parse().unwrap_or(0))
            .collect()
    };
    let mut at = 0;
    while at < codes.len() {
        match codes[at] {
            0 => *style = Sgr::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            4 => style.underline = true,
            7 => style.reverse = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            24 => style.underline = false,
            27 => style.reverse = false,
            code @ 30..=37 => style.fg = Some(basic(code - 30, false)),
            39 => style.fg = None,
            code @ 40..=47 => style.bg = Some(basic(code - 40, false)),
            49 => style.bg = None,
            code @ 90..=97 => style.fg = Some(basic(code - 90, true)),
            code @ 100..=107 => style.bg = Some(basic(code - 100, true)),
            code @ (38 | 48) => {
                let (color, used) = extended(&codes[at + 1..]);
                if code == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
                at += used;
            }
            _ => {}
        }
        at += 1;
    }
}

/// A `38;5;n` or `38;2;r;g;b` color, and how many parameters it used.
fn extended(rest: &[u16]) -> (Option<Color>, usize) {
    let byte = |value: u16| value.min(255) as u8;
    match rest {
        [5, n, ..] => (Some(Color::Fixed(byte(*n))), 2),
        [2, r, g, b, ..] => (Some(Color::Rgb(byte(*r), byte(*g), byte(*b))), 4),
        _ => (None, rest.len()),
    }
}

fn basic(index: u16, bright: bool) -> Color {
    const NORMAL: [Color; 8] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Purple,
        Color::Cyan,
        Color::White,
    ];
    const BRIGHT: [Color; 8] = [
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightPurple,
        Color::LightCyan,
        Color::LightGray,
    ];
    let table = if bright { &BRIGHT } else { &NORMAL };
    table[usize::from(index.min(7))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_colors_and_drops_other_escapes() {
        let lines = parse(
            "\x1b[38;2;87;199;255massets\x1b[0m \x1b]8;;file:///x\x1b\\link\x1b]8;;\x1b\\\n\x1b[1;31mred\x1b[22m plain",
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(plain(&lines[0]), "assets link");
        assert_eq!(lines[0][0].style.fg, Some(Color::Rgb(87, 199, 255)));
        assert_eq!(lines[0][1].style, Sgr::default());
        assert_eq!(plain(&lines[1]), "red plain");
        assert!(lines[1][0].style.bold);
        assert_eq!(lines[1][0].style.fg, Some(Color::Red));
        assert!(!lines[1][1].style.bold);
        assert_eq!(lines[1][1].style.fg, Some(Color::Red));
    }

    #[test]
    fn reads_256_colors_and_bright_backgrounds() {
        let lines = parse("\x1b[38;5;208;101mx\x1b[39;49my");
        assert_eq!(lines[0][0].style.fg, Some(Color::Fixed(208)));
        assert_eq!(lines[0][0].style.bg, Some(Color::LightRed));
        assert_eq!(lines[0][1].style, Sgr::default());
    }
}
