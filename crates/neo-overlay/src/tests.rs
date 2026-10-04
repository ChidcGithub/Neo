/// shader.wgsl 的任何语法/类型错误都会在窗口线程启动时才炸（wgpu 在
/// create_shader_module 报 naga 错）。把它前移到单测：解析 + 校验。
#[test]
fn shader_parses_and_validates() {
    let src = include_str!("shader.wgsl");
    let module = naga::front::wgsl::parse_str(src).expect("shader.wgsl 解析失败");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("shader.wgsl 校验失败");
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
    img: &[u8], tex_w: u32, tex_h: u32, out_w: u32, out_h: u32, source: &str,
) -> Option<Vec<u8>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&Default::default())).ok()?;

    let desktop_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-desktop"),
        size: wgpu::Extent3d { width: tex_w, height: tex_h, depth_or_array_layers: 1 },
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
        wgpu::Extent3d { width: tex_w, height: tex_h, depth_or_array_layers: 1 },
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
        size: wgpu::Extent3d { width: out_w, height: out_h, depth_or_array_layers: 1 },
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
        wgpu::Extent3d { width: out_w, height: out_h, depth_or_array_layers: 1 },
    );
    queue.submit(std::iter::once(enc.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| ());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range().unwrap();
    Some(data.to_vec())
}

/// 离屏渲染验证透镜真的在扭曲「桌面」：灰度棋盘纹理进管线、读回像素断言 ——
/// 1. 屏幕中心完全透明（早退区不画）；
/// 2. 透镜带内 alpha 接近不透明（折射区要显示扭曲桌面）；
/// 3. 带内存在与原图错位的像素（折射确实发生了位移）；
/// 4. 色度有界：彩色流光是设计（颜色是「光」），但必须是粉彩渐变
///    级别，不能出现通道错接式的爆色。
#[test]
fn lens_refracts_desktop_offscreen() {
    const W: u32 = 960;
    const H: u32 = 540; // 假想 1600x900 的 0.6x 离屏

    // 合成桌面：16px 灰度棋盘（高对比，折射错位一眼可辨；灰度便于查「无色」）
    let mut img = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let v = if ((x / 16) + (y / 16)) % 2 == 0 { 40u8 } else { 215u8 };
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

    // 恢复优化前的无条件采样，逐像素确认薄裙裁剪不损失画质。
    let source = include_str!("shader.wgsl");
    assert_eq!(source.matches("if (vis > 0.0)").count(), 1);
    let reference_source = source.replace("if (vis > 0.0)", "if (true)");
    let reference = render_lens_source(&img, W, H, W, H, &reference_source)
        .expect("首次离屏渲染已成功，参考渲染不应失败");
    let max_delta = data.iter().zip(&reference).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(max_delta <= 1, "优化前后像素最大差 {max_delta} 超过 1 LSB");
    eprintln!("合成棋盘 GPU 验收：{} 像素，优化前后最大通道差 {max_delta} LSB", W * H);

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
    // 灰度棋盘 + 粉彩流光：每像素 |r-g|+|g-b| 的量级应在「彩色但温和」
    // 区间；超过 150 意味着通道错接 / 爆色之类的管线事故。
    assert!(
        mean_chroma > 1.0 && mean_chroma < 150.0,
        "平均色度 {mean_chroma:.1} 异常：流光滑失（≈0）或爆色（>150）"
    );
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
            assert_eq!(pixel[3], 255, "折射主体必须完全遮住未折射桌面，包括负抖动像素");
            for channel in &pixel[..3] {
                let over_black = *channel as u16;
                let over_white = *channel as u16 + 255 - pixel[3] as u16;
                assert_eq!(over_black, over_white, "主体合成结果不能随底下的桌面变化");
            }
        } else if mask[1] == 255 && pixel[3] > 0 && pixel[3] < 255 {
            transition += 1;
        }
    }
    assert!(core > 100 && transition > 100, "主体和柔和边缘都必须保留: {core}, {transition}");
    assert_eq!(frame[((H / 2 * W + W / 2) * 4 + 3) as usize], 0,
        "屏幕中心不应被覆盖");

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
                    assert_eq!(frame[((y * w + x) * 4 + 3) as usize], 255,
                        "物理边界不能漏底: {w}x{h}, ({x}, {y})");
                }
            }
        }
        assert_eq!(frame[((h / 2 * w + w / 2) * 4 + 3) as usize], 0);
        let color = render_lens_source(&image, w, h, w, h, &current.replace(output, color_output)).unwrap();
        let old_color = render_lens_source(&image, w, h, w, h, &old.replace(output, color_output)).unwrap();
        assert_eq!(color, old_color, "颜色光带的宽度、配色、亮度应逐像素保持不变");
        // 不只检查独立的光带场：最终预乘输出叠到均匀桌面后也必须一致。
        // 黑底验证光带/高光亮度，彩色底同时覆盖染色与新增桌面覆盖率。
        for background in [[0u8, 0, 0, 255], [32, 48, 64, 255]] {
            let flat = background.repeat((w * h) as usize);
            for intensity in ["1.0f", "0.4f"] {
                let actual = render_lens_source(&flat, w, h, w, h,
                    &current.replace("u.intensity", intensity)).unwrap();
                let expected = render_lens_source(&flat, w, h, w, h,
                    &old.replace("u.intensity", intensity)).unwrap();
                for (index, (a, b)) in actual.chunks_exact(4).zip(expected.chunks_exact(4)).enumerate() {
                    for channel in 0..3 {
                        let composite = |p: &[u8]| p[channel] as f32
                            + background[channel] as f32 * (1.0 - p[3] as f32 / 255.0);
                        assert!((composite(a) - composite(b)).abs() <= 2.0,
                            "扩展不能改变最终合成的光带: {w}x{h}, 像素 {index}, 通道 {channel}, 强度 {intensity}");
                    }
                }
            }
        }
        let fallback = render_lens_source(&image, w, h, w, h, &current.replace("u.refr", "0.0")).unwrap();
        let old_fallback = render_lens_source(&image, w, h, w, h, &old.replace("u.refr", "0.0")).unwrap();
        assert_eq!(fallback, old_fallback, "无桌面纹理时保持原有降级效果");
    }
}

