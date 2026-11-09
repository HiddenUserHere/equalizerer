//! The application icon, generated in code because the project ships no image files.
//!
//! A rounded square in the theme accent colour with four white bars of different heights, like
//! an equalizer. The same pixels serve the tray icon and the window icon.

/// Width and height of the icon in pixels.
pub const SIZE: u32 = 32;

/// Corner radius of the background square, in pixels.
const CORNER_RADIUS: u32 = 8;

/// Background colour: the theme accent `#3b82f6`, fully opaque.
const ACCENT: [u8; 4] = [0x3b, 0x82, 0xf6, 0xff];

/// Colour of the bars.
const BAR_COLOUR: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

/// Bars as `(left, width, top, bottom)` in pixels. Each bar is centred vertically; the heights
/// differ so the icon reads as an equalizer.
const BARS: [(u32, u32, u32, u32); 4] = [
    (5, 4, 10, 22),
    (11, 4, 6, 26),
    (17, 4, 9, 23),
    (23, 4, 12, 20),
];

/// Returns the icon as tightly packed RGBA8 pixels, row by row from the top left, so the length
/// is `SIZE * SIZE * 4`.
#[must_use]
pub fn rgba() -> Vec<u8> {
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            pixels.extend_from_slice(&pixel(x, y));
        }
    }
    pixels
}

/// Colour of one pixel. Pixels outside the rounded square are fully transparent.
fn pixel(x: u32, y: u32) -> [u8; 4] {
    if !inside_rounded_square(x, y) {
        return [0, 0, 0, 0];
    }
    let in_bar = BARS
        .iter()
        .any(|&(left, width, top, bottom)| x >= left && x < left + width && y >= top && y < bottom);
    if in_bar { BAR_COLOUR } else { ACCENT }
}

/// Whether the centre of pixel `(x, y)` lies inside the square with rounded corners.
///
/// The test runs in doubled units, so that pixel centres are whole numbers: the centre of pixel
/// `x` is `2x + 1`. That keeps the arithmetic in integers.
fn inside_rounded_square(x: u32, y: u32) -> bool {
    let px = i64::from(2 * x + 1);
    let py = i64::from(2 * y + 1);
    let radius = i64::from(2 * CORNER_RADIUS);
    let far_edge = i64::from(2 * SIZE);
    // The corner arcs are centred `CORNER_RADIUS` in from two edges; only the corner squares
    // can fall outside the arc.
    let dx = px - px.clamp(radius, far_edge - radius);
    let dy = py - py.clamp(radius, far_edge - radius);
    dx * dx + dy * dy <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel_at(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * SIZE + x) * 4) as usize;
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    }

    #[test]
    fn has_the_expected_size() {
        assert_eq!(rgba().len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn centre_is_the_accent_colour() {
        let pixels = rgba();
        // Column 16 lies between the second and third bar, so the centre is background.
        assert_eq!(pixel_at(&pixels, 16, 16), ACCENT);
    }

    #[test]
    fn bar_pixel_is_white() {
        let pixels = rgba();
        // The first bar spans columns 5..9 and rows 10..22.
        assert_eq!(pixel_at(&pixels, 6, 16), [255, 255, 255, 255]);
    }

    #[test]
    fn corners_are_transparent() {
        let pixels = rgba();
        for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
            assert_eq!(
                pixel_at(&pixels, x, y)[3],
                0,
                "corner ({x}, {y}) must be transparent"
            );
        }
    }
}
