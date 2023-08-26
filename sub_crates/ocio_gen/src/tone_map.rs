use colorbox::{
    chroma::Chromaticities,
    lut::{Lut1D, Lut3D},
    transforms::rgb_gamut,
};

use crate::config::{ExponentLUTMapper, Interpolation, Transform};

/// A filmic(ish) tonemapping operator.
///
/// - `chromaticities`: the RGBW chromaticities of the target output
///   color space.
/// - `fixed_point`: the luminance that should map to itself (aside from
///   being affected by `exposure` below).  For example, you might set
///   this to 0.18 (18% gray) so that colors of that brightness remain
///   roughly the same.  Note that large-size toes (> 1.0) can impact
///   the fixed point, making it not quite fixed.
/// - `exposure`: input exposure adjustment before applying the tone mapping.
///   Input color values are simply multiplied by this, so 1.0 does nothing.
///   Useful for tuning the over-all brightness of tone mappers.
/// - `toe`: `(slope, size)` pair that determines the look of the toe.
///   `slope` is in [0, infinity], and determines the slope of the toe at
///   `x = 0`.  0.0 gives maximum contrast, 1.0 is neutral, and > 1.0 is
///   washed out.  `size` is how far the toe extends out of the darks,
///   with 0.0 disabling the toe, and larger values growing its effects
///   out from the darks into the mids and brights.  A `size` of 1.0
///   means that only colors below the fixed point will be noticeably
///   impacted by the toe.
/// - `shoulder`: the strength of the shoulder.  0.0 is equivalent to
///   a linear clamped shoulder, and larger values make the shoulder
///   progressively softer and higher dynamic range.  1.0 is a reasonable
///   default.
/// - `saturation_effect`: how much the filmic curve affects the saturation
///   of colors.  The first number is the effect for fully saturated colors,
///   and the second is the bias for half-saturated input colors.  Both are
///   in [0.0, 1.0].  0.0 gives the least effect, and 1.0 the most.  A piar
///   of (0.0, 0.5) is in some sense "correct", but in practice neither
///   number should be zero.  (0.1, 0.7) is a pretty reasonable default.
/// - `minimum_desaturation_smoothness`: ensures a minimum smoothness of
///   the desaturation transition as colors start to blow out.  0.25 is a
///   reasonable default.
#[derive(Debug, Copy, Clone)]
pub struct Tonemapper {
    chromaticities: Chromaticities,
    fixed_point: f64,
    exposure: f64,
    toe: (f64, f64), // (slope, size)
    shoulder: f64,
    saturation_effect: (f64, f64), // (effect, bias)
    minimum_desaturation_smoothness: f64,

    res_1d: usize,
    res_3d: usize,
    mapper_3d: ExponentLUTMapper,
}

impl Default for Tonemapper {
    fn default() -> Tonemapper {
        Tonemapper {
            chromaticities: colorbox::chroma::REC709,
            fixed_point: 0.18,
            exposure: 1.0,
            toe: (0.0, 1.0),
            shoulder: 1.0,
            saturation_effect: (0.0, 0.5),
            minimum_desaturation_smoothness: 0.25,

            res_1d: 1 << 12,
            res_3d: 31 + 1,
            mapper_3d: ExponentLUTMapper::new(1.5, 1.0, [true, true, true], true),
        }
    }
}

impl Tonemapper {
    pub fn new(
        chromaticities: Option<Chromaticities>,
        fixed_point: f64,
        exposure: f64,
        toe: (f64, f64),
        shoulder: f64,
        saturation_effect: (f64, f64),
        minimum_desaturation_smoothness: f64,
    ) -> Self {
        Tonemapper {
            chromaticities: chromaticities.unwrap_or(colorbox::chroma::REC709),
            fixed_point: fixed_point,
            exposure: exposure,
            toe: toe,
            shoulder: shoulder,
            saturation_effect: saturation_effect,
            minimum_desaturation_smoothness: minimum_desaturation_smoothness,

            ..Self::default()
        }
    }

    pub fn eval_1d(&self, x: f64) -> f64 {
        if x <= 0.0 {
            0.0
        } else {
            filmic::curve(
                x * self.exposure,
                self.fixed_point,
                self.toe.0,
                self.toe.1,
                self.shoulder,
            )
            .min(1.0)
        }
    }

