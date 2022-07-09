#version 140

uniform sampler2D tex;
uniform vec4 bg;

in vec2 v_tex_coords;
out vec4 f_color;

float srgb(float value) {
    return value <= 0.0031308 ? value * 12.92 : 1.055 * pow(value, 1.0f/2.4f) - 0.055;
}

void main() {
    vec4 src = texture(tex, v_tex_coords);

    vec4 dst = vec4(
        (src.rgb * src.a + bg.rgb * (1.0 - src.a)),
        src.a + bg.a * (1.0 - src.a)
    );

    dst.r = srgb(dst.r);
    dst.g = srgb(dst.g);
    dst.b = srgb(dst.b);

    f_color = dst;
}

