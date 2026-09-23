//! The Milky Way, as a field of weighted points along the galactic plane.
//!
//! Not an image: the band is the plane of the galaxy seen from inside it, so
//! it is sampled in galactic coordinates and carried through the same
//! transforms as the stars. It rises, turns and sets with them.
//!
//! The brightness profile is a coarse model of the real one, which is enough
//! for a band seen at a few percent opacity: brightest toward the centre in
//! Sagittarius, a second swell through Cygnus, faint toward the anticentre in
//! Auriga, and the Great Rift — the dust lane that splits it from Cygnus to
//! Sagittarius — cut through the middle.

use crate::coords::{equatorial_to_horizontal, galactic_to_equatorial};
use crate::projection::{project, Projected};

/// One sample of the band, above the horizon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowPoint {
    pub position: Projected,
    /// Relative surface brightness, 0 to 1.
    pub weight: f32,
}

/// Galactic longitude step, and the latitudes sampled either side of the plane.
const LONGITUDE_STEP: f64 = 3.0;
const LATITUDES: [f64; 7] = [-12.0, -8.0, -4.0, 0.0, 4.0, 8.0, 12.0];

/// Surface brightness at a galactic longitude and latitude, 0 to 1.
pub fn brightness(longitude_deg: f64, latitude_deg: f64) -> f64 {
    // Signed distance from the centre, −180..180.
    let from_centre = ((longitude_deg + 180.0).rem_euclid(360.0)) - 180.0;
    let gauss = |x: f64, width: f64| (-(x / width).powi(2)).exp();

    let along = 0.30 + 0.70 * gauss(from_centre, 45.0) + 0.35 * gauss(longitude_deg - 75.0, 22.0);
    // Thicker toward the bulge.
    let thickness = 5.0 + 4.0 * gauss(from_centre, 30.0);
    let across = gauss(latitude_deg, thickness);
    // The Great Rift: a dark lane just north of the plane, Cygnus to Sagittarius.
    let rift = if (-15.0..=70.0).contains(&from_centre) {
        1.0 - 0.55 * gauss(latitude_deg - 1.0, 2.5)
    } else {
        1.0
    };
    // A little clumping, so the band is a cloud rather than an airbrush stroke.
    // Deterministic: the sky must not reshuffle itself every minute.
    let clump = 0.75 + 0.5 * hash01(longitude_deg, latitude_deg);
    (along * across * rift * clump).clamp(0.0, 1.0)
}

fn hash01(a: f64, b: f64) -> f64 {
    let mut x = (a * 73.0 + b * 911.0) as i64 as u64;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x % 10_000) as f64 / 10_000.0
}

/// Every sample of the band that is above the horizon, projected.
pub fn visible(local_sidereal_deg: f64, latitude_deg: f64) -> Vec<GlowPoint> {
    let mut out = Vec::new();
    let steps = (360.0 / LONGITUDE_STEP) as usize;
    for i in 0..steps {
        let l = i as f64 * LONGITUDE_STEP;
        for b in LATITUDES {
            let weight = brightness(l, b);
            if weight < 0.04 {
                continue;
            }
            let horizontal =
                equatorial_to_horizontal(galactic_to_equatorial(l, b), local_sidereal_deg, latitude_deg);
            if let Some(position) = project(horizontal) {
                out.push(GlowPoint {
                    position,
                    weight: weight as f32,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brightest toward the centre, faint at the anticentre, and concentrated
    /// on the plane — the three things anyone who has seen it would check.
    #[test]
    fn the_band_is_brightest_toward_sagittarius_and_thin() {
        let centre = brightness(0.0, -4.0);
        let anticentre = brightness(180.0, -4.0);
        assert!(centre > anticentre * 1.8, "{centre} vs {anticentre}");
        assert!(brightness(90.0, 0.0) > brightness(90.0, 12.0) * 2.0);
    }

    /// The rift darkens the plane itself between Cygnus and Sagittarius.
    #[test]
    fn the_great_rift_cuts_the_band() {
        assert!(brightness(30.0, 1.0) < brightness(30.0, -5.0));
    }
}
