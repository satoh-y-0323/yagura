//! The `--shutdown` CLI's decision of whether to send `shutdown` and whether
//! to wait for the daemon's end, apart from the connection and the operating
//! system that carry it out.

use std::future::Future;

use anyhow::{Context, Result};

const UNCONFIRMED: &str =
    "the shutdown message was sent, but the daemon's end could not be confirmed";

/// Given `watch`, the watch of the daemon's end taken before anything is
/// sent, sends `shutdown` with `send` and waits for the end with `wait`. A
/// daemon that no longer runs (`Ok(None)`) is neither sent to nor waited for.
///
/// # Errors
///
/// Fails without waiting when `send` fails; fails after sending when `watch`
/// failed, and when `wait` fails. A daemon whose end is not confirmed is never
/// reported as stopped.
pub(crate) async fn shut_down_and_confirm<W, S, SF, F, FF>(
    watch: Result<Option<W>>,
    send: S,
    wait: F,
) -> Result<()>
where
    S: FnOnce() -> SF,
    SF: Future<Output = Result<()>>,
    F: FnOnce(W) -> FF,
    FF: Future<Output = Result<()>>,
{
    if matches!(watch, Ok(None)) {
        return Ok(());
    }
    send()
        .await
        .context("failed to send the shutdown message")?;
    let Some(watch) = watch.context(UNCONFIRMED)? else {
        return Ok(());
    };
    wait(watch).await.context(UNCONFIRMED)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use anyhow::{Result, anyhow};

    use super::{UNCONFIRMED, shut_down_and_confirm};

    /// The watch handed to the fake `wait`, so a test sees it arrive there.
    struct FakeWatch(u32);

    struct Calls {
        sent: Cell<bool>,
        waited_on: Cell<Option<u32>>,
    }

    impl Calls {
        fn new() -> Self {
            Self {
                sent: Cell::new(false),
                waited_on: Cell::new(None),
            }
        }
    }

    async fn run(
        calls: &Calls,
        watch: Result<Option<FakeWatch>>,
        send: Result<()>,
        wait: Result<()>,
    ) -> Result<()> {
        shut_down_and_confirm(
            watch,
            || async {
                calls.sent.set(true);
                send
            },
            |watch: FakeWatch| async move {
                calls.waited_on.set(Some(watch.0));
                wait
            },
        )
        .await
    }

    fn chain(error: &anyhow::Error) -> String {
        format!("{error:#}")
    }

    #[tokio::test]
    async fn a_daemon_already_gone_is_neither_sent_to_nor_waited_for() {
        let calls = Calls::new();
        let result = run(&calls, Ok(None), Ok(()), Ok(())).await;
        assert!(result.is_ok(), "{result:?}");
        assert!(!calls.sent.get());
        assert_eq!(calls.waited_on.get(), None);
    }

    #[tokio::test]
    async fn a_failed_watch_still_sends_but_fails_with_the_reason_without_waiting() {
        let calls = Calls::new();
        let result = run(
            &calls,
            Err(anyhow!("cannot open the process")),
            Ok(()),
            Ok(()),
        )
        .await;
        let error = result.expect_err("an unwatched end must not be reported as stopped");
        assert!(chain(&error).contains(UNCONFIRMED), "{error:#}");
        assert!(
            chain(&error).contains("cannot open the process"),
            "{error:#}"
        );
        assert!(calls.sent.get());
        assert_eq!(calls.waited_on.get(), None);
    }

    #[tokio::test]
    async fn a_failed_send_fails_without_waiting() {
        let calls = Calls::new();
        let result = run(
            &calls,
            Ok(Some(FakeWatch(7))),
            Err(anyhow!("broken pipe")),
            Ok(()),
        )
        .await;
        let error = result.expect_err("an unsent shutdown must fail");
        assert!(
            chain(&error).contains("failed to send the shutdown message"),
            "{error:#}"
        );
        assert!(calls.sent.get());
        assert_eq!(calls.waited_on.get(), None);
    }

    #[tokio::test]
    async fn a_failed_wait_fails_with_the_reason() {
        let calls = Calls::new();
        let result = run(
            &calls,
            Ok(Some(FakeWatch(7))),
            Ok(()),
            Err(anyhow!("wait failed")),
        )
        .await;
        let error = result.expect_err("an unconfirmed end must not be reported as stopped");
        assert!(chain(&error).contains(UNCONFIRMED), "{error:#}");
        assert!(chain(&error).contains("wait failed"), "{error:#}");
        assert!(calls.sent.get());
        assert_eq!(calls.waited_on.get(), Some(7));
    }

    #[tokio::test]
    async fn a_confirmed_end_succeeds_after_sending_and_waiting() {
        let calls = Calls::new();
        let result = run(&calls, Ok(Some(FakeWatch(7))), Ok(()), Ok(())).await;
        assert!(result.is_ok(), "{result:?}");
        assert!(calls.sent.get());
        assert_eq!(calls.waited_on.get(), Some(7));
    }
}
