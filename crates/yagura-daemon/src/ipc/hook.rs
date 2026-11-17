//! The hook event door: a CLI's hook names a profile and an event, and the
//! daemon does what that profile's `[hook_events]` says for it.
//!
//! The table is looked up in the named profile on every request, not in the
//! window's own profile, so a hook written into a CLI's settings keeps working
//! after the profile changes and for windows opened without that profile. The
//! request carries no state between its two round trips: the second one is
//! answered from a fresh read. The time a door is named is the window's own
//! ([`WindowEntry::hook_timeouts`]): its CLI was given it when it started.

use std::sync::Arc;

use yagura_protocol::{
    DaemonMessage, Door, HookInputValue, HookPointer, HookPointerKind, WindowStatus,
};

use crate::inbox::{DeliverySpec, DoorRead, silence};
use crate::profile::{self, ListedProfileFile, validate_profile_id};
use crate::profile_hook_events::{
    HookDeliver, HookEffect, HookEvent, HookOrigin, is_hook_event_name,
};
use crate::profile_interrupt::{AiMeans, HookStops, ProfileInterrupt, TurnRole};
use crate::window::WindowEntry;

use super::inbox::{HeldSlot, StopEnd, answer_read, answer_read_ending};
use super::{HookDoor, IpcServer, Outbound, error_msg};

/// What a profile says about one event: the event, how its CLI marks a
/// subagent's hook, and how it wants the text a hook prints.
struct Resolved {
    event: HookEvent,
    subagent: Option<String>,
    delivery: Option<DeliverySpec>,
    /// The events of the profile's `[interrupt]` hook form, when it has one.
    stops: Option<HookStops>,
}

/// How a hook event's read ends.
pub(super) enum HookRead<'a> {
    /// A door that reads (from [`yagura_protocol::DOOR_READ_MIN_REVISION`]):
    /// what the read takes stays in the connection's slot for the door's
    /// `deliver_printed`.
    Door(&'a HeldSlot),
    /// A `run_hook` of a door that prints after it took: nothing is taken,
    /// and the event's status and session are still recorded.
    Older,
}

/// The values `event` reads from a hook's input, in the order they are asked
/// for and answered: the profile's `subagent` pointer first, on every event.
fn wanted_pointers(subagent: Option<&str>, event: &HookEvent) -> Vec<HookPointer> {
    let subagent = subagent.into_iter().map(|pointer| HookPointer {
        pointer: pointer.to_string(),
        kind: HookPointerKind::Presence,
        value: None,
    });
    let session = event.session.iter().map(|pointer| HookPointer {
        pointer: pointer.clone(),
        kind: HookPointerKind::Session,
        value: None,
    });
    let when = event.when.iter().map(|(pointer, value)| HookPointer {
        pointer: pointer.clone(),
        kind: HookPointerKind::Equals,
        value: Some(value.clone()),
    });
    subagent.chain(session).chain(when).collect()
}

fn warn_status_not_recorded(entry: &WindowEntry, profile_id: &str, name: &str) {
    tracing::warn!(
        window_id = %entry.window_id(),
        profile = profile_id,
        event = name,
        "run_hook could not record the event's status; continuing"
    );
}

