// Test-only snapshot of the pre-analytic noise/wave and +/-2px angular derivative.
// Shared geometry, profile and compositing come from shader.wgsl; do not use in production.
fn reference_vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let k = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, k.x), mix(c, d, k.x), k.y);
}

fn reference_fbm(p_in: vec2<f32>) -> f32 {
    var v = 0.0;
    var a = 0.5;
    var q = p_in;
    for (var i = 0; i < 4; i++) {
        v = v + a * reference_vnoise(q);
        q = q * 2.02 + vec2<f32>(31.7, 17.3);
        a = a * 0.5;
    }
    return v;
}

fn reference_wave_field(dir: vec2<f32>, rdir: vec2<f32>, t: f32, amp: f32) -> vec2<f32> {
    let w1 = reference_fbm(rdir * 2.2 + vec2<f32>(t * 0.10, 3.0)) - 0.5;
    let w2 = reference_fbm(dir * 4.5 + vec2<f32>(-t * 0.45, 9.0)) - 0.5;
    let w3 = reference_fbm(dir * 9.0 + vec2<f32>(t * 0.8, 21.0)) - 0.5;
    let wave = (w1 * 1.15 + w2 * 0.42 + w3 * 0.14) * amp;
    let s1 = reference_fbm(rdir * 1.9 + vec2<f32>(t * 0.07 + 0.5, 7.0)) - 0.5;
    let s2 = reference_fbm(dir * 3.6 + vec2<f32>(-t * 0.33, 15.0)) - 0.5;
    let wave_b = (s1 * 1.0 + s2 * 0.30) * amp;
    return vec2<f32>(wave, wave_b);
}

fn reference_thickness_ds(q: vec2<f32>, tang: vec2<f32>, rot: mat2x2<f32>,
    t: f32, amp: f32, d: f32, width: f32, px: f32, ds: f32) -> f32 {
    let q_p = q + tang * ds;
    let q_m = q - tang * ds;
    let dir_p = q_p / max(length(q_p), 1e-4);
    let dir_m = q_m / max(length(q_m), 1e-4);
    let w_p = reference_wave_field(dir_p, rot * dir_p, t, amp);
    let w_m = reference_wave_field(dir_m, rot * dir_m, t, amp);
    return (thickness(d, w_p.x, w_p.y, width, px)
          - thickness(d, w_m.x, w_m.y, width, px)) / (2.0 * ds);
}
