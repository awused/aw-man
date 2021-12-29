use image::{
    DynamicImage, GenericImageView, ImageBuffer, Pixel, Primitive, Rgba, Rgba32FImage, RgbaImage,
};
use num_traits::{clamp, NumCast, ToPrimitive};
use rayon::iter::{ParallelBridge, ParallelIterator};

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
//

// See http://cs.brown.edu/courses/cs123/lectures/08_Image_Processing_IV.pdf
// for some of the theory behind image scaling and convolution


/// Available Sampling Filters.
///
/// ## Examples
///
/// To test the different sampling filters on a real example, you can find two
/// examples called
/// [`scaledown`](https://github.com/image-rs/image/tree/master/examples/scaledown)
/// and
/// [`scaleup`](https://github.com/image-rs/image/tree/master/examples/scaleup)
/// in the `examples` directory of the crate source code.
///
/// Here is a 3.58 MiB
/// [test image](https://github.com/image-rs/image/blob/master/examples/scaledown/test.jpg)
/// that has been scaled down to 300x225 px:
///
/// <!-- NOTE: To test new test images locally, replace the GitHub path with `../../../docs/` -->
/// <div style="display: flex; flex-wrap: wrap; align-items: flex-start;">
///   <div style="margin: 0 8px 8px 0;">
///     <img src="https://raw.githubusercontent.com/image-rs/image/master/examples/scaledown/scaledown-test-near.png" title="Nearest"><br>
///     Nearest Neighbor
///   </div>
///   <div style="margin: 0 8px 8px 0;">
///     <img src="https://raw.githubusercontent.com/image-rs/image/master/examples/scaledown/scaledown-test-tri.png" title="Triangle"><br>
///     Linear: Triangle
///   </div>
///   <div style="margin: 0 8px 8px 0;">
///     <img src="https://raw.githubusercontent.com/image-rs/image/master/examples/scaledown/scaledown-test-cmr.png" title="CatmullRom"><br>
///     Cubic: Catmull-Rom
///   </div>
///   <div style="margin: 0 8px 8px 0;">
///     <img src="https://raw.githubusercontent.com/image-rs/image/master/examples/scaledown/scaledown-test-gauss.png" title="Gaussian"><br>
///     Gaussian
///   </div>
///   <div style="margin: 0 8px 8px 0;">
///     <img src="https://raw.githubusercontent.com/image-rs/image/master/examples/scaledown/scaledown-test-lcz2.png" title="Lanczos3"><br>
///     Lanczos with window 3
///   </div>
/// </div>
///
/// ## Speed
///
/// Time required to create each of the examples above, tested on an Intel
/// i7-4770 CPU with Rust 1.37 in release mode:
///
/// <table style="width: auto;">
///   <tr>
///     <th>Nearest</th>
///     <td>31 ms</td>
///   </tr>
///   <tr>
///     <th>Triangle</th>
///     <td>414 ms</td>
///   </tr>
///   <tr>
///     <th>CatmullRom</th>
///     <td>817 ms</td>
///   </tr>
///   <tr>
///     <th>Gaussian</th>
///     <td>1180 ms</td>
///   </tr>
///   <tr>
///     <th>Lanczos3</th>
///     <td>1170 ms</td>
///   </tr>
/// </table>
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterType {
    /// Nearest Neighbor
    Nearest,

    /// Linear Filter
    Triangle,

    /// Cubic Filter
    CatmullRom,

    /// Gaussian Filter
    Gaussian,

    /// Lanczos with window 3
    Lanczos3,
}

/// A Representation of a separable filter.
struct Filter<'a> {
    /// The filter's filter function.
    pub(crate) kernel: Box<dyn Fn(f32) -> f32 + Sync + 'a>,

    /// The window on which this filter operates.
    pub(crate) support: f32,
}

struct FloatNearest(f32);

// to_i64, to_u64, and to_f64 implicitly affect all other lower conversions.
// Note that to_f64 by default calls to_i64 and thus needs to be overridden.
impl ToPrimitive for FloatNearest {
    // to_{i,u}64 is required, to_{i,u}{8,16} are usefull.
    // If a usecase for full 32 bits is found its trivial to add
    fn to_i8(&self) -> Option<i8> {
        self.0.round().to_i8()
    }

    fn to_i16(&self) -> Option<i16> {
        self.0.round().to_i16()
    }

    fn to_i64(&self) -> Option<i64> {
        self.0.round().to_i64()
    }

    fn to_u8(&self) -> Option<u8> {
        self.0.round().to_u8()
    }

    fn to_u16(&self) -> Option<u16> {
        self.0.round().to_u16()
    }

    fn to_u64(&self) -> Option<u64> {
        self.0.round().to_u64()
    }

    fn to_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }
}

// sinc function: the ideal sampling filter.
fn sinc(t: f32) -> f32 {
    let a = t * std::f32::consts::PI;

    if t == 0.0 { 1.0 } else { a.sin() / a }
}