/// One `run_hook` request as it was read.
pub(super) struct HookRequest<'a> {
    pub(super) window_id: &'a str,
    pub(super) token: &'a str,
    pub(super) profile_id: &'a str,
    pub(super) name: &'a str,
    pub(super) input: Option<Vec<HookInputValue>>,
    /// The process id of the connection's peer.
    pub(super) peer: Option<u32>,
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
            peer,
        } = request;
        let Some(entry) = self
            .authenticate_window(
                outbound,
                HookDoor::new("run_hook", peer),
                token,
                Some(window_id),
            )
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
        if entry.named_profile().is_some_and(|own| own != profile_id) {
            // A nested CLI runs its hooks every turn, so this is no warning.
            tracing::debug!(
                window_id = %entry.window_id(),
                profile = profile_id,
                event = name,
                "run_hook names another profile than its window's; nothing was done"
            );
            finish(DoorRead::silent(), None);
            return;
        }
        let Some(Resolved {
            event,
            subagent,
            delivery,
            stops,
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

        let hook_timeout_sec = entry
            .hook_timeouts()
            .and_then(|timeouts| timeouts.for_event(name));
        let pointers = wanted_pointers(subagent.as_deref(), &event);
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

        let origin = HookOrigin::from_subagent_value(
            subagent.is_some() && !matches!(values.next(), Some(HookInputValue::Absent)),
        );
        if event.session.is_some()
            && let Some(HookInputValue::Session(session)) = values.next()
            && HookEffect::Session.applies_to(origin)
        {
            entry.record_session_id(&session);
        }
        // A main hook moves the turn before anything is answered, so a stop
        // is judged against the turn this hook belongs to.
        let role = stops
            .as_ref()
            .map_or(TurnRole::Other, |stops| stops.role(name));
        if stops.is_some() && HookEffect::Turn.applies_to(origin) {
            entry.turn().main_hook(role);
        }
        // What is left answers `when`, one value per condition.
        let met = values.all(|value| value == HookInputValue::Equal(true));
        let spec = delivery
            .unwrap_or_else(|| entry.inbox().spec().clone())
            .for_event(name);
        let reads =
            matches!(read, HookRead::Door(_)) && met && HookEffect::Deliver.applies_to(origin);
        let records_status = event
            .status
            .filter(|&status| HookEffect::Status(status).applies_to(origin));

        // A stop leaves the inbox alone: what waits there is the next door's.
        if let TurnRole::Stop(answer) = role
            && HookEffect::Stop.applies_to(origin)
            && entry.turn().stops_now()
        {
            if let Some(status) = records_status {
                self.record_hook_status(&entry, status, profile_id, name);
            }
            if answer.ends_turn() && HookEffect::Turn.applies_to(origin) {
                entry.turn().stop_ended_turn();
            }
            finish(DoorRead::printing(answer.render()), hook_timeout_sec);
            return;
        }
        if event.deliver == Some(HookDeliver::Block) {
            let block = reads.then(|| entry.inbox().read_block(Some(&spec)));
            if let (Some(block), HookRead::Door(slot)) = (block, &read)
                && block.prints()
            {
                // The block makes the CLI go on with it: a turn of its own.
                if stops.is_some() {
                    entry.turn().block_started();
                }
                let end = event
                    .status
                    .and_then(|status| self.stop_end(&entry, status, profile_id, name));
                answer_read_ending(&entry, outbound, block, slot, hook_timeout_sec, end);
                return;
            }
        }
        if let Some(status) = records_status {
            self.record_hook_status(&entry, status, profile_id, name);
        }
        let taken = match event.deliver {
            Some(HookDeliver::Context) if reads => entry
                .inbox()
                .read_context(self.chat.intro_for(&entry), Some(&spec)),
            Some(HookDeliver::Take) if reads => entry.inbox().read_take(Some(&spec)),
            _ => DoorRead::silent(),
        };
        finish(taken, hook_timeout_sec);
    }

    /// Records a hook event's `status` on `entry`.
    fn record_hook_status(
        &self,
        entry: &WindowEntry,
        status: WindowStatus,
        profile_id: &str,
        name: &str,
    ) {
        if self.apply_status_event(entry, None, status).is_some() {
            warn_status_not_recorded(entry, profile_id, name);
        }
    }

    /// Where a stop's block decides `entry`'s status, charged now as the one
    /// status event the stop reports; `None` when the budget is spent.
    fn stop_end(
        &self,
        entry: &Arc<WindowEntry>,
        status: WindowStatus,
        profile_id: &str,
        name: &str,
    ) -> Option<StopEnd> {
        if self.charge_status_event_budget() {
            return Some(StopEnd::new(entry, status));
        }
        warn_status_not_recorded(entry, profile_id, name);
        None
    }

    /// The time `entry`'s CLI gives the hook `door` reads for, in whole
    /// seconds, as the window's open kept it; `None` when the window keeps no
    /// time for the door.
    pub(super) fn window_hook_timeout(entry: &WindowEntry, door: &Door) -> Option<u64> {
        entry.hook_timeouts()?.for_door(door)
    }

    /// The `[interrupt]` table of the profile `entry` was launched from.
    /// `None` when the window was opened without a profile, or the profile
    /// cannot be read or has no such table.
    pub(super) async fn window_interrupt(&self, entry: &WindowEntry) -> Option<ProfileInterrupt> {
        let profile_id = entry.launch_profile()?;
        self.read_hook_profile(profile_id, |listed| listed.profile.interrupt.clone())
            .await
    }

    /// Reads `profile_id` and picks `name` out of its `[hook_events]`. `None`
    /// when the profile cannot be read or has no such event.
    async fn resolve_hook_event(&self, profile_id: &str, name: &str) -> Option<Resolved> {
        let name = name.to_string();
        self.read_hook_profile(profile_id, move |listed| {
            let events = listed.profile.hook_events.as_ref()?;
            let event = events.get(&name)?.clone();
            let subagent = events.subagent_pointer().map(str::to_string);
            let delivery = listed.profile.delivery.as_ref().map(|d| d.spec());
            let stops = match listed.profile.interrupt.as_ref().map(|i| &i.ai) {
                Some(AiMeans::Hook(stops)) => Some(stops.clone()),
                _ => None,
            };
            Some(Resolved {
                event,
                subagent,
                delivery,
                stops,
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
