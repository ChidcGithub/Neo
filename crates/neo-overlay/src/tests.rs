/// shader.wgsl 的任何语法/类型错误都会在窗口线程启动时才炸（wgpu 在
/// create_shader_module 报 naga 错）。把它前移到单测：解析 + 校验。
#[test]
fn shader_parses_and_validates() {
    let src = include_str!("shader.wgsl");
    for source in [src.to_owned(), old_angular_reference_source()] {
        let module = naga::front::wgsl::parse_str(&source).expect("shader 解析失败");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("shader 校验失败");
    }
}

#[test]
fn shader_uses_brand_d500_without_color_cycle() {
    let source = include_str!("shader.wgsl");
    let [r, g, b, _] = neo_theme::palette::deepseek::D500.to_array();
    assert_eq!([r, g, b], [52, 96, 180]);
    let declaration =
        format!("const THEME_BLUE: vec3<f32> = vec3<f32>({r}.0, {g}.0, {b}.0) / 255.0;");
    assert!(
        source.contains(&declaration),
        "shader 品牌蓝必须与 neo-theme D500 同步"
    );
    assert!(source.contains("let raw_col = THEME_BLUE;"));
    assert!(!source.contains("fn palette(") && !source.contains("atan2("));
    let fbm = source
        .split("fn fbm(")
        .nth(1)
        .unwrap()
        .split("// 圆角矩形")
        .next()
        .unwrap();
    assert!(fbm.contains("for (var i = 0; i < 4; i++)"), "保留四层 FBM");
    assert!(source.contains("let refraction_gain = 1.5;"));
    assert!(source.contains("smoothstep(0.0, 70.0 * px, -d_edge)"));
}

const ANGULAR_DERIVATIVE: &str = "let dH_ds = thickness_ds(d, w0, width, px, length(q));";
const FINAL_OUTPUT: &str = "return vec4<f32>(premultiplied, alpha);";

fn replace_once(source: &str, old: &str, new: &str) -> String {
    assert_eq!(
        source.matches(old).count(),
        1,
        "失效的 shader 测试锚点: {old}"
    );
    source.replacen(old, new, 1)
}

// 只快照被替换的旧函数，不复制整个 shader；中心值也来自旧标量实现，
// 避免参考与解析实现共享同一个波场错误。其余几何、光带、合成保持原样。
fn old_angular_reference_source() -> String {
    let source = replace_once(
        include_str!("shader.wgsl"),
        "let w0 = wave_field(dir, rot * dir, t, amp);",
        "let w0 = reference_wave_field(dir, rot * dir, t, amp);",
    );
    let source = replace_once(
        &source,
        ANGULAR_DERIVATIVE,
        "let dH_ds = reference_thickness_ds(q, tang, rot, t, amp, d, width, px, 2.0);",
    );
    format!("{source}\n{}", include_str!("shader_reference_tests.wgsl"))
}

#[test]
fn analytic_wave_evaluates_five_four_octave_fields_only_once() {
    let source = include_str!("shader.wgsl");
    assert_eq!(source.matches("wave_field(").count(), 2, "定义 + 一次求值");
    assert_eq!(source.matches("= fbm(").count(), 5);
    assert_eq!(source.matches("vnoise(q)").count(), 1);
    let noise = source
        .split("fn vnoise(")
        .nth(1)
        .unwrap()
        .split("fn fbm(")
        .next()
        .unwrap();
    assert_eq!(noise.matches("hash12(").count(), 4);
    assert!(source.contains("for (var i = 0; i < 4; i++)"));
    assert_eq!(
        source.matches("= sample_rough(").count(),
        3,
        "三通道各五采样"
    );
    assert!(source.contains("let refraction_gain = 1.5;"));
}

/// 用透镜管线离屏渲一帧：「桌面」RGBA 图 (tex_w×tex_h)，渲染到 out_w×out_h。
/// 返回 premultiplied 像素。无 GPU 的环境返回 None（调用方自行决定跳过还是失败）。
fn render_lens_frame(
    img: &[u8],
    tex_w: u32,
    tex_h: u32,
    out_w: u32,
    out_h: u32,
) -> Option<Vec<u8>> {
    render_lens_source(img, tex_w, tex_h, out_w, out_h, include_str!("shader.wgsl"))
}

