//! Candles, volume and indicator series drawn with instanced quads, straight into iced's
//! render pass.
//!
//! Bars upload once per data set ([`ChartData::generation`]) and studies once per change;
//! panning and zooming only rewrite a small uniform per pane. Each pane draws inside its own
//! scissor rect. One [`Pipeline`] serves every chart, keyed by chart id.

use std::{collections::HashMap, ops::Range, sync::Arc};

use iced::{
    Color, Rectangle, mouse,
    wgpu::{self, util::DeviceExt},
    widget::shader,
};

use crate::{
    ChartData, ChartState, Panes,
    study::{OutputStyle, Placement, Study},
    theme,
    viewport::{Projection, VOLUME_HEIGHT},
};

/// Indicator line width, in logical pixels.
const LINE_WIDTH: f32 = 1.5;

/// The shader-widget program: turns the chart state into a [`Primitive`] each frame.
pub(crate) struct Candles<'a> {
    pub(crate) chart: &'a ChartState,
}

impl<Message> shader::Program<Message> for Candles<'_> {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, bounds: Rectangle) -> Primitive {
        // The widget is padded to the plot, so its bounds are the plot.
        let panes = self.chart.panes(bounds.size());
        Primitive::new(self.chart, &panes)
    }
}

/// Per-bar vertex data. Coordinates are relative to the newest bar (and, in the price pane,
/// its close) so `f32` stays precise where people look most.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BarInstance {
    index: f32,
    ohlc: [f32; 4],
    volume: f32,
}

/// One piece of an indicator line: (index, value) at each end.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Segment {
    a: [f32; 2],
    b: [f32; 2],
    color: [f32; 4],
}

/// One slice of shading between two lines: (index, upper, lower) at each end.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Band {
    a: [f32; 3],
    b: [f32; 3],
    color: [f32; 4],
}

/// One histogram bar from zero.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistBar {
    index: f32,
    value: f32,
    color: [f32; 4],
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
    top: f32,
    height: f32,
    line_width: f32,
    up: [f32; 4],
    down: [f32; 4],
    up_volume: [f32; 4],
    down_volume: [f32; 4],
}

/// Index and price that coordinates are relative to.
fn origin(data: &ChartData) -> (f64, f64) {
    let last = data.candles.len().saturating_sub(1);
    let price = data.candles.last().map_or(0.0, |c| c.close);
    (last as f64, price)
}

#[derive(Debug)]
pub(crate) struct Primitive {
    chart_id: u64,
    data: Arc<ChartData>,
    studies: Arc<Vec<Study>>,
    studies_generation: u64,
    /// Price pane first, then indicator panes, top to bottom. Missing the parts that depend
    /// on the scale factor and surface format.
    panes: Vec<View>,
    spacing: f32,
    visible: Range<u32>,
}

