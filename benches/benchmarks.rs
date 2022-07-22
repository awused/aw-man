#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use aw_man::natsort::{key, ParsedString};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use image::{ImageBuffer, Luma, LumaA, Rgb, Rgba};
use rand::Rng;
use rayon::prelude::*;

const CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456790123456789._-...";
#[derive(Clone)]
struct TestSize(usize, usize);

static LENGTHS: &[usize] = &[1, 100, 1000];
static COUNTS: &[usize] = &[10, 1000, 50000];

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
    group.sample_size(10);

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
                        let hm: AHashMap<String, ParsedString> =
                            unsorted.iter().map(|s| (s.to_string(), key(OsStr::new(s)))).collect();
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

fn parsed_string_safe(c: &mut Criterion) {
    let mut group = c.benchmark_group("parsed_string_safe");
    group.sample_size(10);

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let unsorted = init_strings(s);
                        let start = Instant::now();
                        let mut unsorted: Vec<_> = unsorted
                            .into_iter()
                            .map(|s| ParsedString::from(OsString::from(s)))
                            .collect();
                        unsorted.sort_unstable();
                        // let sorted: Vec<_> = unsorted
                        //     .into_iter()
                        //     .map(|s| s.into_original().into_string().unwrap())
                        //     .collect();
                        total += start.elapsed();
                        // drop(sorted);
                    }
                    total
                })
            });
        }
    }
}

fn parsed_string_unsafe(c: &mut Criterion) {
    let mut group = c.benchmark_group("parsed_string_unsafe");
    group.sample_size(10);

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let unsorted = init_strings(s);
                        let start = Instant::now();
                        let mut unsorted: Vec<_> = unsorted
                            .into_iter()
                            .map(|s| ParsedString::from(OsString::from(s)))
                            .collect();
                        unsorted.sort_unstable();
                        // let sorted: Vec<_> = unsorted
                        //     .into_iter()
                        //     .map(|s| unsafe {
                        //         s.into_original().into_string().unwrap_unchecked()
                        //         // String::from_utf8_unchecked(s.into_original().into_vec())
                        //     })
                        //     .collect();
                        total += start.elapsed();
                        // drop(sorted);
                    }
                    total
                })
            });
        }
    }
}

fn parsed_string_rayon(c: &mut Criterion) {
    let mut group = c.benchmark_group("parsed_string_rayon");
    group.sample_size(10);

    for len in LENGTHS {
        for n in COUNTS {
            let s = TestSize(*len, *n);
            group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let unsorted = init_strings(s);
                        let start = Instant::now();
                        let mut unsorted: Vec<_> = unsorted
                            .par_iter()
                            .map(|s| ParsedString::from(OsString::from(s)))
                            .collect();
                        unsorted.par_sort();
                        let sorted: Vec<_> = unsorted
                            .into_par_iter()
                            .map(|s| s.into_original().into_string().unwrap())
                            .collect();
                        total += start.elapsed();
                        drop(sorted);
                    }
                    total
                })
            });
        }
    }
}


static SWIZZLE_LENS: &[usize] = &[64, 1_048_576, 1_048_576 * 64, 1_048_576 * 256, 1_048_576 * 1024];

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


fn benchmark_resample(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample");
    group.sample_size(50);


    drop(rayon::ThreadPoolBuilder::new().num_threads(16).build_global());

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
                        let vec = img.as_raw();
                        let start = Instant::now();
                        let _pimg = aw_man::resample::resize_par_linear::<4>(
                            vec,
                            img.dimensions().into(),
                            (res.0, res.1).into(),
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

fn benchmark_resample_rgb(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_rgb");
    group.sample_size(50);


    drop(rayon::ThreadPoolBuilder::new().num_threads(16).build_global());

    let img = ImageBuffer::from_fn(7680, 4320, |x, y| {
        Rgb::from([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });

    for res in [(7056, 3888), (3840, 2160), (1920, 1080), (1280, 720)] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}x{}", res.0, res.1)),
            &res,
            |b, _s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let vec = img.as_raw();
                        let start = Instant::now();
                        let _pimg = aw_man::resample::resize_par_linear::<3>(
                            vec,
                            img.dimensions().into(),
                            (res.0, res.1).into(),
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

fn benchmark_resample_greyalpha(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_greyalpha");
    group.sample_size(50);


    drop(rayon::ThreadPoolBuilder::new().num_threads(16).build_global());

    let img =
        ImageBuffer::from_fn(7680, 4320, |x, y| LumaA::from([(x % 256) as u8, (y % 256) as u8]));

    for res in [(7056, 3888), (3840, 2160), (1920, 1080), (1280, 720)] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}x{}", res.0, res.1)),
            &res,
            |b, _s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let vec = img.as_raw();
                        let start = Instant::now();
                        let _pimg = aw_man::resample::resize_par_linear::<2>(
                            vec,
                            img.dimensions().into(),
                            (res.0, res.1).into(),
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

fn benchmark_resample_grey(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_grey");
    group.sample_size(50);


    drop(rayon::ThreadPoolBuilder::new().num_threads(16).build_global());

    let img = ImageBuffer::from_fn(7680, 4320, |x, y| Luma::from([((x + y) % 256) as u8]));

    for res in [(7056, 3888), (3840, 2160), (1920, 1080), (1280, 720)] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}x{}", res.0, res.1)),
            &res,
            |b, _s| {
                b.iter_custom(|iters| {
                    let mut total = Duration::from_secs(0);

                    for _i in 0..iters {
                        let vec = img.as_raw();
                        let start = Instant::now();
                        let _pimg = aw_man::resample::resize_par_linear::<1>(
                            vec,
                            img.dimensions().into(),
                            (res.0, res.1).into(),
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
    parsed_string_safe,
    parsed_string_unsafe,
    parsed_string_rayon,
    bench_swizzle,
    benchmark_resample,
    benchmark_resample_rgb,
    benchmark_resample_greyalpha,
    benchmark_resample_grey,
);
criterion_main!(benches);