fn render_lens_source(
    img: &[u8],
    tex_w: u32,
    tex_h: u32,
    out_w: u32,
    out_h: u32,
    source: &str,
) -> Option<Vec<u8>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).ok()?;

    let desktop_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-desktop"),
        size: wgpu::Extent3d {
            width: tex_w,
            height: tex_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &desktop_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        img,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * tex_w),
            rows_per_image: Some(tex_h),
        },
        wgpu::Extent3d {
            width: tex_w,
            height: tex_h,
            depth_or_array_layers: 1,
        },
    );

    let (pipeline, bind_layout) = super::Gfx::lens_pipeline_source(&device, source);

    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-uniform"),
        size: 48,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // time=1.7, level=0, intensity=1, spin=0.05, refr=1, pad×3, res（渲染）, tex（桌面）
    let uni: [f32; 12] = [
        1.7,
        0.0,
        1.0,
        0.05,
        1.0,
        0.0,
        0.0,
        0.0,
        out_w as f32,
        out_h as f32,
        tex_w as f32,
        tex_h as f32,
    ];
    queue.write_buffer(&uniform_buf, 0, bytemuck::cast_slice(&uni));

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let desktop_view = desktop_tex.create_view(&Default::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("test-bind"),
        layout: &bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&desktop_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-target"),
        size: wgpu::Extent3d {
            width: out_w,
            height: out_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test-enc"),
    });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    // 读回（out_w*4 必须是 256 的倍数：调用方选尺寸时注意）
    assert_eq!((out_w * 4) % 256, 0, "读回行宽需 256 对齐");
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-readback"),
        size: (out_w * out_h * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * out_w),
                rows_per_image: Some(out_h),
            },
        },
        wgpu::Extent3d {
            width: out_w,
            height: out_h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| ());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range().unwrap();
    Some(data.to_vec())
}

/// 在 GPU 上检查实际 WGSL（不是 Rust 复刻）：旧标量值 + 小步长有限差分。
#[test]
fn analytic_noise_and_fbm_match_finite_differences_offscreen() {
    let helpers = include_str!("shader.wgsl")
        .split("@fragment")
        .next()
        .unwrap();
    let image = [0, 0, 0, 255].repeat(128 * 128);
    for (function, epsilon, tolerance) in [
        ("vnoise", "0.001", "0.003 + 0.003 * abs(expected)"),
        ("fbm", "0.002", "0.04 + 0.02 * abs(expected)"),
    ] {
        // 负坐标与多个噪声格；FBM 后三层有频率链式因子。
        // 差分点避开精确整数：hash 的 fract 在 GPU 舍入下不宜用跨格差分校验。
        // 整数格点的解析梯度另按 cubic smoothstep 的零斜率检查。
        let source = format!(
            r#"{helpers}
{}
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{
    let p = (floor(in.pos.xy) - vec2<f32>(64.0) + vec2<f32>(0.37)) / 8.0;
    let h = {epsilon};
    let actual = {function}(p);
    let expected = vec2<f32>(
        reference_{function}(p + vec2<f32>(h, 0.0)) - reference_{function}(p - vec2<f32>(h, 0.0)),
        reference_{function}(p + vec2<f32>(0.0, h)) - reference_{function}(p - vec2<f32>(0.0, h))) / (2.0 * h);
    let error = abs(actual.yz - expected) / ({tolerance});
    // 标量/向量代码经驱动优化后 hash 的浮点舍入可放大，允许 5e-4 值误差。
    let value_error = max(abs(actual.x - reference_{function}(p)) / 0.0005,
        length(vnoise(floor(p)).yz) / 0.000001);
    let ratios = vec3<f32>(error, value_error);
    return vec4<f32>(ratios * 0.5, select(1.0, 0.0, all(ratios <= vec3<f32>(1.0))));
}}
"#,
            include_str!("shader_reference_tests.wgsl")
        );
        let Some(frame) = render_lens_source(&image, 128, 128, 128, 128, &source) else {
            eprintln!("无 GPU adapter，跳过解析噪声有限差分测试");
            return;
        };
        assert_gradient_probe(&frame, function);
    }
}

fn assert_gradient_probe(frame: &[u8], label: &str) {
    let mut maxima = [0u8; 3];
    let mut failures = 0;
    for pixel in frame.chunks_exact(4) {
        for c in 0..3 {
            maxima[c] = maxima[c].max(pixel[c]);
        }
        failures += usize::from(pixel[3] != 0);
    }
    eprintln!(
        "{label}: 最大误差/容差（8-bit 读回）≈ {:?}, 失败点 {failures}",
        maxima.map(|v| v as f32 * 2.0 / 255.0)
    );
    assert_eq!(failures, 0, "{label}: 有限差分与解析梯度超出容差");
}

