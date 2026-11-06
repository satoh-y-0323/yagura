//! The one process-wide `tracing` default of this test binary.
//!
//! `#[traced_test]` installs a global default the first time one of its tests runs, and
//! panics if another one is already set. Every other global install here therefore goes
//! through the same `Once`: whichever test comes first installs one subscriber that
//! writes the lines `#[traced_test]` reads and also keeps every event's fields for
//! [`captured_events`].

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::{Context, SubscriberExt as _};

/// One captured `tracing` event: its level, and every recorded field as a string. A
/// `&str`/`String` field recorded through `record_debug` comes back in `Debug` quotes;
/// they are stripped so a `%field` and a plain string field compare the same way.
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    pub level: tracing::Level,
    pub fields: HashMap<String, String>,
}

impl CapturedEvent {
    pub fn message(&self) -> Option<&str> {
        self.fields.get("message").map(String::as_str)
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: HashMap<String, String>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        let normalized = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .map(str::to_string)
            .unwrap_or(text);
        self.fields.insert(field.name().to_string(), normalized);
    }
}

fn events() -> &'static Mutex<Vec<CapturedEvent>> {
    static EVENTS: OnceLock<Mutex<Vec<CapturedEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

struct Capture;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        events()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(CapturedEvent {
                level: *event.metadata().level(),
                fields: visitor.fields,
            });
    }
}

/// Installs this binary's global default unless a test already has. Always enabled
/// and global rather than thread-scoped: `tracing`'s per-callsite interest cache is
/// process-wide, so a scoped subscriber can lose to a thread that touched a call site
/// first with nothing installed, and events fired from spawned tasks must be seen.
pub fn install() {
    tracing_test::internal::INITIALIZED.call_once(|| {
        let lines = tracing_subscriber::fmt::layer()
            .with_writer(tracing_test::internal::MockWriter::new(
                tracing_test::internal::global_buf(),
            ))
            .with_level(true)
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
        let subscriber = tracing_subscriber::registry().with(lines).with(Capture);
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other global tracing default may be set in this test binary");
    });
}

/// Every event captured since [`install`] first ran, in emission order.
pub fn captured_events() -> Vec<CapturedEvent> {
    install();
    events()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}
