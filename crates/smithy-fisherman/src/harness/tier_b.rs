//! Tier B — the picture.
//!
//! Thresholds are set from a measured known-good frame and documented with
//! that measurement. Widening a threshold to pass is deleting the check; if
//! a check is red for a real bug, leave it red.

use crate::fisherman::{
    self as f, door_glow, door_openness, scene_at, stage_layout, window_light, Scene, BUILD_SECONDS,
};
use crate::routine::{Doing, Place};

use super::raster::{is_bg, is_lamp_warm, is_rim, luminance, render_scene, STEEL_BODY};
use super::report::CheckResult;
use super::{height, launched_built, BAND, DAY, SUNRISE, SUNSET, WIDTH};

pub fn run_all() -> Vec<CheckResult> {
    vec![
        contrast(),
        ink_budget(),
        fire_where_fire_is(),
        light_agrees(),
    ]
}

fn sample(hours: f64, launched: f64, frame: u64) -> Scene {
    scene_at(
        WIDTH,
        height(),
        BAND,
        hours,
        SUNRISE,
        SUNSET,
        DAY,
        launched,
        frame,
    )
}

fn contrast() -> CheckResult {
    // Mean luminance of RIM-ish stroke pixels vs STEEL_BODY behind them.
    // Without this he vanishes against the frame — previously only judged
    // by eye, and only in the light the tester happened to render.
    //
    // Measured 2026-08-03 on fishing@10h (built): mean RIM luminance ≈ 148,
    // STEEL_BODY luminance ≈ 47, delta ≈ 101. Threshold 40 leaves headroom
    // for AA without accepting a near-invisible rim.
    const MIN_DELTA: f64 = 40.0;

    let scene = sample(10.0, launched_built(), 40);
    let ink = render_scene(&scene);
    let steel = {
        let c = STEEL_BODY.to_rgba8();
        luminance((c.r, c.g, c.b))
    };
    let mut sum = 0.0;
    let mut n = 0u64;
    for y in 0..ink.height() {
        for x in 0..ink.width() {
            let Some((r, g, b, _)) = ink.pixel(x, y) else {
                continue;
            };
            if is_rim((r, g, b)) {
                sum += luminance((r, g, b));
                n += 1;
            }
        }
    }
    let mean = if n == 0 { 0.0 } else { sum / n as f64 };
    let delta = mean - steel;

    CheckResult {
        name: "contrast",
        tier: "B",
        pass: n > 0 && delta > MIN_DELTA,
        measured: delta,
        threshold: Some(MIN_DELTA),
        detail: format!(
            "mean RIM luminance {mean:.1} − steel {steel:.1} = {delta:.1} over {n} pixels; min {MIN_DELTA}"
        ),
        flips: vec![],
    }
}

fn ink_budget() -> CheckResult {
    // Non-background coverage per representative tile. Blank frame or solid
    // blob both fail. Measured 2026-08-03 on 1100×132 tiles:
    //   fishing 0.0122, cooking 0.0115, reading 0.0094, build-55% 0.0077.
    // LO 0.005 sits under the sparsest known-good (mid-build); HI 0.25 is
    // far above a full scene — a solid blob would be ~1.0.
    const LO: f64 = 0.005;
    const HI: f64 = 0.25;

    let scenes = [
        sample(10.0, launched_built(), 40),
        sample(10.0, BUILD_SECONDS * 0.55, 40),
        sample(20.5, launched_built(), 60), // reading indoors
        sample(18.5, launched_built(), 40), // cooking
    ];
    let mut worst = 0.0;
    let mut failures = 0u64;

    for scene in &scenes {
        let ink = render_scene(scene);
        let total = (ink.width() * ink.height()) as f64;
        let mut painted = 0u64;
        for y in 0..ink.height() {
            for x in 0..ink.width() {
                let Some((r, g, b, _)) = ink.pixel(x, y) else {
                    continue;
                };
                if !is_bg((r, g, b)) {
                    painted += 1;
                }
            }
        }
        let frac = painted as f64 / total;
        if frac < LO || frac > HI {
            failures += 1;
            worst = frac;
        } else if worst == 0.0 {
            worst = frac;
        }
    }

    CheckResult {
        name: "ink_budget",
        tier: "B",
        pass: failures == 0,
        measured: worst,
        threshold: Some(HI),
        detail: format!("{failures} tiles with non-bg coverage outside [{LO}, {HI}]"),
        flips: vec![],
    }
}

