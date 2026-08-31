//! Consumers of the event stream.
//!
//! One producer, many consumers. Adding a consumer costs nothing, which is why the ledger, the
//! undo journal and both renderers are all just sinks.

use std::sync::{Arc, Mutex};

use super::event::Event;

/// Something that observes events.
///
/// Emission must not block the loop. A sink that needs to do slow work should queue it.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &Event);
}

/// Fans one event out to several sinks in registration order.
#[derive(Default)]
pub struct Broadcast {
    sinks: Vec<Arc<dyn EventSink>>,
}

impl Broadcast {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sinks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }
}

impl EventSink for Broadcast {
    fn emit(&self, event: &Event) {
        for sink in &self.sinks {
            sink.emit(event);
        }
    }
}

impl std::fmt::Debug for Broadcast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broadcast")
            .field("sinks", &self.sinks.len())
            .finish()
    }
}

/// Keeps every event it is given. For tests and for the session summary.
#[derive(Debug, Default)]
pub struct Collector {
    events: Mutex<Vec<Event>>,
}

impl Collector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// # Panics
    /// If another thread panicked while holding the lock.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("collector lock poisoned").clone()
    }

    /// # Panics
    /// If another thread panicked while holding the lock.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("collector lock poisoned").len()
    }

    /// # Panics
    /// If another thread panicked while holding the lock.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events
            .lock()
            .expect("collector lock poisoned")
            .is_empty()
    }
}

impl EventSink for Collector {
    fn emit(&self, event: &Event) {
        self.events
            .lock()
            .expect("collector lock poisoned")
            .push(event.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::TaskId;
    use crate::core::vocab::TaskStatus;

    fn started() -> Event {
        Event::TaskStarted {
            id: TaskId::new(0),
            summary: "test".into(),
        }
    }

    #[test]
    fn collector_keeps_events_in_order() {
        let collector = Collector::new();
        collector.emit(&started());
        collector.emit(&Event::TaskFinished {
            id: TaskId::new(0),
            status: TaskStatus::Completed,
        });

        let events = collector.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], Event::TaskStarted { .. }));
        assert!(matches!(events[1], Event::TaskFinished { .. }));
    }

    #[test]
    fn broadcast_reaches_every_sink() {
        let a = Arc::new(Collector::new());
        let b = Arc::new(Collector::new());
        let bus = Broadcast::new()
            .with(Arc::clone(&a) as Arc<dyn EventSink>)
            .with(Arc::clone(&b) as Arc<dyn EventSink>);

        bus.emit(&started());

        assert_eq!(bus.len(), 2);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn broadcast_with_no_sinks_is_harmless() {
        Broadcast::new().emit(&started());
    }

    #[test]
    fn collector_is_usable_from_several_threads() {
        let collector = Arc::new(Collector::new());
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..250 {
                        collector.emit(&started());
                    }
                });
            }
        });
        assert_eq!(collector.len(), 1000);
    }
}