#[test]
fn analytic_wave_and_thickness_chain_match_finite_differences_offscreen() {
    let helpers = include_str!("shader.wgsl")
        .split("@fragment")
        .next()
        .unwrap();
    let image = [0, 0, 0, 255].repeat(128 * 128);
    // θ 覆盖整圈（含接缝），旋转/时间/电平/宽高尺度不同；d 跨不对称剖面两侧及拖尾窗。
    for (scale, time, level) in [(0.333333, 1.7, 0.0), (1.5, 8.0, 1.0)] {
        let source = format!(
            r#"{helpers}
{}
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{
    let theta = floor(in.pos.x) / 127.0 * 6.2831853;
    let dir = vec2<f32>(cos(theta), sin(theta));
    let tang = vec2<f32>(-dir.y, dir.x);
    let ra = {time} * 0.05;
    let rot = mat2x2<f32>(cos(ra), -sin(ra), sin(ra), cos(ra));
    let amp = 1.0 + {level} * 0.38;
    let waves = wave_field(dir, rot * dir, {time}, amp);
    let h = 0.0005;
    let dp = vec2<f32>(cos(theta + h), sin(theta + h));
    let dm = vec2<f32>(cos(theta - h), sin(theta - h));
    let expected = (reference_wave_field(dp, rot * dp, {time}, amp)
        - reference_wave_field(dm, rot * dm, {time}, amp)) / (2.0 * h);
    let wave_error = abs(waves.zw - expected) / (vec2<f32>(0.035) + 0.02 * abs(expected));
    let px = {scale};
    let radius = (80.0 + in.pos.y * 4.0) * px;
    let d = (20.0 - floor(in.pos.y)) * px;
    let ds = radius * h;
    let expected_ds = reference_thickness_ds(dir * radius, tang, rot, {time}, amp, d, 46.0 * px, px, ds);
    let actual_ds = thickness_ds(d, waves, 46.0 * px, px, radius);
    let thickness_error = abs(actual_ds - expected_ds) / (0.00015 / px + 0.03 * abs(expected_ds));
    let ratios = vec3<f32>(wave_error, thickness_error);
    return vec4<f32>(ratios * 0.5, select(1.0, 0.0, all(ratios <= vec3<f32>(1.0))));
}}
"#,
            include_str!("shader_reference_tests.wgsl")
        );
        let Some(frame) = render_lens_source(&image, 128, 128, 128, 128, &source) else {
            eprintln!("无 GPU adapter，跳过解析波场链式导数测试");
            return;
        };
        assert_gradient_probe(&frame, &format!("wave/thickness scale={scale}"));
    }
}

