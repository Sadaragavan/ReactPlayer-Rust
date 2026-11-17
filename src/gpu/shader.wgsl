// Reaction Player compositor.
//
// One pass, one draw per quad, no conversion on the CPU:
//   * BT.709 YUV -> RGB with the frame's own range flag,
//   * per-layer UV crop, opacity and alpha blend,
//   * two-source compare for the Alignment Studio (onion skin / wipe line).
//
// The vertex stage maps a pixel rectangle onto the render target itself, so the
// pass never touches the viewport (only the scissor, for clipping), and the
// fragment stage samples the planes at their true extent.

struct Quad {
    // Normalized crop of the source: (left, top, right, bottom).
    uv_crop: vec4<f32>,
    // Destination rectangle in target pixels: (x, y, width, height).
    rect: vec4<f32>,
    // (luma width, luma height, format flag, range flag).
    // format: 0 = YUV420P, 1 = NV12. range: 0 = limited, 1 = full.
    tex: vec4<f32>,
    // (opacity, mode, wipe percent, onion mix).
    // mode: 0 = single source, 1 = onion skin, 2 = wipe line.
    params: vec4<f32>,
    // (target width, target height, srgb target flag, unused).
    target: vec4<f32>,
};

@group(0) @binding(0) var<uniform> quad: Quad;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex_y: texture_2d<f32>;
@group(0) @binding(3) var tex_u: texture_2d<f32>;
@group(0) @binding(4) var tex_v: texture_2d<f32>;
@group(0) @binding(5) var tex_y2: texture_2d<f32>;
@group(0) @binding(6) var tex_u2: texture_2d<f32>;
@group(0) @binding(7) var tex_v2: texture_2d<f32>;

const ACCENT: vec3<f32> = vec3<f32>(0.980392, 0.8, 0.082353);

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Fullscreen triangle in rect space: (0,0), (2,0), (0,2).
    var corners = array<vec2<f32>, 3>(vec2<f32>(0.0, 0.0), vec2<f32>(2.0, 0.0), vec2<f32>(0.0, 2.0));
    let c = corners[index];
    let px = quad.rect.xy + c * quad.rect.zw;
    let ndc = vec2<f32>(
        px.x / max(quad.target.x, 1.0) * 2.0 - 1.0,
        1.0 - px.y / max(quad.target.y, 1.0) * 2.0,
    );
    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc, 0.0, 1.0);
    // y == 0 is the top of the rect, which is where texture row 0 lives.
    out.uv = c;
    return out;
}

fn sample_planes(uv: vec2<f32>, second: bool) -> vec3<f32> {
    var luma: f32 = 0.0;
    var cb: f32 = 0.0;
    var cr: f32 = 0.0;
    if second {
        luma = textureSample(tex_y2, samp, uv).r;
        if quad.tex.z > 0.5 {
            let uv8 = textureSample(tex_u2, samp, uv);
            cb = uv8.r;
            cr = uv8.g;
        } else {
            cb = textureSample(tex_u2, samp, uv).r;
            cr = textureSample(tex_v2, samp, uv).r;
        }
    } else {
        luma = textureSample(tex_y, samp, uv).r;
        if quad.tex.z > 0.5 {
            let uv8 = textureSample(tex_u, samp, uv);
            cb = uv8.r;
            cr = uv8.g;
        } else {
            cb = textureSample(tex_u, samp, uv).r;
            cr = textureSample(tex_v, samp, uv).r;
        }
    }

    // BT.709, studio range expanded to full, chroma centred on 0.5.
    var y = luma;
    var u = cb - 0.5;
    var v = cr - 0.5;
    if quad.tex.w < 0.5 {
        y = (luma - 16.0 / 255.0) * (255.0 / 219.0);
        u = u * (255.0 / 224.0);
        v = v * (255.0 / 224.0);
    }
    let r = y + 1.5748 * v;
    let g = y - 0.1873 * u - 0.4681 * v;
    let b = y + 1.8556 * u;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = mix(quad.uv_crop.xy, quad.uv_crop.zw, in.uv);
    let mode = i32(round(quad.params.y));

    var rgb = sample_planes(uv, false);
    if mode == 1 {
        // Onion skin: the second capture decides the mix.
        let other = sample_planes(uv, true);
        rgb = mix(rgb, other, clamp(quad.params.w, 0.0, 1.0));
    } else if mode == 2 {
        // Wipe line: A left of the divider, B right of it, accent on the seam.
        let other = sample_planes(uv, true);
        let wipe = clamp(quad.params.z, 0.0, 1.0);
        let edge = clamp(1.5 / max(quad.rect.z, 1.0), 0.0, 0.05);
        if in.uv.x > wipe + edge {
            rgb = other;
        } else if in.uv.x >= wipe - edge {
            rgb = ACCENT;
        }
    }

    if quad.target.z > 0.5 {
        rgb = srgb_to_linear(rgb);
    }
    let alpha = clamp(quad.params.x, 0.0, 1.0);
    return vec4<f32>(rgb * alpha, alpha);
}
