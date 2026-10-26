use super::{Counters, Uniform};
use std::marker::PhantomData;

const SLOTS: usize = 256;
const MAX_PAGES: usize = 16;

pub(super) struct Page {
    pub buffer: wgpu::Buffer,
    pub geometry: wgpu::BindGroup,
    pub paint: Option<wgpu::BindGroup>,
}
pub(super) struct Arena<T = Uniform> {
    pub pages: Vec<Page>,
    pub used: usize,
    stride: usize,
    marker: PhantomData<T>,
}
impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            used: 0,
            stride: 0,
            marker: PhantomData,
        }
    }
}
impl<T: bytemuck::Pod> Arena<T> {
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
    pub fn required_bytes(&self, device: &wgpu::Device, count: usize) -> Option<u64> {
        let stride = Self::stride(device);
        let needed = self.used.checked_add(count)?.div_ceil(SLOTS);
        if needed > MAX_PAGES || (SLOTS * stride) as u64 > device.limits().max_buffer_size {
            return None;
        }
        Some((needed.max(self.pages.len()) * SLOTS * stride) as u64)
    }
    fn stride(device: &wgpu::Device) -> usize {
        (std::mem::size_of::<T>() as u32)
            .div_ceil(device.limits().min_uniform_buffer_offset_alignment) as usize
            * device.limits().min_uniform_buffer_offset_alignment as usize
    }
    #[allow(clippy::too_many_arguments)]
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        geometry_layout: &wgpu::BindGroupLayout,
        paint_layout: &wgpu::BindGroupLayout,
        resolved: &[&wgpu::TextureView],
        values: &[T],
        budget: u64,
        counters: &mut Counters,
    ) -> Option<usize> {
        if self.required_bytes(device, values.len())? > budget {
            return None;
        }
        let stride = Self::stride(device);
        let end = self.used.checked_add(values.len())?;
        let needed = end.div_ceil(SLOTS);
        self.stride = stride;
        while self.pages.len() < needed {
            let buffer = super::super::memory::create_buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("vector uniform arena"),
                    size: (SLOTS * stride) as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
                super::super::Class::Vector,
                false,
            )
            .ok()?;
            let geometry = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vector arena geometry"),
                layout: geometry_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<T>() as u64),
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
                let mut entries = vec![wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &p.buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<T>() as u64),
                    }),
                }];
                entries.extend(
                    resolved
                        .iter()
                        .enumerate()
                        .map(|(i, view)| wgpu::BindGroupEntry {
                            binding: i as u32 + 1,
                            resource: wgpu::BindingResource::TextureView(view),
                        }),
                );
                p.paint = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vector arena paint"),
                    layout: paint_layout,
                    entries: &entries,
                }));
                counters.bind_groups += 1;
            }
            copied += count;
        }
        self.used = end;
        Some(start)
    }
}