/// 最终图像不是逐像素等价：局部解析斜率替代旧 ±2px 平均斜率。
/// 排除透明内区统计 RGB，防止大量零像素掩盖实际透镜带误差。
#[test]
fn analytic_wave_quality_against_old_reference_offscreen() {
    let source = include_str!("shader.wgsl");
    let reference = old_angular_reference_source();
    for (w, h, tex_w, tex_h, time, level, intensity, refr) in [
        (256, 144, 512, 288, "1.7", "0.0", "1.0", "1.0"),
        (192, 320, 384, 640, "8.0", "1.0", "1.0", "1.0"),
        (768, 432, 1280, 720, "3.4", "0.6", "0.4", "1.0"),
        (1152, 648, 1920, 1080, "8.0", "1.0", "1.0", "1.0"),
        (256, 144, 256, 144, "8.0", "1.0", "0.4", "0.0"),
    ] {
        let configure = |s: &str| {
            s.replace("u.time", time)
                .replace("u.level", level)
                .replace("u.intensity", &format!("{intensity}f"))
                .replace("u.refr", refr)
        };
        let current = configure(source);
        let old = configure(&reference);
        // 合成桌面同时有高频棋盘、单像素文字状细线和彩色渐变，无真实桌面采集。
        let mut image = vec![0u8; (tex_w * tex_h * 4) as usize];
        for y in 0..tex_h {
            for x in 0..tex_w {
                let i = ((y * tex_w + x) * 4) as usize;
                let value = if (x / 8 + y / 8) % 2 == 0 { 40 } else { 215 };
                image[i..i + 4].copy_from_slice(&[
                    value,
                    if y % 13 == 0 { 240 } else { value },
                    (x * 255 / tex_w) as u8,
                    255,
                ]);
            }
        }
        let Some(actual) = render_lens_source(&image, tex_w, tex_h, w, h, &current) else {
            eprintln!("无 GPU adapter，跳过旧参考质量测试");
            return;
        };
        let expected = render_lens_source(&image, tex_w, tex_h, w, h, &old).unwrap();
        let (mut sum, mut square, mut count) = (0u64, 0u64, 0u64);
        let mut histogram = [0u64; 256];
        let (mut max_rgb, mut max_alpha) = (0, 0);
        for (i, (a, b)) in actual
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .enumerate()
        {
            max_alpha = max_alpha.max(a[3].abs_diff(b[3]));

            let (x, y) = (i as u32 % w, i as u32 / w);
            if refr == "1.0" && intensity == "1.0" && (x == 0 || y == 0 || x == w - 1 || y == h - 1)
            {
                assert_eq!((a[3], b[3]), (255, 255), "四边四角向外折射不能漏底");
            }
            if a[3].max(b[3]) > 0 {
                for c in 0..3 {
                    let delta = a[c].abs_diff(b[c]);
                    histogram[delta as usize] += 1;
                    sum += delta as u64;
                    square += (delta as u64).pow(2);
                    count += 1;
                    max_rgb = max_rgb.max(delta);
                }
            }
        }
        let mean = sum as f64 / count as f64;
        let rmse = (square as f64 / count as f64).sqrt();
        let mut cumulative = 0;
        let p99 = histogram
            .iter()
            .position(|n| {
                cumulative += n;
                cumulative * 100 >= count * 99
            })
            .unwrap();
        eprintln!("old-reference {w}x{h} tex={tex_w}x{tex_h} t={time} level={level} intensity={intensity} refr={refr}: visible RGB MAE={mean:.4}, RMSE={rmse:.4}, P99={p99}, max={max_rgb}, alpha max={max_alpha} LSB");
        // 低分辨率旧 ±2px 差分有明显低通作用，单独限定压力场景预算；
        // 正常尺度用更严格预算，不能让小尺寸的宽容差掩盖回归。
        let (mae_limit, rmse_limit, p99_limit, max_limit) = match (w, h, refr) {
            (_, _, "0.0") => (0.01, 0.1, 1, 2),
            (256, 144, _) => (5.0, 14.0, 70, 150),
            (192, 320, _) => (11.0, 23.0, 95, 170),
            (768, 432, _) => (1.0, 4.0, 18, 85),
            _ => (3.5, 10.0, 50, 165),
        };
        assert!(
            mean <= mae_limit && rmse <= rmse_limit && p99 <= p99_limit && max_rgb <= max_limit,
            "解析斜率与旧有限差分的视觉偏差超出预算"
        );
        assert!(max_alpha <= 8, "覆盖率偏差不能超过 8 LSB");

        // 独立光带应保持原值，合成公式不变；上面的最终像素允许折射/高光近似误差。
        let light = "return vec4<f32>(band_col * glow * 0.95, glow * 0.42);";
        let actual_light = render_lens_source(
            &image,
            tex_w,
            tex_h,
            w,
            h,
            &replace_once(&current, FINAL_OUTPUT, light),
        )
        .unwrap();
        let expected_light = render_lens_source(
            &image,
            tex_w,
            tex_h,
            w,
            h,
            &replace_once(&old, FINAL_OUTPUT, light),
        )
        .unwrap();
        assert!(
            actual_light
                .iter()
                .zip(&expected_light)
                .all(|(a, b)| a.abs_diff(*b) <= 1),
            "主题、光带边界与亮度不得改变"
        );
        let bounds =
            "return vec4<f32>(outer_refraction, light_vis, select(0.0, 1.0, vis > 0.0), 1.0);";
        let actual_bounds = render_lens_source(
            &image,
            tex_w,
            tex_h,
            w,
            h,
            &replace_once(&current, FINAL_OUTPUT, bounds),
        )
        .unwrap();
        let expected_bounds = render_lens_source(
            &image,
            tex_w,
            tex_h,
            w,
            h,
            &replace_once(&old, FINAL_OUTPUT, bounds),
        )
        .unwrap();
        assert!(
            actual_bounds
                .iter()
                .zip(&expected_bounds)
                .all(|(a, b)| a.abs_diff(*b) <= 1),
            "向外折射范围、光覆盖场与透明内区不能改变"
        );
        let center = ((h / 2 * w + w / 2) * 4) as usize;
        assert_eq!(&actual[center..center + 4], &[0, 0, 0, 0]);
    }
}

