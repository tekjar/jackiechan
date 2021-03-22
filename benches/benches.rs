use criterion::{black_box, criterion_group, criterion_main, Criterion};
use jackiechan::{mpmc, spsc};

fn spsc_send(n: usize) {
    let (tx, rx) = spsc::bounded(n);
    for i in 0..n {
        tx.try_send(i).unwrap();
    }
}

fn mpmc_send(n: usize) {
    let (tx, rx) = mpmc::bounded(n);
    for i in 0..n {
        tx.try_send(i).unwrap();
    }
}

fn flume_send(n: usize) {
    let (tx, rx) = flume::bounded(n);
    for i in 0..n {
        tx.try_send(i).unwrap();
    }
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("spsc try send", |b| b.iter(|| spsc_send(black_box(1000))));
    c.bench_function("mpmc try send", |b| b.iter(|| mpmc_send(black_box(1000))));
    c.bench_function("flume try send", |b| b.iter(|| flume_send(black_box(1000))));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
