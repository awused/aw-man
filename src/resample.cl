// Originally adapted from the image crate. Substantially modified to perform scaling in
// linear light and premultiplied alpha.

// The MIT License (MIT)
//
// Copyright (c) 2014 PistonDevelopers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

// See http://cs.brown.edu/courses/cs123/lectures/08_Image_Processing_IV.pdf
// for some of the theory behind image scaling and convolution

__constant float srgb_lut[256] = {
    0.0, 0.000303527, 0.000607054, 0.000910581, 0.001214108, 0.001517635, 0.001821162, 0.0021246888,
    0.002428216, 0.0027317428, 0.00303527, 0.0033465358, 0.0036765074, 0.004024717, 0.004391442,
    0.0047769533, 0.0051815165, 0.0056053917, 0.006048833, 0.0065120906, 0.00699541, 0.007499032,
    0.008023193, 0.008568126, 0.009134059, 0.009721218, 0.010329823, 0.010960094, 0.011612245,
    0.012286488, 0.0129830325, 0.013702083, 0.014443844, 0.015208514, 0.015996294, 0.016807375,
    0.017641954, 0.01850022, 0.019382361, 0.020288562, 0.02121901, 0.022173885, 0.023153367,
    0.024157632, 0.02518686, 0.026241222, 0.027320892, 0.02842604, 0.029556835, 0.030713445,
    0.031896032, 0.033104766, 0.034339808, 0.035601314, 0.03688945, 0.038204372, 0.039546236,
    0.0409152, 0.04231141, 0.04373503, 0.045186203, 0.046665087, 0.048171826, 0.049706567,
    0.051269457, 0.052860647, 0.054480277, 0.05612849, 0.05780543, 0.059511237, 0.061246052,
    0.063010015, 0.064803265, 0.06662594, 0.06847817, 0.070360094, 0.07227185, 0.07421357,
    0.07618538, 0.07818742, 0.08021982, 0.08228271, 0.08437621, 0.08650046, 0.08865558, 0.09084171,
    0.093058966, 0.09530747, 0.09758735, 0.099898726, 0.10224173, 0.104616486, 0.107023105,
    0.10946171, 0.11193243, 0.114435375, 0.116970666, 0.11953843, 0.122138776, 0.12477182,
    0.12743768, 0.13013647, 0.13286832, 0.13563333, 0.13843161, 0.14126329, 0.14412847, 0.14702727,
    0.14995979, 0.15292615, 0.15592647, 0.15896083, 0.16202937, 0.1651322, 0.1682694, 0.17144111,
    0.1746474, 0.17788842, 0.18116425, 0.18447499, 0.18782078, 0.19120169, 0.19461784, 0.19806932,
    0.20155625, 0.20507874, 0.20863687, 0.21223076, 0.2158605, 0.2195262, 0.22322796, 0.22696587,
    0.23074006, 0.23455058, 0.23839757, 0.24228112, 0.24620132, 0.25015828, 0.2541521, 0.25818285,
    0.26225066, 0.2663556, 0.2704978, 0.2746773, 0.27889428, 0.28314874, 0.28744084, 0.29177064,
    0.29613826, 0.30054379, 0.3049873, 0.30946892, 0.31398872, 0.31854677, 0.3231432, 0.3277781,
    0.33245152, 0.33716363, 0.34191442, 0.34670407, 0.3515326, 0.35640013, 0.3613068, 0.3662526,
    0.3712377, 0.37626213, 0.38132602, 0.38642943, 0.39157248, 0.39675522, 0.40197778, 0.4072402,
    0.4125426, 0.41788507, 0.42326766, 0.4286905, 0.43415365, 0.43965718, 0.4452012, 0.4507858,
    0.45641103, 0.462077, 0.4677838, 0.47353148, 0.47932017, 0.48514995, 0.49102086, 0.49693298,
    0.5028865, 0.50888133, 0.5149177, 0.52099556, 0.5271151, 0.5332764, 0.5394795, 0.54572445,
    0.55201143, 0.5583404, 0.5647115, 0.57112485, 0.57758045, 0.58407843, 0.59061885, 0.59720176,
    0.60382736, 0.61049557, 0.6172066, 0.6239604, 0.63075715, 0.63759685, 0.6444797, 0.65140563,
    0.65837485, 0.6653873, 0.67244315, 0.6795425, 0.6866853, 0.69387174, 0.7011019, 0.70837575,
    0.7156935, 0.7230551, 0.73046076, 0.7379104, 0.7454042, 0.7529422, 0.7605245, 0.76815116,
    0.7758222, 0.7835378, 0.7912979, 0.7991027, 0.80695224, 0.8148466, 0.82278574, 0.8307699,
    0.838799, 0.8468732, 0.8549926, 0.8631572, 0.8713671, 0.8796224, 0.8879231, 0.8962694,
    0.9046612, 0.91309863, 0.92158186, 0.9301109, 0.9386857, 0.9473065, 0.9559733, 0.9646863,
    0.9734453, 0.9822506, 0.9911021, 1.0,
};

float srgb(float value) {
    return value <= 0.0031308 ? value * 12.92 : 1.055 * pow(value, 1.0f/2.4f) - 0.055;
}