/// 离屏渲染验证透镜真的在扭曲「桌面」：灰度棋盘纹理进管线、读回像素断言 ——
/// 1. 屏幕中心完全透明（早退区不画）；
/// 2. 透镜带内 alpha 接近不透明（折射区要显示扭曲桌面）；
/// 3. 带内存在与原图错位的像素（折射确实发生了位移）；
/// 4. 色度有界：主题蓝流光是设计（颜色是「光」），不能出现通道错接式的爆色。
#[test]
fn lens_refracts_desktop_offscreen() {
    const W: u32 = 960;
    const H: u32 = 540; // 假想 1600x900 的 0.6x 离屏

    // 合成桌面：16px 灰度棋盘（高对比，折射错位一眼可辨；灰度便于查「无色」）
    let mut img = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let v = if ((x / 16) + (y / 16)) % 2 == 0 {
                40u8
            } else {
                215u8
            };
            let i = ((y * W + x) * 4) as usize;
            img[i] = v;
            img[i + 1] = v;
            img[i + 2] = v;
            img[i + 3] = 255;
        }
    }

    let Some(data) = render_lens_frame(&img, W, H, W, H) else {
        eprintln!("无 GPU adapter，跳过离屏渲染测试");
        return;
    };

    // 恢复优化前的无条件磨砂核计算与采样，逐像素确认不损失画质。
    let source = include_str!("shader.wgsl");
    assert_eq!(source.matches("if (vis > 0.0)").count(), 1);
    let kernel_start = source.find("            let rr =").unwrap();
    let kernel_end = source.find("            refr_rgb.r =").unwrap();
    let kernel = &source[kernel_start..kernel_end];
    let reference_source = source
        .replace(kernel, "")
        .replace("if (vis > 0.0)", "if (true)");
    let anchor = "    // ---- 菲涅尔";
    assert_eq!(reference_source.matches(anchor).count(), 1);
    let reference_source = reference_source.replace(anchor, &format!("{kernel}\n{anchor}"));
    let reference = render_lens_source(&img, W, H, W, H, &reference_source)
        .expect("首次离屏渲染已成功，参考渲染不应失败");
    let max_delta = data
        .iter()
        .zip(&reference)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(max_delta <= 1, "优化前后像素最大差 {max_delta} 超过 1 LSB");
    eprintln!(
        "合成棋盘 GPU 验收：{} 像素，优化前后最大通道差 {max_delta} LSB",
        W * H
    );

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };

    // 1. 屏幕中心完全透明（早退区）
    let center = px(W / 2, H / 2);
    assert_eq!(center[3], 0, "屏幕中心应完全透明, got {center:?}");

    // 2. 左缘透镜带中点 alpha 接近不透明
    //    （inset 12 + 透镜脊 d≈-22 → x≈34，y 取垂直中点避开圆角）
    let band = px(34, H / 2);
    assert!(
        band[3] >= 180,
        "透镜带内应接近不透明, got alpha={} at (34, {})",
        band[3],
        H / 2
    );

    // 3 & 4. 左缘竖带扫描：折射错位存在性 + 平均色度
    let mut shifted = 0u64;
    let mut chroma_sum = 0u64;
    let mut count = 0u64;
    for y in (H / 4)..(H * 3 / 4) {
        for x in 12..64 {
            let p = px(x, y);
            if p[3] < 128 {
                continue; // 只看透镜带内不透明像素
            }
            count += 1;
            let si = ((y * W + x) * 4) as usize;
            if p[0] != img[si] {
                shifted += 1;
            }
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            chroma_sum += (r - g).unsigned_abs() as u64 + (g - b).unsigned_abs() as u64;
        }
    }
    assert!(count > 1000, "透镜带覆盖像素太少: {count}");
    assert!(
        shifted > count / 4,
        "折射错位像素过少（{shifted}/{count}），透镜没在扭曲桌面"
    );
    let mean_chroma = chroma_sum as f64 / count as f64;
    // 灰度棋盘 + 主题蓝流光：每像素 |r-g|+|g-b| 的量级应在「有色但温和」
    // 区间；超过 150 意味着通道错接 / 爆色之类的管线事故。
    assert!(
        mean_chroma > 1.0 && mean_chroma < 150.0,
        "平均色度 {mean_chroma:.1} 异常：流光滑失（≈0）或爆色（>150）"
    );
}

