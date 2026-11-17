// reaction-player compositor shader.
//
// One pipeline, one quad per layer. Each layer is drawn as its own paint
// callback with the viewport set to that layer's on-screen rectangle, so the
// quad simply fills clip space. Inputs are YUV planes uploaded straight from
// FFmpeg's buffers (three R8 planes for YUV420P, R8 + Rg8 for NV12); the format
// flag picks the sampling path. Output is premultiplied alpha so layers stack
// with `src = One, dst = OneMinusSrcAlpha`.

struct Uniforms {
    // 0 = YUV420P, 1 = NV12
    format: u32,
    // 1.0 = limited (MPEG) range, 0.0 = full (JPEG) range
    limited_range: f32,
    // 1 = alignment compare (sample the second layer too)
    compare_mode: u32,
    // 0 = onion skin, 1 = wipe
    compare_kind: u32,

    // Normalized crop window of this layer's texture (u0, v0, u1, v1).
    crop: vec4<f32>,

    // Per-layer opacity.
    opacity: f32,
    // Wipe position, 0..1 across the quad.
    wipe: f32,
    // Onion-skin weight of layer B (0 = all A, 1 = all B).
    onion: f32,
    // Padding, keeps the struct at 48 bytes with 16-byte alignment.
    pad0: f32,
};

struct CmpUniforms {
    // 0 = YUV420P, 1 = NV12 (B's format)
    format: u32,
    limited_range: f32,
    opacity: f32,
    pad0: f32,
    // B's crop window.
    crop: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex_y: texture_2d<f32>;
@group(0) @binding(2) var tex_u: texture_2d<f32>;
@group(0) @binding(3) var tex_v: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

@group(1) @binding(0) var<uniform> cmp: CmpUniforms;
@group(1) @binding(1) var tex_b_y: texture_2d<f32>;
@group(1) @binding(2) var tex_b_u: texture_2d<f32>;
@group(1) @binding(3) var tex_b_v: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Destination-space UV, 0..1 across the quad.
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles in destination UV space.
    var quad = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0)
    );
    let q = quad[vi];
    var out: VsOut;
    // egui sets the viewport to the callback rect, so [-1,1] covers exactly
    // this layer. y is flipped because clip space grows upwards.
    out.pos = vec4<f32>(q.x * 2.0 - 1.0, 1.0 - q.y * 2.0, 0.0, 1.0);
    out.uv = q;
    return out;
}

fn sample_plane(tex: texture_2d<f32>, uv: vec2<f32>) -> f32 {
    return textureSampleLevel(tex, samp, uv, 0.0).r;
}

// Sample one YUV frame (a or b) at destination uv `d_uv`.
fn sample_yuv(
    d_uv: vec2<f32>,
    crop: vec4<f32>,
    format_flag: u32,
    limited: f32,
    y_tex: texture_2d<f32>,
    u_tex: texture_2d<f32>,
    v_tex: texture_2d<f32>,
) -> vec3<f32> {
    // Map destination UV into the crop window.
    let uv = crop.xy + d_uv * (crop.zw - crop.xy);
    let yv = sample_plane(y_tex, uv);
    var uu: f32;
    var vv: f32;
    if (format_flag == 1u) {
        // NV12: chroma is interleaved in the Rg8 plane.
        let c = textureSampleLevel(u_tex, samp, uv, 0.0);
        uu = c.r;
        vv = c.g;
    } else {
        uu = sample_plane(u_tex, uv);
        vv = sample_plane(v_tex, uv);
    }

    // BT.709 with limited (MPEG) or full (JPEG) range expansion.
    var yn: f32;
    var un: f32;
    var vn: f32;
    if (limited > 0.5) {
        yn = (yv - 16.0 / 255.0) * (255.0 / 219.0);
        un = (uu - 128.0 / 255.0) * (255.0 / 224.0);
        vn = (vv - 128.0 / 255.0) * (255.0 / 224.0);
    } else {
        yn = yv;
        un = uu - 128.0 / 255.0;
        vn = vv - 128.0 / 255.0;
    }
    let r = yn + 1.5748 * vn;
    let g = yn - 0.187324 * un - 0.468124 * vn;
    let b = yn + 1.8556 * un;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Accent colour, #FACC15 in linear 0..1.
const ACCENT = vec3<f32>(0.9803922, 0.8, 0.0823529);

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    var color = sample_yuv(uv, u.crop, u.format, u.limited_range, tex_y, tex_u, tex_v);
    var alpha = u.opacity;

    if (u.compare_mode == 1u) {
        let other = sample_yuv(
            uv,
            cmp.crop,
            cmp.format,
            cmp.limited_range,
            tex_b_y,
            tex_b_u,
            tex_b_v,
        );
        if (u.compare_kind == 0u) {
            // Onion skin: cross-fade A and B.
            color = mix(color, other, clamp(u.onion, 0.0, 1.0));
        } else {
            // Wipe: B to the right of the split, A to the left.
            let split = clamp(u.wipe, 0.0, 1.0);
            color = select(color, other, uv.x >= split);
            // A sharp divider in the accent colour. Deriving the width from
            // fwidth keeps it between one and two physical pixels.
            let dx = max(fwidth(uv.x), 1e-6);
            if (abs(uv.x - split) <= 0.75 * dx) {
                color = ACCENT;
                alpha = 1.0;
            }
        }
    }

    // Premultiplied output.
    return vec4<f32>(color * alpha, alpha);
}
