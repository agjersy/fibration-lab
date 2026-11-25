//! Per-session event fan-out (IPC.md sections 3 and 7).
//!
//! Control events (everything except terminal output) travel on one bounded broadcast. A
//! subscriber that falls behind does not silently lose them: its next event is
//! `SessionEvent::Resync`, and the renderer refetches its state.
//!
//! Terminal output has its own path, so a flood from a shell cannot displace `proposal`,
//! `turn_done`, or `run_done` (U1). Every subscriber owns a queue in which the pending chunks
//! of one terminal are merged into a single string, capped at [`TERMINAL_PENDING_CAP`] bytes
//! (the oldest output is dropped and a notice is inserted). `terminal_exit` travels on the same
//! path so it can never overtake the output before it.

use fibration_protocol::ui::{SessionEvent, TerminalId};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::Notify;

/// Capacity of the control broadcast.
pub const CONTROL_CAPACITY: usize = 8192;
/// Pending output kept per terminal and subscriber before the oldest part is dropped.
pub const TERMINAL_PENDING_CAP: usize = 1 << 20;

const DROPPED_NOTICE: &str = "\r\n[... terminal output dropped ...]\r\n";

#[derive(Default)]
struct TermPending {
    data: String,
    dropped: bool,
    /// `Some(code)` once the terminal exited.
    exit: Option<Option<i64>>,
}

#[derive(Default)]
struct TermState {
    order: VecDeque<TerminalId>,
    pending: HashMap<TerminalId, TermPending>,
}

#[derive(Default)]
struct TermQueue {
    state: Mutex<TermState>,
    notify: Notify,
}

impl TermQueue {
    fn with<R>(&self, f: impl FnOnce(&mut TermState) -> R) -> R {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }

    fn push(&self, id: &str, data: Option<&str>, exit: Option<Option<i64>>) {
        self.with(|s| {
            if !s.pending.contains_key(id) {
                s.order.push_back(id.to_string());
            }
            let p = s.pending.entry(id.to_string()).or_default();
            if let Some(d) = data {
                p.data.push_str(d);
                if p.data.len() > TERMINAL_PENDING_CAP {
                    let mut start = p.data.len() - TERMINAL_PENDING_CAP / 2;
                    while start < p.data.len() && !p.data.is_char_boundary(start) {
                        start += 1;
                    }
                    p.data.drain(..start);
                    p.dropped = true;
                }
            }
            if exit.is_some() {
                p.exit = exit;
            }
        });
        self.notify.notify_one();
    }

    /// The merged pending output (and exit) of the terminal that has waited longest.
    fn take_one(&self) -> Vec<SessionEvent> {
        self.with(|s| {
            let Some(id) = s.order.pop_front() else {
                return vec![];
            };
            let Some(p) = s.pending.remove(&id) else {
                return vec![];
            };
            let mut out = Vec::new();
            if !p.data.is_empty() {
                let data = if p.dropped {
                    format!("{DROPPED_NOTICE}{}", p.data)
                } else {
                    p.data
                };
                out.push(SessionEvent::TerminalData {
                    terminal_id: id.clone(),
                    data,
                });
            }
            if let Some(code) = p.exit {
                out.push(SessionEvent::TerminalExit {
                    terminal_id: id,
                    code,
                });
            }
            out
        })
    }

    fn pending_bytes(&self) -> usize {
        self.with(|s| s.pending.values().map(|p| p.data.len()).sum())
    }
}

struct HubInner {
    control: broadcast::Sender<SessionEvent>,
    queues: Mutex<Vec<Weak<TermQueue>>>,
}

/// The event hub of one session. Cheap to clone.
#[derive(Clone)]
pub struct EventHub {
    inner: Arc<HubInner>,
}

impl Default for EventHub {
    fn default() -> Self {
        EventHub::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        EventHub::with_capacity(CONTROL_CAPACITY)
    }

