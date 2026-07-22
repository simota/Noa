// Rounded-card composite for the Session Overview (REQ-OV-12/14, v2 mockup
// parity). Draws one tile texture as a rounded-corner card with a border /
// focus ring, sampled over the near-black backdrop the composite pass clears
// to. One draw call per tile; `rect`/`surface_size` place the quad, and the
// fragment shader applies the rounded-rect SDF alpha mask + border stroke.

struct CardUniforms {
    // Vec4-first std140 order (see CLAUDE.md GPU gotcha): x, y, w, h in px.
    rect: vec4<f32>,
    border_color: vec4<f32>,
    glow_color: vec4<f32>,
    // Source UV sub-rect `[u, v, w, h]` sampled across the quad; `[0,0,1,1]`
    // for a full draw. A clipped draw (e.g. the pane-drag snapshot sliding
    // past a window edge) shrinks the quad AND passes the matching sub-rect
    // so the visible pixels still map to the same source texels — the card
    // stays glued to the cursor instead of stretching.
    src_uv: vec4<f32>,
    surface_size: vec2<f32>,
    corner_radius: f32,
    border_width: f32,
    glow_width: f32,
    // Whole-card opacity multiplier (fade-in transitions); 1.0 = opaque.
    opacity: f32,
};

@group(0) @binding(0) var tile_tex: texture_2d<f32>;
@group(0) @binding(1) var tile_sampler: sampler;
@group(0) @binding(2) var<uniform> u: CardUniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local_px: vec2<f32>,
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
    let outset = max(u.glow_width, 0.0);
    let draw_origin = u.rect.xy - vec2<f32>(outset);
    let draw_size = u.rect.zw + vec2<f32>(outset * 2.0);
    let px = draw_origin + corner * draw_size;
    // Pixel -> NDC (y points down in pixel space, up in NDC).
    let ndc = vec2<f32>(
        px.x / u.surface_size.x * 2.0 - 1.0,
        1.0 - px.y / u.surface_size.y * 2.0,
    );

    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.local_px = corner * draw_size - vec2<f32>(outset);
    out.uv = u.src_uv.xy + (out.local_px / u.rect.zw) * u.src_uv.zw;
    return out;
}

// Signed distance to a rounded box centered at the origin; negative inside.
fn sd_round_box(p: vec2<f32>, half: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - (half - vec2<f32>(radius));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let half = u.rect.zw * 0.5;
    let p = in.local_px - half;
    let d = sd_round_box(p, half, u.corner_radius);
    let glow_width = max(u.glow_width, 0.0);

    // Card coverage: 1 inside, fading to 0 over ~1px at the rounded edge.
    let coverage = 1.0 - smoothstep(0.0, 1.0, d);
    if coverage <= 0.0 {
        if glow_width <= 0.0 {
            discard;
        }
        let glow_alpha = (1.0 - smoothstep(0.0, glow_width, d)) * u.glow_color.a * u.opacity;
        if glow_alpha <= 0.0 {
            discard;
        }
        return vec4<f32>(u.glow_color.rgb, glow_alpha);
    }

    let tex = textureSample(tile_tex, tile_sampler, clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    // `inset` is the distance from the edge, growing inward. The border stroke
    // occupies the outermost `border_width` px.
    let inset = -d;
    let border_mix = 1.0 - smoothstep(u.border_width - 1.0, u.border_width, inset);
    let rgb = mix(tex.rgb, u.border_color.rgb, clamp(border_mix, 0.0, 1.0));
    // Multiply in the sampled texture's own alpha (not just the card-shape
    // coverage) so a translucent source texture — e.g. the session sidebar's
    // band rendered under background-opacity < 1 — stays translucent through
    // the composite instead of being forced opaque. Every existing caller
    // renders its source texture with clear alpha 1.0, so `tex.a` is 1.0 there
    // and this is a no-op (unchanged output).
    return vec4<f32>(rgb, coverage * tex.a * u.opacity);
}
