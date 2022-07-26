#version 140

// Reads will be linear with unassociated alpha
uniform sampler2D tex;
// Linear RGB with premultiplied alpha
uniform vec4 bg;
uniform bool grey;

in vec2 v_tex_coords;
out vec4 f_color;

float srgb(float value) {
    return value <= 0.0031308 ? value * 12.92 : 1.055 * pow(value, 1.0f/2.4f) - 0.055;
}

void main() {
    vec4 src = texture(tex, v_tex_coords);

    float src_a = src.a;
    if (grey) {
      // Swizzling is applied after sRGB conversion, which is very backwards.
      // For opaque greyscale images, src_a will be 1.0 so they'll all take the same branches.
      // For grey_alpha images this is necessary to get the real alpha value back.
      src_a = srgb(src_a);
    }

    float a = (src_a + bg.a * (1.0 - src_a));
    // dst is in linear RGB with premultiplied alpha
    vec4 dst =
      vec4((src.rgb * src_a + bg.rgb * (1.0 - src_a)), a);

    dst.r = srgb(dst.r);
    if (grey) {
      dst.g = dst.b = dst.r;
    } else {
      dst.g = srgb(dst.g);
      dst.b = srgb(dst.b);
    }

    f_color = dst;
}