// lanczos kernel function. A windowed sinc function.
fn lanczos(x: f32, t: f32) -> f32 {
    if x.abs() < t {
        sinc(x) * sinc(x / t)
    } else {
        0.0
    }
}

// Calculate a splice based on the b and c parameters.
// from authors Mitchell and Netravali.
fn bc_cubic_spline(x: f32, b: f32, c: f32) -> f32 {
    let a = x.abs();

    let k = if a < 1.0 {
        (12.0 - 9.0 * b - 6.0 * c) * a.powi(3)
            + (-18.0 + 12.0 * b + 6.0 * c) * a.powi(2)
            + (6.0 - 2.0 * b)
    } else if a < 2.0 {
        (-b - 6.0 * c) * a.powi(3)
            + (6.0 * b + 30.0 * c) * a.powi(2)
            + (-12.0 * b - 48.0 * c) * a
            + (8.0 * b + 24.0 * c)
    } else {
        0.0
    };

    k / 6.0
}

/// The Gaussian Function.
/// ```r``` is the standard deviation.
pub(crate) fn gaussian(x: f32, r: f32) -> f32 {
    ((2.0 * std::f32::consts::PI).sqrt() * r).recip() * (-x.powi(2) / (2.0 * r.powi(2))).exp()
}

/// Calculate the lanczos kernel with a window of 3
pub(crate) fn lanczos3_kernel(x: f32) -> f32 {
    lanczos(x, 3.0)
}

/// Calculate the gaussian function with a
/// standard deviation of 0.5
pub(crate) fn gaussian_kernel(x: f32) -> f32 {
    gaussian(x, 0.5)
}

/// Calculate the Catmull-Rom cubic spline.
/// Also known as a form of `BiCubic` sampling in two dimensions.
pub(crate) fn catmullrom_kernel(x: f32) -> f32 {
    bc_cubic_spline(x, 0.0, 0.5)
}

/// Calculate the triangle function.
/// Also known as `BiLinear` sampling in two dimensions.
pub(crate) fn triangle_kernel(x: f32) -> f32 {
    if x.abs() < 1.0 { 1.0 - x.abs() } else { 0.0 }
}

/// Calculate the box kernel.
/// Only pixels inside the box should be considered, and those
/// contribute equally.  So this method simply returns 1.
pub(crate) fn box_kernel(_x: f32) -> f32 {
    1.0
}

#[inline]
fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        f32::powf((s + 0.055) / 1.055, 2.4)
    }
}

#[inline]
fn linear_to_srgb(s: f32) -> f32 {
    if s <= 0.0031308 {
        s * 12.92
    } else {
        1.055 * f32::powf(s, 1.0 / 2.4) - 0.055
    }
}


// Sample the rows of the supplied image using the provided filter.
// The height of the image remains unchanged.
// ```new_width``` is the desired width of the new image
// ```filter``` is the filter to use for sampling.
// ```image``` is not necessarily Rgba and the order of channels is passed through.
fn horizontal_par_sample(
    image: &Rgba32FImage,
    new_width: u32,
    filter: &mut Filter,
) -> (RgbaImage, RgbaImage) {
    let (width, height) = image.dimensions();

    let max: f32 = NumCast::from(u8::DEFAULT_MAX_VALUE).unwrap();
    let min: f32 = NumCast::from(u8::DEFAULT_MIN_VALUE).unwrap();
    let ratio = width as f32 / new_width as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let src_support = filter.support * sratio;

    // Create a rotated image and fix it later
    let mut out = ImageBuffer::new(height, new_width);

    out.chunks_exact_mut(height as usize * 4)
        .enumerate()
        .par_bridge()
        .for_each(|(outx, outcol)| {
            // Find the point in the input image corresponding to the centre
            // of the current pixel in the output image.
            let inputx = (outx as f32 + 0.5) * ratio;

            // Left and right are slice bounds for the input pixels relevant
            // to the output pixel we are calculating.  Pixel x is relevant
            // if and only if (x >= left) && (x < right).

            // Invariant: 0 <= left < right <= width

            let left = (inputx - src_support).floor() as i64;
            let left = clamp(left, 0, <i64 as From<_>>::from(width) - 1) as u32;

            let right = (inputx + src_support).ceil() as i64;
            let right = clamp(
                right,
                <i64 as From<_>>::from(left) + 1,
                <i64 as From<_>>::from(width),
            ) as u32;

            // Go back to left boundary of pixel, to properly compare with i
            // below, as the kernel treats the centre of a pixel as 0.
            let inputx = inputx - 0.5;
            let mut ws = Vec::with_capacity((right - left) as usize);

            let mut sum = 0.0;
            for i in left..right {
                let w = (filter.kernel)((i as f32 - inputx) / sratio);
                ws.push(w);
                sum += w;
            }
            ws.iter_mut().for_each(|w| *w /= sum);

            outcol
                .chunks_exact_mut(4)
                .enumerate()
                .for_each(|(y, chunk)| {
                    let mut t = (0.0, 0.0, 0.0, 0.0);

                    for (i, w) in ws.iter().enumerate() {
                        let p = image.get_pixel(left + i as u32, y as u32);

                        #[allow(deprecated)]
                        let vec = p.channels4();

                        t.0 += vec.0 * w;
                        t.1 += vec.1 * w;
                        t.2 += vec.2 * w;
                        t.3 += vec.3 * w;
                    }

                    t.0 = linear_to_srgb(t.0) * max;
                    t.1 = linear_to_srgb(t.1) * max;
                    t.2 = linear_to_srgb(t.2) * max;

                    let t = (
                        NumCast::from(FloatNearest(clamp(t.0, min, max))).unwrap(),
                        NumCast::from(FloatNearest(clamp(t.1, min, max))).unwrap(),
                        NumCast::from(FloatNearest(clamp(t.2, min, max))).unwrap(),
                        NumCast::from(FloatNearest(clamp(t.3, min, max))).unwrap(),
                    );

                    chunk[0] = t.0;
                    chunk[1] = t.1;
                    chunk[2] = t.2;
                    chunk[3] = t.3;
                });
        });

    let ret = ImageBuffer::from_fn(new_width, height, |x, y| out[(y, x)]);
    (ret, out)
}