    pub fn eval_1d_inv(&self, y: f64) -> f64 {
        if y <= 0.0 {
            0.0
        } else if y >= 1.0 {
            // Infinity would actually be correct here, but it leads
            // to issues in the generated LUTs.  So instead we just
            // choose an extremely large finite number that fits
            // within an f32 (since later processing may be done in
            // f32).
            (f32::MAX / 2.0) as f64
        } else {
            filmic::curve_inv(y, self.fixed_point, self.toe.0, self.toe.1, self.shoulder)
                / self.exposure
        }
    }

    pub fn eval_rgb(&self, rgb: [f64; 3]) -> [f64; 3] {
        use colorbox::{
            matrix::{
                compose, invert, rgb_to_rgb_matrix, rgb_to_xyz_matrix, transform_color,
                xyz_chromatic_adaptation_matrix, AdaptationMethod,
            },
            transforms::oklab,
        };

        // Define the parent RGB space.
        let parent_rgb_chroma = Chromaticities {
            r: wavelength_to_xy(630.0),
            g: wavelength_to_xy(520.0),
            b: wavelength_to_xy(460.0),
            w: self.chromaticities.w,
        };
        let to_parent_rgb_mat = rgb_to_rgb_matrix(self.chromaticities, parent_rgb_chroma);
        let parent_luma_weights = colorbox::matrix::rgb_to_xyz_matrix(parent_rgb_chroma)[1];

        // Get color in parent RGB space.
        let rgb_par = transform_color(rgb, to_parent_rgb_mat);

        // Compute luma, both linear and tone mapped, in parent RGB space.
        let luma_linear = (rgb_par[0].max(0.0) * parent_luma_weights[0])
            + (rgb_par[1].max(0.0) * parent_luma_weights[1])
            + (rgb_par[2].max(0.0) * parent_luma_weights[2]);
        let luma_tonemapped = self.eval_1d(luma_linear);

        // Compute saturation in parent RGB space.
        let saturation_par = {
            let min = rgb_par[0].min(rgb_par[1]).min(rgb_par[2]).max(0.0);
            let max = rgb_par[0].max(rgb_par[1]).max(rgb_par[2]);
            if max < 1.0e-14 {
                0.0
            } else {
                1.0 - (min / max)
            }
        }
        .clamp(0.0, 1.0);

        // Compute the tone mapped color value.  We do this in the target
        // RGB space, rather than parent RGB space.  But we use the
        // values computed in parent RGB space to drive it.
        let rgb_tonemapped = if saturation_par < 1.0e-10 {
            [luma_tonemapped; 3]
        } else {
            let rgb_linear = rgb_gamut::open_domain_clip(rgb, luma_linear, 0.0);
            let rgb_scaled = vscale(rgb_linear, luma_tonemapped / luma_linear);

            const BLEND: f64 = 0.05;
            let minc = rgb_scaled[0].min(rgb_scaled[1]).min(rgb_scaled[2]);
            let maxc = rgb_scaled[0].max(rgb_scaled[1]).max(rgb_scaled[2]);

            // Filmic desaturation.
            let sat0 = if luma_tonemapped < 1.0e-14 {
                0.0
            } else {
                let step = 1.0
                    - bias(saturation_par, 1.0 - self.saturation_effect.1)
                        * (1.0 - self.saturation_effect.0);
                let step = step.clamp(0.0, 0.99999);
                let sat =
                    (1.0 - (self.eval_1d(luma_linear * step) / luma_tonemapped)) / (1.0 - step);

                // Ensure it's within the open-domain gamut.
                let limit = 1.0 / (1.0 - (minc / luma_tonemapped));
                soft_min(sat, limit, BLEND)
            };

            // Gamut-ceiling-based desaturation.
            let sat1 = {
                let tmp = soft_max(sat0, 1.0, BLEND);
                let new_maxc = lerp(luma_tonemapped, maxc, tmp);
                if new_maxc < 1.0e-14 {
                    1.0e+15
                } else {
                    let clamped_new_maxc = reinhard(new_maxc, self.minimum_desaturation_smoothness);
                    let a = clamped_new_maxc / new_maxc;
                    let b = luma_tonemapped / (new_maxc - (clamped_new_maxc * luma_tonemapped));
                    let fac = ((a - 1.0) * b + a).clamp(0.0, 1.0);
                    fac * tmp
                }
            };

            // Do the desaturation based on the minimum of the two above
            // approaches.
            let t = (soft_min(sat0, sat1, BLEND) + (BLEND * 0.5)).max(0.0);
            vlerp([luma_tonemapped; 3], rgb_scaled, t)
        };

        // Adjust hue to account for the Abney effect.
        let rgb_abney = {
            let to_xyz_mat = compose(&[
                rgb_to_xyz_matrix(self.chromaticities),
                // Adapt to a D65 white point, since that's what OkLab uses.
                xyz_chromatic_adaptation_matrix(
                    self.chromaticities.w,
                    (0.31272, 0.32903), // D65
                    AdaptationMethod::Hunt,
                ),
            ]);
            let from_xyz_mat = invert(to_xyz_mat).unwrap();

            let lab1 = oklab::from_xyz_d65(transform_color(rgb, to_xyz_mat));
            let len1 = ((lab1[1] * lab1[1]) + (lab1[2] * lab1[2])).sqrt();
            let lab2 = oklab::from_xyz_d65(transform_color(rgb_tonemapped, to_xyz_mat));
            let len2 = ((lab2[1] * lab2[1]) + (lab2[2] * lab2[2])).sqrt();

            let lab3 = if len1 < 1.0e-10 {
                lab2
            } else {
                [lab2[0], lab1[1] / len1 * len2, lab1[2] / len1 * len2]
            };

            transform_color(oklab::to_xyz_d65(lab3), from_xyz_mat)
        };

        // A final hard gamut clip for safety, but it should do very little if anything.
        rgb_gamut::closed_domain_clip(
            rgb_gamut::open_domain_clip(rgb_abney, luma_tonemapped, 0.0),
            luma_tonemapped,
            0.0,
        )
    }

