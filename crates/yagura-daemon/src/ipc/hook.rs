//! The hook event door: a CLI's hook names a profile and an event, and the
//! daemon does what that profile's `[hook_events]` says for it.
//!
//! The table is looked up in the named profile on every request, not in the
//! window's own profile, so a hook written into a CLI's settings keeps working
//! after the profile changes and for windows opened without that profile. The
//! request carries no state between its two round trips: the second one is
//! answered from a fresh read.

use yagura_protocol::{DaemonMessage, Door, HookInputValue, HookPointer, HookPointerKind};

use crate::inbox::{DeliverySpec, DoorRead, silence};
use crate::profile::{self, ListedProfileFile, validate_profile_id};
use crate::profile_hook_events::{HookDeliver, HookEvent, is_hook_event_name};
use crate::window::WindowEntry;

use super::inbox::{HeldSlot, answer_read};
use super::{IpcServer, Outbound, error_msg};

/// What a profile says about one event: the event, how that profile's CLI
/// wants the text a hook prints, and the time its CLI gives the hook.
struct Resolved {
    event: HookEvent,
    delivery: Option<DeliverySpec>,
    hook_timeout_sec: Option<u64>,
}

/// How a hook event's read ends.
pub(super) enum HookRead<'a> {
    /// A door of revision 34: what the read takes stays in the connection's
    /// slot for the door's `deliver_printed`.
    Door(&'a HeldSlot),
    /// A `run_hook` of a door before revision 34, which prints after it took:
    /// nothing is taken, and the event's status and session are still recorded.
    Older,
}

/// The values `event` reads from a hook's input, in the order they are asked
/// for and answered.
fn wanted_pointers(event: &HookEvent) -> Vec<HookPointer> {
    let skip_if = event.skip_if.iter().map(|pointer| HookPointer {
        pointer: pointer.clone(),
        kind: HookPointerKind::Presence,
    });
    let session = event.session.iter().map(|pointer| HookPointer {
        pointer: pointer.clone(),
        kind: HookPointerKind::Session,
    });
    skip_if.chain(session).collect()
}

/// One `run_hook` request as it was read.
pub(super) struct HookRequest<'a> {
    pub(super) window_id: &'a str,
    pub(super) token: &'a str,
    pub(super) profile_id: &'a str,
    pub(super) name: &'a str,
    pub(super) input: Option<Vec<HookInputValue>>,
}

impl IpcServer {
    /// Answers a hook event's read as `read` says. See
    /// [`crate::profile_hook_events::HookEvent`] for the order the parts of an
    /// event run in.
    pub(super) async fn handle_run_hook(
        &self,
        outbound: &Outbound,
        request: HookRequest<'_>,
        read: HookRead<'_>,
    ) {
        let HookRequest {
            window_id,
            token,
            profile_id,
            name,
            input,
        } = request;
        let Some(entry) = self
            .authenticate_window(outbound, "run_hook", token, Some(window_id))
            .await
        else {
            return;
        };
        let finish = |taken: DoorRead, hook_timeout_sec: Option<u64>| match &read {
            HookRead::Door(slot) => answer_read(&entry, outbound, taken, slot, hook_timeout_sec),
            HookRead::Older => outbound.reply(&silence()),
        };
        if !validate_profile_id(profile_id) || !is_hook_event_name(name) {
            outbound.reply(&error_msg(
                "invalid_argument",
                "run_hook needs a profile id and an event name of the allowed shape",
            ));
            return;
        }
        let Some(Resolved {
            event,
            delivery,
            hook_timeout_sec,
        }) = self.resolve_hook_event(profile_id, name).await
        else {
            tracing::warn!(
                window_id = %entry.window_id(),
                profile = profile_id,
                event = name,
                "run_hook names an event its profile does not define; nothing was done"
            );
            finish(DoorRead::silent(), None);
            return;
        };

        let pointers = wanted_pointers(&event);
        let values = match input {
            None if !pointers.is_empty() => {
                outbound.reply(&DaemonMessage::HookInputWanted { pointers });
                return;
            }
            None => Vec::new(),
            Some(values) if values.len() == pointers.len() => values,
            Some(_) => {
                outbound.reply(&error_msg(
                    "invalid_argument",
                    "run_hook input must answer each requested pointer",
                ));
                return;
            }
        };
        let mut values = values.into_iter();

        if event.skip_if.is_some() && !matches!(values.next(), Some(HookInputValue::Absent)) {
            finish(DoorRead::silent(), hook_timeout_sec);
            return;
        }
        if event.session.is_some()
            && let Some(HookInputValue::Session(session)) = values.next()
        {
            entry.record_session_id(&session);
        }
        if let Some(status) = event.status
            && self.apply_status_event(&entry, None, status).is_some()
        {
            tracing::warn!(
                window_id = %entry.window_id(),
                profile = profile_id,
                event = name,
                "run_hook could not record the event's status; continuing"
            );
        }
        let taken = match (&read, event.deliver) {
            (HookRead::Door(_), Some(HookDeliver::Context)) => entry
                .inbox()
                .read_context(self.chat.intro_for(&entry), delivery.as_ref()),
            (HookRead::Door(_), Some(HookDeliver::Take)) => {
                entry.inbox().read_take(delivery.as_ref())
            }
            _ => DoorRead::silent(),
        };
        finish(taken, hook_timeout_sec);
    }

    /// The time the CLI of `entry`'s profile gives the hook `door` reads for,
    /// in whole seconds; `None` when the window has no profile, the profile
    /// cannot be read, or it gives no time for the door. A hook event's read is
    /// answered from the profile it names instead, in
    /// [`Self::handle_run_hook`].
    pub(super) async fn window_hook_timeout(
        &self,
        entry: &WindowEntry,
        door: &Door,
    ) -> Option<u64> {
        let profile_id = entry.vendor()?;
        let door = door.clone();
        self.read_hook_profile(profile_id, move |listed| {
            listed.profile.hook_timeouts.for_door(&door)
        })
        .await
    }

    /// Reads `profile_id` and picks `name` out of its `[hook_events]`, with the
    /// time its CLI gives that hook. `None` when the profile cannot be read or
    /// has no such event.
    async fn resolve_hook_event(&self, profile_id: &str, name: &str) -> Option<Resolved> {
        let name = name.to_string();
        self.read_hook_profile(profile_id, move |listed| {
            let event = listed.profile.hook_events.as_ref()?.get(&name)?.clone();
            let delivery = listed.profile.delivery.as_ref().map(|d| d.spec());
            let hook_timeout_sec = listed.profile.hook_timeouts.for_event(&name);
            Some(Resolved {
                event,
                delivery,
                hook_timeout_sec,
            })
        })
        .await
    }

    /// Reads `profile_id` and takes what `pick` makes of it. The read is
    /// blocking file I/O, so it runs on a blocking thread under
    /// `hook_profile_reads`; nothing times it out, since a blocking thread
    /// cannot be stopped. `None` when the profile cannot be read or `pick`
    /// finds nothing.
    async fn read_hook_profile<T: Send + 'static>(
        &self,
        profile_id: &str,
        pick: impl FnOnce(&ListedProfileFile) -> Option<T> + Send + 'static,
    ) -> Option<T> {
        let _permit = self.hook_profile_reads.acquire().await.ok()?;
        let config = self.config.clone();
        let profile_id = profile_id.to_string();
        tokio::task::spawn_blocking(move || {
            let listed = profile::read_listed_profile(&config, &profile_id)?;
            pick(&listed)
        })
        .await
        .ok()
        .flatten()
    }
}
