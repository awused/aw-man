use image::{GenericImageView, ImageBuffer, Pixel, Primitive, Rgba32FImage};
use num_traits::{clamp, NumCast, ToPrimitive};

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
pub(crate) struct Filter<'a> {
    /// The filter's filter function.
    pub(crate) kernel: Box<dyn Fn(f32) -> f32 + 'a>,

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

// Sample the rows of the supplied image using the provided filter.
// The height of the image remains unchanged.
// ```new_width``` is the desired width of the new image
// ```filter``` is the filter to use for sampling.
// ```image``` is not necessarily Rgba and the order of channels is passed through.
fn horizontal_sample<P, S>(
    image: &Rgba32FImage,
    new_width: u32,
    filter: &mut Filter,
) -> ImageBuffer<P, Vec<S>>
where
    P: Pixel<Subpixel = S> + 'static,
    S: Primitive + 'static,
{
    let (width, height) = image.dimensions();
    let mut out = ImageBuffer::new(new_width, height);
    let mut ws = Vec::new();

    let max: f32 = NumCast::from(S::DEFAULT_MAX_VALUE).unwrap();
    let min: f32 = NumCast::from(S::DEFAULT_MIN_VALUE).unwrap();
    let ratio = width as f32 / new_width as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let src_support = filter.support * sratio;

    for outx in 0..new_width {
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

        ws.clear();
        let mut sum = 0.0;
        for i in left..right {
            let w = (filter.kernel)((i as f32 - inputx) / sratio);
            ws.push(w);
            sum += w;
        }
        ws.iter_mut().for_each(|w| *w /= sum);

        for y in 0..height {
            let mut t = (0.0, 0.0, 0.0, 0.0);

            for (i, w) in ws.iter().enumerate() {
                let p = image.get_pixel(left + i as u32, y);

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

            #[allow(deprecated)]
            let t = Pixel::from_channels(
                NumCast::from(FloatNearest(clamp(t.0, min, max))).unwrap(),
                NumCast::from(FloatNearest(clamp(t.1, min, max))).unwrap(),
                NumCast::from(FloatNearest(clamp(t.2, min, max))).unwrap(),
                NumCast::from(FloatNearest(clamp(t.3, min, max))).unwrap(),
            );

            out.put_pixel(outx, y, t);
        }
    }

    out
}

fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        f32::powf((s + 0.055) / 1.055, 2.4)
    }
}

fn linear_to_srgb(s: f32) -> f32 {
    if s <= 0.0031308 {
        s * 12.92
    } else {
        1.055 * f32::powf(s, 1.0 / 2.4) - 0.055
    }
}

// Sample the columns of the supplied image using the provided filter.
// The width of the image remains unchanged.
// ```new_height``` is the desired height of the new image
// ```filter``` is the filter to use for sampling.
// The return value is not necessarily Rgba, the underlying order of channels in ```image``` is
// preserved.
fn vertical_sample<I, P, S>(image: &I, new_height: u32, filter: &mut Filter) -> Rgba32FImage
where
    I: GenericImageView<Pixel = P>,
    P: Pixel<Subpixel = S> + 'static,
    S: Primitive + 'static,
{
    let (width, height) = image.dimensions();
    let mut out = ImageBuffer::new(width, new_height);
    let mut ws = Vec::new();

    let max: f32 = NumCast::from(S::DEFAULT_MAX_VALUE).unwrap();
    let ratio = height as f32 / new_height as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let src_support = filter.support * sratio;

    for outy in 0..new_height {
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

        ws.clear();
        let mut sum = 0.0;
        for i in left..right {
            let w = (filter.kernel)((i as f32 - inputy) / sratio);
            ws.push(w);
            sum += w;
        }
        ws.iter_mut().for_each(|w| *w /= sum);

        for x in 0..width {
            let mut t = (0.0, 0.0, 0.0, 0.0);

            for (i, w) in ws.iter().enumerate() {
                let p = image.get_pixel(x, left + i as u32);

                #[allow(deprecated)]
                let (k1, k2, k3, k4) = p.channels4();
                let vec: (f32, f32, f32, f32) = (
                    NumCast::from(k1).unwrap(),
                    NumCast::from(k2).unwrap(),
                    NumCast::from(k3).unwrap(),
                    NumCast::from(k4).unwrap(),
                );

                let linear = (
                    srgb_to_linear(vec.0 / max),
                    srgb_to_linear(vec.1 / max),
                    srgb_to_linear(vec.2 / max),
                );

                t.0 += linear.0 * w;
                t.1 += linear.1 * w;
                t.2 += linear.2 * w;
                t.3 += vec.3 * w;
            }


            #[allow(deprecated)]
            // This is not necessarily Rgba.
            let t = Pixel::from_channels(t.0, t.1, t.2, t.3);

            out.put_pixel(x, outy, t);
        }
    }

    out
}


/// Resize the supplied image to the specified dimensions in linear light, assuming srgb input.
/// ```nwidth``` and ```nheight``` are the new dimensions.
/// ```filter``` is the sampling filter to use.
pub fn resize_linear<I: GenericImageView>(
    image: &I,
    nwidth: u32,
    nheight: u32,
    filter: FilterType,
) -> ImageBuffer<I::Pixel, Vec<<I::Pixel as Pixel>::Subpixel>>
where
    I::Pixel: 'static,
    <I::Pixel as Pixel>::Subpixel: 'static,
{
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

    // Note: tmp is not necessarily actually Rgba
    let tmp: Rgba32FImage = vertical_sample(image, nheight, &mut method);
    horizontal_sample(&tmp, nwidth, &mut method)
}
