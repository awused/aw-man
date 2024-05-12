#version 140

// Reads will be linear with unassociated alpha
uniform sampler2D tex;
// Linear RGB with premultiplied alpha
uniform highp vec4 bg;
uniform bool grey_alpha;

in highp vec2 v_tex_coords;
out highp vec4 f_color;

highp float linear_to_srgb(highp float value) {
    return value <= 0.0031308 ? value * 12.92 : 1.055 * pow(value, 1.0f/2.4f) - 0.055;
}

void main() {
    highp vec4 src = texture(tex, v_tex_coords);

    highp float src_a = src.a;
    if (grey_alpha) {
      // Swizzling is applied after sRGB->linear conversion, which is very backwards.
      // For grey_alpha images this is necessary to get the real alpha value back.
      src_a = linear_to_srgb(src_a);
    }

    highp float a = (src_a + bg.a * (1.0 - src_a));
    // dst is in linear RGB with premultiplied alpha
    highp vec4 dst =
      vec4((src.rgb * src_a + bg.rgb * (1.0 - src_a)), a);

    // f_color is a linear texture incorrectly treated as srgb by GTK.
    // Explicitly write srgb float values into the linear texture.
    dst.r = linear_to_srgb(dst.r);
    dst.g = linear_to_srgb(dst.g);
    dst.b = linear_to_srgb(dst.b);

    f_color = dst;
}