/// 合成纹理验收：只减弱共同折射位移，不改变流光/高光/透明度。
#[test]
fn weaker_refraction_preserves_color_and_alpha_offscreen() {
    const W: u32 = 256;
    const H: u32 = 144;
    let source = include_str!("shader.wgsl");
    assert_eq!(source.matches("let refraction_gain = 2.0;").count(), 1);
    let baseline = source.replace("let refraction_gain = 2.0;", "let refraction_gain = 3.0;");
    let flat = [112, 128, 144, 255].repeat((W * H) as usize);
    let Some(current) = render_lens_source(&flat, W, H, W, H, source) else {
        eprintln!("无 GPU adapter，跳过折射增益离屏测试");
        return;
    };
    let original = render_lens_source(&flat, W, H, W, H, &baseline).unwrap();
    assert!(current.iter().zip(&original).all(|(a, b)| a.abs_diff(*b) <= 1),
        "均匀桌面上的流光颜色、高光和透明度不应随折射增益改变");

    let mut patterned = flat;
    for (i, pixel) in patterned.chunks_exact_mut(4).enumerate() {
        let value = if (i % W as usize / 4 + i / W as usize / 4) % 2 == 0 { 40 } else { 210 };
        pixel[..3].fill(value);
    }
    let current = render_lens_source(&patterned, W, H, W, H, source).unwrap();
    let original = render_lens_source(&patterned, W, H, W, H, &baseline).unwrap();
    let mut changed = 0;
    for (a, b) in current.chunks_exact(4).zip(original.chunks_exact(4)) {
        assert_eq!(a[3], b[3], "折射增益不能改变覆盖层透明度");
        if a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > 2) { changed += 1; }
    }
    assert!(changed > 100, "折射减弱应改变纹理采样位置，实际仅 {changed} 像素变化");
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
