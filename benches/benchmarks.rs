use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use aw_man::natsort::{key, ParsedString};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
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
                        unsorted.sort_by_cached_key(|st| key(st));

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
                        let hm: HashMap<String, ParsedString> =
                            unsorted.iter().map(|s| (s.to_string(), key(s))).collect();
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
                            .map(|s| (s.to_string(), key(s)))
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
    benchmark_parallel_map_key
);
criterion_main!(benches);