/// 隔离流光场：只能是蓝色，随时间有明暗变化，且不能扩大 70px 光带。
#[test]
fn theme_blue_flows_without_widening_band_offscreen() {
    const W: u32 = 512;
    const H: u32 = 288;
    let source = include_str!("shader.wgsl");
    let output = "return vec4<f32>(premultiplied, alpha);";
    assert_eq!(source.matches(output).count(), 1);
    let light_source = source.replace(
        output,
        "return vec4<f32>(band_col * glow * 0.95, glow * 0.42);",
    );
    let image = [0, 0, 0, 255].repeat((W * H) as usize);
    let Some(first) = render_lens_source(&image, W, H, W, H, &light_source) else {
        eprintln!("无 GPU adapter，跳过主题蓝流动离屏测试");
        return;
    };
    let later =
        render_lens_source(&image, W, H, W, H, &light_source.replace("u.time", "8.0")).unwrap();
    let mut changed = 0;
    for frame in [&first, &later] {
        let mut blue = 0;
        let (mut darkest, mut brightest) = (u8::MAX, 0);
        for (i, pixel) in frame.chunks_exact(4).enumerate() {
            if pixel[3] >= 4 {
                // 暗部 UNORM 量化可能使相邻通道相等，但不能颠倒蓝色通道次序。
                assert!(
                    pixel[2] >= pixel[1] && pixel[1] >= pixel[0],
                    "流光必须保持主题蓝，不应绕环变色: {pixel:?}"
                );
            }
            if pixel[3] >= 16 {
                assert!(
                    pixel[2] > pixel[1] && pixel[1] > pixel[0],
                    "可见光带必须有明确蓝色色度: {pixel:?}"
                );
                blue += 1;
            }
            let (x, y) = (i as u32 % W, i as u32 / W);
            let distance = x.min(W - 1 - x).min(y.min(H - 1 - y)) as f32 + 0.5;
            if distance >= 70.0 * H as f32 / 432.0 {
                assert_eq!(pixel, &[0, 0, 0, 0], "流光不能超出原有宽度");
            }
            if x == 0 || x == W - 1 || y == 0 || y == H - 1 {
                darkest = darkest.min(pixel[2]);
                brightest = brightest.max(pixel[2]);
            }
        }
        assert!(blue > 1000, "应保留连续可见的蓝色光带");
        assert!(brightest - darkest > 2, "单色光带仍应沿周长有明暗起伏");
    }
    for (a, b) in first.chunks_exact(4).zip(later.chunks_exact(4)) {
        if a[2].abs_diff(b[2]) > 1 {
            changed += 1;
        }
    }
    assert!(changed > 100, "单色不能变成静态边框: {changed}");

    // 配色只影响 RGB，不能改变最终预乘输出的覆盖率（含无桌面降级）。
    for refr in ["0.0", "1.0"] {
        let blue_source = source.replace("u.refr", refr);
        let neutral_source =
            blue_source.replace("let raw_col = THEME_BLUE;", "let raw_col = vec3<f32>(0.5);");
        let blue = render_lens_source(&image, W, H, W, H, &blue_source).unwrap();
        let neutral = render_lens_source(&image, W, H, W, H, &neutral_source).unwrap();
        assert!(
            blue.chunks_exact(4)
                .zip(neutral.chunks_exact(4))
                .all(|(a, b)| a[3] == b[3]),
            "单色配色不得改变透明合成覆盖率，refr={refr}"
        );
    }
}

/// 在黑/白底上合成同一帧：折射主体不得再透出未折射桌面。
#[test]
fn refraction_core_replaces_desktop_without_background_leak_offscreen() {
    const W: u32 = 256;
    const H: u32 = 144;
    let source = include_str!("shader.wgsl");
    let image = [112, 128, 144, 255].repeat((W * H) as usize);
    let Some(frame) = render_lens_source(&image, W, H, W, H, source) else {
        eprintln!("无 GPU adapter，跳过折射覆盖率离屏测试");
        return;
    };
    // GPU 直接标记完全覆盖与过渡区域，避免把 UNORM 舍入的近似 1 当成主体。
    let output = "return vec4<f32>(premultiplied, alpha);";
    assert_eq!(source.matches(output).count(), 1);
    let mask_source = source.replace(output,
        "return vec4<f32>(select(0.0, 1.0, vis >= 1.0), select(0.0, 1.0, vis > 0.0 && vis < 1.0), 0.0, 1.0);");
    let mask = render_lens_source(&image, W, H, W, H, &mask_source).unwrap();
    let (mut core, mut transition) = (0, 0);
    for (pixel, mask) in frame.chunks_exact(4).zip(mask.chunks_exact(4)) {
        if mask[0] == 255 {
            core += 1;
            assert_eq!(
                pixel[3], 255,
                "折射主体必须完全遮住未折射桌面，包括负抖动像素"
            );
            for channel in &pixel[..3] {
                let over_black = *channel as u16;
                let over_white = *channel as u16 + 255 - pixel[3] as u16;
                assert_eq!(over_black, over_white, "主体合成结果不能随底下的桌面变化");
            }
        } else if mask[1] == 255 && pixel[3] > 0 && pixel[3] < 255 {
            transition += 1;
        }
    }
    assert!(
        core > 100 && transition > 100,
        "主体和柔和边缘都必须保留: {core}, {transition}"
    );
    assert_eq!(
        frame[((H / 2 * W + W / 2) * 4 + 3) as usize],
        0,
        "屏幕中心不应被覆盖"
    );

    // 没有可用桌面纹理时，不能用不透明空白代替半透明流光。
    let fallback_source = source.replace("u.refr", "0.0");
    let fallback = render_lens_source(&image, W, H, W, H, &fallback_source).unwrap();
    assert!(fallback.chunks_exact(4).all(|pixel| pixel[3] < 255));
    assert!(fallback.chunks_exact(4).any(|pixel| pixel[3] > 0));
}