    /// Generates a 1D and 3D LUT to apply the tone mapping.
    ///
    /// The LUTs should be applied with the transforms yielded by
    /// `tone_map_transforms()` further below.
    pub fn generate_luts(&self) -> (Lut1D, Lut3D) {
        let lut_1d = Lut1D::from_fn_1(self.res_1d, 0.0, 1.0, |n| self.eval_1d_inv(n as f64) as f32);

        // The 3d LUT is generated to compensate for the missing bits
        // after just the tone mapping curve is applied per-channel.
        // It's sort of a "diff" that can be applied afterwards to get
        // the full rgb transform.
        let lut_3d = Lut3D::from_fn(
            [self.res_3d; 3],
            [0.0; 3],
            [self.mapper_3d.lut_max() as f32; 3],
            |(a, b, c)| {
                // Convert from LUT space to RGB.
                let rgb = self.mapper_3d.from_lut([a as f64, b as f64, c as f64]);

                // Convert from tonemapped space back to linear.
                let rgb_linear = [
                    self.eval_1d_inv(rgb[0]),
                    self.eval_1d_inv(rgb[1]),
                    self.eval_1d_inv(rgb[2]),
                ];

                // Figure out what it should map to.
                let rgb_adjusted = self.eval_rgb(rgb_linear);

                // Convert back to LUT space.
                let abc_final = self.mapper_3d.to_lut(rgb_adjusted);

                (
                    abc_final[0] as f32,
                    abc_final[1] as f32,
                    abc_final[2] as f32,
                )
            },
        );

        (lut_1d, lut_3d)
    }

    pub fn tone_map_transforms(&self, lut_1d_path: &str, lut_3d_path: &str) -> Vec<Transform> {
        let mut transforms = Vec::new();

        // Clip colors to 1.0 saturation, so they're within the range
        // of our LUTs.  This is a slight abuse of the ACES gamut mapper,
        // which is intended for compression rather than clipping.  We
        // use extreme parameters to make it behave like a clipper.
        transforms.extend([Transform::ACESGamutMapTransform {
            threshhold: [0.999, 0.999, 0.999],
            limit: [10.0, 10.0, 10.0],
            power: 4.0,
            direction_inverse: false,
        }]);

        // Apply tone map curve.
        transforms.extend([Transform::FileTransform {
            src: lut_1d_path.into(),
            interpolation: Interpolation::Linear,
            direction_inverse: true,
        }]);

        // Apply chroma LUT.
        transforms.extend(self.mapper_3d.transforms_lut_3d(lut_3d_path));

        transforms
    }
}

