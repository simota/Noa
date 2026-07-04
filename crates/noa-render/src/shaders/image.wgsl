// Kitty-graphics image quad. Draws one placement's texture as a straight-alpha
// quad over the cell grid. One draw call per placement; `dest_rect` places the
// quad in window pixels and `src_uv` selects the (possibly cropped) source
// region. The target uses ALPHA_BLENDING so transparent image regions reveal
// the cells beneath.

struct ImageUniforms {
    // Vec4-first std140 order (see CLAUDE.md GPU gotcha): window px x, y, w, h.
    dest_rect: vec4<f32>,
    // Normalized source uv: x, y, w, h in [0,1].
    src_uv: vec4<f32>,
    surface_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var image_tex: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> u: ImageUniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex_index];
    let px = u.dest_rect.xy + corner * u.dest_rect.zw;
    // Pixel -> NDC (y points down in pixel space, up in NDC).
    let ndc = vec2<f32>(
        px.x / u.surface_size.x * 2.0 - 1.0,
        1.0 - px.y / u.surface_size.y * 2.0,
    );

    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = u.src_uv.xy + corner * u.src_uv.zw;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // Straight (non-premultiplied) alpha; the ALPHA_BLENDING target state does
    // the src.a * src + (1-src.a) * dst composite.
    return textureSample(image_tex, image_sampler, in.uv);
}
