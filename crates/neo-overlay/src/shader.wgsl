// neo-overlay 全屏跑马灯 — 彩色流光 × 液态玻璃边缘（实时屏幕波纹折射）
// 视觉：贴屏幕边缘一圈弯月面透镜 —— 厚度场（不对称高斯剖面 × 三层 fbm 液态波纹）
//       数值梯度得表面法线 → 近轴折射位移采样桌面纹理
//       + IOR 色散（RGB 三通道不同折射系数，物理彩边）
//       + 粗糙度磨砂（泊松盘多采样，透镜越厚光程越长越糊）
//       + Schlick 菲涅尔（波脊斜率大处泛起棱线高光，微染流光同色）
//       + Apple Intelligence 七色粉彩板沿周长环流 —— 颜色是「光」，
//         贴波脊亮起、渗进折射与高光；桌面扭曲是「玻璃」，两者各归其位
// 渲染：0.6x 离屏 + 外层线性放大 blit（低频透镜内容放大几乎无损，省 ~65% GPU）
// 输出 premultiplied alpha（交换链 CompositeAlphaMode::PreMultiplied）
// 坐标：几何在 y-up 空间（对齐旧 GLSL 移植约定）；uv 采样前翻 y

struct Uniforms {
    time: f32,          // tAcc（按速度积分后的动画时钟）
    level: f32,         // 语音电平包络 0..1（已做快攻慢放）
    intensity: f32,     // 整体强度（相位插值后，show/hide 淡入淡出）
    spin: f32,          // 环流角速度（相位插值后）
    refr: f32,          // 抓屏纹理就位 = 1，否则 0（降级为淡白玻璃边）
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

// ---- 噪声（VCC 移植沿用，液态波纹的驱动场） ----

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

// 圆角矩形 SDF：屏幕边缘为基准，透镜贴边
fn sd_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Apple Intelligence 七色粉彩板（VCC 社区采样，RGB 插值全程避开浑浊灰区）
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

// 桌面折射采样（uv 截边）
fn sample_desktop(uv: vec2<f32>) -> vec3<f32> {
    let c = clamp(uv, vec2<f32>(0.001, 0.001), vec2<f32>(0.999, 0.999));
    return textureSampleLevel(desktop, smp, c, 0.0).rgb;
}

// 液态波场：核心层（长涌随环流旋转 + 中浪 + 细碎纹）与低频错拍层。
// 返回 (wave, wave_b)；法线的环向数值差分会重复调用它。
fn wave_field(dir: vec2<f32>, rdir: vec2<f32>, t: f32, amp: f32) -> vec2<f32> {
    let w1 = fbm(rdir * 2.2 + vec2<f32>(t * 0.10, 3.0)) - 0.5;
    let w2 = fbm(dir * 4.5 + vec2<f32>(-t * 0.45, 9.0)) - 0.5;
    let w3 = fbm(dir * 9.0 + vec2<f32>(t * 0.8, 21.0)) - 0.5;
    let wave = (w1 * 1.15 + w2 * 0.42 + w3 * 0.14) * amp;
    let s1 = fbm(rdir * 1.9 + vec2<f32>(t * 0.07 + 0.5, 7.0)) - 0.5;
    let s2 = fbm(dir * 3.6 + vec2<f32>(-t * 0.33, 15.0)) - 0.5;
    let wave_b = (s1 * 1.0 + s2 * 0.30) * amp;
    return vec2<f32>(wave, wave_b);
}

// 弯月透镜厚度剖面（不对称高斯：贴屏缘薄而陡，向屏心延展但快速收敛 —
// 内侧 σ 收窄到 0.70×：剖面尾巴太宽会把透镜糊/流光拖进屏幕中部）
fn lens_profile(d: f32, center: f32, width: f32) -> f32 {
    let x = (d - center) / select(width * 0.70, width * 0.55, d > center);
    return exp(-x * x);
}

// 总厚度场：剖面 × 低频呼吸 × 拖尾窗；厚度脊线随核心波内外游移（液态感来源）
fn thickness(d: f32, wave: f32, wave_b: f32, width: f32) -> f32 {
    let center = -width * (0.45 + 0.60 * wave);
    // 拖尾窗：深度 45→105（渲染 px）渐隐到零。没有它，高斯长尾会把
    // 透镜的雾感/折射糊/菲涅尔残光一直拖进屏幕中部。
    let tail = 1.0 - smoothstep(45.0, 105.0, -d);
    return lens_profile(d, center, width) * (0.70 + 0.30 * wave_b) * tail;
}

// 粗糙磨砂采样：泊松 4 点 + 每像素随机旋转（洗掉核方向性）
fn sample_rough(base_uv: vec2<f32>, r: vec2<f32>, jr: mat2x2<f32>) -> vec3<f32> {
    var poisson = array<vec2<f32>, 4>(
        vec2<f32>(-0.9420, -0.3994), vec2<f32>(0.9456, -0.7689),
        vec2<f32>(-0.0942, 0.9294), vec2<f32>(0.3450, 0.2936)
    );
    var c = sample_desktop(base_uv) * 0.4;
    for (var i = 0; i < 4; i++) {
        c = c + sample_desktop(base_uv + jr * poisson[i] * r) * 0.15;
    }
    return c;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = u.resolution;
    // y-up 几何坐标
    let fc = vec2<f32>(in.pos.x, res.y - in.pos.y);
    let c2 = res * 0.5;
    let q = fc - c2;
    let t = u.time;

    let inset = 12.0;
    let b = c2 - vec2<f32>(inset, inset);
    let d = sd_box(q, b, 14.0);   // < 0 在框内侧

    // 透镜带外早退（拖尾窗到此已归零）
    if (d < -120.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // 语音响应：电平推高波浪振幅
    let amp = 1.0 + u.level * 0.38;

    // 周期方向（绕环无缝）与定向环流（刚体旋转保持单位圆周期性）
    let dir = q / max(length(q), 1e-4);
    let ra = t * u.spin;
    let rot = mat2x2<f32>(cos(ra), -sin(ra), sin(ra), cos(ra));

    let width = 46.0;
    let w0 = wave_field(dir, rot * dir, t, amp);

    // ---- 表面法线：厚度场的数值梯度（径向 + 环向分别差分） ----
    let ee = 1.5;
    let gx = sd_box(q + vec2<f32>(ee, 0.0), b, 14.0) - sd_box(q - vec2<f32>(ee, 0.0), b, 14.0);
    let gy = sd_box(q + vec2<f32>(0.0, ee), b, 14.0) - sd_box(q - vec2<f32>(0.0, ee), b, 14.0);
    let g2 = vec2<f32>(gx, gy);
    let n_up = g2 / max(length(g2), 1e-4);   // SDF 内法线（y-up，指向屏外）
    let tang = vec2<f32>(-dir.y, dir.x);     // 环向切线（单位圆）

    // 径向导数：波场冻结，只有剖面随 d 起伏
    let dH_dd = (thickness(d + ee, w0.x, w0.y, width)
               - thickness(d - ee, w0.x, w0.y, width)) / (2.0 * ee);
    // 环向导数：d 冻结，波场沿周长传播（方向与环流都从扰动后的位置重算）
    let ds = 2.0;
    let q_p = q + tang * ds;
    let q_m = q - tang * ds;
    let dir_p = q_p / max(length(q_p), 1e-4);
    let dir_m = q_m / max(length(q_m), 1e-4);
    let w_p = wave_field(dir_p, rot * dir_p, t, amp);
    let w_m = wave_field(dir_m, rot * dir_m, t, amp);
    let dH_ds = (thickness(d, w_p.x, w_p.y, width)
               - thickness(d, w_m.x, w_m.y, width)) / (2.0 * ds);

    let grad = n_up * dH_dd + tang * dH_ds;  // y-up 平面梯度
    let lift = 28.0;                          // 透镜特征隆起（渲染 px）：斜率 → 法线
    let n3 = normalize(vec3<f32>(-grad * lift, 1.0));

    // 有效厚度（出场包络在这里生效：show 时透镜从边缘涌起）
    let thick = thickness(d, w0.x, w0.y, width) * u.intensity;

    // ---- 折射：近轴近似，位移方向垂直于等厚线、幅度随厚度 ----
    let bend = 42.0;                          // 最大折射位移（渲染 px）
    let off = -n3.xy * thick * bend;          // y-up
    let uv0 = in.pos.xy / res;                // top-down 0..1（与桌面纹理同向）
    let off_uv = vec2<f32>(off.x, -off.y) / res;  // 几何 y-up → uv 翻 y

    // 粗糙度：厚度越大光程越长，磨砂越强（薄边保持清晰）
    let rr = 1.0 + 6.0 * thick;
    let rough_r = vec2<f32>(rr, rr) / res;
    let jitter_ang = hash12(fc) * 6.2831853;
    let jr = mat2x2<f32>(cos(jitter_ang), -sin(jitter_ang), sin(jitter_ang), cos(jitter_ang));

    // ---- 菲涅尔（Schlick）：波脊斜率大 → 掠射 → 棱线高光 ----
    let fres = 0.04 + 0.96 * pow(1.0 - n3.z, 5.0);
    let spec = fres * u.intensity;

    // 物理边缘收口（含四角），1.5px 抗锯齿
    let d_edge = sd_box(q, c2, 0.0);
    let edge = 1.0 - smoothstep(-1.0, 1.0, d_edge);
    // 透镜可见度：只认「真正厚」的脊线（薄裙近乎全透）——
    // 低门槛会让宽厚的薄裙变成一层 96% 不透明的脏雾。
    let vis = smoothstep(0.22, 0.52, thick) * edge;

    // ---- 彩色流光：七色粉彩板沿周长环流（arc 参数 + 环流角速度驱动），
    // 亮度贴波脊（wave 大处更亮）。颜色是「光」，折射是「玻璃」。----
    let arc = atan2(q.y, q.x) * 0.1591549 + 0.5;
    let band_col = palette(arc - t * (u.spin * 0.45 + 0.02));
    let glow = vis * u.intensity * (0.30 + 0.55 * (0.5 + 0.5 * w0.x));

    var col = vec3<f32>(0.0, 0.0, 0.0);
    var alpha = 0.0;
    if (u.refr > 0.5 && thick > 0.003) {
        // 色散：RGB 各用不同折射系数（蓝偏折最大），物理彩边
        var refr_rgb: vec3<f32>;
        refr_rgb.r = sample_rough(uv0 + off_uv * 0.90, rough_r, jr).r;
        refr_rgb.g = sample_rough(uv0 + off_uv * 1.00, rough_r, jr).g;
        refr_rgb.b = sample_rough(uv0 + off_uv * 1.10, rough_r, jr).b;
        // 扭曲桌面为主体，被流光轻微渗色（光穿有色玻璃边）；菲涅尔处
        // 轻微压暗折射（能量守恒感）；棱线高光染一点同色
        let spec_col = mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.30);
        col = refr_rgb * mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.16) * (1.0 - 0.20 * fres)
            + band_col * glow * 0.95
            + spec_col * spec * 0.60;
        alpha = vis * 0.88;
        // 棱线高光与流光处更不透明一点，脊线立得住
        alpha = max(alpha, (spec * 0.85 + glow * 0.40) * edge);
    } else if (thick > 0.003) {
        // 无抓屏降级：彩色玻璃边（剖面微光 + 棱线），不假装有折射
        col = band_col * (0.10 * thick) + mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.4) * spec * 0.55;
        alpha = min(vis * 0.35 + spec * 0.50 * edge + glow * 0.25, 0.65);
    }

    // 抖动去色带：柔光渐变在暗底上极易 banding，加 +/-1 LSB 噪声
    let dith = (hash12(fc + vec2<f32>(fract(t) * 61.7)) - 0.5) * (1.8 / 255.0);
    col = col + vec3<f32>(dith);
    alpha = clamp(alpha + dith, 0.0, 1.0);

    // premultiplied alpha 输出
    return vec4<f32>(col * alpha, alpha);
}
