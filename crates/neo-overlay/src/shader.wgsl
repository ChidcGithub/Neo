// neo-overlay 全屏跑马灯 — Apple Intelligence 光环（VCC edgeglow v2 逐行移植）
// 结构：七色粉彩板沿屏缘环布 + 双层光带（锐核 + 宽柔晕，错拍律动）
//       + 周期 fbm 液态置换 + 白色扫光沿周长旅行
//       + 不对称光场（贴物理边最厚最亮，向内延展渐淡）
//       + 桌面实时折射（RGB 色散位移采样，光环随壁纸色调渗色）
// 渲染：0.6x 离屏 + 外层线性放大 blit（VCC 同款省 GPU 手法，低频内容几乎无损）
// 输出 premultiplied alpha（交换链 CompositeAlphaMode::PreMultiplied）
// 注意：GLSL gl_FragCoord 原点左下/y 向上；WGSL position 原点左上/y 向下，fc 做了 y 翻转对齐

struct Uniforms {
    time: f32,          // tAcc（按速度积分后的动画时钟）
    level: f32,         // 语音电平包络 0..1（已做快攻慢放）
    intensity: f32,     // 整体强度（相位插值后）
    spin: f32,          // 环流角速度（相位插值后）
    refr: f32,          // 桌面折射强度 0..1
    pad0: f32,
    pad1: f32,
    pad2: f32,
    resolution: vec2<f32>,  // 渲染分辨率（0.6x 离屏）
    tex_size: vec2<f32>,    // 桌面纹理尺寸
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var desktop: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // 无顶点缓冲全屏三角形
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    var out: VsOut;
    let xy = p[vi];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

// ---- VCC 原版噪声（逐行移植，保证视觉一致） ----

fn hash12(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p = p + vec2<f32>(dot(p, p + vec2<f32>(45.32)));
    return fract(p.x * p.y);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let k = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, k.x), mix(c, d, k.x), k.y);
}

fn fbm(p_in: vec2<f32>) -> f32 {
    var v = 0.0;
    var a = 0.5;
    var q = p_in;
    for (var i = 0; i < 4; i++) {
        v = v + a * vnoise(q);
        q = q * 2.02 + vec2<f32>(31.7, 17.3);
        a = a * 0.5;
    }
    return v;
}

// 圆角矩形 SDF：屏幕边缘为基准，光带贴边
fn sd_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Apple Intelligence 七色板（VCC 社区采样照搬，RGB 插值全程避开浑浊灰区）
// 8D9FFF 蓝 → C686FF 浅紫 → BC82F3 紫 → F5B9EA 粉 → FFBA71 琥珀 → FF6778 珊瑚 → AA6EEE 紫罗兰
fn palette(t_in: f32) -> vec3<f32> {
    let c0 = vec3<f32>(0.553, 0.624, 1.000);
    let c1 = vec3<f32>(0.776, 0.525, 1.000);
    let c2 = vec3<f32>(0.737, 0.510, 0.953);
    let c3 = vec3<f32>(0.961, 0.725, 0.918);
    let c4 = vec3<f32>(1.000, 0.729, 0.443);
    let c5 = vec3<f32>(1.000, 0.404, 0.471);
    let c6 = vec3<f32>(0.667, 0.431, 0.933);
    let t = fract(t_in) * 7.0;
    var c = mix(c0, c1, clamp(t, 0.0, 1.0));
    c = mix(c, c2, clamp(t - 1.0, 0.0, 1.0));
    c = mix(c, c3, clamp(t - 2.0, 0.0, 1.0));
    c = mix(c, c4, clamp(t - 3.0, 0.0, 1.0));
    c = mix(c, c5, clamp(t - 4.0, 0.0, 1.0));
    c = mix(c, c6, clamp(t - 5.0, 0.0, 1.0));
    c = mix(c, c0, clamp(t - 6.0, 0.0, 1.0));
    return c;
}

// 不对称光带：外半边（d > center，朝屏幕物理边缘）厚而浓、铺满到边，
// 内半边（d < center，朝屏幕中心）延展而渐淡
fn gauss_asym(d: f32, center: f32, sig_in: f32, sig_out: f32) -> f32 {
    let x = (d - center) / select(sig_in, sig_out, d > center);
    return exp(-x * x);
}

// 桌面折射采样（uv 截边）
fn sample_desktop(uv: vec2<f32>) -> vec3<f32> {
    let c = clamp(uv, vec2<f32>(0.001, 0.001), vec2<f32>(0.999, 0.999));
    return textureSampleLevel(desktop, smp, c, 0.0).rgb;
}

