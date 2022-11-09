use consensus::processes::difficulty::{calc_average_target__, calc_average_target_naive__, calc_average_target_unoptimized__};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use math::Uint256;
use rand::Rng;

pub fn daa_average_target_benchmark(c: &mut Criterion) {
    let targets = gen_random_close_targets();
    c.bench_function("difficulty::calc_average_target", |b| b.iter(|| calc_average_target__(black_box(&targets))));
    c.bench_function("difficulty::calc_average_target_unoptimized", |b| {
        b.iter(|| calc_average_target_unoptimized__(black_box(&targets)))
    });
    c.bench_function("difficulty::calc_average_target_naive", |b| b.iter(|| calc_average_target_naive__(black_box(&targets))));
}

fn gen_random_close_targets() -> Vec<Uint256> {
    let mut targets = Vec::with_capacity(2641);
    let mut thread_rng = rand::thread_rng();
    for _ in 0..2641 {
        let random_target = thread_rng.gen::<u64>();
        targets.push(Uint256::from(random_target));
    }
    targets
}

criterion_group!(benches, daa_average_target_benchmark);
criterion_main!(benches);
