//! The `run_hook` door: a CLI's hook names a profile and an event, and the
//! daemon does what that profile's `[hook_events]` says for it.
//!
//! The table is looked up in the named profile on every request, not in the
//! window's own profile, so a hook written into a CLI's settings keeps working
//! after the profile changes and for windows opened without that profile. The
//! request carries no state between its two round trips: the second one is
//! answered from a fresh read.

use yagura_protocol::{DaemonMessage, HookInputValue, HookPointer, HookPointerKind};

use crate::inbox::{DeliverySpec, DoorRead};
use crate::profile::{self, validate_profile_id};
use crate::profile_hook_events::{HookDeliver, HookEvent, is_hook_event_name};

use super::inbox::{HeldSlot, answer_read};
use super::{IpcServer, Outbound, error_msg};

/// What a profile says about one event: the event, and how that profile's CLI
/// wants the text a hook prints.
struct Resolved {
    event: HookEvent,
    delivery: Option<DeliverySpec>,
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
    /// Answers `run_hook`, holding what it takes in `held` for the door's
    /// `deliver_done`, or settling it first when `None`. See
    /// [`crate::profile_hook_events::HookEvent`] for the order the parts of an
    /// event run in.
    pub(super) async fn handle_run_hook(
        &self,
        outbound: &Outbound,
        request: HookRequest<'_>,
        held: Option<&HeldSlot>,
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
        if !validate_profile_id(profile_id) || !is_hook_event_name(name) {
            outbound.reply(&error_msg(
                "invalid_argument",
                "run_hook needs a profile id and an event name of the allowed shape",
            ));
            return;
        }
        let Some(Resolved { event, delivery }) = self.resolve_hook_event(profile_id, name).await
        else {
            tracing::warn!(
                window_id = %entry.window_id(),
                profile = profile_id,
                event = name,
                "run_hook names an event its profile does not define; nothing was done"
            );
            answer_read(&entry, outbound, DoorRead::silent(), held).await;
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
            answer_read(&entry, outbound, DoorRead::silent(), held).await;
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
        let read = match event.deliver {
            Some(HookDeliver::Context) => entry
                .inbox()
                .read_context(self.chat.intro_for(&entry), delivery.as_ref()),
            Some(HookDeliver::Take) => entry.inbox().read_take(delivery.as_ref()),
            None => DoorRead::silent(),
        };
        answer_read(&entry, outbound, read, held).await;
    }

    /// Reads `profile_id` and picks `name` out of its `[hook_events]`. The read
    /// is blocking file I/O, so it runs on a blocking thread under
    /// `hook_profile_reads`; nothing times it out, since a blocking thread
    /// cannot be stopped. `None` when the profile cannot be read or has no such
    /// event.
    async fn resolve_hook_event(&self, profile_id: &str, name: &str) -> Option<Resolved> {
        let _permit = self.hook_profile_reads.acquire().await.ok()?;
        let config = self.config.clone();
        let profile_id = profile_id.to_string();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let listed = profile::read_listed_profile(&config, &profile_id)?;
            let event = listed.profile.hook_events.as_ref()?.get(&name)?.clone();
            let delivery = listed.profile.delivery.as_ref().map(|d| d.spec());
            Some(Resolved { event, delivery })
        })
        .await
        .ok()
        .flatten()
    }
}
