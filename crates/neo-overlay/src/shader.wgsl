// neo-overlay 全屏跑马灯 — 主题蓝流光 × 液态玻璃边缘（实时屏幕波纹折射）
// 视觉：贴屏幕边缘一圈弯月面透镜 —— 厚度场（不对称高斯剖面 × 五场四层 fbm 液态波纹）
//       径向数值差分 + 环向解析梯度得表面法线 → 近轴折射位移采样桌面纹理
//       + IOR 色散（RGB 三通道不同折射系数，物理彩边）
//       + 粗糙度磨砂（泊松盘多采样，透镜越厚光程越长越糊）
//       + Schlick 菲涅尔（波脊斜率大处泛起棱线高光，微染流光同色）
//       + DeepSeek D500 单色随波场明暗流动 —— 颜色是「光」，
//         贴波脊亮起、渗进折射与高光；桌面扭曲是「玻璃」，两者各归其位
// 渲染：最高 0.6x 离屏，受像素预算限制；边缘裁剪后线性放大 blit
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
    resolution: vec2<f32>,  // 像素预算约束后的离屏分辨率
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

// (value, ∂value/∂x, ∂value/∂y)：四个格点 hash 同时供插值和解析梯度使用。
fn vnoise(p: vec2<f32>) -> vec3<f32> {
    let i = floor(p);
    let f = fract(p);
    let k = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    let dk = 6.0 * f * (1.0 - f);
    let lo = mix(a, b, k.x);
    let hi = mix(c, d, k.x);
    return vec3<f32>(mix(lo, hi, k.y),
        mix(b - a, d - c, k.y) * dk.x, (hi - lo) * dk.y);
}

fn fbm(p_in: vec2<f32>) -> vec3<f32> {
    var v = 0.0;
    var gradient = vec2<f32>(0.0);
    var frequency = 1.0;
    var a = 0.5;
    var q = p_in;
    for (var i = 0; i < 4; i++) {
        let noise = vnoise(q);
        v = v + a * noise.x;
        gradient = gradient + a * noise.yz * frequency;
        q = q * 2.02 + vec2<f32>(31.7, 17.3);
        frequency = frequency * 2.02;
        a = a * 0.5;
    }
    return vec3<f32>(v, gradient);
}

// 圆角矩形 SDF：屏幕边缘为基准，透镜贴边
fn sd_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// 固定品牌主题蓝，与 neo-theme::palette::deepseek::D500 同步（由测试校验）。
const THEME_BLUE: vec3<f32> = vec3<f32>(52.0, 96.0, 180.0) / 255.0;

// 桌面折射采样（uv 截边）
fn sample_desktop(uv: vec2<f32>) -> vec3<f32> {
    let c = clamp(uv, vec2<f32>(0.001, 0.001), vec2<f32>(0.999, 0.999));
    return textureSampleLevel(desktop, smp, c, 0.0).rgb;
}

// 液态波场：核心层（长涌随环流旋转 + 中浪 + 细碎纹）与低频错拍层。
// 返回 (wave, wave_b, ∂wave/∂θ, ∂wave_b/∂θ)，一次求值保留五场 × 四层。
fn wave_field(dir: vec2<f32>, rdir: vec2<f32>, t: f32, amp: f32) -> vec4<f32> {
    let w1 = fbm(rdir * 2.2 + vec2<f32>(t * 0.10, 3.0));
    let w2 = fbm(dir * 4.5 + vec2<f32>(-t * 0.45, 9.0));
    let w3 = fbm(dir * 9.0 + vec2<f32>(t * 0.8, 21.0));
    let wave = ((w1.x - 0.5) * 1.15 + (w2.x - 0.5) * 0.42 + (w3.x - 0.5) * 0.14) * amp;
    let s1 = fbm(rdir * 1.9 + vec2<f32>(t * 0.07 + 0.5, 7.0));
    let s2 = fbm(dir * 3.6 + vec2<f32>(-t * 0.33, 15.0));
    let wave_b = ((s1.x - 0.5) * 1.0 + (s2.x - 0.5) * 0.30) * amp;
    let tang = vec2<f32>(-dir.y, dir.x);
    let rtang = vec2<f32>(-rdir.y, rdir.x);
    let wave_theta = (dot(w1.yz, rtang) * 2.2 * 1.15
        + dot(w2.yz, tang) * 4.5 * 0.42 + dot(w3.yz, tang) * 9.0 * 0.14) * amp;
    let wave_b_theta = (dot(s1.yz, rtang) * 1.9
        + dot(s2.yz, tang) * 3.6 * 0.30) * amp;
    return vec4<f32>(wave, wave_b, wave_theta, wave_b_theta);
}

