use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendMetrics {
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub errors: u64,
}

#[derive(Debug, Default)]
struct BackendMetricsInner {
    puts: AtomicU64,
    gets: AtomicU64,
    deletes: AtomicU64,
    errors: AtomicU64,
}

static METRICS: OnceLock<BackendMetricsInner> = OnceLock::new();

fn metrics_inner() -> &'static BackendMetricsInner {
    METRICS.get_or_init(BackendMetricsInner::default)
}

pub(crate) fn inc_put() {
    metrics_inner().puts.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn inc_get() {
    metrics_inner().gets.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn inc_delete() {
    metrics_inner().deletes.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn inc_error() {
    metrics_inner().errors.fetch_add(1, Ordering::Relaxed);
}

pub fn metrics_snapshot() -> BackendMetrics {
    let inner = metrics_inner();
    BackendMetrics {
        puts: inner.puts.load(Ordering::Relaxed),
        gets: inner.gets.load(Ordering::Relaxed),
        deletes: inner.deletes.load(Ordering::Relaxed),
        errors: inner.errors.load(Ordering::Relaxed),
    }
}

