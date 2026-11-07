//! Building overview levels by downsampling cached tiles.
use super::*;

/// The children of `key` that lie inside the canvas, in TL, TR, BL, BR order.
pub(super) fn pyramid_children(scene: &RenderScene, key: CacheKey) -> Vec<CacheKey> {
    let mut out = Vec::new();
    if key.0 <= 0 {
        return out;
    }
    for y in 0..2 {
        for x in 0..2 {
            let child = (key.0 - 1, key.1 * 2 + x, key.2 * 2 + y);
            let span = 256i64 << child.0;
            if (child.1 as i64) * span < scene.canvas.x1 as i64
                && (child.2 as i64) * span < scene.canvas.y1 as i64
            {
                out.push(child);
            }
        }
    }
    out
}

impl Renderer {
    pub(super) fn downsample_cached(
        &self,
        scene: &RenderScene,
        key: CacheKey,
        slot: u32,
    ) -> Result<bool> {
        if key.0 <= 0 {
            return Ok(false);
        }
        let child_level = key.0 - 1;
        let scale = (1u32 << child_level) as i32;
        let mut children = Vec::new();
        for y in 0..2 {
            for x in 0..2 {
                let k = (child_level, key.1 * 2 + x, key.2 * 2 + y);
                if k.1 * 256 * scale >= scene.canvas.x1 || k.2 * 256 * scale >= scene.canvas.y1 {
                    continue;
                }
                let Some(c) = self.cache_map.get(&k) else {
                    return Ok(false);
                };
                children.push((x, y, c.slot));
            }
        }
        self.downsample_slots(&children, slot)?;
        Ok(true)
    }
    /// A [`uniform`] subtree: the downsample of four copies of the top-left child.
    pub(super) fn downsample_uniform(&self, key: CacheKey, slot: u32) -> Result<bool> {
        let Some(c) = self.cache_map.get(&(key.0 - 1, key.1 * 2, key.2 * 2)) else {
            return Ok(false);
        };
        let children: Vec<_> = (0..4).map(|i| (i % 2, i / 2, c.slot)).collect();
        self.downsample_slots(&children, slot)?;
        Ok(true)
    }
    /// Downsample the 2x2 children at `(x, y, slot)` into `slot`; absent ones
    /// are transparent.
    pub(super) fn downsample_slots(&self, children: &[(i32, i32, u32)], slot: u32) -> Result<()> {
        let mut e = self.device.create_command_encoder(&Default::default());
        // WebGPU has no command-encoder clear. An empty pass clears reused texels so a
        // missing edge child stays transparent. clear_texture aborts on this backend.
        {
            let clear = self.scratch.create_view(&Default::default());
            let _pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pyramid clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &clear,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        for &(x, y, from) in children {
            e.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.cache,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: from,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x as u32 * 256,
                        y: y as u32 * 256,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let pipeline = if self.integer_cache {
            self.downsample_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer display pyramid",
                    include_str!("../../shaders/downsample-q14.wgsl"),
                )
            })
        } else {
            &self.downsample
        };
        let input = self.scratch.create_view(&Default::default());
        let ub = buffer(
            &self.governor,
            &self.device,
            &self.queue,
            "pyramid slot",
            &[[slot, 0, 0, 0]],
            wgpu::BufferUsages::UNIFORM,
        )?;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display pyramid"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.cache_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ub.as_entire_binding(),
                },
            ],
        });
        {
            let mut p = e.begin_compute_pass(&Default::default());
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(16, 16, 1);
        }
        self.queue.submit(Some(e.finish()));
        // This takes &self; the uniform is its own and the submit above was its use.
        memory::destroy_buffer(&self.governor, &ub, false);
        self.governor.submitted(&self.queue);
        Ok(())
    }
}
