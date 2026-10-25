use super::{Counters, Uniform};

const SLOTS: usize = 256;
const MAX_PAGES: usize = 16;

pub(super) struct Page {
    pub buffer: wgpu::Buffer,
    pub geometry: wgpu::BindGroup,
    pub paint: Option<wgpu::BindGroup>,
}
#[derive(Default)]
pub(super) struct Arena {
    pub pages: Vec<Page>,
    pub used: usize,
    stride: usize,
}
impl Arena {
    pub fn bytes(&self) -> u64 {
        (self.pages.len() * SLOTS * self.stride) as u64
    }
    pub fn reset(&mut self) {
        self.used = 0;
    }
    pub fn invalidate_paint(&mut self) {
        for p in &mut self.pages {
            p.paint = None;
        }
    }
    pub fn retire(self, retired: &mut super::super::Retired) {
        for p in self.pages {
            retired.buffer(&p.buffer);
        }
    }
    pub fn offset(&self, index: usize) -> (usize, u32) {
        (index / SLOTS, ((index % SLOTS) * self.stride) as u32)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry_layout: &wgpu::BindGroupLayout,
        paint_layout: &wgpu::BindGroupLayout,
        resolved: &wgpu::TextureView,
        values: &[Uniform],
        budget: u64,
        counters: &mut Counters,
    ) -> Option<usize> {
        let stride = (std::mem::size_of::<Uniform>() as u32)
            .div_ceil(device.limits().min_uniform_buffer_offset_alignment)
            as usize
            * device.limits().min_uniform_buffer_offset_alignment as usize;
        let end = self.used.checked_add(values.len())?;
        let needed = end.div_ceil(SLOTS);
        if needed > MAX_PAGES
            || (needed.max(self.pages.len()) * SLOTS * stride) as u64 > budget
            || (SLOTS * stride) as u64 > device.limits().max_buffer_size
        {
            return None;
        }
        self.stride = stride;
        while self.pages.len() < needed {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vector uniform arena"),
                size: (SLOTS * stride) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let geometry = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vector arena geometry"),
                layout: geometry_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<Uniform>() as u64),
                    }),
                }],
            });
            self.pages.push(Page {
                buffer,
                geometry,
                paint: None,
            });
            counters.uniform_buffers += 1;
            counters.bind_groups += 1;
        }
        let start = self.used;
        let mut copied = 0;
        while copied < values.len() {
            let (page, offset) = self.offset(start + copied);
            let count = (SLOTS - (start + copied) % SLOTS).min(values.len() - copied);
            let mut bytes = vec![0_u8; count * stride];
            for (i, v) in values[copied..copied + count].iter().enumerate() {
                let data = bytemuck::bytes_of(v);
                bytes[i * stride..i * stride + data.len()].copy_from_slice(data);
            }
            let p = &mut self.pages[page];
            queue.write_buffer(&p.buffer, offset as u64, &bytes);
            counters.upload_bytes += bytes.len() as u64;
            if p.paint.is_none() {
                p.paint = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vector arena paint"),
                    layout: paint_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &p.buffer,
                                offset: 0,
                                size: wgpu::BufferSize::new(std::mem::size_of::<Uniform>() as u64),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(resolved),
                        },
                    ],
                }));
                counters.bind_groups += 1;
            }
            copied += count;
        }
        self.used = end;
        Some(start)
    }
}