/// A "filmic" tone mapping curve.
///
/// The basic idea behind this is to layer a toe function underneath
/// a generalized Reinhard function.  This has no particular basis in
/// anything, but in practice produces pleasing results that are easy
/// to adjust for different looks.
///
/// Note: this maps [0.0, inf] to [0.0, 1.0].
///
/// https://www.desmos.com/calculator/pfzvawfekp
mod filmic {
    use super::{reinhard, reinhard_inv};

    /// - `fixed_point`: the value of `x` that should approximately map
    ///   to itself.  Generally this should be luminance level of a
    ///   medium gray.  Note that extreme toes will cause the fixed point
    ///   to not actually be quite fixed.
    /// - `toe_slope`: the slope of the toe at `x = 0`.  0.0 is max
    ///   contrast, 1.0 is neutral, and > 1.0 washes things out.
    /// - `toe_size`: how far the toe extends out of the darks.  Zero
    ///   disables the toe, and larger values grow its effects
    ///   progressively from the darks into the mids and brights.  1.0 is
    ///   a reasonable value, and means that only colors below the fixed
    ///   point will be noticeably impacted by the toe.
    /// - `shoulder`: the strength of the shoulder.  0.0 is equivalent to
    ///   a linear clamped shoulder, and larger values make the shoulder
    ///   progressively softer and higher dynamic range. 1.0 is a
    ///   reasonable default.
    #[inline(always)]
    pub fn curve(x: f64, fixed_point: f64, toe_slope: f64, toe_size: f64, shoulder: f64) -> f64 {
        assert!(toe_slope >= 0.0);
        assert!(toe_size >= 0.0);
        assert!(shoulder >= 0.0);

        if x <= 0.0 {
            x * toe_slope
        } else {
            let fixed_point_compensation = reinhard_inv(fixed_point, shoulder) / fixed_point;

            let t = toe(
                x,
                toe_slope / fixed_point_compensation,
                toe_size * fixed_point,
            );
            reinhard(t * fixed_point_compensation, shoulder)
        }
    }

    #[inline(always)]
    pub fn curve_inv(
        x: f64,
        fixed_point: f64,
        toe_slope: f64,
        toe_size: f64,
        shoulder: f64,
    ) -> f64 {
        assert!(toe_slope >= 0.0);
        assert!(toe_size >= 0.0);
        assert!(shoulder >= 0.0);

        if x <= 0.0 {
            if toe_slope > 0.0 {
                x / toe_slope
            } else {
                -f64::INFINITY
            }
        } else if x >= 1.0 {
            f64::INFINITY
        } else {
            let fixed_point_compensation = reinhard_inv(fixed_point, shoulder) / fixed_point;

            let t = reinhard_inv(x, shoulder) / fixed_point_compensation;
            toe_inv(
                t,
                toe_slope / fixed_point_compensation,
                toe_size * fixed_point,
            )
        }
    }

    /// Point beyond which we assume the toe is linear.  The toe
    /// goes linear very quickly, so this doesn't need to be super
    /// large.
    const TOE_LINEAR_POINT: f64 = 1.0e+4;

    /// - `slope`: the slope of the toe at `x = 0`.
    /// - `size`: how far the toe extends out of the darks.  Zero is no
    ///   effect at all (not even on darks), and larger values grow its
    ///   effects progressively further from the darks into the mids and
    ///   eventually to the brights.
    fn toe(x: f64, slope: f64, size: f64) -> f64 {
        // Special cases and validation.
        if x < 0.0 {
            return x * slope;
        } else if size <= 0.0 || x > TOE_LINEAR_POINT {
            return x;
        }

        // Convert slope to factor.
        let n = 1.0 - slope.max(0.0);

        // The 0.125 factor is to make the contrast adjustment only
        // noticeably affect values < 1.0.  This makes scaling work
        // fairly intuitively, where you know anything over your scale
        // factor won't be affected.
        let w = size * 0.125;

        let x = x / w;
        (x - (n * x * (-x).exp2())) * w
    }