impl Primitive {
    fn new(chart: &ChartState, panes: &Panes) -> Self {
        let (base_index, base_price) = origin(&chart.data);
        let price = &panes.price;
        let plot_height = panes.all().map(|p| p.offset + p.height).fold(0.0, f32::max);
        let drawable = price.width >= 1.0 && price.height >= 1.0;

        let view = |p: &Projection, base: f64, vol_sy: f32| View {
            x0: (p.left - base_index) as f32,
            sx: p.spacing as f32,
            y0: (p.top - base) as f32,
            sy: (f64::from(p.height) / (p.top - p.bottom)) as f32,
            size: [p.width, plot_height],
            px: 1.0,
            body: 1.0,
            vol_sy,
            top: p.offset,
            height: p.height,
            line_width: LINE_WIDTH,
            up: [0.0; 4],
            down: [0.0; 4],
            up_volume: [0.0; 4],
            down_volume: [0.0; 4],
        };
        let vol_sy = if price.max_volume > 0.0 {
            (f64::from(price.height * VOLUME_HEIGHT) / price.max_volume) as f32
        } else {
            0.0
        };
        let views = std::iter::once(view(price, base_price, vol_sy))
            .chain(panes.indicators.iter().map(|(_, p)| view(p, 0.0, 0.0)))
            .collect();

        Self {
            chart_id: chart.id,
            data: Arc::clone(&chart.data),
            studies: Arc::clone(&chart.studies),
            studies_generation: chart.studies_generation,
            panes: views,
            spacing: price.spacing as f32,
            visible: if drawable {
                price.visible.start as u32..price.visible.end as u32
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
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let Pipeline {
            layout,
            charts,
            srgb,
            ..
        } = pipeline;
        let color = |c: Color| {
            if *srgb {
                c.into_linear()
            } else {
                [c.r, c.g, c.b, c.a]
            }
        };

        let buffers = charts.entry(self.chart_id).or_default();
        if buffers.bars_generation != self.data.generation {
            buffers.upload_bars(device, &self.data);
        }
        if buffers.studies_generation != self.studies_generation {
            buffers.upload_studies(device, &self.data, &self.studies, &color);
            buffers.studies_generation = self.studies_generation;
        }
        buffers.ensure_panes(device, layout, self.panes.len());

        let scale = viewport.scale_factor();
        // Body width: ~70% of the bar spacing, as an odd number of physical pixels.
        let body_px = ((self.spacing * scale * 0.7).floor() as u32).max(1);
        let body_px = if body_px.is_multiple_of(2) {
            body_px - 1
        } else {
            body_px
        };
        let target = (viewport.physical_width(), viewport.physical_height());

        for (pane, gpu) in self.panes.iter().zip(&mut buffers.panes) {
            let view = View {
                px: 1.0 / scale,
                body: body_px as f32 / scale,
                up: color(theme::UP),
                down: color(theme::DOWN),
                up_volume: color(theme::UP_VOLUME),
                down_volume: color(theme::DOWN_VOLUME),
                ..*pane
            };
            queue.write_buffer(&gpu.uniform, 0, bytemuck::bytes_of(&view));
            gpu.scissor = scissor(bounds, pane.top, pane.height, scale, target);
        }
        buffers.used = true;
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(buffers) = pipeline.charts.get(&self.chart_id) else {
            return true;
        };
        if self.visible.is_empty() {
            return true;
        }

        for (k, gpu) in buffers.panes.iter().enumerate().take(self.panes.len()) {
            let [x, y, width, height] = gpu.scissor;
            if width == 0 || height == 0 {
                continue;
            }
            pass.set_scissor_rect(x, y, width, height);
            pass.set_bind_group(0, &gpu.bind_group, &[]);

            draw_instances(pass, &pipeline.bands, buffers.bands.get(k));
            if k == 0
                && let Some(bars) = &buffers.bars
            {
                pass.set_vertex_buffer(0, bars.slice(..));
                pass.set_pipeline(&pipeline.volume);
                pass.draw(0..6, self.visible.clone());
                pass.set_pipeline(&pipeline.candles);
                pass.draw(0..12, self.visible.clone());
            }
            draw_instances(pass, &pipeline.hists, buffers.hists.get(k));
            draw_instances(pass, &pipeline.segments, buffers.segments.get(k));
        }
        true
    }
}

fn draw_instances(
    pass: &mut wgpu::RenderPass<'_>,
    pipeline: &wgpu::RenderPipeline,
    batch: Option<&Batch>,
) {
    if let Some(Batch {
        buffer: Some(buffer),
        range,
    }) = batch
        && !range.is_empty()
    {
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..6, range.clone());
    }
}

/// The physical-pixel rect of a pane, clamped to the render target.
fn scissor(bounds: &Rectangle, top: f32, height: f32, scale: f32, target: (u32, u32)) -> [u32; 4] {
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let x0 = (bounds.x * scale).floor().clamp(0.0, tw);
    let y0 = ((bounds.y + top) * scale).floor().clamp(0.0, th);
    let x1 = ((bounds.x + bounds.width) * scale).ceil().clamp(0.0, tw);
    let y1 = ((bounds.y + top + height) * scale).ceil().clamp(0.0, th);
    [
        x0 as u32,
        y0 as u32,
        (x1 - x0).max(0.0) as u32,
        (y1 - y0).max(0.0) as u32,
    ]
}

pub(crate) struct Pipeline {
    candles: wgpu::RenderPipeline,
    volume: wgpu::RenderPipeline,
    hists: wgpu::RenderPipeline,
    bands: wgpu::RenderPipeline,
    segments: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// iced converts colors to linear space when the surface is sRGB; we must match it.
    srgb: bool,
    charts: HashMap<u64, ChartBuffers>,
}

const BAR_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32, 1 => Float32x4, 2 => Float32];
const SEGMENT_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
const BAND_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
const HIST_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32, 1 => Float32, 2 => Float32x4];

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("persephone chart"),
            source: wgpu::ShaderSource::Wgsl(include_str!("candles.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("persephone view"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("persephone chart"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline =
            |vertex: &str, fragment: &str, stride: usize, attributes: &[wgpu::VertexAttribute]| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(vertex),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some(vertex),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: stride as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes,
                        }],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(fragment),
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

        let bar = size_of::<BarInstance>();
        Self {
            candles: pipeline("vs_candle", "fs_main", bar, &BAR_ATTRIBUTES),
            volume: pipeline("vs_volume", "fs_main", bar, &BAR_ATTRIBUTES),
            hists: pipeline("vs_hist", "fs_main", size_of::<HistBar>(), &HIST_ATTRIBUTES),
            bands: pipeline("vs_band", "fs_main", size_of::<Band>(), &BAND_ATTRIBUTES),
            segments: pipeline(
                "vs_segment",
                "fs_line",
                size_of::<Segment>(),
                &SEGMENT_ATTRIBUTES,
            ),
            layout,
            srgb: format.is_srgb(),
            charts: HashMap::new(),
        }
    }

    /// Drops buffers of charts that were not drawn this frame (closed or hidden tiles).
    fn trim(&mut self) {
        self.charts
            .retain(|_, buffers| std::mem::take(&mut buffers.used));
    }
}