// 弯月透镜厚度剖面（不对称高斯：贴屏缘薄而陡，向屏心延展但快速收敛 —
// 内侧 σ 收窄到 0.70×：剖面尾巴太宽会把透镜糊/流光拖进屏幕中部）
fn lens_profile(d: f32, center: f32, width: f32) -> f32 {
    let x = (d - center) / select(width * 0.70, width * 0.55, d > center);
    return exp(-x * x);
}

// 总厚度场：剖面 × 低频呼吸 × 拖尾窗；厚度脊线随核心波内外游移（液态感来源）
fn thickness(d: f32, wave: f32, wave_b: f32, width: f32, px: f32) -> f32 {
    let center = -width * (0.45 + 0.60 * wave);
    // 拖尾窗：深度 45→105（渲染 px）渐隐到零。没有它，高斯长尾会把
    // 透镜的雾感/折射糊/菲涅尔残光一直拖进屏幕中部。
    let tail = 1.0 - smoothstep(45.0 * px, 105.0 * px, -d);
    return lens_profile(d, center, width) * (0.70 + 0.30 * wave_b) * tail;
}

// d 冻结：链式求导剖面中心与呼吸项；θ 的导数除以 |q| 才是每渲染像素的导数。
// 保持径向差分不变。这里是局部解析导数，不等价于旧的 ±2px 环向平均斜率。
fn thickness_ds(d: f32, waves: vec4<f32>, width: f32, px: f32, radius: f32) -> f32 {
    let center = -width * (0.45 + 0.60 * waves.x);
    let sigma = select(width * 0.70, width * 0.55, d > center);
    let profile = lens_profile(d, center, width);
    let tail = 1.0 - smoothstep(45.0 * px, 105.0 * px, -d);
    let center_theta = -width * 0.60 * waves.z;
    let profile_theta = profile * (2.0 * (d - center) / (sigma * sigma)) * center_theta;
    return (profile_theta * (0.70 + 0.30 * waves.y)
        + profile * 0.30 * waves.w) * tail / max(radius, 1e-4);
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
    // 分辨率无关尺度：所有 px 尺寸按离屏高度归一（432p 离屏 = 调参基准），
    // 高分辨率屏上色带/透镜不再等比缩水
    let px = res.y / 432.0;
    // y-up 几何坐标
    let fc = vec2<f32>(in.pos.x, res.y - in.pos.y);
    let c2 = res * 0.5;
    let q = fc - c2;
    let t = u.time;

    let inset = 12.0 * px;
    let b = c2 - vec2<f32>(inset, inset);
    let d = sd_box(q, b, 14.0 * px);   // < 0 在框内侧

    // 透镜带外早退（拖尾窗到此已归零）
    if (d < -120.0 * px) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // 语音响应：电平推高波浪振幅
    let amp = 1.0 + u.level * 0.38;

    // 周期方向（绕环无缝）与定向环流（刚体旋转保持单位圆周期性）
    let dir = q / max(length(q), 1e-4);
    let ra = t * u.spin;
    let rot = mat2x2<f32>(cos(ra), -sin(ra), sin(ra), cos(ra));

    let width = 46.0 * px;
    let w0 = wave_field(dir, rot * dir, t, amp);

    // ---- 表面法线：径向差分 + 环向解析梯度 ----
    let ee = 1.5;
    let gx = sd_box(q + vec2<f32>(ee, 0.0), b, 14.0 * px) - sd_box(q - vec2<f32>(ee, 0.0), b, 14.0 * px);
    let gy = sd_box(q + vec2<f32>(0.0, ee), b, 14.0 * px) - sd_box(q - vec2<f32>(0.0, ee), b, 14.0 * px);
    let g2 = vec2<f32>(gx, gy);
    let n_up = g2 / max(length(g2), 1e-4);   // SDF 内法线（y-up，指向屏外）
    let tang = vec2<f32>(-dir.y, dir.x);     // 环向切线（单位圆）

    // 径向导数：波场冻结，只有剖面随 d 起伏
    let dH_dd = (thickness(d + ee, w0.x, w0.y, width, px)
               - thickness(d - ee, w0.x, w0.y, width, px)) / (2.0 * ee);
    // 环向导数：复用中心波场的解析导数，省去两次五场四层 FBM。
    let dH_ds = thickness_ds(d, w0, width, px, length(q));

    let grad = n_up * dH_dd + tang * dH_ds;  // y-up 平面梯度
    let lift = 30.0 * px;                     // 透镜特征隆起（渲染 px）：斜率 → 法线
    let n3 = normalize(vec3<f32>(-grad * lift, 1.0));

    // 有效厚度（出场包络在这里生效：show 时透镜从边缘涌起）
    let thick = thickness(d, w0.x, w0.y, width, px) * u.intensity;

    // 仅将折射从波脊向外延续到视口边界（含四角），不移动内侧边界，
    // 不改变供流光/高光使用的厚度场与法线；没有桌面纹理时不启用。
    let ridge = -width * (0.45 + 0.60 * w0.x);
    let outer_refraction = select(0.0,
        smoothstep(ridge - width * 0.25, ridge, d), u.refr > 0.5);
    let refr_thick = max(thick, outer_refraction * u.intensity);

    // ---- 折射：近轴近似，位移方向垂直于等厚线、幅度随厚度 ----
    let bend = 64.0 * px;                     // 基准折射位移（渲染 px）
    let off = -n3.xy * refr_thick * bend;          // y-up
    let uv0 = in.pos.xy / res;                // top-down 0..1（与桌面纹理同向）
    let off_uv = vec2<f32>(off.x, -off.y) / res;  // 几何 y-up → uv 翻 y
    // 共同位移降至 1.5 倍；色散仍用基准偏移，保持彩边宽度。
    let refraction_gain = 1.5;
    let refracted_uv = uv0 + off_uv * refraction_gain;


    // ---- 菲涅尔（Schlick）：波脊斜率大 → 掠射 → 棱线高光 ----
    let fres = 0.04 + 0.96 * pow(1.0 - n3.z, 5.0);
    let spec = fres * u.intensity;

    // 物理边缘收口（含四角），1.5px 抗锯齿
    let d_edge = sd_box(q, c2, 0.0);
    let edge = 1.0 - smoothstep(-1.0, 1.0, d_edge);
    // 透镜可见度：只认「真正厚」的脊线（薄裙近乎全透）——
    // 低门槛会让宽厚的薄裙变成一层 96% 不透明的脏雾。
    let light_vis = smoothstep(0.22, 0.52, thick) * edge;
    let vis = max(light_vis,
        outer_refraction * smoothstep(0.22, 0.52, u.intensity));

    // ---- 单色主题蓝：沿用旋转波场的明暗起伏，不再计算颜色环流 ----
    // 色带贴死物理屏幕边缘（含四角：用锐角屏幕 SDF 而非内缩带圆角的透镜盒），
    // 贴边=1、向内 70px 渐隐；越贴边越鲜艳（去饱和向内渐淡）。
    // 亮度起伏贴波脊（wave 大处更亮），但保底亮度让边缘不断节。
    // 颜色是「光」，折射是「玻璃」。
    let cm = 1.0 - smoothstep(0.0, 70.0 * px, -d_edge);
    let raw_col = THEME_BLUE;
    let lum = dot(raw_col, vec3<f32>(0.299, 0.587, 0.114));
    let band_col = mix(vec3<f32>(lum, lum, lum), raw_col, 0.55 + 0.75 * cm);
    let glow = u.intensity * cm * (0.42 + 0.45 * (0.5 + 0.5 * w0.x));

    // 流光底色先行（透镜带外也有淡淡的色晕），玻璃内容叠上来；
    // 色带不乘 edge —— 最后一行像素也要吃满颜色（贴边无空隙）
    var col = band_col * glow * 0.95;
    var alpha = glow * 0.42;
    var refr_rgb = vec3<f32>(0.0);
    if (u.refr > 0.5 && refr_thick > 0.003) {
        // 色散：RGB 各用不同折射系数（蓝偏折最大），物理彩边
        // 薄裙 vis=0 时桌面项严格为零：省掉 15 次纹理采样，仍保留棱线高光。
        if (vis > 0.0) {
            // 只在实际采样时计算磨砂核，薄裙/无桌面降级不需要随机旋转。
            // 厚度越大光程越长，磨砂越强；采样半径与点数保持不变。
            let rr = (1.0 + 6.0 * thick) * px;
            let rough_r = vec2<f32>(rr, rr) / res;
            let jitter_ang = hash12(fc) * 6.2831853;
            let jr = mat2x2<f32>(cos(jitter_ang), -sin(jitter_ang), sin(jitter_ang), cos(jitter_ang));
            refr_rgb.r = sample_rough(refracted_uv - off_uv * 0.10, rough_r, jr).r;
            refr_rgb.g = sample_rough(refracted_uv, rough_r, jr).g;
            refr_rgb.b = sample_rough(refracted_uv + off_uv * 0.10, rough_r, jr).b;
        }
        // 扭曲桌面为主体（乘 vis：薄裙区不折射），被流光轻微渗色；
        // 菲涅尔处轻微压暗折射（能量守恒感）；棱线高光染一点同色
        if (thick > 0.003) {
            let spec_col = mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.30);
            col = col + refr_rgb * mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.16) * (1.0 - 0.20 * fres) * light_vis
                + spec_col * spec * 0.60;
            // 棱线高光更不透明一点，脊线立得住
            alpha = max(alpha, spec * 0.85 * edge);
        }
    } else if (thick > 0.003) {
        // 无抓屏降级：主题蓝玻璃边（剖面微光 + 棱线），不假装有折射
        col = col + band_col * (0.10 * thick)
            + mix(vec3<f32>(1.0, 1.0, 1.0), band_col, 0.4) * spec * 0.55;
        alpha = max(alpha, min(vis * 0.35 + spec * 0.50 * edge, 0.65));
    }

    // 抖动去色带：柔光渐变在暗底上极易 banding，加 +/-1 LSB 噪声
    let dith = (hash12(fc + vec2<f32>(fract(t) * 61.7)) - 0.5) * (1.8 / 255.0);
    col = col + vec3<f32>(dith);
    alpha = clamp(alpha + dith, 0.0, 1.0);
    // 先按扩展前的覆盖率合成光带，避免折射补满边缘时一起放大流光/高光。
    if (u.refr > 0.5 && thick > 0.003) {
        alpha = max(alpha, light_vis);
    }
    let light_alpha = alpha;
    var premultiplied = col * light_alpha;
    if (u.refr > 0.5 && refr_thick > 0.003) {
        // 新增覆盖只替换原本透出的桌面，不增加染色或高光。
        // 抖动后补满覆盖率，保证主体及物理边界不会漏出未折射原图。
        alpha = max(alpha, vis);
        premultiplied = premultiplied + refr_rgb * (alpha - light_alpha);
    }

    // premultiplied alpha 输出
    return vec4<f32>(premultiplied, alpha);
}
