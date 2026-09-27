//! The backdrop sky, rendered headlessly, so the stars can be looked at.
//!
//! By day the field fades almost to nothing, which is correct and also means
//! the running app can go weeks without anyone seeing it at night. This paints
//! the editor pane from the same marks `celestial::sky_backdrop` fills —
//! ground, Milky Way, stars — at an hour you choose, and reports the brightest
//! pixel against body text, since that is the number the backdrop lives under.
//!
//!     cargo run -p smithy-editor --example sky [HOUR] [OUT.png]
//!
//! `HOUR` is local hours tonight (default 23.0).

use floem::peniko::kurbo::{BezPath, PathEl};
use floem::peniko::Color;
use smithy_editor::celestial::{current_location, ground, milky_way_marks, star_marks};
use smithy_sky::SkyState;

const PANE_W: u32 = 1100;
const PANE_H: u32 = 760;

fn to_ts(path: &BezPath) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            PathEl::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            PathEl::QuadTo(a, b) => pb.quad_to(a.x as f32, a.y as f32, b.x as f32, b.y as f32),
            PathEl::CurveTo(a, b, c) => pb.cubic_to(
                a.x as f32, a.y as f32, b.x as f32, b.y as f32, c.x as f32, c.y as f32,
            ),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

fn fill(pm: &mut tiny_skia::Pixmap, path: &BezPath, c: Color) {
    let [r, g, b, a] = c.components;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::from_rgba(r, g, b, a.clamp(0.0, 1.0)).expect("colour"));
    paint.anti_alias = true;
    if let Some(p) = to_ts(path) {
        pm.fill_path(
            &p,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
}

/// WCAG relative luminance of an 8-bit pixel.
fn luminance(r: u8, g: u8, b: u8) -> f64 {
    let ch = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

fn main() {
    let hour: f64 = std::env::args()
        .nth(1)
        .and_then(|h| h.parse().ok())
        .unwrap_or(23.0);
    let out = std::env::args().nth(2).unwrap_or_else(|| "sky.png".into());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs_f64();
    let offset = smithy_editor::localtime::utc_offset_hours();
    let local_midnight = ((now / 3600.0 + offset) / 24.0).floor() * 24.0 - offset;
    let at = (local_midnight + hour) * 3600.0;
    let sky = SkyState::at(
        current_location(),
        smithy_sky::time::julian_date_from_unix(at),
    );

    let (w, h) = (f64::from(PANE_W), f64::from(PANE_H));
    let mut pm = tiny_skia::Pixmap::new(PANE_W, PANE_H).expect("pixmap");
    let g = ground(sky.darkness).components;
    pm.fill(tiny_skia::Color::from_rgba(g[0], g[1], g[2], 1.0).expect("ground"));

    let band = milky_way_marks(&sky, w, h);
    for (path, colour) in &band {
        fill(&mut pm, path, *colour);
    }
    let stars = star_marks(&sky, w, h, 0.0);
    for (path, colour) in &stars {
        fill(&mut pm, path, *colour);
    }
    pm.save_png(&out).expect("save");

    // The brightest pixel, and body text's contrast against it.
    let brightest = pm
        .pixels()
        .iter()
        .map(|p| luminance(p.red(), p.green(), p.blue()))
        .fold(0.0, f64::max);
    let fg = smithy_editor::design::FG.components;
    let fg_l = luminance(
        (fg[0] * 255.0) as u8,
        (fg[1] * 255.0) as u8,
        (fg[2] * 255.0) as u8,
    );
    println!(
        "{hour:.1}h: darkness {:.2}, {} stars up, {} band samples, {} marks -> {out}\n\
         brightest pixel luminance {brightest:.4}; body text over it {:.2}:1",
        sky.darkness,
        sky.stars.len(),
        band.len(),
        stars.len(),
        (fg_l + 0.05) / (brightest + 0.05)
    );
}