// 5-tap 十字柔化采样：折射要的是柔焦渗色，太清晰会把桌面硬边缘印进光环（割裂感）
fn sample_desktop_blur(uv: vec2<f32>, r: vec2<f32>) -> vec3<f32> {
    var c = sample_desktop(uv) * 0.4;
    c = c + sample_desktop(uv + vec2<f32>(r.x, 0.0)) * 0.15;
    c = c + sample_desktop(uv - vec2<f32>(r.x, 0.0)) * 0.15;
    c = c + sample_desktop(uv + vec2<f32>(0.0, r.y)) * 0.15;
    c = c + sample_desktop(uv - vec2<f32>(0.0, r.y)) * 0.15;
    return c;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = u.resolution;
    // 对齐 GLSL 坐标系：原点左下、y 向上
    let fc = vec2<f32>(in.pos.x, res.y - in.pos.y);
    let c2 = res * 0.5;
    let q = fc - c2;
    let t = u.time;

    let inset = 12.0;
    let b = c2 - vec2<f32>(inset, inset);
    let d = sd_box(q, b, 14.0);   // < 0 在屏幕内侧；小圆角让光带拐角贴近物理角尖

    // 中心深处早退：两层高斯在 -280px 外贡献 < 0.1%，直接透明（省 GPU）
    if (d < -280.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // 语音响应：电平推高波浪振幅与亮度
    let amp = 1.0 + u.level * 0.38;

    // 周期坐标：单位圆方向（绕环无缝，不用 atan）
    let dir = q / max(length(q), 1e-4);

    // 定向环流：长涌噪声场绕屏幕旋转（刚体旋转保持单位圆周期性 → 天然无缝）
    let ra = t * u.spin;
    let rot = mat2x2<f32>(cos(ra), -sin(ra), sin(ra), cos(ra));
    let rdir = rot * dir;

    // 核心层波浪：长涌（随环流旋转）+ 中浪 + 细碎折射纹（局部）
    let w1 = fbm(rdir * 2.2 + vec2<f32>(t * 0.10, 3.0)) - 0.5;
    let w2 = fbm(dir * 4.5 + vec2<f32>(-t * 0.45, 9.0)) - 0.5;
    let w3 = fbm(dir * 9.0 + vec2<f32>(t * 0.8, 21.0)) - 0.5;
    let wave = (w1 * 1.15 + w2 * 0.42 + w3 * 0.14) * amp;

    // 光晕层波浪：低频为主 + 独立相位（错拍律动，两层不齐步）
    let s1 = fbm(rdir * 1.9 + vec2<f32>(t * 0.07 + 0.5, 7.0)) - 0.5;
    let s2 = fbm(dir * 3.6 + vec2<f32>(-t * 0.33, 15.0)) - 0.5;
    let wave_b = (s1 * 1.0 + s2 * 0.30) * amp;

    let width = 44.0;

    // 色散折射相位：RGB 三通道轻微不同相位 → 边缘真实折射彩边
    let ca = 0.05 * sin(t * 0.7 + dir.x * 12.0 + dir.y * 7.0);
    // 核心光带：外半边厚而浓（铺满到屏幕物理边缘），内半边延展渐淡
    let core_c = -width * (0.50 + 0.80 * wave);
    let br = gauss_asym(d, core_c + ca * width, width * 0.42, width * 1.45);
    let bg = gauss_asym(d, core_c, width * 0.42, width * 1.45);
    let bb = gauss_asym(d, core_c - ca * width, width * 0.42, width * 1.45);
    let core = (br + bg + bb) / 3.0;

    // 柔光晕：重心贴边，外半边大范围铺开，内半边深延展渐淡
    let bloom_c = -width * (0.85 + 0.60 * wave_b);
    let bloom = gauss_asym(d, bloom_c, width * 1.55, width * 2.60);

    // 亮脊线：光在液体边缘波峰上集中（贴核心内缘的镜面高光）
    let cx = (d - core_c + width * 0.60) / (width * 0.15);
    let crest = exp(-cx * cx);

    // 周长亮度呼吸斑块
    let bright = 0.75 + 0.5 * fbm(dir * 1.8 + vec2<f32>(-t * 0.13, 4.2));

    // 周长标量（仅用于扫光与色板，fract 距离天然无缝）
    let ang = atan2(q.y, q.x) / 6.28318530718 + 0.5;

    // 白色扫光波峰 x2（沿周长旅行的镜面高光，一主一副）
    let sp1 = fract(t * 0.085);
    var dd1 = abs(fract(ang - sp1));
    dd1 = min(dd1, 1.0 - dd1);
    let sweep1 = exp(-dd1 * dd1 * 7000.0) * 0.60;
    let sp2 = fract(t * 0.085 + 0.5);
    var dd2 = abs(fract(ang - sp2));
    dd2 = min(dd2, 1.0 - dd2);
    let sweep2 = exp(-dd2 * dd2 * 11000.0) * 0.32;
    let sweep = sweep1 + sweep2;

    // 上缘略亮（macOS 光环气质，略收敛）
    let bias = mix(0.85, 1.12, smoothstep(-c2.y, c2.y, q.y));

    // 语音响应：亮度与不透明度随电平抬起（温和，不洗白色板）
    let lvl_b = 1.0 + u.level * 0.55;

    // ---- 实时折射层（Neo 增强，VCC 无）：光带覆盖处对桌面做位移 + RGB 色散采样 ----
    let band = clamp(core + bloom * 0.6, 0.0, 1.0) * u.intensity * u.refr;
    var refr_rgb = vec3<f32>(0.5, 0.5, 0.5);
    if (band > 0.002) {
        // 位移沿 SDF 内法线（数值梯度，垂直于光带，全环连续一致；径向会在角落拧曲）
        let ee = 1.5;
        let gx = sd_box(q + vec2<f32>(ee, 0.0), b, 14.0) - sd_box(q - vec2<f32>(ee, 0.0), b, 14.0);
        let gy = sd_box(q + vec2<f32>(0.0, ee), b, 14.0) - sd_box(q - vec2<f32>(0.0, ee), b, 14.0);
        let g2 = vec2<f32>(gx, gy);
        let n_up = g2 / max(length(g2), 1e-4);   // y-up 空间，指向屏外
        let shift = (wave_b * 8.0 + 5.0) * band;  // 渲染 px
        // 关键：几何在 y-up 空间，uv 在 top-down 空间，位移向量必须翻 y
        let n_td = vec2<f32>(n_up.x, -n_up.y);
        let uv0 = in.pos.xy / res;                // top-down 0..1（与桌面纹理同向）
        let off = -n_td * shift / res;            // 负号：向屏心内位移
        // 柔焦采样 + 收窄的 RGB 色散（1.15/1.00/0.85），硬边缘不显形
        let blur_r = vec2<f32>(5.0, 5.0) / res;
        refr_rgb.r = sample_desktop_blur(uv0 + off * 1.15, blur_r).r;
        refr_rgb.g = sample_desktop_blur(uv0 + off * 1.00, blur_r).g;
        refr_rgb.b = sample_desktop_blur(uv0 + off * 0.85, blur_r).b;
    }

    let pal = palette(ang + t * 0.03);
    let core_col = pal * bright * bias * lvl_b;
    // 光晕色混入折射桌面色：光环随壁纸色调渗色（低强度，保持粉彩板主导）
    var bloom_col = mix(pal, vec3<f32>(1.0, 1.0, 1.0), 0.22) * bright * bias;
    bloom_col = mix(bloom_col, refr_rgb * 1.2, clamp(0.28 * band, 0.0, 1.0));

    var col = core_col * core * (0.9 + 1.0 * sweep)
            + bloom_col * bloom * (0.46 + 0.12 * sweep) * lvl_b
            + vec3<f32>(1.0, 1.0, 1.0) * sweep * 0.42 * bg
            + vec3<f32>(1.0, 1.0, 1.0) * crest * (0.15 + 0.5 * sweep) * bg * u.intensity
            + refr_rgb * core * 0.06 * band;

    // 光铺满到屏幕物理边界（含四角，直角盒 SDF 判定），1.5px 收口抗锯齿
    let d_edge = sd_box(q, c2, 0.0);
    let edge = 1.0 - smoothstep(-1.0, 1.0, d_edge);
    var alpha = clamp(
        (core * (0.62 + 0.55 * bright) * bias * (0.7 + 0.9 * sweep) * lvl_b
         + bloom * 0.42 * (0.5 + 0.5 * bright)) * u.intensity,
        0.0, 1.0) * edge;

    // 抖动去色带：柔光渐变在暗底上极易 banding，加 +/-1 LSB 噪声
    let dith = (hash12(fc + vec2<f32>(fract(t) * 61.7)) - 0.5) * (1.8 / 255.0);
    col = col + vec3<f32>(dith);
    alpha = clamp(alpha + dith, 0.0, 1.0);

    // premultiplied alpha 输出
    return vec4<f32>(col * alpha, alpha);
}