    /// Inverse of `toe()`.  There is no analytic inverse, so we do it
    /// numerically.
    fn toe_inv(x: f64, slope: f64, size: f64) -> f64 {
        // Special cases and validation.
        if x < 0.0 {
            return x / slope;
        } else if x > TOE_LINEAR_POINT {
            // Really far out it's close enough to linear to not matter.
            return x;
        }

        // A binary search with a capped number of iterations.
        // Something like newton iteration would be faster, but I
        // can't be bothered to figure that out right now, and this
        // isn't performance critical.
        const RELATIVE_ERROR_THRESHOLD: f64 = 1.0e-8;
        let mut min = 0.0;
        let mut max = TOE_LINEAR_POINT;
        for _ in 0..64 {
            let y = (min + max) * 0.5;
            let x2 = toe(y, slope, size);
            if x >= x2 {
                min = y;
                if ((x - x2) / x) <= RELATIVE_ERROR_THRESHOLD {
                    break;
                }
            } else {
                max = y;
            }
        }

        min
    }

    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn toe_round_trip() {
            let size = 2.0;
            for slope in [0.0, 0.5, 1.0, 1.5, 2.0] {
                for i in 0..4096 {
                    // Non-linear mapping for x so we test both very
                    // small and very large values.
                    let x = ((i as f64 / 100.0).exp2() - 1.0) / 10000.0;

                    // Forward.
                    let y = toe(x, slope, size);
                    let x2 = toe_inv(y, slope, size);
                    if x == 0.0 {
                        assert!(x2 == 0.0);
                    } else {
                        assert!(((x - x2).abs() / x) < 0.000_000_1);
                    }

                    // Reverse.
                    let y = toe_inv(x, slope, size);
                    let x2 = toe(y, slope, size);
                    if x == 0.0 {
                        assert!(x2 == 0.0);
                    } else {
                        assert!(((x - x2).abs() / x) < 0.000_000_1);
                    }
                }
            }
        }

        #[test]
        fn filmic_curve_round_trip() {
            let fixed_point = 0.18;
            let toe = (0.25, 0.8);
            let shoulder = 1.4;
            for i in 0..4096 {
                // Forward.
                let x = i as f64 / 64.0;
                let y = curve(x, fixed_point, toe.0, toe.1, shoulder);
                let x2 = curve_inv(y, fixed_point, toe.0, toe.1, shoulder);
                assert!((x - x2).abs() < 0.000_001);

                // Reverse.
                let x = i as f64 / 4096.0;
                let y = curve_inv(x, fixed_point, toe.0, toe.1, shoulder);
                let x2 = curve(y, fixed_point, toe.0, toe.1, shoulder);
                assert!((x - x2).abs() < 0.000_001);
            }
        }
    }
}

/// Computes the CIE xy chromaticity coordinates of a pure wavelength of light.
///
/// `wavelength` is given in nanometers.
fn wavelength_to_xy(wavelength: f64) -> (f64, f64) {
    use colorbox::{tables::cie_1931_xyz as xyz, transforms::xyz_to_xyy};

    let t = ((wavelength - xyz::MIN_WAVELENGTH as f64)
        / (xyz::MAX_WAVELENGTH as f64 - xyz::MIN_WAVELENGTH as f64))
        .clamp(0.0, 1.0);
    let ti = t * (xyz::X.len() - 1) as f64;

    let i1 = ti as usize;
    let i2 = (i1 + 1).min(xyz::X.len() - 1);
    let a = if i1 == i2 {
        0.0
    } else {
        (ti - i1 as f64) / (i2 - i1) as f64
    }
    .clamp(0.0, 1.0) as f32;

    let x = (xyz::X[i1] * (1.0 - a)) + (xyz::X[i2] * a);
    let y = (xyz::Y[i1] * (1.0 - a)) + (xyz::Y[i2] * a);
    let z = (xyz::Z[i1] * (1.0 - a)) + (xyz::Z[i2] * a);

    let xyy = xyz_to_xyy([x as f64, y as f64, z as f64]);

    (xyy[0], xyy[1])
}

