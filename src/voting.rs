//! Per-fingerprint vote registry.
//!
//! Each browser tab POSTs its (browser-fingerprint, freq_hz) pair to
//! `/api/vote` every few seconds. A vote stays valid as long as a
//! heartbeat arrives within [`VOTE_TTL`]; tabs that close or stop
//! pinging drop off automatically. The winning frequency is the one
//! with the most active votes (ties broken by the lowest frequency).
//!
//! Because the fingerprint is computed client-side from canvas/WebGL
//! quirks and stashed in `localStorage`, multiple tabs of the same
//! Chrome profile collapse to one vote, while different profiles on
//! the same machine get separate votes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::Notify;

/// How long a vote survives after its last heartbeat. Must comfortably
/// exceed the client heartbeat cadence so one dropped packet does not
/// reshuffle the winner.
pub const VOTE_TTL: Duration = Duration::from_secs(12);

/// Rolling window over which a distinct fingerprint counts as a
/// "daily listener". Anyone who voted or opened the stream within this
/// window is tallied once.
pub const DAILY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
struct Vote {
    freq_hz: u32,
    last_seen: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    votes: HashMap<String, Vote>,
    /// Last time each fingerprint was seen (voting OR listening), for the
    /// rolling daily-listener count. Pruned lazily in `daily_listeners`.
    seen: HashMap<String, Instant>,
}

pub struct VoteRegistry {
    inner: Mutex<Inner>,
    /// Notified whenever the active vote map changes in a way that
    /// could shift the winner (new fingerprint, freq change for an
    /// existing fingerprint, or removal). The vote daemon parks on
    /// this to retune promptly without busy-polling.
    pub notify: Notify,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoteTally {
    pub freq_hz: u32,
    pub votes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoteSnapshot {
    pub winner_hz: Option<u32>,
    pub tallies: Vec<VoteTally>,
    pub active_voters: usize,
}

impl VoteRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            notify: Notify::new(),
        })
    }

    /// Insert or refresh a vote. Returns true if the freq for this
    /// fingerprint changed (or it is a new fingerprint) — i.e. the
    /// winner could plausibly shift and the daemon should re-evaluate.
    pub fn cast(&self, fingerprint: String, freq_hz: u32) -> bool {
        let now = Instant::now();
        let mut g = self.inner.lock();
        let changed = match g.votes.get(&fingerprint) {
            Some(v) => v.freq_hz != freq_hz || (now - v.last_seen) >= VOTE_TTL,
            None => true,
        };
        g.seen.insert(fingerprint.clone(), now);
        g.votes.insert(
            fingerprint,
            Vote {
                freq_hz,
                last_seen: now,
            },
        );
        drop(g);
        if changed {
            self.notify.notify_waiters();
        }
        changed
    }

    /// Drop a vote (e.g. user clicked Stop or closed the tab cleanly).
    pub fn clear(&self, fingerprint: &str) {
        let removed = self.inner.lock().votes.remove(fingerprint).is_some();
        if removed {
            self.notify.notify_waiters();
        }
    }

    /// Evict votes whose last heartbeat is older than [`VOTE_TTL`].
    pub fn prune_stale(&self) {
        let cutoff = Instant::now() - VOTE_TTL;
        let mut g = self.inner.lock();
        let before = g.votes.len();
        g.votes.retain(|_, v| v.last_seen >= cutoff);
        let evicted = g.votes.len() != before;
        drop(g);
        if evicted {
            self.notify.notify_waiters();
        }
    }

    /// Current winner, tallies, and active voter count.
    pub fn snapshot(&self) -> VoteSnapshot {
        let cutoff = Instant::now() - VOTE_TTL;
        let g = self.inner.lock();
        let mut counts: HashMap<u32, usize> = HashMap::new();
        let mut active = 0usize;
        for v in g.votes.values() {
            if v.last_seen < cutoff {
                continue;
            }
            active += 1;
            *counts.entry(v.freq_hz).or_insert(0) += 1;
        }
        let mut tallies: Vec<VoteTally> = counts
            .into_iter()
            .map(|(freq_hz, votes)| VoteTally { freq_hz, votes })
            .collect();
        // Most votes first; ties broken by the lowest freq for
        // deterministic behaviour (otherwise HashMap iteration order
        // would cause the winner to flap).
        tallies.sort_by(|a, b| b.votes.cmp(&a.votes).then(a.freq_hz.cmp(&b.freq_hz)));
        let winner_hz = tallies.first().map(|t| t.freq_hz);
        VoteSnapshot {
            winner_hz,
            tallies,
            active_voters: active,
        }
    }

    /// Record that a fingerprint is alive (e.g. opened the audio stream)
    /// without casting a vote, so it still counts as a daily listener.
    pub fn mark_seen(&self, fingerprint: &str) {
        if fingerprint.is_empty() {
            return;
        }
        self.inner
            .lock()
            .seen
            .insert(fingerprint.to_string(), Instant::now());
    }

    /// Distinct fingerprints seen within [`DAILY_WINDOW`]. Prunes older
    /// entries as a side effect so the map can't grow without bound.
    pub fn daily_listeners(&self) -> usize {
        let cutoff = Instant::now() - DAILY_WINDOW;
        let mut g = self.inner.lock();
        g.seen.retain(|_, &mut t| t >= cutoff);
        g.seen.len()
    }

    pub fn winner(&self) -> Option<u32> {
        self.snapshot().winner_hz
    }

    pub fn tallies(&self) -> HashMap<u32, usize> {
        self.snapshot()
            .tallies
            .into_iter()
            .map(|t| (t.freq_hz, t.votes))
            .collect()
    }
}
