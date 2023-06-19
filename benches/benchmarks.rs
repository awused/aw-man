#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::prelude::OsStringExt;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use aw_man::natsort::{key, ParsedString};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use image::{ImageBuffer, Luma, LumaA, Rgb, Rgba};
use ocl::{Device, DeviceType, Platform, ProQue};
use rand::Rng;
use rayon::prelude::*;

const CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456790123456789._-...";
const GPU_PREFIX: &str = "NVIDIA";

#[derive(Clone)]
struct TestSize(usize, usize);

static LENGTHS: &[usize] = &[1, 100, 1000];
static COUNTS: &[usize] = &[10, 1000, 50000];

impl fmt::Display for TestSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "length: {}, count: {}", self.0, self.1)
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
                        let _sorted: Vec<_> = unsorted
                            .into_iter()
                            .map(|s| s.into_original().into_string().unwrap())
                            .collect();
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
                        let _sorted: Vec<_> = unsorted
                            .into_iter()
                            .map(|s| unsafe {
                                // s.into_original().into_string().unwrap_unchecked()
                                String::from_utf8_unchecked(s.into_original().into_vec())
                            })
                            .collect();
                        total += start.elapsed();
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

static SORT_METHODS: &[&'static str] = &["sort", "sort_unstable", "par_sort", "par_sort_unstable"];

#[derive(Clone)]
struct SortSize(&'static str, TestSize);

impl fmt::Display for SortSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "method: {}, {}", self.0, self.1)
    }
}

fn sort_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_only");
    group.sample_size(10);

    for name in SORT_METHODS {
        for len in LENGTHS {
            for n in COUNTS {
                let s = SortSize(name, TestSize(*len, *n));
                group.bench_with_input(BenchmarkId::from_parameter(s.clone()), &s, |b, s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let unsorted = init_strings(&s.1);
                            let mut unsorted: Vec<_> = unsorted
                                .par_iter()
                                .map(|s| ParsedString::from(OsString::from(s)))
                                .collect();
                            let start = Instant::now();
                            match *name {
                                "sort" => unsorted.sort(),
                                "sort_unstable" => unsorted.sort_unstable(),
                                "par_sort" => unsorted.par_sort(),
                                "par_sort_unstable" => unsorted.par_sort_unstable(),
                                _ => unreachable!(),
                            }
                            total += start.elapsed();
                        }
                        total
                    })
                });
            }
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

// Take the first available matching the prefix, if any.
// No method to differentiate between identical GPUs but this should be fine.
pub fn find_best_opencl_device() -> Option<(Platform, Device)> {
    for platform in Platform::list() {
        if let Some(device) = Device::list(platform, Some(DeviceType::GPU))
            .iter()
            .flatten()
            .find(|d| d.name().unwrap_or_else(|_| "".to_string()).starts_with(GPU_PREFIX))
        {
            return Some((platform, *device));
        }
    }

    if !GPU_PREFIX.is_empty() {
        println!("Could not find matching GPU for prefix \"{GPU_PREFIX}\", try --show-gpus");
    }

    // The code in resample.rs is faster than running resample.cl on the CPU.
    None
}

pub fn find_cpu_opencl_device() -> Option<(Platform, Device)> {
    for platform in Platform::list() {
        let devices = Device::list(platform, Some(DeviceType::CPU));
        let Ok(devices) = devices else {
            continue;
        };

        if let Some(device) = devices.first() {
            return Some((platform, *device));
        }
    }

    None
}

