//! Annotations d'une capture : flèche, cadre, flou (pixellisation). Rastérisées sur l'image RGBA,
//! la même fonction sert à l'aperçu et à l'export.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Arrow,
    Rect,
    Blur,
}

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub tool: Tool,
    /// pixels de l'image
    pub a: (f32, f32),
    pub b: (f32, f32),
}

pub const RED: [u8; 4] = [229, 59, 59, 255];

/// Image finale : base + marques, dans l'ordre.
pub fn render(base: &[u8], w: u32, h: u32, marks: &[Mark]) -> Vec<u8> {
    let mut img = base.to_vec();
    let thick = ((w.max(h) as f32) / 400.0).clamp(3.0, 8.0);
    for m in marks {
        match m.tool {
            Tool::Blur => pixelate(&mut img, w, h, m.a, m.b),
            Tool::Rect => {
                let (l, t, r, b) = (m.a.0.min(m.b.0), m.a.1.min(m.b.1), m.a.0.max(m.b.0), m.a.1.max(m.b.1));
                for (p, q) in [((l, t), (r, t)), ((r, t), (r, b)), ((r, b), (l, b)), ((l, b), (l, t))] {
                    line(&mut img, w, h, p, q, thick);
                }
            }
            Tool::Arrow => {
                line(&mut img, w, h, m.a, m.b, thick);
                let (dx, dy) = (m.b.0 - m.a.0, m.b.1 - m.a.1);
                let len = (dx * dx + dy * dy).sqrt();
                if len > 1.0 {
                    let head = (thick * 5.0).min(len * 0.5);
                    let (ux, uy) = (dx / len, dy / len);
                    for s in [1.0f32, -1.0] {
                        // pointes à ±28°
                        let (c, sn) = (0.883f32, 0.469f32 * s);
                        let (rx, ry) = (ux * c - uy * sn, ux * sn + uy * c);
                        line(&mut img, w, h, m.b, (m.b.0 - rx * head, m.b.1 - ry * head), thick);
                    }
                }
            }
        }
    }
    img
}

/// Trait épais : disques posés le long du segment.
fn line(img: &mut [u8], w: u32, h: u32, a: (f32, f32), b: (f32, f32), thick: f32) {
    let r = thick / 2.0;
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let steps = (len / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let (cx, cy) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let (x0, x1) = ((cx - r).floor().max(0.0) as i64, ((cx + r).ceil() as i64).min(w as i64 - 1));
        let (y0, y1) = ((cy - r).floor().max(0.0) as i64, ((cy + r).ceil() as i64).min(h as i64 - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                if (x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2) <= r * r {
                    let o = (y as usize * w as usize + x as usize) * 4;
                    img[o..o + 4].copy_from_slice(&RED);
                }
            }
        }
    }
}

/// Floute en gros pixels (illisible même agrandi).
fn pixelate(img: &mut [u8], w: u32, h: u32, a: (f32, f32), b: (f32, f32)) {
    let cl = |v: f32, m: u32| (v.max(0.0) as u32).min(m);
    let (l, r) = (cl(a.0.min(b.0), w), cl(a.0.max(b.0), w));
    let (t, bt) = (cl(a.1.min(b.1), h), cl(a.1.max(b.1), h));
    let bs = 12u32;
    let mut y = t;
    while y < bt {
        let mut x = l;
        while x < r {
            let (x2, y2) = ((x + bs).min(r), (y + bs).min(bt));
            let mut sum = [0u32; 4];
            let n = (x2 - x) * (y2 - y);
            for yy in y..y2 {
                for xx in x..x2 {
                    let o = ((yy * w + xx) * 4) as usize;
                    (0..4).for_each(|c| sum[c] += img[o + c] as u32);
                }
            }
            let avg: Vec<u8> = sum.iter().map(|s| (s / n.max(1)) as u8).collect();
            for yy in y..y2 {
                for xx in x..x2 {
                    let o = ((yy * w + xx) * 4) as usize;
                    img[o..o + 4].copy_from_slice(&avg);
                }
            }
            x = x2;
        }
        y += bs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marques() {
        let (w, h) = (100u32, 60u32);
        let mut base = vec![255u8; (w * h * 4) as usize];
        // damier pour vérifier le flou
        for (i, p) in base.chunks_exact_mut(4).enumerate() {
            if (i % 2) == 0 {
                p[..3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let out = render(&base, w, h, &[Mark { tool: Tool::Rect, a: (10.0, 10.0), b: (50.0, 40.0) }, Mark { tool: Tool::Blur, a: (60.0, 0.0), b: (200.0, 100.0) }, Mark { tool: Tool::Arrow, a: (0.0, 59.0), b: (40.0, 50.0) }]);
        let px = |x: u32, y: u32| &out[((y * w + x) * 4) as usize..][..4];
        assert_eq!(px(10, 25), RED);
        assert_eq!(px(30, 25), &base[((25 * w + 30) * 4) as usize..][..4]); // intérieur du cadre intact
        assert_eq!(px(61, 1), px(62, 1)); // flouté : bloc uniforme
        assert_eq!(out.len(), base.len());
    }
}
