use super::*;

fn texture(device: &wgpu::Device, size: (u32, u32), usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scissor-equivalence"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    })
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    bind: &wgpu::BindGroup,
    size: (u32, u32),
    scissor: bool,
) -> Vec<u8> {
    let (w, h) = size;
    let target = texture(
        device,
        size,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = target.create_view(&Default::default());
    let mut enc = device.create_command_encoder(&Default::default());
    // Dirty the entire attachment first: Clear must erase stale center pixels,
    // independently of the last scissor state and of which fragments get drawn.
    for dirty in [true, false] {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scissor-equivalence-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(if dirty {
                        wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }
                    } else {
                        wgpu::Color::TRANSPARENT
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !dirty {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            if scissor {
                for (x, y, rw, rh) in edge_scissors(size) {
                    pass.set_scissor_rect(x, y, rw, rh);
                    pass.draw(0..3, 0..1);
                }
            } else {
                pass.draw(0..3, 0..1);
            }
        }
    }
    let stride =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scissor-readback"),
        size: u64::from(stride) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
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
                bytes_per_row: Some(stride),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let (tx, rx) = channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(20)),
        })
        .expect("bounded GPU readback");
    rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..(w * 4) as usize].iter().copied())
        .collect()
}

#[test]
fn offscreen_scissor_matches_full_frame_and_clears_stale_center() {
    // No surface, HWND, desktop capture, external images or exported files.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .expect("DX12 adapter required for scissor equivalence");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    let (pipeline, layout) = Gfx::lens_pipeline(&device);
    let desktop = texture(
        &device,
        (64, 64),
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let mut pixels = Vec::new();
    for y in 0..64u8 {
        for x in 0..64u8 {
            pixels.extend_from_slice(&[x * 4, y * 4, (x ^ y) * 4, 255]);
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &desktop,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(256),
            rows_per_image: Some(64),
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scissor-uniform"),
        size: 48,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind = Gfx::make_bind_group(
        &device,
        &layout,
        &uniform,
        &desktop.create_view(&Default::default()),
        &sampler,
    );
    for (size, time, intensity, refr) in [
        (offscreen_size((3840, 2160)), 1.7, 1.0, 1.0),
        (offscreen_size((11520, 2160)), 9.2, 0.76, 1.0),
        ((257, 143), 3.1, 0.25, 0.0),
        ((64, 143), 0.0, 1.0, 1.0), // full-screen fallback
        ((1, 1), 2.0, 0.0, 0.0),
    ] {
        let uni: [f32; 12] = [
            time,
            0.7,
            intensity,
            0.07,
            refr,
            0.0,
            0.0,
            0.0,
            size.0 as f32,
            size.1 as f32,
            64.0,
            64.0,
        ];
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let full = render(&device, &queue, &pipeline, &bind, size, false);
        let clipped = render(&device, &queue, &pipeline, &bind, size, true);
        let differences = full.iter().zip(&clipped).filter(|(a, b)| a != b).count();
        assert_eq!(
            differences, 0,
            "scissor changed {differences} channels at {size:?}"
        );
        if edge_scissors(size).count() == 4 {
            let center = ((size.1 / 2 * size.0 + size.0 / 2) * 4) as usize;
            assert_eq!(&clipped[center..center + 4], &[0, 0, 0, 0]);
            assert!(
                clipped.chunks_exact(4).any(|p| p[3] > 0),
                "not a vacuous transparent frame"
            );
        }
    }
}