/// 折射覆盖四条物理边和四角；扩展不改变流光场及无桌面降级。
#[test]
fn refraction_reaches_screen_edges_without_changing_color_band_offscreen() {
    let source = include_str!("shader.wgsl");
    let extension = "smoothstep(ridge - width * 0.25, ridge, d)";
    assert_eq!(source.matches(extension).count(), 1);
    let baseline = source.replace(extension, "0.0");
    let output = "return vec4<f32>(premultiplied, alpha);";
    assert_eq!(source.matches(output).count(), 1);
    let color_output = "return vec4<f32>(band_col * glow * 0.95, glow * 0.42);";
    for (w, h, time, level) in [(256u32, 144u32, "1.7", "0.0"), (192, 320, "8.0", "1.0")] {
        let current = source.replace("u.time", time).replace("u.level", level);
        let old = baseline.replace("u.time", time).replace("u.level", level);
        let image = [112, 128, 144, 255].repeat((w * h) as usize);
        let Some(frame) = render_lens_source(&image, w, h, w, h, &current) else {
            eprintln!("无 GPU adapter，跳过贴边折射离屏测试");
            return;
        };
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    assert_eq!(
                        frame[((y * w + x) * 4 + 3) as usize],
                        255,
                        "物理边界不能漏底: {w}x{h}, ({x}, {y})"
                    );
                }
            }
        }
        assert_eq!(frame[((h / 2 * w + w / 2) * 4 + 3) as usize], 0);
        let color =
            render_lens_source(&image, w, h, w, h, &current.replace(output, color_output)).unwrap();
        let old_color =
            render_lens_source(&image, w, h, w, h, &old.replace(output, color_output)).unwrap();
        assert_eq!(
            color, old_color,
            "颜色光带的宽度、配色、亮度应逐像素保持不变"
        );
        // 不只检查独立的光带场：最终预乘输出叠到均匀桌面后也必须一致。
        // 黑底验证光带/高光亮度，彩色底同时覆盖染色与新增桌面覆盖率。
        for background in [[0u8, 0, 0, 255], [32, 48, 64, 255]] {
            let flat = background.repeat((w * h) as usize);
            for intensity in ["1.0f", "0.4f"] {
                let actual = render_lens_source(
                    &flat,
                    w,
                    h,
                    w,
                    h,
                    &current.replace("u.intensity", intensity),
                )
                .unwrap();
                let expected =
                    render_lens_source(&flat, w, h, w, h, &old.replace("u.intensity", intensity))
                        .unwrap();
                for (index, (a, b)) in actual
                    .chunks_exact(4)
                    .zip(expected.chunks_exact(4))
                    .enumerate()
                {
                    for channel in 0..3 {
                        let composite = |p: &[u8]| {
                            p[channel] as f32
                                + background[channel] as f32 * (1.0 - p[3] as f32 / 255.0)
                        };
                        assert!((composite(a) - composite(b)).abs() <= 2.0,
                            "扩展不能改变最终合成的光带: {w}x{h}, 像素 {index}, 通道 {channel}, 强度 {intensity}");
                    }
                }
            }
        }
        let fallback =
            render_lens_source(&image, w, h, w, h, &current.replace("u.refr", "0.0")).unwrap();
        let old_fallback =
            render_lens_source(&image, w, h, w, h, &old.replace("u.refr", "0.0")).unwrap();
        assert_eq!(fallback, old_fallback, "无桌面纹理时保持原有降级效果");
    }
}

