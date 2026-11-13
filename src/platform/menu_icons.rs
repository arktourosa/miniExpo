//! Small icons for miniExpo's own context-menu entries, drawn from a few shapes so there are no
//! image assets to ship and they stay sharp at any size. Pure maths — nothing here touches
//! Windows, so it can be checked anywhere.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Refresh,
    Explorer,
    PowerShell,
    NewWindow,
    Favorite,
}

type Rgb = [f32; 3];

fn rgb(hex: u32) -> Rgb {
    [((hex >> 16) & 0xFF) as f32, ((hex >> 8) & 0xFF) as f32, (hex & 0xFF) as f32]
}

/// Distance from (px, py) to the segment a–b.
fn seg_dist(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

fn in_rect(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32) -> bool {
    x >= l && x <= r && y >= t && y <= b
}

fn in_rounded(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> bool {
    if !in_rect(x, y, l, t, r, b) {
        return false;
    }
    let cx = x.clamp(l + rad, r - rad);
    let cy = y.clamp(t + rad, b - rad);
    (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad
}

fn in_polygon(x: f32, y: f32, pts: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The colour of the icon at (x, y) on a 16 × 16 design grid, or None where it is transparent.
fn sample(icon: Icon, x: f32, y: f32) -> Option<Rgb> {
    match icon {
        Icon::Refresh => {
            let blue = rgb(0x2F6EBA);
            let (dx, dy) = (x - 8.0, y - 8.0);
            let r = (dx * dx + dy * dy).sqrt();
            // Angle in screen terms (y down); the gap in the ring sits at the upper right.
            let deg = dy.atan2(dx).to_degrees();
            let in_gap = deg > -75.0 && deg < -20.0;
            if (r - 5.2).abs() <= 1.0 && !in_gap {
                return Some(blue);
            }
            // Arrow head at the clockwise end of the arc, pointing along the circle.
            let head = [(8.0 + 2.0, 8.0 - 8.4), (13.6, 4.6), (9.2, 5.2)];
            if in_polygon(x, y, &head) {
                return Some(blue);
            }
            None
        }
        Icon::Explorer => {
            if in_rounded(x, y, 1.5, 2.8, 7.2, 6.2, 1.0) {
                return Some(rgb(0xE0A100));
            }
            if in_rounded(x, y, 1.5, 4.6, 14.5, 13.4, 1.2) {
                return Some(rgb(0xFFC83D));
            }
            None
        }
        Icon::PowerShell => {
            if !in_rounded(x, y, 1.0, 2.5, 15.0, 13.5, 1.6) {
                return None;
            }
            let white = rgb(0xFFFFFF);
            let chevron = seg_dist(x, y, (4.0, 5.6), (7.0, 8.0)).min(seg_dist(x, y, (7.0, 8.0), (4.0, 10.4)));
            if chevron <= 0.75 || seg_dist(x, y, (8.4, 10.8), (12.0, 10.8)) <= 0.7 {
                return Some(white);
            }
            Some(rgb(0x1F3F73))
        }
        Icon::NewWindow => {
            let plus = seg_dist(x, y, (8.0, 6.6), (8.0, 11.6)).min(seg_dist(x, y, (5.5, 9.1), (10.5, 9.1)));
            if plus <= 0.7 && in_rect(x, y, 2.0, 2.0, 14.0, 14.0) {
                return Some(rgb(0x1E78D4));
            }
            if !in_rounded(x, y, 1.0, 2.0, 15.0, 14.0, 1.4) {
                return None;
            }
            // Title bar, then the body inside a thin frame.
            if y < 5.0 {
                return Some(rgb(0x4A5568));
            }
            if x < 2.0 || x > 14.0 || y > 13.0 {
                return Some(rgb(0x4A5568));
            }
            Some(rgb(0xFFFFFF))
        }
        Icon::Favorite => {
            let mut pts = [(0.0, 0.0); 10];
            for (i, p) in pts.iter_mut().enumerate() {
                let r = if i % 2 == 0 { 7.0 } else { 3.0 };
                let a = (-90.0 + 36.0 * i as f32).to_radians();
                *p = (8.0 + r * a.cos(), 8.6 + r * a.sin());
            }
            if in_polygon(x, y, &pts) {
                Some(rgb(0xF2B01E))
            } else {
                None
            }
        }
    }
}

/// `size` × `size` pixels, BGRA with the colour premultiplied by alpha — the layout Windows
/// wants for a menu item's bitmap. Each pixel is averaged over a 4 × 4 grid for smooth edges.
pub fn render(icon: Icon, size: usize) -> Vec<u8> {
    const SS: usize = 4;
    let scale = 16.0 / size as f32;
    let mut out = vec![0u8; size * size * 4];
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut hits) = (0.0f32, 0.0f32, 0.0f32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    if let Some(c) = sample(icon, x, y) {
                        r += c[0];
                        g += c[1];
                        b += c[2];
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let cover = hits as f32 / (SS * SS) as f32;
            let i = (py * size + px) * 4;
            // average colour × coverage = premultiplied
            out[i] = (b / hits as f32 * cover).round() as u8;
            out[i + 1] = (g / hits as f32 * cover).round() as u8;
            out[i + 2] = (r / hits as f32 * cover).round() as u8;
            out[i + 3] = (cover * 255.0).round() as u8;
        }
    }
    out
}