float4 lookup_premult_linear(
        global uchar *src_image,
        int2 coord,
        int width,
        uchar channels) {
    ulong offset = (ulong)coord.y * (ulong)width + (ulong)coord.x;
    uchar4 pix;
    if (channels == 4) {
        // RGBA
        pix = vload4(offset, src_image);
    } else if (channels == 3) {
        // RGB
        pix = (uchar4)(vload3(offset, src_image), 255);
    } else if (channels == 1) {
        // R
        pix = (uchar4)(src_image[offset], 0, 0, 255);
    } else {
        // RA, basically never
        uchar2 load = vload2(offset, src_image);
        pix = (uchar4)(load.x, 0, 0, load.y);
    }

    // read_image methods always return 1 for alpha, not whatever the value is for opaque
    float a = (float)pix.w / 255.0f;
    float4 out = (float4)(
        srgb_lut[pix.x] * a,
        srgb_lut[pix.y] * a,
        srgb_lut[pix.z] * a,
        a);
    return out;
}

void write_srgb(
        global uchar *dst_image,
        int2 coord,
        int width,
        uchar channels,
        float4 pix) {
    ulong offset = (ulong)coord.y * (ulong)width + (ulong)coord.x;
    float a_inv = pix.w > 0 ? 1.0f / pix.w : 0.0f;
    if (isinf(a_inv)) {
        a_inv = 0.0;
    }

    // Do explicit rounding, to result in closer to CPU results.
    float4 outf = (float4)(
        srgb(pix.x * a_inv) * 255.0,
        srgb(pix.y * a_inv) * 255.0,
        srgb(pix.z * a_inv) * 255.0,
        pix.w * 255.0);

    uchar4 out = convert_uchar4_sat(round(outf));

    if (channels == 4) {
        // RGBA
        vstore4(out, offset, dst_image);
    } else if (channels == 3) {
        // RGB
        vstore3(out.xyz, offset, dst_image);
    } else if (channels == 1) {
        // R
        dst_image[offset] = out.x;
    } else {
        // RA, basically never
        vstore2(out.xw, offset, dst_image);
    }
}

float catmullrom(float x) {
    float a = fabs(x);
    float k = 0.0;

    if (a < 1.0) {
        k = mad(mad(9.0f, a, -15.0f), a*a, 6.0f);
    } else if (a < 2.0) {
        k = mad(mad(mad(-3.0f, a, 15.0f), a, -24.0f), a, 12.0f);
    }

    return k / 6.0;
}

__kernel void catmullrom_vertical(
        global uchar *src_image,
        int2 src_bounds,
        global float *dst_image,
        int2 dst_bounds,
        uchar channels) {
    int2 dst_coord = (int2)(get_global_id(0), get_global_id(1));

    float2 ratio = convert_float2(src_bounds)/convert_float2(dst_bounds);
    float2 support_ratio = max(ratio, 1.0f);
    float2 support = support_ratio * 2;

    float2 src_centre = (convert_float2(dst_coord) + 0.5f) * ratio;

    int2 top_left = clamp(convert_int2_rtn(src_centre - support), 0, src_bounds-1);
    int2 bottom_right = clamp(convert_int2_rtp(src_centre + support), top_left+1, src_bounds);

    src_centre = src_centre - 0.5f;

    float4 out_pix = 0;

    float weight_sum = 0.0;
    for (int y = top_left.y; y < bottom_right.y; y++) {
        float w = catmullrom(((float)(y) - src_centre.y) / support_ratio.y);

        out_pix += lookup_premult_linear(
                src_image,
                (int2)(dst_coord.x, y),
                src_bounds.x,
                channels) * w;
        weight_sum += w;
    }

    out_pix /= weight_sum;
    // NOTE: do not clamp intermediate values

    ulong offset = (ulong)dst_coord.y * (ulong)dst_bounds.x + (ulong)dst_coord.x;
    if (channels == 4) {
        vstore4(out_pix, offset, dst_image);
    } else if (channels == 3) {
        vstore3(out_pix.xyz, offset, dst_image);
    } else if (channels == 1) {
        dst_image[offset] = out_pix.x;
    } else {
        vstore2(out_pix.xw, offset, dst_image);
    }
}

__kernel void catmullrom_horizontal(
        global float *src_image,
        int2 src_bounds,
        global uchar *dst_image,
        int2 dst_bounds,
        uchar channels) {
    int2 dst_coord = (int2)(get_global_id(0), get_global_id(1));

    float2 ratio = convert_float2(src_bounds)/convert_float2(dst_bounds);
    float2 support_ratio = max(ratio, 1.0f);
    float2 support = support_ratio * 2;

    float2 in_centre = (convert_float2(dst_coord) + 0.5f) * ratio;

    int2 top_left = clamp(convert_int2_rtn(in_centre - support), 0, src_bounds-1);
    int2 bottom_right = clamp(convert_int2_rtp(in_centre + support), top_left+1, src_bounds);

    in_centre = in_centre - 0.5f;

    float4 out_pix = 0;

    float weight_sum = 0.0;
    for (int x = top_left.x; x < bottom_right.x; x++) {
        float w = catmullrom(((float)(x) - in_centre.x) / support_ratio.x);

        float4 src_pix;
        ulong offset = (ulong)dst_coord.y * (ulong)src_bounds.x + (ulong)x;
        if (channels == 4) {
            src_pix = vload4(offset, src_image);
        } else if (channels == 3) {
            src_pix = (float4)(vload3(offset, src_image), 1.0);
        } else if (channels == 1) {
            src_pix = (float4)(src_image[offset], 0, 0, 1.0);
        } else {
            float2 load = vload2(offset, src_image);
            src_pix = (float4)(load.x, 0, 0, load.y);
        }

        out_pix += src_pix * w;
        weight_sum += w;
    }

    out_pix /= weight_sum;

    write_srgb(dst_image, dst_coord, dst_bounds.x, channels, out_pix);
}
