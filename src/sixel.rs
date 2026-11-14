//! Sixel output, for terminals that show sixel images but not kitty
//! graphics.

use icy_sixel::{EncodeOptions, QuantizeMethod};

/// The drawing as one sixel image, ready to print.
///
/// With the terminal's `background` known, the drawing is blended onto it
/// first, so the card's round corners and the lines' soft edges stay smooth.
/// Without it, pixels less than half opaque are left out, which sixel can
/// do, and the rest are drawn whole.
///
/// # Errors
///
/// Fails when the encoder rejects the image, such as one too large for it.
pub fn encode(
    mut rgba: Vec<u8>,
    width: u32,
    height: u32,
    background: Option<[u8; 3]>,
) -> Result<String, String> {
    if let Some(background) = background {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for (channel, under) in pixel.iter_mut().zip(background) {
                let over = u32::from(*channel) * alpha + u32::from(under) * (255 - alpha);
                *channel = ((over + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
    }
    let options = EncodeOptions {
        max_colors: 256,
        // No dithering: a table is flat colors and text, which dithering
        // only speckles.
        diffusion: 0.0,
        quantize_method: QuantizeMethod::Wu,
    };
    icy_sixel::sixel_encode(&rgba, width as usize, height as usize, &options)
        .map_err(|err| format!("couldn't encode the sixel image: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether two colors match within sixel's precision: it stores colors
    /// as whole percentages.
    fn close(got: &[u8], want: [u8; 3]) -> bool {
        got.iter()
            .zip(want)
            .all(|(got, want)| got.abs_diff(want) <= 3)
    }

    /// A 9 × 13 image: a blue square with a transparent border.
    fn image() -> Vec<u8> {
        let mut rgba = vec![0u8; 9 * 13 * 4];
        for y in 2..11 {
            for x in 2..7 {
                let at = (y * 9 + x) * 4;
                rgba[at..at + 4].copy_from_slice(&[40, 90, 200, 255]);
            }
        }
        rgba
    }

    #[test]
    fn decodes_back_to_the_same_image() {
        let sixel = encode(image(), 9, 13, None).expect("encodes");
        assert!(sixel.starts_with("\x1bP"), "a DCS sequence");
        assert!(sixel.ends_with("\x1b\\"), "ended by ST");
        let decoded = icy_sixel::SixelImage::decode(sixel.as_bytes()).expect("decodes");
        assert_eq!((decoded.width, decoded.height), (9, 13));
        let at = (5 * 9 + 4) * 4;
        assert!(close(&decoded.pixels[at..at + 3], [40, 90, 200]));
    }

    #[test]
    fn blends_onto_a_known_background() {
        let sixel = encode(image(), 9, 13, Some([250, 250, 250])).expect("encodes");
        let decoded = icy_sixel::SixelImage::decode(sixel.as_bytes()).expect("decodes");
        assert!(close(&decoded.pixels[0..3], [250, 250, 250]));
        assert_eq!(decoded.pixels[3], 255);
    }
}
