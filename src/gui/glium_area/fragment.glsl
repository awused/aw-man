#version 140

// Reads will be linear with unpremultiplied alpha
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

    float a = (src.a + bg.a * (1.0 - src.a));
    // dst is in linear RGB with premultiplied alpha
    vec4 dst =
      vec4((src.rgb * src.a + bg.rgb * (1.0 - src.a)), a);

    dst.r = srgb(dst.r);
    if (grey) {
      dst.g = dst.b = dst.r;
    } else {
      dst.g = srgb(dst.g);
      dst.b = srgb(dst.b);
    }

    f_color = dst;
}

