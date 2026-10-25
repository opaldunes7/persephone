// Candles and volume as instanced, pixel-snapped quads.
//
// One instance per bar. `vs_candle` emits 12 vertices (wick quad, then body quad) and
// `vs_volume` emits 6. Positions are in plot-local logical pixels, y down; the render pass
// viewport already covers the plot, so they map straight to clip space.

struct View {
    // Bar index at x = 0 (relative to the newest bar) and pixels per bar.
    x0: f32,
    sx: f32,
    // Price at y = 0 (relative to the newest close) and pixels per price unit.
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
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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

fn price_y(price: f32) -> f32 {
    return (view.y0 - price) * view.sy;
}

fn quad(vertex: u32, left: f32, right: f32, top: f32, bottom: f32) -> vec4<f32> {
    let c = corner(vertex);
    let p = vec2<f32>(mix(left, right, c.x), mix(top, bottom, c.y));
    return vec4<f32>(p.x / view.size.x * 2.0 - 1.0, 1.0 - p.y / view.size.y * 2.0, 0.0, 1.0);
}

// Left edge of the bar's body; the wick occupies the physical pixel column at its center.
fn body_left(bar: Bar) -> f32 {
    let center = snap((bar.index - view.x0) * view.sx);
    return center - (view.body - view.px) * 0.5;
}

fn rising(bar: Bar) -> bool {
    return bar.ohlc.w >= bar.ohlc.x;
}

@vertex
fn vs_candle(@builtin(vertex_index) vertex: u32, bar: Bar) -> VertexOut {
    let left = body_left(bar);
    var out: VertexOut;
    out.color = select(view.down, view.up, rising(bar));

    if vertex < 6u {
        let x = left + (view.body - view.px) * 0.5;
        let top = snap(price_y(bar.ohlc.y));
        let bottom = max(snap(price_y(bar.ohlc.z)), top + view.px);
        out.position = quad(vertex, x, x + view.px, top, bottom);
    } else {
        let top = snap(price_y(max(bar.ohlc.x, bar.ohlc.w)));
        let bottom = max(snap(price_y(min(bar.ohlc.x, bar.ohlc.w))), top + view.px);
        out.position = quad(vertex, left, left + view.body, top, bottom);
    }
    return out;
}

@vertex
fn vs_volume(@builtin(vertex_index) vertex: u32, bar: Bar) -> VertexOut {
    let left = body_left(bar);
    let bottom = view.size.y;
    let top = min(snap(bottom - bar.volume * view.vol_sy), bottom - view.px);
    var out: VertexOut;
    out.color = select(view.down_volume, view.up_volume, rising(bar));
    out.position = quad(vertex, left, left + view.body, top, bottom);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