    /// A hub whose control broadcast holds `capacity` events (tests use a small one).
    pub fn with_capacity(capacity: usize) -> Self {
        let (control, _) = broadcast::channel(capacity.max(1));
        EventHub {
            inner: Arc::new(HubInner {
                control,
                queues: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The control broadcast, for producers that only need to send (task loggers, agents).
    pub fn sender(&self) -> broadcast::Sender<SessionEvent> {
        self.inner.control.clone()
    }

    /// Send one event. Terminal output and exit are routed to the terminal path.
    pub fn emit(&self, e: SessionEvent) {
        match e {
            SessionEvent::TerminalData { terminal_id, data } => {
                self.terminal_data(&terminal_id, &data)
            }
            SessionEvent::TerminalExit { terminal_id, code } => {
                self.terminal_exit(&terminal_id, code)
            }
            other => {
                let _ = self.inner.control.send(other);
            }
        }
    }

    fn queues(&self) -> Vec<Arc<TermQueue>> {
        let mut g = self.inner.queues.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|w| w.strong_count() > 0);
        g.iter().filter_map(Weak::upgrade).collect()
    }

    pub fn terminal_data(&self, id: &str, data: &str) {
        if data.is_empty() {
            return;
        }
        for q in self.queues() {
            q.push(id, Some(data), None);
        }
    }

    pub fn terminal_exit(&self, id: &str, code: Option<i64>) {
        for q in self.queues() {
            q.push(id, None, Some(code));
        }
    }

    pub fn subscribe(&self) -> Subscription {
        let queue = Arc::new(TermQueue::default());
        self.inner
            .queues
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::downgrade(&queue));
        Subscription {
            control: self.inner.control.subscribe(),
            queue,
            spill: VecDeque::new(),
        }
    }
}

/// One subscriber: control events in order, `Resync` after a gap, merged terminal output.
pub struct Subscription {
    control: broadcast::Receiver<SessionEvent>,
    queue: Arc<TermQueue>,
    spill: VecDeque<SessionEvent>,
}

impl Subscription {
    /// The next event. Control events come first; a lagged control stream yields
    /// `SessionEvent::Resync` (never `RecvError::Lagged`). `Err(Closed)` when the session's
    /// hub is gone. Cancel-safe.
    pub async fn recv(&mut self) -> Result<SessionEvent, RecvError> {
        loop {
            if let Some(e) = self.spill.pop_front() {
                return Ok(e);
            }
            match self.control.try_recv() {
                Ok(e) => return Ok(e),
                Err(TryRecvError::Lagged(_)) => return Ok(SessionEvent::Resync),
                Err(TryRecvError::Closed) => return Err(RecvError::Closed),
                Err(TryRecvError::Empty) => {}
            }
            self.spill.extend(self.queue.take_one());
            if !self.spill.is_empty() {
                continue;
            }
            tokio::select! {
                biased;
                r = self.control.recv() => match r {
                    Ok(e) => return Ok(e),
                    Err(RecvError::Lagged(_)) => return Ok(SessionEvent::Resync),
                    Err(RecvError::Closed) => return Err(RecvError::Closed),
                },
                _ = self.queue.notify.notified() => {}
            }
        }
    }

    /// Bytes of terminal output waiting in this subscriber's queue.
    pub fn pending_terminal_bytes(&self) -> usize {
        self.queue.pending_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lagged_subscriber_gets_resync_and_terminal_output_cannot_displace_control() {
        let hub = EventHub::with_capacity(16);
        let mut sub = hub.subscribe();
        // A terminal flood far larger than the control capacity.
        for i in 0..5000 {
            hub.emit(SessionEvent::TerminalData {
                terminal_id: "t1".into(),
                data: format!("line {i}\r\n"),
            });
        }
        hub.emit(SessionEvent::IndexChanged);
        hub.terminal_exit("t1", Some(0));
        // The control event was not displaced and comes first.
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::IndexChanged
        ));
        // The flood arrives as one merged chunk, then the exit.
        match sub.recv().await.unwrap() {
            SessionEvent::TerminalData { terminal_id, data } => {
                assert_eq!(terminal_id, "t1");
                assert!(data.starts_with("line 0\r\n"));
                assert!(data.ends_with("line 4999\r\n"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::TerminalExit { code: Some(0), .. }
        ));
        // Overflowing the control broadcast yields exactly one resync, then the newest events.
        for _ in 0..100 {
            hub.emit(SessionEvent::PackagesChanged);
        }
        assert!(matches!(sub.recv().await.unwrap(), SessionEvent::Resync));
        assert!(matches!(
            sub.recv().await.unwrap(),
            SessionEvent::PackagesChanged
        ));
    }

    #[tokio::test]
    async fn terminal_backlog_is_capped() {
        let hub = EventHub::new();
        let mut sub = hub.subscribe();
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..64 {
            hub.terminal_data("t", &chunk);
        }
        assert!(sub.pending_terminal_bytes() <= TERMINAL_PENDING_CAP);
        match sub.recv().await.unwrap() {
            SessionEvent::TerminalData { data, .. } => {
                assert!(data.starts_with(DROPPED_NOTICE));
                assert!(data.len() <= TERMINAL_PENDING_CAP + DROPPED_NOTICE.len());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(sub.pending_terminal_bytes(), 0);
    }
}
