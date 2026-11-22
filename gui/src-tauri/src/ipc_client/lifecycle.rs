//! The lifetime of one client's connection, decided in one place.
//!
//! [`Lifecycle::on`](transition::Lifecycle::on) is a pure transition
//! function: it takes the owner's state and one event and returns the next
//! state and the actions the owner carries out. It does no I/O, takes no
//! lock, reads no clock and spawns no task, so every lifetime decision can be
//! checked without a socket.
//!
//! The owner task carries those actions out, and is the only code that
//! connects, builds a [`Generation`], ends one or publishes the client's
//! state. Callers ask it for a connection and use the open generation; a
//! generation's reader alone tells it how that generation ended.

mod connect;
mod generation;
mod owner;
mod reader_link;
mod transition;

pub(crate) use generation::{BoxedRead, Generation, Settled, StepReply, Take};
pub(crate) use owner::Shared;
pub(crate) use reader_link::{Interrupt, ReaderLink};
pub(crate) use transition::{ClientKind, EndKind, Registration, TakenAnswer, Until};
