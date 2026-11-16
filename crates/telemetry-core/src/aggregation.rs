use serde::Serialize;
use std::collections::BTreeMap;

/// Histograms use upper-inclusive millisecond boundaries; the last bucket is +Inf.
pub const SPAN_BUCKET_BOUNDS_MS: [f64; 8] = [1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1_000.0, 5_000.0];

#[derive(Clone, Debug, Default, Serialize)]
pub struct CounterAggregate {
    pub count: u64,
    pub sum: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpanAggregate {
    pub count: u64,
    pub sum_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub buckets: [u64; 9],
}

impl SpanAggregate {
    pub(super) fn new(duration_ms: f64) -> Self {
        let mut aggregate = Self {
            count: 0,
            sum_ms: 0.0,
            min_ms: duration_ms,
            max_ms: duration_ms,
            buckets: [0; 9],
        };
        aggregate.add(duration_ms);
        aggregate
    }

    pub(super) fn add(&mut self, duration_ms: f64) {
        self.count = self.count.saturating_add(1);
        self.sum_ms += duration_ms;
        self.min_ms = self.min_ms.min(duration_ms);
        self.max_ms = self.max_ms.max(duration_ms);
        let bucket = SPAN_BUCKET_BOUNDS_MS
            .iter()
            .position(|upper| duration_ms <= *upper)
            .unwrap_or(8);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Aggregates {
    pub counters: BTreeMap<String, CounterAggregate>,
    pub spans: BTreeMap<String, SpanAggregate>,
}
