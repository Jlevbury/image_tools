pub type Curve = Vec<(f32, f32)>;

pub fn lerp_slice(slice: &[f32], t: f32) -> f32 {
    let i1 = ((slice.len() - 1) as f32 * t) as usize;
    let alpha = ((slice.len() - 1) as f32 * t) - i1 as f32;

    if i1 == (slice.len() - 1) {
        *slice.last().unwrap()
    } else {
        let v1 = slice[i1];
        let v2 = slice[i1 + 1];
        v1 + ((v2 - v1) * alpha)
    }
}

/// Assumes the slice represents a monotonic function in the range [0.0, 1.0].
pub fn flip_slice_xy(slice: &[f32], resolution: usize) -> Vec<f32> {
    let mut curve = Vec::new();
    let mut prev_x = 0.0;
    let mut prev_y = 0.0;
    for i in 0..slice.len() {
        let x = (i as f32 / (slice.len() - 1) as f32).max(prev_x);
        let y = slice[i].max(prev_y);
        curve.push((x, y));
        prev_x = x;
        prev_y = y;
    }

    let mut flipped = Vec::new();
    let mut prev_x = 0.0;
    for i in 0..resolution {
        let y = i as f32 / (resolution - 1) as f32;
        let x = lerp_curve_at_y(&curve, y).max(prev_x);
        flipped.push(x);
        prev_x = x;
    }

    flipped
}

// Returns the y value at the given x value.
#[allow(dead_code)]
pub fn lerp_curve_at_x(curve: &[(f32, f32)], t: f32) -> f32 {
    let (p1, p2) = match curve.binary_search_by(|v| v.0.partial_cmp(&t).unwrap()) {
        Ok(i) => return curve[i].1, // Early out.
        Err(i) => {
            if i == 0 {
                ((0.0f32, 0.0f32), curve[i])
            } else if i == curve.len() {
                (curve[i - 1], (1.0f32, 1.0f32))
            } else {
                (curve[i - 1], curve[i])
            }
        }
    };

    let alpha = (t - p1.0) / (p2.0 - p1.0);
    p1.1 + ((p2.1 - p1.1) * alpha)
}

// Returns the x value at the given y value.
#[allow(dead_code)]
pub fn lerp_curve_at_y(curve: &[(f32, f32)], t: f32) -> f32 {
    let (p1, p2) = match curve.binary_search_by(|v| v.1.partial_cmp(&t).unwrap()) {
        Ok(i) => return curve[i].0, // Early out.
        Err(i) => {
            if i == 0 {
                ((0.0f32, 0.0f32), curve[i])
            } else if i == curve.len() {
                (curve[i - 1], (1.0f32, 1.0f32))
            } else {
                (curve[i - 1], curve[i])
            }
        }
    };

    let alpha = (t - p1.1) / (p2.1 - p1.1);
    p1.0 + ((p2.0 - p1.0) * alpha)
}
