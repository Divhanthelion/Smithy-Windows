//! Close-ups of the rail, at the app's own proportions, with measurements.
//!
//! The other sheets fit every scene into one strip, which is the right shape
//! for a regression check and the wrong one for judging a drawing: at thumbnail
//! size a man twice the height of his own door looks fine. This lays each scene
//! out exactly as the app does — a 44px rail under a 1440px window — then
//! zooms in on the part of the rail where something is happening, and prints
//! the sizes that matter so proportions are read off numbers, not squinted at.
//!
//!     cargo run -p smithy-fisherman --features harness --example look [OUT_DIR]
//!
//! Writes one PNG per scene plus `all.png`, every close-up stacked.

use std::path::PathBuf;

use smithy_fisherman::fisherman::{place_position, stage_layout, Scene};
use smithy_fisherman::harness::font::draw_label;
use smithy_fisherman::harness::raster::{render_scene, PixmapInk, STEEL_DEEP};
use smithy_fisherman::{Doing, Part, Place};

/// `forged::FRAME_INSET`: the rail's height in the app.
const APP_BAND: f64 = 44.0;
/// The window's default width, which sets how long the rail is.
const APP_WIDTH: f64 = 1440.0;
/// Close-up magnification. Proportions are the app's; only pixels are added.
const ZOOM: f64 = 3.0;

struct Shot {
    name: &'static str,
    doing: Doing,
    place: Place,
    previous: Place,
    progress: f64,
    completion: f64,
    seconds: f64,
}

const fn shot(
    name: &'static str,
    doing: Doing,
    place: Place,
    previous: Place,
    progress: f64,
    completion: f64,
    seconds: f64,
) -> Shot {
    Shot {
        name,
        doing,
        place,
        previous,
        progress,
        completion,
        seconds,
    }
}

const SHOTS: &[Shot] = &[
    shot("build-20", Doing::Walking, Place::Garden, Place::Garden, 0.0, 0.20, 8.0),
    shot("build-55", Doing::Walking, Place::Garden, Place::Garden, 0.0, 0.55, 8.0),
    shot("build-85", Doing::Walking, Place::Garden, Place::Garden, 0.0, 0.85, 8.0),
    shot("exercising", Doing::Exercising, Place::Doorstep, Place::Hut, 0.5, 1.0, 8.0),
    shot("coffee", Doing::Coffee, Place::Doorstep, Place::Hut, 0.5, 1.0, 8.0),
    shot("gardening", Doing::Gardening, Place::Garden, Place::Doorstep, 0.5, 1.0, 9.0),
    shot("cooking", Doing::Cooking, Place::Fire, Place::Perch, 0.5, 1.0, 18.0),
    shot("eating", Doing::Eating, Place::Doorstep, Place::Fire, 0.5, 1.0, 19.0),
    shot("smoking", Doing::Smoking, Place::Garden, Place::Garden, 0.5, 1.0, 12.0),
    shot("walking-out", Doing::Walking, Place::Perch, Place::Garden, 0.05, 1.0, 10.0),
    shot("walking-home", Doing::Walking, Place::Hut, Place::Garden, 0.92, 1.0, 20.0),
    shot("reading", Doing::Reading, Place::Hut, Place::Doorstep, 0.5, 1.0, 22.0),
    shot("sleeping", Doing::Sleeping, Place::Hut, Place::Doorstep, 0.5, 1.0, 23.5),
    shot("fishing", Doing::Fishing, Place::Perch, Place::Garden, 0.5, 1.0, 10.0),
];

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fisherman-look"));
    std::fs::create_dir_all(&out).expect("mkdir");

    let (w, band) = (APP_WIDTH * ZOOM, APP_BAND * ZOOM);
    // Headroom above the rail, so anything poking out of it shows. In the app
    // that region belongs to the shell, which paints over whatever is there.
    let h = band * 1.6;
    let (scale, stage_left, stage) = stage_layout(w, band);
    let label_h = 22u32;

    let mut crops = Vec::new();
    println!("rail {band}px, man's unit box {scale:.0}px (all numbers at {ZOOM}x)\n");
    for s in SHOTS {
        let scene = Scene {
            width: w,
            height: h,
            band,
            doing: s.doing,
            place: s.place,
            previous: s.previous,
            progress: s.progress,
            completion: s.completion,
            frame: (s.seconds * 5.0) as u64,
            seconds: s.seconds,
        };
        let ink = render_scene(&scene);

        // Home is the hut and everything near it; the perch is its own crop.
        let (x0, x1) = if s.place == Place::Perch && s.doing != Doing::Walking {
            let perch = stage_left + place_position(Place::Perch) * stage;
            (perch - band * 2.5, perch + band * 5.0)
        } else if s.completion < 1.0 {
            // The build runs from the hut out to the lumber pile.
            (stage_left - band * 1.2, stage_left + 0.30 * stage + band * 2.5)
        } else if s.doing == Doing::Walking && s.place == Place::Perch {
            let at = stage_left + 0.3 * stage;
            (at - band * 5.0, at + band * 5.0)
        } else {
            (stage_left - band * 1.2, stage_left + band * 8.0)
        };
        let (x0, x1) = (x0.max(0.0) as u32, (x1.min(w)) as u32);

        let mut crop = PixmapInk::new(x1 - x0, h as u32 + label_h, STEEL_DEEP);
        crop.blit_from(&ink, x0, 0, x1 - x0, h as u32, 0, label_h);
        draw_label(&mut crop, 4, 4, &s.name.to_uppercase(), 2);
        crop.save(&out.join(format!("{}.png", s.name)));

        let figure = ink.part_bounds(Part::Figure);
        let hut = ink.part_bounds(Part::Hut);
        let tall = |b: Option<(u32, u32, u32, u32)>| b.map(|(_, y0, _, y1)| y1 - y0);
        println!(
            "{:<13} figure {:>4}px tall   hut {:>4}px tall (roof incl.)",
            s.name,
            tall(figure).map_or("-".into(), |t| t.to_string()),
            tall(hut).map_or("-".into(), |t| t.to_string()),
        );
        crops.push(crop);
    }

    let sheet_w = crops.iter().map(|c| c.width()).max().unwrap_or(1);
    let sheet_h: u32 = crops.iter().map(|c| c.height() + 6).sum();
    let mut sheet = PixmapInk::new(sheet_w, sheet_h, STEEL_DEEP);
    let mut y = 0;
    for crop in &crops {
        sheet.blit_from(crop, 0, 0, crop.width(), crop.height(), 0, y);
        y += crop.height() + 6;
    }
    sheet.save(&out.join("all.png"));
}
