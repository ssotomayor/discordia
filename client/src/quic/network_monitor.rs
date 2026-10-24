use std::future::Future;

pub(super) struct NetworkMonitor(tokio::task::JoinHandle<()>);

impl NetworkMonitor {
    pub(super) fn start<F, Fut>(mut changes: tokio::sync::watch::Receiver<u64>, refresh: F) -> Self
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        // Android callbacks must keep reaching iroh while the WebView stops rendering.
        Self(tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                refresh().await;
            }
        }))
    }
}

impl Drop for NetworkMonitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn refreshes_without_ui_polling_and_coalesces_pending_changes() {
        let (changes, receiver) = tokio::sync::watch::channel(0);
        let refreshed = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());
        let _monitor = NetworkMonitor::start(receiver, {
            let refreshed = refreshed.clone();
            let notify = notify.clone();
            move || {
                refreshed.fetch_add(1, Ordering::SeqCst);
                notify.notify_one();
                std::future::ready(())
            }
        });
        changes.send_replace(1);
        changes.send_replace(2);
        tokio::time::timeout(std::time::Duration::from_secs(1), notify.notified())
            .await
            .expect("native task does not require a VirtualDom poll");
        assert_eq!(refreshed.load(Ordering::SeqCst), 1);
        changes.send_replace(3);
        tokio::time::timeout(std::time::Duration::from_secs(1), notify.notified())
            .await
            .expect("subsequent route changes refresh again");
        assert_eq!(refreshed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropping_the_connection_cancels_an_inflight_refresh() {
        let (changes, receiver) = tokio::sync::watch::channel(0);
        let started = Arc::new(tokio::sync::Notify::new());
        let monitor = NetworkMonitor::start(receiver, {
            let started = started.clone();
            move || {
                started.notify_one();
                std::future::pending::<()>()
            }
        });
        changes.send_replace(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("refresh started");
        drop(monitor);
        tokio::time::timeout(std::time::Duration::from_secs(1), changes.closed())
            .await
            .expect("the worker releases its endpoint and subscription");
    }
}
