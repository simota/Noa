// Terminal background-image quad. Draws the decoded PNG behind the whole
// surface (design: below the cell background quads, above the LoadOp::Clear
// color). One draw call per tile; `dest_rect` places the quad in window pixels
// and each tile samples the full image (uv 0..1). The target uses
// ALPHA_BLENDING with straight alpha, and the fragment scales the sampled alpha
// by `opacity` (`background-image-opacity`) — independent of the clear color's
// `background-opacity` (spec NFR-3 / AC-9).

struct BgImageUniforms {
    // Vec4-first std140 order (see CLAUDE.md GPU gotcha): window px x, y, w, h.
    dest_rect: vec4<f32>,
    surface_size: vec2<f32>,
    // UV extent across the quad. `[1, 1]` for a single (non-repeat) placement;
    // `[surface/tile]` for repeat, where the Repeat-address sampler wraps the
    // over-1.0 uv to tile the image across the whole surface in ONE draw.
    uv_scale: vec2<f32>,
    opacity: f32,
    // Three scalars, not a vec3, to keep the std140 tail padding explicit
    // (CLAUDE.md gotcha). The struct rounds up to 48 bytes total.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var image_tex: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> u: BgImageUniforms;

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
    // Pixel -> NDC (y points down in pixel space, up in NDC). A dest rect that
    // overflows the surface (e.g. `cover`) is clipped by the rasterizer.
    let ndc = vec2<f32>(
        px.x / u.surface_size.x * 2.0 - 1.0,
        1.0 - px.y / u.surface_size.y * 2.0,
    );

    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    // uv spans [0, uv_scale]; the Repeat sampler wraps values over 1.0 to tile.
    out.uv = corner * u.uv_scale;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let c = textureSample(image_tex, image_sampler, in.uv);
    // Straight (non-premultiplied) alpha scaled by background-image-opacity;
    // the ALPHA_BLENDING target composites src.a * src + (1 - src.a) * dst.
    return vec4<f32>(c.rgb, c.a * u.opacity);
}
