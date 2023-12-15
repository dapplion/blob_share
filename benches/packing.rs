use blob_share::packing::{pack_items_brute_force, pack_items_greedy_sorted, pack_items_knapsack};
use criterion::{criterion_group, criterion_main, Criterion};
use rand::{rngs::StdRng, Rng, SeedableRng};

fn brute_force_benchmark(c: &mut Criterion) {
    let mut rng = StdRng::seed_from_u64(0);

    for n in [100, 1000, 10000] {
        let max_len = 131072;
        let cost_per_len = 1000000000;
        let range_len = 1..max_len;
        let range_cost_per_len = cost_per_len..2 * cost_per_len;
        let mut items = (0..n)
            .map(|_| {
                (
                    rng.gen_range(range_len.clone()),
                    rng.gen_range(range_cost_per_len.clone()),
                )
            })
            .collect::<Vec<(usize, u128)>>();

        items.sort_by(|a, b| a.0.cmp(&b.0));

        c.bench_function(&format!("greedy sorted n={n}"), |b| {
            b.iter(|| pack_items_greedy_sorted(&items, max_len, cost_per_len).unwrap());
        });
    }

    // performance is highly dependant on max_len
    for (n, max_len) in [
        (32, 4096),
        (32, 131072),
        (250, 4096),
        (1000, 4096),
        (10000, 4096),
    ] {
        let items = vec![(1, 10); n];
        let cost_per_len = 1;

        c.bench_function(&format!("knapsack n={n} max_len={max_len}"), |b| {
            b.iter(|| pack_items_knapsack(&items, max_len, cost_per_len).unwrap());
        });
    }

    for n in [8, 16, 31] {
        let items = vec![(1, 10); n];
        // performance is only dependant on n, values or irrelevant
        let max_len = 2 * n;
        let cost_per_len = 1;
        c.bench_function(&format!("brute_force n={n}"), |b| {
            b.iter(|| pack_items_brute_force(&items, max_len, cost_per_len).unwrap());
        });
    }
}

criterion_group!(benches, brute_force_benchmark);
criterion_main!(benches);
