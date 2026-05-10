use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use sodp::delta::{DeltaOp, DeltaOpKind};
use sodp::fanout::encode_delta_body;
use sodp::frame::{self, Frame, types};

fn bench_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_codec");

    let frame = Frame {
        frame_type: types::DELTA,
        stream_id: 42,
        seq: 1000,
        body: json!({
            "version": 5,
            "ops": [
                {"op": "UPDATE", "path": "/likes", "value": 11},
                {"op": "ADD", "path": "/newField", "value": "hello"}
            ]
        }),
    };

    let encoded = frame.encode().unwrap();

    group.bench_function("encode", |b| b.iter(|| black_box(&frame).encode().unwrap()));

    group.bench_function("decode", |b| {
        b.iter(|| Frame::decode(black_box(&encoded)).unwrap())
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let bytes = black_box(&frame).encode().unwrap();
            Frame::decode(&bytes).unwrap()
        })
    });

    group.finish();

    // encode_delta_body benchmark
    let mut group2 = c.benchmark_group("delta_encode");

    let ops = vec![
        DeltaOp {
            op: DeltaOpKind::Update,
            path: "/likes".into(),
            value: Some(json!(11)),
        },
        DeltaOp {
            op: DeltaOpKind::Add,
            path: "/newField".into(),
            value: Some(json!("hello")),
        },
    ];

    let body_mp = encode_delta_body(5, &ops);

    group2.bench_function("encode_delta_body", |b| {
        b.iter(|| encode_delta_body(black_box(5), black_box(&ops)))
    });

    group2.bench_function("delta_bytes", |b| {
        b.iter(|| frame::delta_bytes(black_box(42), black_box(1000), black_box(&body_mp)))
    });

    group2.finish();
}

criterion_group!(benches, bench_codec);
criterion_main!(benches);