// Sample the columns of the supplied image using the provided filter.
// The width of the image remains unchanged.
// ```new_height``` is the desired height of the new image
// ```filter``` is the filter to use for sampling.
// The return value is not necessarily Rgba, the underlying order of channels in ```image``` is
// preserved.
fn vertical_par_sample(image: &RgbaImage, new_height: u32, filter: &mut Filter) -> Rgba32FImage {
    let (width, height) = image.dimensions();

    let ratio = height as f32 / new_height as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let src_support = filter.support * sratio;

    let mut out = ImageBuffer::new(width, new_height);

    out.chunks_exact_mut(width as usize * 4)
        .enumerate()
        .par_bridge()
        .for_each(|(outy, outrow)| {
            // For an explanation of this algorithm, see the comments
            // in horizontal_sample.
            let inputy = (outy as f32 + 0.5) * ratio;

            let left = (inputy - src_support).floor() as i64;
            let left = clamp(left, 0, <i64 as From<_>>::from(height) - 1) as u32;

            let right = (inputy + src_support).ceil() as i64;
            let right = clamp(
                right,
                <i64 as From<_>>::from(left) + 1,
                <i64 as From<_>>::from(height),
            ) as u32;

            let inputy = inputy - 0.5;
            let mut ws = Vec::with_capacity((right - left) as usize);

            let mut sum = 0.0;
            for i in left..right {
                let w = (filter.kernel)((i as f32 - inputy) / sratio);
                ws.push(w);
                sum += w;
            }
            ws.iter_mut().for_each(|w| *w /= sum);

            outrow
                .chunks_exact_mut(4)
                .enumerate()
                .for_each(|(x, chunk)| {
                    let mut t = (0.0, 0.0, 0.0, 0.0);


                    for (i, w) in ws.iter().enumerate() {
                        let p = image.get_pixel(x as u32, left + i as u32);

                        #[allow(deprecated)]
                        let vec = p.channels4();

                        t.0 += SRGB_LUT[vec.0 as usize] * w;
                        t.1 += SRGB_LUT[vec.1 as usize] * w;
                        t.2 += SRGB_LUT[vec.2 as usize] * w;
                        t.3 += <f32 as NumCast>::from(vec.3).unwrap() * w;
                    }


                    chunk[0] = t.0;
                    chunk[1] = t.1;
                    chunk[2] = t.2;
                    chunk[3] = t.3;
                });
        });

    out
}

/// Resize the supplied image to the specified dimensions in linear light, assuming srgb input.
/// ```nwidth``` and ```nheight``` are the new dimensions.
/// ```filter``` is the sampling filter to use.
pub fn resize_par_linear(
    image: &RgbaImage,
    nwidth: u32,
    nheight: u32,
    filter: FilterType,
) -> RgbaImage {
    let mut method = match filter {
        FilterType::Nearest => Filter {
            kernel: Box::new(box_kernel),
            support: 0.0,
        },
        FilterType::Triangle => Filter {
            kernel: Box::new(triangle_kernel),
            support: 1.0,
        },
        FilterType::CatmullRom => Filter {
            kernel: Box::new(catmullrom_kernel),
            support: 2.0,
        },
        FilterType::Gaussian => Filter {
            kernel: Box::new(gaussian_kernel),
            support: 3.0,
        },
        FilterType::Lanczos3 => Filter {
            kernel: Box::new(lanczos3_kernel),
            support: 3.0,
        },
    };

    let vert = vertical_par_sample(image, nheight, &mut method);
    let (ret, horiz_flipped) = horizontal_par_sample(&vert, nwidth, &mut method);

    // Drop everything in one single task
    rayon::spawn(move || {
        drop(vert);
        drop(horiz_flipped);
    });
    ret
}

// Results from doing the calculations as f64
#[allow(clippy::unreadable_literal)]
#[rustfmt::skip]
const SRGB_LUT: [f32; 256] = [
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
];
