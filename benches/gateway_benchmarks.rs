use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rustynail::memory::{InMemoryStore, MemoryStore};

fn bench_inmemory_store_add(c: &mut Criterion) {
    let store = InMemoryStore::new(100);
    c.bench_function("inmemory_store_add", |b| {
        b.iter(|| {
            store.add_message(
                black_box("bench-user"),
                black_box("User: hello world".to_string()),
            );
        })
    });
}

fn bench_inmemory_store_get(c: &mut Criterion) {
    let store = InMemoryStore::new(200);
    // Pre-seed with 100 messages
    for i in 0..100 {
        store.add_message("bench-user", format!("User: message {}", i));
    }
    c.bench_function("inmemory_store_get", |b| {
        b.iter(|| {
            let _ = black_box(store.get_history(black_box("bench-user")));
        })
    });
}

fn bench_config_load(c: &mut Criterion) {
    // Benchmark YAML parse + env var overhead using a minimal in-memory YAML
    const MINIMAL_YAML: &str = r#"
gateway:
  http_port: 8080
  websocket_port: 18789
channels: {}
agents:
  api_key: bench_key
"#;

    c.bench_function("config_load_yaml", |b| {
        b.iter(|| {
            let _: rustynail::config::Config =
                serde_yaml::from_str(black_box(MINIMAL_YAML)).expect("parse failed");
        })
    });
}

fn bench_message_stats_record(c: &mut Criterion) {
    use rustynail::gateway::dashboard::MessageStats;
    use rustynail::types::Message;

    let stats = MessageStats::new();
    let msg = Message::new(
        "bench-channel".to_string(),
        "bench-user".to_string(),
        "Bench User".to_string(),
        "hello benchmark".to_string(),
    );

    c.bench_function("message_stats_record_tokens", |b| {
        b.iter(|| {
            stats.record_tokens(black_box(100), black_box(50));
        })
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("message_stats_record_inbound", |b| {
        b.iter(|| {
            rt.block_on(async {
                stats.record_inbound_async(black_box(&msg)).await;
            });
        })
    });
}

criterion_group!(
    benches,
    bench_inmemory_store_add,
    bench_inmemory_store_get,
    bench_config_load,
    bench_message_stats_record,
);
criterion_main!(benches);