fn fire_where_fire_is() -> CheckResult {
    // Part::Fire pixels must sit near the pit. "A hearth that teleports to
    // the doorstep reads as a decal" — the comment at paint's fire_base is
    // the assertion. Tagged, not coloured: the part mask is exact.
    let scene = sample(18.5, launched_built(), 40); // cooking at the fire
                                                    // Force Cooking/Fire if the clock landed elsewhere (cigarette overlay).
    let scene = Scene {
        doing: Doing::Cooking,
        place: Place::Fire,
        previous: Place::Perch,
        progress: 0.5,
        completion: 1.0,
        frame: 40,
        seconds: 8.0,
        ..scene
    };
    let ink = render_scene(&scene);
    let (scale, _, _) = stage_layout(WIDTH, BAND);
    let fire_base = f::fire_pit(WIDTH, height(), BAND);
    // Generous pit bbox — flames flicker and sparks rise; the failure mode
    // is the whole hearth at the doorstep, not a spark one band high.
    let pad = scale * 0.55;
    let x0 = (fire_base.x - pad).max(0.0);
    let x1 = (fire_base.x + pad).min(WIDTH);
    let y0 = (fire_base.y - pad * 1.2).max(0.0);
    let y1 = (fire_base.y + pad * 0.4).min(height());

    let mut fire_total = 0u64;
    let mut fire_in = 0u64;
    for y in 0..ink.height() {
        for x in 0..ink.width() {
            if ink.part_at(x, y) != Some(crate::Part::Fire) {
                continue;
            }
            fire_total += 1;
            let xf = x as f64;
            let yf = y as f64;
            if xf >= x0 && xf <= x1 && yf >= y0 && yf <= y1 {
                fire_in += 1;
            }
        }
    }

    // At least some fire, and ≥ 85% of it inside the pit bbox. Measured
    // 2026-08-03 cooking frame: all FIRE_* cores landed in-bbox; 0.85 leaves
    // AA fringe without accepting a doorstep hearth.
    const MIN_IN_FRAC: f64 = 0.85;
    let frac = if fire_total == 0 {
        0.0
    } else {
        fire_in as f64 / fire_total as f64
    };
    let pass = fire_total > 0 && frac >= MIN_IN_FRAC;

    CheckResult {
        name: "fire_where_fire_is",
        tier: "B",
        pass,
        measured: frac,
        threshold: Some(MIN_IN_FRAC),
        detail: format!(
            "{fire_in}/{fire_total} Fire-tagged pixels inside pit bbox (frac {frac:.3})"
        ),
        flips: vec![],
    }
}

fn light_agrees() -> CheckResult {
    // The doorway's light and the state of the house must agree, three ways:
    // he walks home to a *dark* doorway (the house is empty — a glow here is
    // the window-lit-while-he-walks lie in door form); a sleeping hut's shut
    // door never glows; and the one true spill left is the build's handover,
    // where the lamp lit at 97% greets him through the opening door.
    let mut failures = 0u64;
    let hut = f::hut_for(WIDTH, height(), BAND);
    let doorway = hut.door();
    let (door_x0, door_x1, door_y0, door_y1) = (doorway.x0, doorway.x1, doorway.y0, doorway.y1);
    // Strict lamp colours only: his FIGURE_EDGE gold stands *in* the doorway
    // on these frames, and the old loose r/g/b heuristic counted his own
    // outline as lamplight.
    let warm_in_doorway = |ink: &super::raster::PixmapInk| {
        let mut warm = 0u64;
        for y in door_y0.max(0.0) as u32..door_y1.min(height()) as u32 {
            for x in door_x0.max(0.0) as u32..door_x1.min(WIDTH) as u32 {
                let Some((r, g, b, _)) = ink.pixel(x, y) else {
                    continue;
                };
                if is_lamp_warm((r, g, b)) {
                    warm += 1;
                }
            }
        }
        warm
    };
    // Window bleed upper bound on a dark frame, measured 2026-08-03.
    const FRINGE: u64 = 20;

    // Arriving walk, door opening: the house is dark until he is inside, so
    // the doorway must be a hole, not a glow.
    let arriving = Scene {
        width: WIDTH,
        height: height(),
        band: BAND,
        doing: Doing::Walking,
        place: Place::Hut,
        previous: Place::Garden,
        progress: 0.92,
        completion: 1.0,
        frame: 40,
        seconds: 8.0,
    };
    let glow = door_glow(
        window_light(arriving.doing, arriving.place, arriving.progress),
        door_openness(
            arriving.doing,
            arriving.place,
            arriving.previous,
            arriving.progress,
        ),
    );
    let warm = warm_in_doorway(&render_scene(&arriving));
    if glow > 0.01 || warm > FRINGE {
        failures += 1;
    }

    // Sleeping mid-block, door shut: no doorway spill.
    let asleep = sample(23.5, launched_built(), 60);
    let asleep = Scene {
        doing: Doing::Sleeping,
        place: Place::Hut,
        previous: Place::Doorstep,
        progress: 0.5,
        completion: 1.0,
        frame: 60,
        seconds: 12.0,
        ..asleep
    };
    let glow2 = door_glow(
        window_light(asleep.doing, asleep.place, asleep.progress),
        door_openness(asleep.doing, asleep.place, asleep.previous, asleep.progress),
    );
    let warm2 = warm_in_doorway(&render_scene(&asleep));
    if glow2 > 0.01 || warm2 > FRINGE {
        failures += 1;
    }

    // The handover home at the end of a night build: the lamp went on at
    // 97%, he is walking to the door, and the spill through it is the one
    // doorway glow the rail still makes. If this goes dark the finish of
    // every evening launch loses its payoff. Measured 2026-10-03: 161 warm
    // doorway pixels, eight times the fringe.
    let handover = sample(20.5, BUILD_SECONDS * 0.995, 40);
    let warm3 = warm_in_doorway(&render_scene(&handover));
    if warm3 <= FRINGE {
        failures += 1;
    }

    CheckResult {
        name: "light_agrees",
        tier: "B",
        pass: failures == 0,
        // The handover's warm count, since the two dark cases assert an
        // absence a zero cannot summarise.
        measured: warm3 as f64,
        threshold: Some(FRINGE as f64),
        detail: format!(
            "{failures} disagreements (arriving glow={glow:.3} warm={warm}; \
             sleeping glow={glow2:.3} warm={warm2}; handover warm={warm3})"
        ),
        flips: vec![],
    }
}
