use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::{Value, json};
use sodp::delta::diff;

fn make_object(n: usize) -> Value {
    let mut map = serde_json::Map::with_capacity(n);
    for i in 0..n {
        map.insert(format!("field_{i}"), json!(i));
    }
    Value::Object(map)
}

fn mutate_fields(obj: &Value, count: usize) -> Value {
    let mut new = obj.clone();
    let map = new.as_object_mut().unwrap();
    for (i, (_, v)) in map.iter_mut().enumerate() {
        if i >= count {
            break;
        }
        *v = json!("changed");
    }
    new
}

fn bench_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_diff");

    // Small: 5 fields, 1 changed
    let small_old = make_object(5);
    let small_new = mutate_fields(&small_old, 1);
    group.bench_function("small_5f_1c", |b| {
        b.iter(|| diff(black_box(&small_old), black_box(&small_new)))
    });

    // Medium: 20 fields, 3 changed
    let med_old = make_object(20);
    let med_new = mutate_fields(&med_old, 3);
    group.bench_function("medium_20f_3c", |b| {
        b.iter(|| diff(black_box(&med_old), black_box(&med_new)))
    });

    // Large: 100 fields, 5 changed
    let large_old = make_object(100);
    let large_new = mutate_fields(&large_old, 5);
    group.bench_function("large_100f_5c", |b| {
        b.iter(|| diff(black_box(&large_old), black_box(&large_new)))
    });

    // Full replacement: all fields changed
    let full_old = make_object(100);
    let full_new = mutate_fields(&full_old, 100);
    group.bench_function("full_replace_100f", |b| {
        b.iter(|| diff(black_box(&full_old), black_box(&full_new)))
    });

    group.finish();
}

criterion_group!(benches, bench_diff);
criterion_main!(benches);
