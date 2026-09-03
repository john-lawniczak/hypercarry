use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use hypercarry_core::metrics::{FundingInterval, FundingStats, basis, hourly_spread};
use rust_decimal::Decimal;
use std::{hint::black_box, str::FromStr};

fn dec(value: &str) -> Decimal {
    Decimal::from_str(value).expect("benchmark decimal literal is valid")
}

fn metric_benchmarks(criterion: &mut Criterion) {
    let perp_mark = dec("101.25");
    let spot_mid = dec("100.75");
    criterion.bench_function("basis", |bencher| {
        bencher.iter(|| basis(black_box(perp_mark), black_box(spot_mid)));
    });

    let hourly = FundingInterval::from_hours(1).expect("one hour is nonzero");
    let eight_hourly = FundingInterval::from_hours(8).expect("eight hours is nonzero");
    let a_rate = dec("0.00012");
    let b_rate = dec("0.0008");
    criterion.bench_function("hourly_spread", |bencher| {
        bencher.iter(|| hourly_spread(black_box(a_rate), hourly, black_box(b_rate), eight_hourly));
    });

    let mut group = criterion.benchmark_group("funding_stats");
    for window_size in [24_usize, 168, 720] {
        let rates: Vec<_> = (0..window_size)
            .map(|index| {
                let coefficient = i64::try_from(index % 17).expect("small index") - 8;
                Decimal::new(coefficient, 6)
            })
            .collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(window_size),
            &rates,
            |bencher, rates| {
                bencher.iter(|| FundingStats::from_hourly_rates(black_box(rates)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, metric_benchmarks);
criterion_main!(benches);