fn benchmark_resample_rgba(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_cpu_rgba");
    group.sample_size(50);

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            Rgba::from([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                ((x ^ y) % 256) as u8,
            ])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
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
                                aw_man::resample::FilterType::CatmullRom,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_rgb(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_cpu_rgb");
    group.sample_size(50);


    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            Rgb::from([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
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
                                aw_man::resample::FilterType::CatmullRom,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_greyalpha(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_cpu_greyalpha");
    group.sample_size(50);

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            LumaA::from([(x % 256) as u8, (y % 256) as u8])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
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
                                aw_man::resample::FilterType::CatmullRom,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_grey(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_cpu_grey");
    group.sample_size(50);

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img =
            ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| Luma::from([((x + y) % 256) as u8]));

        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
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
                                aw_man::resample::FilterType::CatmullRom,
                            );
                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_opencl_rgba(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_opencl_rgba");
    group.sample_size(50);

    let (platform, device) = find_best_opencl_device().unwrap();
    let pro_que = ProQue::builder()
        .src(include_str!("../src/resample.cl"))
        .platform(platform)
        .device(device)
        .build()
        .unwrap();

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            Rgba::from([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                ((x ^ y) % 256) as u8,
            ])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
                |b, _s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let vec = img.as_raw();
                            let start = Instant::now();
                            let _pimg = aw_man::resample::resize_opencl(
                                pro_que.clone(),
                                vec,
                                img.dimensions().into(),
                                (res.0, res.1).into(),
                                4,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_opencl_rgb(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_opencl_rgb");
    group.sample_size(50);

    let (platform, device) = find_best_opencl_device().unwrap();
    let pro_que = ProQue::builder()
        .src(include_str!("../src/resample.cl"))
        .platform(platform)
        .device(device)
        .build()
        .unwrap();

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            Rgb::from([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
                |b, _s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let vec = img.as_raw();
                            let start = Instant::now();
                            let _pimg = aw_man::resample::resize_opencl(
                                pro_que.clone(),
                                vec,
                                img.dimensions().into(),
                                (res.0, res.1).into(),
                                3,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_opencl_greyalpha(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_opencl_greyalpha");
    group.sample_size(50);

    let (platform, device) = find_best_opencl_device().unwrap();
    let pro_que = ProQue::builder()
        .src(include_str!("../src/resample.cl"))
        .platform(platform)
        .device(device)
        .build()
        .unwrap();


    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            LumaA::from([(x % 256) as u8, (y % 256) as u8])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
                |b, _s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let vec = img.as_raw();
                            let start = Instant::now();
                            let _pimg = aw_man::resample::resize_opencl(
                                pro_que.clone(),
                                vec,
                                img.dimensions().into(),
                                (res.0, res.1).into(),
                                2,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

fn benchmark_resample_opencl_grey(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_opencl_grey");
    group.sample_size(50);

    let (platform, device) = find_best_opencl_device().unwrap();
    let pro_que = ProQue::builder()
        .src(include_str!("../src/resample.cl"))
        .platform(platform)
        .device(device)
        .build()
        .unwrap();

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img =
            ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| Luma::from([((x + y) % 256) as u8]));

        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
                |b, _s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let vec = img.as_raw();
                            let start = Instant::now();
                            let _pimg = aw_man::resample::resize_opencl(
                                pro_que.clone(),
                                vec,
                                img.dimensions().into(),
                                (res.0, res.1).into(),
                                1,
                            );
                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
    }
}

// Not really a fair comparison as this will use all available CPU cores.
fn benchmark_resample_cpu_opencl_rgba(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample_cpu_opencl_rgba");
    group.sample_size(50);

    let (platform, device) = find_cpu_opencl_device().unwrap();
    let pro_que = ProQue::builder()
        .src(include_str!("../src/resample.cl"))
        .platform(platform)
        .device(device)
        .build()
        .unwrap();

    for src_res in [(15360, 8640), (7680, 4320), (3840, 2160)] {
        let img = ImageBuffer::from_fn(src_res.0, src_res.1, |x, y| {
            Rgba::from([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                ((x ^ y) % 256) as u8,
            ])
        });
        for res in [(7056, 3888), (3556, 2000), (1920, 1080), (1280, 720), (640, 480)] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{:?} -> {:?}", src_res, res)),
                &(src_res, res),
                |b, _s| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::from_secs(0);

                        for _i in 0..iters {
                            let vec = img.as_raw();
                            let start = Instant::now();
                            let _pimg = aw_man::resample::resize_opencl(
                                pro_que.clone(),
                                vec,
                                img.dimensions().into(),
                                (res.0, res.1).into(),
                                4,
                            );

                            total += start.elapsed();
                        }
                        total
                    })
                },
            );
        }
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
    sort_only,
    bench_swizzle,
    benchmark_resample_rgba,
    benchmark_resample_rgb,
    benchmark_resample_greyalpha,
    benchmark_resample_grey,
    benchmark_resample_opencl_rgba,
    benchmark_resample_opencl_rgb,
    benchmark_resample_opencl_greyalpha,
    benchmark_resample_opencl_grey,
    benchmark_resample_cpu_opencl_rgba,
);
criterion_main!(benches);
