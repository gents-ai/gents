use std::future::Future;

use tokio_util::sync::CancellationToken;

/// Stops a login whose wait is dropped: its callback task would otherwise
/// keep running and hold the callback port.
struct StopOnDrop<S>(Option<S>)
where
    S: FnOnce();

impl<S> Drop for StopOnDrop<S>
where
    S: FnOnce(),
{
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            stop();
        }
    }
}

/// Stopping may overlap a token exchange the provider already accepted.
/// After `stop`, drain that exchange, which each caller bounds with a request
/// timeout, and keep any tokens it issued; dropping `login` would lose them.
pub(crate) async fn until_cancelled<F, T, E, S>(
    login: F,
    cancel: &CancellationToken,
    stop: S,
) -> anyhow::Result<T>
where
    F: Future<Output = Result<T, E>>,
    E: Into<anyhow::Error>,
    S: FnOnce(),
{
    let mut stop = StopOnDrop(Some(stop));
    tokio::pin!(login);
    tokio::select! {
        result = &mut login => return result.map_err(Into::into),
        () = cancel.cancelled() => {}
    }
    if let Some(stop) = stop.0.take() {
        stop();
    }
    login
        .await
        .map_err(|_| anyhow::anyhow!("Sign-in cancelled."))
}

pub(crate) fn print_sign_in_notes(sign_in: &gents::inference::auth::credential::SignIn) {
    if let Some(hint) = sign_in.account_chooser_hint() {
        eprintln!("{hint}");
    }
    if let Some(note) = sign_in.profiles_note() {
        eprintln!("{note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_stops_the_login_and_says_so() {
        let cancel = CancellationToken::new();
        let stopped = CancellationToken::new();
        let mut stops = 0;
        cancel.cancel();
        let login = async {
            stopped.cancelled().await;
            Err::<(), _>(anyhow::anyhow!("stopped"))
        };
        let error = until_cancelled(login, &cancel, || {
            stops += 1;
            stopped.cancel();
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Sign-in cancelled.");
        assert_eq!(stops, 1);
    }

    #[tokio::test]
    async fn tokens_issued_while_stopping_are_kept() {
        let cancel = CancellationToken::new();
        let stopped = CancellationToken::new();
        cancel.cancel();
        let login = async {
            stopped.cancelled().await;
            Ok::<_, anyhow::Error>(7)
        };
        let tokens = until_cancelled(login, &cancel, || stopped.cancel()).await;
        assert_eq!(tokens.unwrap(), 7);
    }

    #[tokio::test]
    async fn dropping_the_wait_stops_the_login_once() {
        let cancel = CancellationToken::new();
        let mut stops = 0;
        let wait = until_cancelled(
            std::future::pending::<anyhow::Result<()>>(),
            &cancel,
            || stops += 1,
        );
        let _ = tokio::time::timeout(std::time::Duration::ZERO, wait).await;
        assert_eq!(stops, 1);
    }
}
