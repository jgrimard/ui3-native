// NV12 -> RGBA conversion. Y plane is R8Unorm, interleaved CbCr plane is Rg8Unorm at half resolution.

struct Params {
    // x: 1.0 = full range (JPEG levels), 0.0 = limited/TV range
    // y: 1.0 = BT.709 matrix, 0.0 = BT.601
    flags: vec4<f32>,
};

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle.
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    out.pos = vec4(p[vi], 0.0, 1.0);
    out.uv = vec2((p[vi].x + 1.0) * 0.5, 1.0 - (p[vi].y + 1.0) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var y = textureSample(y_tex, samp, in.uv).r;
    var c = textureSample(uv_tex, samp, in.uv).rg - vec2(0.5, 0.5);
    if (params.flags.x < 0.5) {
        y = (y - 16.0 / 255.0) * (255.0 / 219.0);
        c = c * (255.0 / 224.0);
    }
    var rgb: vec3<f32>;
    if (params.flags.y > 0.5) {
        // BT.709
        rgb = vec3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    } else {
        // BT.601
        rgb = vec3(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x);
    }
    return vec4(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
}
