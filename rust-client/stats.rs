use crate::events::Events;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, AtomicI64, Ordering},
    },
    time::Instant,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Stats {
    pub total_bytes_up: AtomicI64,
    pub total_bytes_down: AtomicI64,
    pub active_connections: AtomicI32,
    streams: Mutex<BTreeMap<usize, StreamEntry>>,
}

struct StreamEntry {
    incarnation: u64,
    server: SocketAddr,
    since: Instant,
}

/// Состояние одного рабочего потока для события STREAMS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamSnapshot {
    pub worker: usize,
    pub server: String,
    pub seconds: u64,
}

/// Снимок потоков уходит раз в столько тиков (по 500 мс).
const STREAMS_EVERY_TICKS: u32 = 10;

impl Stats {
    fn streams(&self) -> std::sync::MutexGuard<'_, BTreeMap<usize, StreamEntry>> {
        self.streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn stream_up(&self, worker: usize, incarnation: u64, server: SocketAddr) {
        self.streams().insert(
            worker,
            StreamEntry {
                incarnation,
                server,
                since: Instant::now(),
            },
        );
    }

    /// Возвращает время жизни потока, если запись ещё принадлежит этому
    /// воплощению воркера (новое воплощение могло уже занять номер).
    pub fn stream_down(&self, worker: usize, incarnation: u64) -> Option<u64> {
        let mut streams = self.streams();
        if streams.get(&worker)?.incarnation != incarnation {
            return None;
        }
        streams
            .remove(&worker)
            .map(|entry| entry.since.elapsed().as_secs())
    }

    pub fn stream_snapshot(&self) -> Vec<StreamSnapshot> {
        self.streams()
            .iter()
            .map(|(worker, entry)| StreamSnapshot {
                worker: *worker,
                server: entry.server.to_string(),
                seconds: entry.since.elapsed().as_secs(),
            })
            .collect()
    }

    pub async fn run(self: Arc<Self>, events: Events, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        interval.tick().await;
        let mut log_counter = 0u32;
        let mut streams_counter = STREAMS_EVERY_TICKS - 1;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = interval.tick() => {
                    let active = self.active_connections.load(Ordering::Relaxed);
                    let up = self.total_bytes_up.load(Ordering::Relaxed);
                    let down = self.total_bytes_down.load(Ordering::Relaxed);
                    log_counter += 1;
                    if log_counter >= 6 {
                        log_counter = 0;
                        let total_mb = (up + down) as f64 / (1024.0 * 1024.0);
                        crate::log_error!("[СТАТИСТИКА] Активных: {active} | Трафик: {total_mb:.2} МБ");
                    }
                    events.stats(active, up, down);
                    streams_counter += 1;
                    if streams_counter >= STREAMS_EVERY_TICKS {
                        streams_counter = 0;
                        if events.enabled() {
                            events.streams(&self.stream_snapshot());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Stats;

    #[test]
    fn stream_registry_tracks_incarnations() {
        let stats = Stats::default();
        let server = "1.2.3.4:3478".parse().unwrap();
        stats.stream_up(3, 1, server);
        stats.stream_up(5, 2, server);
        assert_eq!(stats.stream_snapshot().len(), 2);
        // Новое воплощение воркера 3 заменило старое: старое не снимает запись.
        stats.stream_up(3, 7, server);
        assert_eq!(stats.stream_down(3, 1), None);
        assert_eq!(stats.stream_down(3, 7), Some(0));
        let snapshot = stats.stream_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].worker, 5);
        assert_eq!(snapshot[0].server, "1.2.3.4:3478");
    }
}
