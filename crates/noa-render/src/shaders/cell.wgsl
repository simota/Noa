// noa cell shader — instanced-quad renderer for both the cell-background
// pass and the cell-text (glyph) pass. Which behavior an instance gets is
// selected by `flags & FLAG_GLYPH`.

// Field order + tail padding MUST match `instance.rs`'s `Uniforms` exactly
// (16-byte members first, then vec2s, then scalars). The tail pad is three
// scalars, NOT a vec3 (vec3 forces 16-byte alignment and would reintroduce a
// size mismatch). Total: 160 bytes.
struct Uniforms {
    projection: mat4x4<f32>,
    grid_padding: vec4<f32>, // top, right, bottom, left
    cursor_color: vec4<f32>,
    bg_color: vec4<f32>,
    screen_size: vec2<f32>,
    cell_size: vec2<f32>,
    grid_size: vec2<f32>,
    cursor_pos: vec2<f32>,
    min_contrast: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(0) @binding(1)
var atlas_tex: texture_2d<f32>;

@group(0) @binding(2)
var mask_atlas_sampler: sampler;

// RGBA8 color-glyph atlas (emoji, WP1). FRAGMENT-only visibility — unlike
// `atlas_tex`, the vertex stage must NOT call `textureDimensions` on this
// texture (see the vs_main glyph branch below: color UV is emitted in texel
// space and normalized only in fs_main). Mixing that up reintroduces the
// CLAUDE.md "bind group visibility" GPU gotcha as a non-unwinding wgpu
// validation abort.
@group(0) @binding(3)
var color_atlas_tex: texture_2d<f32>;

@group(0) @binding(4)
var color_atlas_sampler: sampler;

struct InstanceInput {
    @location(0) glyph_pos: vec2<u32>,
    @location(1) glyph_size: vec2<u32>,
    @location(2) bearing: vec2<i32>,
    @location(3) grid_pos: vec2<u32>,
    @location(4) color: vec4<f32>,
    @location(5) flags: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) flags: u32,
};

const FLAG_GLYPH: u32 = 1u;
const FLAG_DECORATION: u32 = 8u;
const FLAG_DIVIDER: u32 = 16u;
const FLAG_COLOR_GLYPH: u32 = 32u;

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: InstanceInput,
) -> VertexOutput {
    // Unit quad corners, two triangles, generated from the vertex index.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vertex_index];

    let cell_origin = vec2<f32>(f32(instance.grid_pos.x), f32(instance.grid_pos.y)) * uniforms.cell_size
        + vec2<f32>(uniforms.grid_padding.w, uniforms.grid_padding.x);

    var pixel: vec2<f32>;
    var uv: vec2<f32>;

    if (instance.flags & FLAG_DIVIDER) != 0u {
        // Divider quad: grid_pos/glyph_size carry a pixel-space rect.
        let origin = vec2<f32>(f32(instance.grid_pos.x), f32(instance.grid_pos.y));
        let size = vec2<f32>(f32(instance.glyph_size.x), f32(instance.glyph_size.y));
        pixel = origin + corner * size;
        uv = vec2<f32>(0.0, 0.0);
    } else if (instance.flags & FLAG_GLYPH) != 0u {
        // Glyph quad: positioned by bearing, sized by the atlas glyph rect.
        let size = vec2<f32>(f32(instance.glyph_size.x), f32(instance.glyph_size.y));
        let bearing = vec2<f32>(f32(instance.bearing.x), f32(instance.bearing.y));
        // bearing.y is the distance from the cell top to the glyph top.
        pixel = cell_origin + vec2<f32>(bearing.x, bearing.y) + corner * size;

        let atlas_origin = vec2<f32>(f32(instance.glyph_pos.x), f32(instance.glyph_pos.y));
        if (instance.flags & FLAG_COLOR_GLYPH) != 0u {
            // Color atlas: emit TEXEL-SPACE uv here — no textureDimensions on
            // color_atlas_tex in the vertex stage (that binding is
            // FRAGMENT-only). fs_main normalizes it before sampling.
            uv = atlas_origin + corner * size;
        } else {
            let atlas_dims = vec2<f32>(textureDimensions(atlas_tex));
            uv = (atlas_origin + corner * size) / atlas_dims;
        }
    } else if (instance.flags & FLAG_DECORATION) != 0u {
        // Decoration quad: positioned by bearing and sized by glyph_size.
        let size = vec2<f32>(f32(instance.glyph_size.x), f32(instance.glyph_size.y));
        let bearing = vec2<f32>(f32(instance.bearing.x), f32(instance.bearing.y));
        pixel = cell_origin + bearing + corner * size;
        uv = vec2<f32>(0.0, 0.0);
    } else {
        // Background / cursor quad: fills the whole cell.
        pixel = cell_origin + corner * uniforms.cell_size;
        uv = vec2<f32>(0.0, 0.0);
    }

    var out: VertexOutput;
    out.clip_position = uniforms.projection * vec4<f32>(pixel, 0.0, 1.0);
    out.uv = uv;
    out.color = instance.color;
    out.flags = instance.flags;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.flags & FLAG_GLYPH) != 0u {
        if (in.flags & FLAG_COLOR_GLYPH) != 0u {
            // Color glyph: passthrough sample, no foreground-color tint
            // (REQ-EMOJI-2). Only place color_atlas_tex's textureDimensions
            // is called — normalizing the texel-space uv from vs_main.
            let cuv = in.uv / vec2<f32>(textureDimensions(color_atlas_tex));
            return textureSample(color_atlas_tex, color_atlas_sampler, cuv);
        }
        let coverage = textureSample(atlas_tex, mask_atlas_sampler, in.uv).r;
        return vec4<f32>(in.color.rgb, in.color.a * coverage);
    }
    return in.color;
}
