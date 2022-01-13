#![feature(portable_simd)]

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::time::{Duration, Instant};

use aw_man::natsort::{key, ParsedString};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use image::{ImageBuffer, Rgba};
use rand::Rng;
use rayon::prelude::*;

const CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456790123456789._-...";
#[derive(Clone)]
struct TestSize(usize, usize);

static LENGTHS: &[usize] = &[1, 100, 1000];
static COUNTS: &[usize] = &[10, 1000, 5000];

impl fmt::Display for TestSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "length: {}, count {}", self.0, self.1)
    }
}

fn init_strings(s: &TestSize) -> Vec<String> {
    let strlen = s.0;
    let n = s.1;
    let mut rng = rand::thread_rng();

    (0..n)
        .map(|_| {
            (0..strlen)
                .map(|_| {
                    let idx = rng.gen_range(0..CHARACTERS.len());
                    CHARACTERS[idx] as char
                })
                .collect()
        })
        .collect()
}

fn benchmark_cached_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("cached_key");

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            if *n >= 100_000 {
                group.sampling_mode(SamplingMode::Flat);
            } else {
                group.sampling_mode(SamplingMode::Auto);
            }
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let mut unsorted = init_strings(s);
                        let start = Instant::now();
                        unsorted.sort_by_cached_key(|st| key(OsStr::new(st)));

                        total += start.elapsed();
                    }
                    total
                })
            });
        }
    }
}

fn benchmark_map_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("map_key");
    group.sample_size(10);

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let mut unsorted = init_strings(s);
                        let start = Instant::now();
                        let hm: HashMap<String, ParsedString> = unsorted
                            .iter()
                            .map(|s| (s.to_string(), key(OsStr::new(s))))
                            .collect();
                        unsorted.sort_by_cached_key(|st| hm.get(st).unwrap());

                        total += start.elapsed();
                    }
                    total
                })
            });
        }
    }
}

fn benchmark_parallel_map_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("rayon_key");
    group.sample_size(10);

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let mut unsorted = init_strings(s);
                        let start = Instant::now();
                        let hm: HashMap<String, ParsedString> = unsorted
                            .par_iter()
                            .map(|s| (s.to_string(), key(OsStr::new(s))))
                            .collect();
                        unsorted.sort_by_cached_key(|st| hm.get(st).unwrap());

                        total += start.elapsed();
                    }
                    total
                })
            });
        }
    }
}


static SWIZZLE_LENS: &[usize] = &[
    64,
    1_048_576,
    1_048_576 * 64,
    1_048_576 * 256,
    1_048_576 * 1024,
];

fn bench_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();
                    x.chunks_exact_mut(4).for_each(|c| c.swap(0, 2));
                    total += start.elapsed();
                }
                total
            })
        });
    }
}


use std::simd::{simd_swizzle, Simd};

fn bench_simd_4_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_4_swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();

                    for c in x.chunks_exact_mut(4) {
                        let x = Simd::<u8, 4>::from_slice(c);
                        let r = simd_swizzle!(x, [2, 1, 0, 3]);
                        c.copy_from_slice(&r.to_array())
                    }

                    total += start.elapsed();
                }
                total
            })
        });
    }
}

fn bench_simd_8_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_8_swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();

                    for c in x.chunks_exact_mut(8) {
                        let x = Simd::<u8, 8>::from_slice(c);
                        let r = simd_swizzle!(x, [2, 1, 0, 3, 6, 5, 4, 7]);
                        c.copy_from_slice(&r.to_array())
                    }

                    total += start.elapsed();
                }
                total
            })
        });
    }
}

#[rustfmt::skip]
fn bench_simd_16_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_16_swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();

                    for c in x.chunks_exact_mut(16) {
                        let x = Simd::<u8, 16>::from_slice(c);
                        let r = simd_swizzle!(
                            x,
                            [
                                2, 1, 0, 3,
                                6, 5, 4, 7,
                                10, 9, 8, 11,
                                14, 13, 12, 15,
                            ]
                        );
                        c.copy_from_slice(&r.to_array())
                    }

                    total += start.elapsed();
                }
                total
            })
        });
    }
}

#[rustfmt::skip]
fn bench_simd_32_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_32_swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();

                    for c in x.chunks_exact_mut(32) {
                        let x = Simd::<u8, 32>::from_slice(c);
                        let r = simd_swizzle!(
                            x,
                            [
                                2, 1, 0, 3,
                                6, 5, 4, 7,
                                10, 9, 8, 11,
                                14, 13, 12, 15,
                                18, 17, 16, 19,
                                22, 21, 20, 23,
                                26, 25, 24, 27,
                                30, 29, 28, 31,
                            ]
                        );
                        c.copy_from_slice(&r.to_array())
                    }

                    total += start.elapsed();
                }
                total
            })
        });
    }
}

#[rustfmt::skip]
fn bench_simd_64_swizzle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_64_swizzle");
    group.sample_size(10);

    for len in SWIZZLE_LENS {
        group.bench_with_input(BenchmarkId::from_parameter(len.to_string()), len, |b, s| {
            b.iter_custom(|iters| {
                let mut total = Duration::from_secs(0);

                for _i in 0..iters {
                    let mut x: Vec<u8> = Vec::with_capacity(*s);
                    for y in 0..*s {
                        x.push((y % 256) as u8);
                    }
                    let start = Instant::now();

                    for c in x.chunks_exact_mut(64) {
                        let x = Simd::<u8, 64>::from_slice(c);
                        let r = simd_swizzle!(
                            x,
                            [
                                2, 1, 0, 3,
                                6, 5, 4, 7,
                                10, 9, 8, 11,
                                14, 13, 12, 15,
                                18, 17, 16, 19,
                                22, 21, 20, 23,
                                26, 25, 24, 27,
                                30, 29, 28, 31,
                                34, 33, 32, 35,
                                38, 37, 36, 39,
                                42, 41, 40, 43,
                                46, 45, 44, 47,
                                50, 49, 48, 51,
                                54, 53, 52, 55,
                                58, 57, 56, 59,
                                62, 61, 60, 63,
                            ]
                        );
                        c.copy_from_slice(&r.to_array())
                    }

                    total += start.elapsed();
                }
                total
            })
        });
    }
}


fn benchmark_resample(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample");
    group.sample_size(50);


    let img = ImageBuffer::from_fn(7680, 4320, |x, y| {
        Rgba::from([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 127])
    });

    for res in [(7056, 3888), (3840, 2160), (1920, 1080), (1280, 720)] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}x{}", res.0, res.1)),
            &res,
            |b, _s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let img = img.clone();
                        let start = Instant::now();
                        let _pimg = aw_man::resample::resize_par_linear(
                            img,
                            res.0,
                            res.1,
                            aw_man::resample::FilterType::Lanczos3,
                        );

                        total += start.elapsed();
                    }
                    total
                })
            },
        );
    }
}

// Cached keys are fastest for reasonable sizes.
// Mapped keys into cached keys use extra memory and are slower for simple cases, but eventually
// become faster as complexity increases.
// Parallel mapping uses additional memory and multiple threads and is significantly slower (>100%
// slower) on small strings, even as counts increase. As the complexity of individual strings
// inrease they can become several times faster than the single-threaded implementations.
criterion_group!(
    benches,
    benchmark_cached_key,
    benchmark_map_key,
    benchmark_parallel_map_key,
    bench_swizzle,
    bench_simd_4_swizzle,
    bench_simd_8_swizzle,
    bench_simd_16_swizzle,
    bench_simd_32_swizzle,
    bench_simd_64_swizzle,
    benchmark_resample,
);
criterion_main!(benches);