/// Generalized Reinhard curve.
///
/// `p`: a tweaking parameter that affects the shape of the curve,
///      in (0.0, inf].  Larger values make it gentler, lower values
///      make it sharper.  1.0 = standard Reinhard, 0.0 = linear
///      in [0,1].
#[inline(always)]
fn reinhard(x: f64, p: f64) -> f64 {
    // Make out-of-range numbers do something reasonable and predictable.
    if x <= 0.0 {
        return x;
    }

    // Special case so we get linear at `p == 0` instead of undefined.
    // Negative `p` is unsupported, so treat like zero as well.
    if p <= 0.0 {
        return x.min(1.0);
    }

    let tmp = x.powf(-1.0 / p);

    // Special cases for numerical stability.
    if tmp > 1.0e15 {
        return x;
    } else if tmp < 1.0e-15 {
        return 1.0;
    }

    // Actual generalized Reinhard.
    (tmp + 1.0).powf(-p)
}

/// Inverse of `reinhard()`.
#[inline(always)]
fn reinhard_inv(x: f64, p: f64) -> f64 {
    // Make out-of-range numbers do something reasonable and predictable.
    if x <= 0.0 {
        return x;
    } else if x >= 1.0 {
        return std::f64::INFINITY;
    }

    // Special case so we get linear at `p == 0` instead of undefined.
    // Negative `p` is unsupported, so clamp.
    if p <= 0.0 {
        return x;
    }

    let tmp = x.powf(-1.0 / p);

    // Special case for numerical stability.
    if tmp > 1.0e15 {
        return x;
    }

    // Actual generalized Reinhard inverse.
    (tmp - 1.0).powf(-p)
}

/// A [0,1] -> [0,1] mapping, with 0.5 biased up or down.
///
/// `b` is what 0.5 maps to.  Setting it to 0.5 results in a linear
/// mapping.
///
/// Note: `bias()` is its own inverse: simply passing `1.0 - b` instead
/// of `b` gives the inverse.
///
/// https://www.desmos.com/calculator/prxpsydjug
#[inline(always)]
fn bias(x: f64, b: f64) -> f64 {
    if b <= 0.0 {
        0.0
    } else if b >= 1.0 {
        1.0
    } else {
        x / ((((1.0 / b) - 2.0) * (1.0 - x)) + 1.0)
    }
}

fn soft_min(a: f64, b: f64, softness: f64) -> f64 {
    let tmp = -a + b;
    (-a - b + ((tmp * tmp) + (softness * softness)).sqrt()) * -0.5
}

fn soft_max(a: f64, b: f64, softness: f64) -> f64 {
    let tmp = a - b;
    (a + b + ((tmp * tmp) + (softness * softness)).sqrt()) * 0.5
}

fn vscale(a: [f64; 3], scale: f64) -> [f64; 3] {
    [a[0] * scale, a[1] * scale, a[2] * scale]
}

fn vlerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

#[inline(always)]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (a * (1.0 - t)) + (b * t)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn tonemap_1d_round_trip() {
        let toe = (0.8, 0.25);
        let shoulder = 1.4;
        let satfx = (0.4, 0.6);
        let min_smooth = 0.25;
        let curve = Tonemapper::new(None, 0.18, 1.1, toe, shoulder, satfx, min_smooth);
        for i in 0..17 {
            let x = i as f64 / 16.0;
            let x2 = curve.eval_1d(curve.eval_1d_inv(x));
            assert!((x - x2).abs() < 0.000_001);
        }
    }

    #[test]
    fn reinhard_round_trip() {
        for i in 0..17 {
            for p in 0..1 {
                let x = (i - 8) as f64 / 4.0;
                let p = p as f64 / 8.0;
                if p <= 0.0 && x >= 1.0 {
                    continue;
                }
                let x2 = reinhard_inv(reinhard(x, p), p);
                assert!((x - x2).abs() < 0.000_001);
            }
        }
    }
}