/// 合成纹理验收：只减弱共同折射位移，不改变流光/高光/透明度。
#[test]
fn weaker_refraction_preserves_color_and_alpha_offscreen() {
    const W: u32 = 256;
    const H: u32 = 144;
    let source = include_str!("shader.wgsl");
    assert_eq!(source.matches("let refraction_gain = 1.5;").count(), 1);
    let baseline = source.replace("let refraction_gain = 1.5;", "let refraction_gain = 2.0;");
    let flat = [112, 128, 144, 255].repeat((W * H) as usize);
    let Some(current) = render_lens_source(&flat, W, H, W, H, source) else {
        eprintln!("无 GPU adapter，跳过折射增益离屏测试");
        return;
    };
    let original = render_lens_source(&flat, W, H, W, H, &baseline).unwrap();
    assert!(
        current
            .iter()
            .zip(&original)
            .all(|(a, b)| a.abs_diff(*b) <= 1),
        "均匀桌面上的流光颜色、高光和透明度不应随折射增益改变"
    );

    let mut patterned = flat;
    for (i, pixel) in patterned.chunks_exact_mut(4).enumerate() {
        let value = if (i % W as usize / 4 + i / W as usize / 4).is_multiple_of(2) {
            40
        } else {
            210
        };
        pixel[..3].fill(value);
    }
    let current = render_lens_source(&patterned, W, H, W, H, source).unwrap();
    let original = render_lens_source(&patterned, W, H, W, H, &baseline).unwrap();
    let mut changed = 0;
    for (a, b) in current.chunks_exact(4).zip(original.chunks_exact(4)) {
        assert_eq!(a[3], b[3], "折射增益不能改变覆盖层透明度");
        if a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > 2) {
            changed += 1;
        }
    }
    assert!(
        changed > 100,
        "折射减弱应改变纹理采样位置，实际仅 {changed} 像素变化"
    );
}

/// 手动预览：拟真桌面（壁纸渐变 + 窗口块 + 文字行 + 任务栏），按真实管线
/// 0.6x 离屏渲一帧再线性放大回全尺寸，落盘 target/lens-preview.png 供人工调参。
/// `cargo test -p neo-overlay -- --ignored`
#[test]
#[ignore]
fn lens_preview() {
    const W: u32 = 1280;
    const H: u32 = 720;
    const RW: u32 = 768; // 0.6x 离屏（与生产 RENDER_SCALE 一致）
    const RH: u32 = 432;

    // 拟真桌面：竖向**亮**色渐变壁纸（教室场景多是亮底 PPT，暗底会
    // 掩盖雾感误判）+ 左侧一个白「窗口」（含横线文字带）+ 底部任务栏
    let mut img = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let g = 150.0 + 60.0 * (y as f32 / H as f32);
            let (mut r, mut gg, mut b) = (g * 0.96, g * 0.98, g * 1.04);
            // 白窗口：x 160..760, y 120..500
            if (160..760).contains(&x) && (120..500).contains(&y) {
                r = 248.0;
                gg = 250.0;
                b = 252.0;
                // 文字行：每 22px 一条 6px 灰带
                if y > 150 && (y % 22) < 6 && x > 190 && x < 730 {
                    r = 150.0;
                    gg = 152.0;
                    b = 156.0;
                }
            }
            // 任务栏
            if y >= H - 44 {
                r = 226.0;
                gg = 229.0;
                b = 234.0;
                // 任务栏图标格
                if x > 60 && x < 600 && (x % 52) < 36 && y > H - 38 && y < H - 8 {
                    r = 90.0;
                    gg = 140.0;
                    b = 200.0;
                }
            }
            let i = ((y * W + x) * 4) as usize;
            img[i] = r as u8;
            img[i + 1] = gg as u8;
            img[i + 2] = b as u8;
            img[i + 3] = 255;
        }
    }

    let Some(data) = render_lens_frame(&img, W, H, RW, RH) else {
        eprintln!("无 GPU adapter，无法生成预览");
        return;
    };
    // 模拟生产 blit：0.6x 离屏线性放大回全尺寸，再按 premultiplied
    // alpha over 合成回原桌面 —— 这才是用户视角的最终效果
    // （直接存 RGBA 的话，查看器会把透明区显示成白，看不清透镜扭曲）。
    let frame = image::RgbaImage::from_vec(RW, RH, data).expect("帧尺寸不符");
    let up = image::imageops::resize(&frame, W, H, image::imageops::FilterType::Triangle);
    let mut composed = img.clone();
    for (dst, src) in composed.chunks_exact_mut(4).zip(up.chunks_exact(4)) {
        let a = src[3] as u16;
        let inv = 255 - a;
        for c in 0..3 {
            // premultiplied over：out = src.rgb + dst.rgb × (1 - src.a)
            dst[c] = (src[c] as u16 + dst[c] as u16 * inv / 255).min(255) as u8;
        }
        dst[3] = 255;
    }
    let out =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lens-preview.png");
    image::save_buffer(&out, &composed, W, H, image::ColorType::Rgba8).expect("预览图落盘失败");
    eprintln!("预览图 → {}", out.display());
}
