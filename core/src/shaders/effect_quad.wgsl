// Screen effect quad: one premultiplied-alpha RGBA texture drawn over a window's
// content after iced has presented into the same view (LoadOp::Load).
//
// The texture holds premultiplied colour in sRGB-encoded (gamma) space, exactly
// as decoded. For a non-sRGB target that is what iced blends in, so it is output
// as is. For an sRGB target the hardware encodes our output, so colour is
// un-premultiplied, decoded to linear and premultiplied again.

struct Params {
    // Destination rectangle in NDC: left, top, right, bottom.
    rect: vec4<f32>,
    opacity: f32,
    linearize: u32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var effect_sampler: sampler;
@group(0) @binding(1) var effect_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];
    var out: VertexOutput;
    out.position = vec4<f32>(
        mix(params.rect.x, params.rect.z, corner.x),
        mix(params.rect.y, params.rect.w, corner.y),
        0.0,
        1.0,
    );
    out.uv = corner;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(effect_texture, effect_sampler, in.uv);
    var rgb = texel.rgb;
    if (params.linearize != 0u && texel.a > 0.0) {
        rgb = srgb_to_linear(clamp(texel.rgb / texel.a, vec3<f32>(0.0), vec3<f32>(1.0))) * texel.a;
    }
    return vec4<f32>(rgb, texel.a) * params.opacity;
}
