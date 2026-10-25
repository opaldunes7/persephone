// Candles, volume and indicator series as instanced, pixel-snapped quads.
//
// Positions are plot-local logical pixels, y down; the render pass viewport covers the plot,
// and each pane is drawn with its own `View` and scissor rect.
//
//   vs_candle   12 vertices per bar: wick quad, then body quad
//   vs_volume    6 per bar, from the bottom of the price pane
//   vs_hist      6 per bar, from the zero line (MACD histogram)
//   vs_band      6 per segment, shading between two lines (Bollinger fill)
//   vs_segment   6 per segment, an anti-aliased line piece (fs_line feathers its edges)

struct View {
    // Bar index at x = 0 (relative to the newest bar) and pixels per bar.
    x0: f32,
    sx: f32,
    // Value at the top of the pane (relative to the pane's base value) and pixels per unit.
    y0: f32,
    sy: f32,
    // Plot size in logical pixels.
    size: vec2<f32>,
    // One physical pixel, in logical pixels.
    px: f32,
    // Body width in logical pixels: an odd number of physical pixels, so the wick is centered.
    body: f32,
    // Pixels per volume unit.
    vol_sy: f32,
    // This pane's top edge and height within the plot.
    top: f32,
    height: f32,
    // Indicator line width in logical pixels.
    line_width: f32,
    up: vec4<f32>,
    down: vec4<f32>,
    up_volume: vec4<f32>,
    down_volume: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;

struct Bar {
    @location(0) index: f32,
    // open, high, low, close
    @location(1) ohlc: vec4<f32>,
    @location(2) volume: f32,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Signed distance from the line's center, in logical pixels.
    @location(1) across: f32,
};

fn corner(vertex: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return corners[vertex % 6u];
}

fn snap(v: f32) -> f32 {
    return round(v / view.px) * view.px;
}

fn x_of(index: f32) -> f32 {
    return (index - view.x0) * view.sx;
}

fn y_of(value: f32) -> f32 {
    return view.top + (view.y0 - value) * view.sy;
}

fn clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / view.size.x * 2.0 - 1.0, 1.0 - p.y / view.size.y * 2.0, 0.0, 1.0);
}

fn quad(vertex: u32, left: f32, right: f32, top: f32, bottom: f32) -> vec4<f32> {
    let c = corner(vertex);
    return clip(vec2<f32>(mix(left, right, c.x), mix(top, bottom, c.y)));
}

// Left edge of a bar's body; the wick occupies the physical pixel column at its center.
fn body_left(index: f32) -> f32 {
    return snap(x_of(index)) - (view.body - view.px) * 0.5;
}

@vertex
fn vs_candle(@builtin(vertex_index) vertex: u32, bar: Bar) -> VertexOut {
    let left = body_left(bar.index);
    var out: VertexOut;
    out.color = select(view.down, view.up, bar.ohlc.w >= bar.ohlc.x);

    if vertex < 6u {
        let x = left + (view.body - view.px) * 0.5;
        let top = snap(y_of(bar.ohlc.y));
        let bottom = max(snap(y_of(bar.ohlc.z)), top + view.px);
        out.position = quad(vertex, x, x + view.px, top, bottom);
    } else {
        let top = snap(y_of(max(bar.ohlc.x, bar.ohlc.w)));
        let bottom = max(snap(y_of(min(bar.ohlc.x, bar.ohlc.w))), top + view.px);
        out.position = quad(vertex, left, left + view.body, top, bottom);
    }
    return out;
}

@vertex
fn vs_volume(@builtin(vertex_index) vertex: u32, bar: Bar) -> VertexOut {
    let left = body_left(bar.index);
    let bottom = view.top + view.height;
    let top = min(snap(bottom - bar.volume * view.vol_sy), bottom - view.px);
    var out: VertexOut;
    out.color = select(view.down_volume, view.up_volume, bar.ohlc.w >= bar.ohlc.x);
    out.position = quad(vertex, left, left + view.body, top, bottom);
    return out;
}

@vertex
fn vs_hist(
    @builtin(vertex_index) vertex: u32,
    @location(0) index: f32,
    @location(1) value: f32,
    @location(2) color: vec4<f32>,
) -> VertexOut {
    let left = body_left(index);
    let zero = snap(y_of(0.0));
    let tip = snap(y_of(value));
    let top = min(zero, tip);
    let bottom = max(max(zero, tip), top + view.px);
    var out: VertexOut;
    out.color = color;
    out.position = quad(vertex, left, left + view.body, top, bottom);
    return out;
}

@vertex
fn vs_band(
    @builtin(vertex_index) vertex: u32,
    // index, upper, lower at each end of the segment
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) color: vec4<f32>,
) -> VertexOut {
    let c = corner(vertex);
    let x = mix(x_of(a.x), x_of(b.x), c.x);
    let upper = mix(y_of(a.y), y_of(b.y), c.x);
    let lower = mix(y_of(a.z), y_of(b.z), c.x);
    var out: VertexOut;
    out.color = color;
    out.position = clip(vec2<f32>(x, mix(upper, lower, c.y)));
    return out;
}

@vertex
fn vs_segment(
    @builtin(vertex_index) vertex: u32,
    // index, value at each end
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> LineOut {
    let pa = vec2<f32>(x_of(a.x), y_of(a.y));
    let pb = vec2<f32>(x_of(b.x), y_of(b.y));
    let d = pb - pa;
    let len = length(d);
    var dir = vec2<f32>(1.0, 0.0);
    if len > 1e-4 {
        dir = d / len;
    }
    let normal = vec2<f32>(-dir.y, dir.x);

    // Square caps half a width long hide the seams between segments; one extra physical
    // pixel on each side leaves room for the feathered edge.
    let half = view.line_width * 0.5;
    let reach = half + view.px;
    let c = corner(vertex);
    let along = mix(-half, len + half, c.x);
    let across = mix(-reach, reach, c.y);

    var out: LineOut;
    out.position = clip(pa + dir * along + normal * across);
    out.color = color;
    out.across = across;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    let half = view.line_width * 0.5;
    let coverage = clamp((half + view.px * 0.5 - abs(in.across)) / view.px, 0.0, 1.0);
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
