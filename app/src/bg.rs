//! Overlay backgrounds, baked once per theme. The software renderer pays ~130 ms for a
//! full-screen live gradient and a full core for an animated one (specs/native-ui.md,
//! Phase 0), so the 145° gradient and its two soft accent glows are drawn here at low
//! resolution and stretched by Slint — a smooth gradient survives the upscale intact.

use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

pub const BAKE_W: u32 = 430;

/// (overlay-bg-1, overlay-bg-2, overlay-accent) per palette, in Theme.palette order —
/// mirrors ui/theme.slint (warm, ocean, forest, lavender, slate, rose).
const OVERLAY: [(u32, u32, u32); 6] = [
    (0x1a1612, 0x2d2319, 0xe8b866),
    (0x0d1520, 0x152535, 0x6bb3e8),
    (0x0d150d, 0x152515, 0x8bc98b),
    (0x150d18, 0x251528, 0xc9a0e8),
    (0x12151a, 0x1e2228, 0xa0b0c0),
    (0x180d10, 0x281518, 0xe8a8b8),
];

/// Theme name from settings.json → Theme.palette index (unknown → warm).
pub fn palette_index(theme: &str) -> i32 {
    match theme {
        "ocean" => 1,
        "forest" => 2,
        "lavender" => 3,
        "slate" => 4,
        "rose" => 5,
        _ => 0,
    }
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 255) as f32, (c >> 8 & 255) as f32, (c & 255) as f32]
}

/// Bake height for a screen of `aspect` = width / height, so the 145° angle and the
/// glow ellipses keep their on-screen shape after the stretch.
pub fn bake_height(aspect: f32) -> u32 {
    ((BAKE_W as f32 / aspect.max(0.1)).round() as u32).clamp(60, 1200)
}

/// One `radial-gradient(ellipse at X Y, accent-glow 0%, transparent STOP)` layer of an
/// overlay's `body::before`: a 200% × 200% box at −50%, −50%. `opacity` is the layer's
/// resting opacity (the 15 s `moonGlow` / `glow` animations start at 0.8; v3 keeps it static).
pub struct Glow {
    pub at: (f32, f32),
    pub stop: f32,
    pub opacity: f32,
}

/// break.html: two glows, the first drawn over the second.
pub const BREAK: &[Glow] = &[
    Glow { at: (0.3, 0.2), stop: 0.5, opacity: 1.0 },
    Glow { at: (0.7, 0.8), stop: 0.4, opacity: 1.0 },
];
/// sleep-warning.html and game-blocked.html.
pub const MOON: &[Glow] = &[Glow { at: (0.5, 0.3), stop: 0.5, opacity: 0.8 }];
/// shutdown-prompt.html.
pub const SHUTDOWN: &[Glow] = &[Glow { at: (0.5, 0.4), stop: 0.45, opacity: 1.0 }];

/// The toast's transparent colour key (0xRRGGBB): overlay-bg-1 with the lowest blue bit
/// flipped, so anti-aliased corner pixels blend toward the toast's own colour, not a fringe.
pub fn key_color(palette: i32) -> u32 {
    OVERLAY[palette.clamp(0, 5) as usize].0 ^ 1
}

/// The overlay background for `palette` on a screen of `aspect`, as the v2 overlay paints
/// it at rest: linear 145° from bg-1 to bg-2, then `glows` (first listed = topmost), each
/// a farthest-corner ellipse of the 200% box, so most centres sit near or off the edges.
pub fn overlay_background(palette: i32, aspect: f32, glows: &[Glow]) -> Image {
    let (c1, c2, acc) = OVERLAY[palette.clamp(0, 5) as usize];
    let (c1, c2, acc) = (rgb(c1), rgb(c2), rgb(acc));
    let bake_h = bake_height(aspect);
    let (w, h) = (BAKE_W as f32, bake_h as f32);
    // 145° in CSS points down-right; project each pixel onto that direction.
    let (dx, dy) = (145f32.to_radians().sin(), -145f32.to_radians().cos());
    let span = w * dx.abs() + h * dy.abs();
    // Per glow, in bake pixels: centre, and the ellipse radii — closest-side radii scaled
    // until the ellipse reaches the box's farthest corner.
    let shapes: Vec<(f32, f32, f32, f32, f32, f32)> = glows
        .iter()
        .rev() // lowest layer first
        .map(|g| {
            let (bx, by) = (2.0 * g.at.0, 2.0 * g.at.1); // in screen widths/heights
            let (near_x, near_y) = (bx.min(2.0 - bx), by.min(2.0 - by));
            let (far_x, far_y) = (bx.max(2.0 - bx), by.max(2.0 - by));
            let k = ((far_x / near_x).powi(2) + (far_y / near_y).powi(2)).sqrt();
            let alpha = 0x26 as f32 / 255.0 * g.opacity;
            ((bx - 0.5) * w, (by - 0.5) * h, near_x * w * k, near_y * h * k, g.stop, alpha)
        })
        .collect();
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(BAKE_W, bake_h);
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i as u32 % BAKE_W) as f32 + 0.5, (i as u32 / BAKE_W) as f32 + 0.5);
        let t = (((x - w / 2.0) * dx + (y - h / 2.0) * dy) / span + 0.5).clamp(0.0, 1.0);
        let mut c = [0f32; 3];
        for k in 0..3 {
            c[k] = c1[k] + (c2[k] - c1[k]) * t;
        }
        for &(cx, cy, rx, ry, stop, alpha) in &shapes {
            let d = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt();
            let a = (1.0 - d / stop).max(0.0) * alpha;
            for k in 0..3 {
                c[k] = c[k] * (1.0 - a) + acc[k] * a;
            }
        }
        *px = Rgb8Pixel { r: c[0].round() as u8, g: c[1].round() as u8, b: c[2].round() as u8 };
    }
    Image::from_rgb8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_names_map_to_theme_slint_order() {
        assert_eq!(palette_index("warm"), 0);
        assert_eq!(palette_index("rose"), 5);
        assert_eq!(palette_index("no-such-theme"), 0);
    }

    #[test]
    fn background_has_the_bake_size_for_every_palette() {
        for p in 0..6 {
            let size = overlay_background(p, 3440.0 / 1440.0, BREAK).size();
            assert_eq!((size.width, size.height), (BAKE_W, 180));
        }
        assert_eq!(bake_height(16.0 / 9.0), 242);
    }

    #[test]
    fn glows_lift_only_the_corners() {
        // Warm: the top-left corner sits on bg-1 (#1a1612) but the first glow lifts it.
        let buf = overlay_background(0, 16.0 / 9.0, BREAK).to_rgb8().unwrap();
        let corner = buf.as_slice()[0];
        assert!(corner.r > 0x1a + 10, "top-left glow missing: {corner:?}");
    }
}
