use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use sodp::state::StateStore;

fn bench_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("state_store");

    // apply (set): replace full value, compute diff
    group.bench_function("apply_set", |b| {
        let store = StateStore::new();
        store.apply("bench_key", json!({"a": 1, "b": 2, "c": 3, "d": 4, "e": 5}));
        b.iter(|| {
            store.apply(
                black_box("bench_key"),
                black_box(json!({"a": 99, "b": 2, "c": 3, "d": 4, "e": 5})),
            )
        })
    });

    // apply on new key (first write)
    group.bench_function("apply_new_key", |b| {
        let store = StateStore::new();
        let mut i = 0u64;
        b.iter(|| {
            i += 1;
            store.apply(black_box(&format!("key_{i}")), black_box(json!({"x": 1})))
        })
    });

    // apply with large object, small change
    group.bench_function("apply_large_obj", |b| {
        let store = StateStore::new();
        let mut obj = serde_json::Map::with_capacity(50);
        for i in 0..50 {
            obj.insert(format!("f{i}"), json!(i));
        }
        store.apply("large", serde_json::Value::Object(obj.clone()));

        let mut changed = obj.clone();
        changed.insert("f0".into(), json!("changed"));
        let new_val = serde_json::Value::Object(changed);

        b.iter(|| store.apply(black_box("large"), black_box(new_val.clone())))
    });

    group.finish();
}

criterion_group!(benches, bench_state);
criterion_main!(benches);