/// Instances of one kind for one pane.
#[derive(Default)]
struct Batch {
    buffer: Option<wgpu::Buffer>,
    range: Range<u32>,
}

#[derive(Default)]
struct ChartBuffers {
    bars: Option<wgpu::Buffer>,
    bars_generation: u64,
    /// One batch per pane for each kind of study geometry; panes share each buffer.
    segments: Vec<Batch>,
    bands: Vec<Batch>,
    hists: Vec<Batch>,
    studies_generation: u64,
    panes: Vec<PaneBinding>,
    used: bool,
}

struct PaneBinding {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// x, y, width, height in physical pixels.
    scissor: [u32; 4],
}

impl ChartBuffers {
    fn upload_bars(&mut self, device: &wgpu::Device, data: &ChartData) {
        let (base_index, base_price) = origin(data);
        let instances: Vec<BarInstance> = data
            .candles
            .iter()
            .enumerate()
            .map(|(i, c)| BarInstance {
                index: (i as f64 - base_index) as f32,
                ohlc: [c.open, c.high, c.low, c.close].map(|v| (v - base_price) as f32),
                volume: c.volume as f32,
            })
            .collect();
        self.bars = vertex_buffer(device, "persephone bars", &instances);
        self.bars_generation = data.generation;
    }

    /// Turns every study into instances, grouped by pane: overlays in the price pane (relative
    /// to the newest close, like the bars), each separate study in its own pane.
    fn upload_studies(
        &mut self,
        device: &wgpu::Device,
        data: &ChartData,
        studies: &[Study],
        color: &dyn Fn(Color) -> [f32; 4],
    ) {
        let (base_index, base_price) = origin(data);
        let panes = 1 + studies
            .iter()
            .filter(|s| s.placement == Placement::Pane)
            .count();
        let mut segments = vec![Vec::new(); panes];
        let mut bands = vec![Vec::new(); panes];
        let mut hists = vec![Vec::new(); panes];
        let index = |i: usize| (i as f64 - base_index) as f32;

        let mut next_pane = 1;
        for study in studies {
            let (pane, base) = match study.placement {
                Placement::Overlay => (0, base_price),
                Placement::Pane => {
                    next_pane += 1;
                    (next_pane - 1, 0.0)
                }
            };
            let rel = |v: f64| (v - base) as f32;

            if let Some((upper, lower, fill)) = study.fill
                && let (Some(u), Some(l)) = (study.lines.get(upper), study.lines.get(lower))
            {
                let n = u.values.len().min(l.values.len()).min(data.candles.len());
                for i in 1..n {
                    let ends = [u.values[i - 1], l.values[i - 1], u.values[i], l.values[i]];
                    if ends.iter().all(|v| v.is_finite()) {
                        bands[pane].push(Band {
                            a: [index(i - 1), rel(ends[0]), rel(ends[1])],
                            b: [index(i), rel(ends[2]), rel(ends[3])],
                            color: color(fill),
                        });
                    }
                }
            }

            for line in &study.lines {
                let n = line.values.len().min(data.candles.len());
                match line.style {
                    OutputStyle::Line => {
                        for i in 1..n {
                            let (v0, v1) = (line.values[i - 1], line.values[i]);
                            if v0.is_finite() && v1.is_finite() {
                                segments[pane].push(Segment {
                                    a: [index(i - 1), rel(v0)],
                                    b: [index(i), rel(v1)],
                                    color: color(line.color),
                                });
                            }
                        }
                    }
                    OutputStyle::Histogram => {
                        let negative = Color {
                            a: line.color.a,
                            ..theme::DOWN
                        };
                        for (i, &v) in line.values.iter().enumerate().take(n) {
                            if v.is_finite() {
                                hists[pane].push(HistBar {
                                    index: index(i),
                                    value: rel(v),
                                    color: color(if v >= 0.0 { line.color } else { negative }),
                                });
                            }
                        }
                    }
                }
            }
        }

        self.segments = batches(device, "persephone lines", segments);
        self.bands = batches(device, "persephone bands", bands);
        self.hists = batches(device, "persephone histograms", hists);
    }

    fn ensure_panes(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        count: usize,
    ) {
        while self.panes.len() < count {
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
            self.panes.push(PaneBinding {
                uniform,
                bind_group,
                scissor: [0; 4],
            });
        }
    }
}

fn vertex_buffer<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    items: &[T],
) -> Option<wgpu::Buffer> {
    (!items.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(items),
            usage: wgpu::BufferUsages::VERTEX,
        })
    })
}

/// Packs per-pane instance lists into one buffer, remembering each pane's range in it.
fn batches<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    per_pane: Vec<Vec<T>>,
) -> Vec<Batch> {
    let mut all = Vec::new();
    let ranges: Vec<Range<u32>> = per_pane
        .into_iter()
        .map(|items| {
            let start = all.len() as u32;
            all.extend(items);
            start..all.len() as u32
        })
        .collect();
    let buffer = vertex_buffer(device, label, &all);
    ranges
        .into_iter()
        .map(|range| Batch {
            buffer: buffer.clone(),
            range,
        })
        .collect()
}
