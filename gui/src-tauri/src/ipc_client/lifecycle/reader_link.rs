//! The one way news of a generation's end reaches its owner: only a reader's
//! link, one per generation, can build it.

use tokio::sync::oneshot;

use super::transition::EndKind;

/// A generation's end, as its reader saw it.
#[derive(Debug)]
pub(super) struct EndNews {
    generation: u64,
    kind: EndKind,
}

impl EndNews {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn kind(&self) -> EndKind {
        self.kind
    }
}

/// What stops a reader short of its connection's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interrupt {
    /// The owner ended the generation: there is nothing to tell it.
    Cancelled,
    /// A write failed in a way that does not say the peer closed.
    Stopped,
}

/// Where a reader's link hands the end it saw.
pub(super) type EndSink = Box<dyn FnOnce(EndNews) + Send>;

/// What ties a reader to its generation's owner: what stops it, and the
/// way it reports the end it saw.
pub(crate) struct ReaderLink {
    generation: u64,
    cancelled: oneshot::Receiver<()>,
    stopped: Option<oneshot::Receiver<()>>,
    owner: Option<EndSink>,
}

impl std::fmt::Debug for ReaderLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderLink")
            .field("generation", &self.generation)
            .field("cancelled", &self.cancelled)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl ReaderLink {
    pub(super) fn new(
        generation: u64,
        cancelled: oneshot::Receiver<()>,
        stopped: oneshot::Receiver<()>,
        owner: Option<EndSink>,
    ) -> Self {
        Self {
            generation,
            cancelled,
            stopped: Some(stopped),
            owner,
        }
    }

    /// Resolves once the reader is to stop, the cancellation first. A
    /// stop whose sender is gone is never told, so the reader reads on;
    /// only the cancellation stops it without telling the owner. Safe to
    /// drop and call again until it resolved.
    pub(crate) async fn interrupted(&mut self) -> Interrupt {
        let cancelled = &mut self.cancelled;
        let stopped = &mut self.stopped;
        loop {
            tokio::select! {
                biased;
                _ = &mut *cancelled => return Interrupt::Cancelled,
                told = told(stopped) => {
                    *stopped = None;
                    if told {
                        return Interrupt::Stopped;
                    }
                }
            }
        }
    }

    /// Tells the owner the generation ended as `kind`, unless the owner
    /// ended it first.
    pub(crate) fn report(mut self, kind: EndKind) {
        if !matches!(
            self.cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ) {
            return;
        }
        if let Some(owner) = self.owner.take() {
            owner(EndNews {
                generation: self.generation,
                kind,
            });
        }
    }
}

/// Whether the stop was told; `false` once its sender is gone.
async fn told(stopped: &mut Option<oneshot::Receiver<()>>) -> bool {
    match stopped {
        Some(receiver) => receiver.await.is_ok(),
        None => std::future::pending().await,
    }
}
