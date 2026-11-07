//! GPU resource helpers: textures, buffers, pipelines and deferred destruction.
use super::*;

pub(super) fn rgba_pixels(tile: &TileData) -> std::borrow::Cow<'_, [u8]> {
    if tile.format == alto_core::PixelFormat::R8 {
        std::borrow::Cow::Owned(tile.pixels.iter().flat_map(|v| [*v, *v, *v, 255]).collect())
    } else {
        std::borrow::Cow::Borrowed(&tile.pixels)
    }
}
pub(super) fn texture(
    governor: &Arc<Governor>,
    device: &wgpu::Device,
    label: &str,
    layers: u32,
    usage: wgpu::TextureUsages,
) -> Result<wgpu::Texture> {
    memory::create_texture(
        governor,
        device,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        },
        if label == "content atlas" || label == "display tiles" {
            Class::Fixed
        } else {
            Class::Stroke
        },
    )
}
pub(super) fn array_view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}
/// GPU objects to destroy once the commands using them are submitted. On WebGPU,
/// dropping a wgpu object does nothing: the JS object, and the memory behind it, lives
/// until JS collects it. Short-lived buffers and replaced textures are destroyed here
/// instead; WebGPU defers the actual release until submitted work no longer uses them.
#[derive(Clone, Copy)]
pub(super) enum MemoryClass {
    Fixed,
    Effects,
    Stroke,
    Vector,
}
impl MemoryClass {
    pub(super) fn charge(self, b: &mut GpuBytes, n: u64) {
        match self {
            Self::Fixed => b.fixed += n,
            Self::Effects => b.effects += n,
            Self::Stroke => b.stroke += n,
            Self::Vector => b.vector += n,
        }
    }
}
pub(crate) struct Retired {
    pub(super) governor: Arc<Governor>,
    pub(super) buffers: Vec<(wgpu::Buffer, MemoryClass)>,
    pub(super) textures: Vec<(wgpu::Texture, MemoryClass)>,
}
impl Retired {
    pub(super) fn new(governor: Arc<Governor>) -> Self {
        Self {
            governor,
            buffers: Vec::new(),
            textures: Vec::new(),
        }
    }
    pub(crate) fn buffer(&mut self, b: &wgpu::Buffer) {
        self.buffers.push((b.clone(), MemoryClass::Vector));
    }
    pub(crate) fn texture(&mut self, t: wgpu::Texture) {
        self.textures.push((t, MemoryClass::Vector));
    }
    pub(crate) fn effect_texture(&mut self, t: wgpu::Texture) {
        self.textures.push((t, MemoryClass::Effects));
    }
    pub(crate) fn bytes(&self) -> GpuBytes {
        let mut b = GpuBytes::default();
        for (buffer, class) in &self.buffers {
            class.charge(&mut b, buffer.size());
        }
        for (t, class) in &self.textures {
            let n = t.width() as u64
                * t.height() as u64
                * t.depth_or_array_layers() as u64
                * t.sample_count() as u64
                * t.format().block_copy_size(None).unwrap_or(4) as u64;
            class.charge(&mut b, n);
        }
        b.retired_fixed = b.fixed;
        b.retired = b.total();
        b
    }
    pub(super) fn classified_buffer(&mut self, b: &wgpu::Buffer, class: MemoryClass) {
        self.buffers.push((b.clone(), class));
    }
    pub(crate) fn take(&mut self, other: &mut Retired) {
        assert!(
            Arc::ptr_eq(&self.governor, &other.governor),
            "cannot combine retired resources from different GPU owners"
        );
        self.buffers.append(&mut other.buffers);
        self.textures.append(&mut other.textures);
    }
    /// Only after the submit that used them.
    pub(crate) fn release(&mut self) {
        for (b, _) in self.buffers.drain(..) {
            memory::destroy_buffer(&self.governor, &b, false);
        }
        for (t, _) in self.textures.drain(..) {
            memory::destroy_texture(&self.governor, &t, false);
        }
    }
}
/// A buffer filled through the queue. Not mapped at creation: on WebGPU a mapped
/// buffer needs shared memory that lives until JS collects the GPUBuffer (dropping
/// it in Rust does nothing there), and creating one per composed tile ran the
/// renderer out of mappings on slow machines ("size (144) is too large").
pub(super) fn buffer<T: Pod>(
    governor: &Arc<Governor>,
    d: &wgpu::Device,
    q: &wgpu::Queue,
    label: &str,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> Result<wgpu::Buffer> {
    // WGSL runtime arrays still require space for one element when their logical length is zero.
    let zero = T::zeroed();
    let bytes: &[u8] = if data.is_empty() {
        bytemuck::bytes_of(&zero)
    } else {
        bytemuck::cast_slice(data)
    };
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let class = if label.starts_with("vector") || label.starts_with("glyph") {
        Class::Vector
    } else if label.starts_with("M5") {
        Class::Effects
    } else {
        Class::Fixed
    };
    let b = memory::create_buffer(
        governor,
        d,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
        class,
        class == Class::Fixed,
    )?;
    if bytes.len() as u64 == size {
        q.write_buffer(&b, 0, bytes);
    } else {
        let mut padded = bytes.to_vec();
        padded.resize(size as usize, 0);
        q.write_buffer(&b, 0, &padded);
    }
    Ok(b)
}
pub(super) fn compute(d: &wgpu::Device, label: &str, src: &str) -> wgpu::ComputePipeline {
    let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}
pub(super) fn integer_present(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("integer present"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/present-q14.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("integer present"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
