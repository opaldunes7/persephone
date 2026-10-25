//! Candles and volume drawn with instanced quads, straight into iced's render pass.
//!
//! Bars are uploaded once per data set ([`ChartData::generation`]); panning and zooming only
//! rewrite a small uniform. One [`Pipeline`] serves every chart, keyed by chart id.

use std::{collections::HashMap, ops::Range, sync::Arc};

use iced::{
    Color, Rectangle, mouse,
    wgpu::{self, util::DeviceExt},
    widget::shader,
};

use crate::{
    ChartData, ChartState, theme,
    viewport::{Projection, VOLUME_HEIGHT},
};

/// The shader-widget program: turns the chart state into a [`Primitive`] each frame.
pub(crate) struct Candles<'a> {
    pub(crate) chart: &'a ChartState,
}

impl<Message> shader::Program<Message> for Candles<'_> {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, bounds: Rectangle) -> Primitive {
        // The widget is padded to the plot, so its bounds are the plot.
        let projection = self.chart.projection(bounds.size());
        Primitive::new(self.chart.id, &self.chart.data, &projection)
    }
}

/// Per-bar vertex data. Coordinates are relative to the newest bar so `f32` stays precise
/// where people look most.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    index: f32,
    ohlc: [f32; 4],
    volume: f32,
}

/// Mirrors `View` in `candles.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct View {
    x0: f32,
    sx: f32,
    y0: f32,
    sy: f32,
    size: [f32; 2],
    px: f32,
    body: f32,
    vol_sy: f32,
    _pad: [f32; 3],
    up: [f32; 4],
    down: [f32; 4],
    up_volume: [f32; 4],
    down_volume: [f32; 4],
}

/// Index and price that instance coordinates are relative to.
fn origin(data: &ChartData) -> (f64, f64) {
    let last = data.candles.len().saturating_sub(1);
    let price = data.candles.last().map_or(0.0, |c| c.close);
    (last as f64, price)
}

#[derive(Debug)]
pub(crate) struct Primitive {
    chart_id: u64,
    data: Arc<ChartData>,
    /// Everything except what depends on the scale factor and surface format.
    view: View,
    spacing: f32,
    visible: Range<u32>,
}

impl Primitive {
    fn new(chart_id: u64, data: &Arc<ChartData>, p: &Projection) -> Self {
        let (base_index, base_price) = origin(data);
        let drawable = p.width >= 1.0 && p.height >= 1.0;
        let view = View {
            x0: (p.left - base_index) as f32,
            sx: p.spacing as f32,
            y0: (p.top - base_price) as f32,
            sy: (f64::from(p.height) / (p.top - p.bottom)) as f32,
            size: [p.width, p.height],
            px: 1.0,
            body: 1.0,
            vol_sy: if p.max_volume > 0.0 {
                (f64::from(p.height * VOLUME_HEIGHT) / p.max_volume) as f32
            } else {
                0.0
            },
            _pad: [0.0; 3],
            up: [0.0; 4],
            down: [0.0; 4],
            up_volume: [0.0; 4],
            down_volume: [0.0; 4],
        };
        Self {
            chart_id,
            data: Arc::clone(data),
            view,
            spacing: p.spacing as f32,
            visible: if drawable {
                p.visible.start as u32..p.visible.end as u32
            } else {
                0..0
            },
        }
    }
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let Pipeline {
            layout,
            charts,
            srgb,
            ..
        } = pipeline;
        let buffers = charts
            .entry(self.chart_id)
            .or_insert_with(|| ChartBuffers::new(device, layout));
        if buffers.generation != self.data.generation {
            buffers.upload(device, &self.data);
        }

        let scale = viewport.scale_factor();
        // Body width: ~70% of the bar spacing, as an odd number of physical pixels.
        let body_px = ((self.spacing * scale * 0.7).floor() as u32).max(1);
        let body_px = if body_px.is_multiple_of(2) {
            body_px - 1
        } else {
            body_px
        };
        let color = |c: Color| {
            if *srgb {
                c.into_linear()
            } else {
                [c.r, c.g, c.b, c.a]
            }
        };

        let view = View {
            px: 1.0 / scale,
            body: body_px as f32 / scale,
            up: color(theme::UP),
            down: color(theme::DOWN),
            up_volume: color(theme::UP_VOLUME),
            down_volume: color(theme::DOWN_VOLUME),
            ..self.view
        };
        queue.write_buffer(&buffers.uniform, 0, bytemuck::bytes_of(&view));
        buffers.used = true;
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(buffers) = pipeline.charts.get(&self.chart_id)
            && let Some(instances) = &buffers.instances
            && !self.visible.is_empty()
        {
            pass.set_bind_group(0, &buffers.bind_group, &[]);
            pass.set_vertex_buffer(0, instances.slice(..));
            pass.set_pipeline(&pipeline.volume);
            pass.draw(0..6, self.visible.clone());
            pass.set_pipeline(&pipeline.candles);
            pass.draw(0..12, self.visible.clone());
        }
        true
    }
}

pub(crate) struct Pipeline {
    candles: wgpu::RenderPipeline,
    volume: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// iced converts colors to linear space when the surface is sRGB; we must match it.
    srgb: bool,
    charts: HashMap<u64, ChartBuffers>,
}

const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32, 1 => Float32x4, 2 => Float32];

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("persephone candles"),
            source: wgpu::ShaderSource::Wgsl(include_str!("candles.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("persephone view"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("persephone candles"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline = |entry_point: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &ATTRIBUTES,
                    }],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };

        Self {
            candles: pipeline("vs_candle"),
            volume: pipeline("vs_volume"),
            layout,
            srgb: format.is_srgb(),
            charts: HashMap::new(),
        }
    }

    /// Drops buffers of charts that were not drawn this frame (closed tiles).
    fn trim(&mut self) {
        self.charts
            .retain(|_, buffers| std::mem::take(&mut buffers.used));
    }
}

struct ChartBuffers {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: Option<wgpu::Buffer>,
    generation: u64,
    used: bool,
}

impl ChartBuffers {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("persephone view"),
            size: size_of::<View>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("persephone view"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        Self {
            uniform,
            bind_group,
            instances: None,
            generation: 0,
            used: false,
        }
    }

    fn upload(&mut self, device: &wgpu::Device, data: &ChartData) {
        let (base_index, base_price) = origin(data);
        let instances: Vec<Instance> = data
            .candles
            .iter()
            .enumerate()
            .map(|(i, c)| Instance {
                index: (i as f64 - base_index) as f32,
                ohlc: [c.open, c.high, c.low, c.close].map(|v| (v - base_price) as f32),
                volume: c.volume as f32,
            })
            .collect();

        self.instances = (!instances.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("persephone bars"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.generation = data.generation;
    }
}
