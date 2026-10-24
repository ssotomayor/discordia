use dioxus_core::{ScopeId, Task, VirtualDom};
use std::{cell::RefCell, task::Waker, time::Duration};

#[derive(Default)]
pub(crate) struct ServiceTasks {
    tasks: RefCell<Vec<Task>>,
    wake: RefCell<Option<tokio::task::JoinHandle<()>>>,
}

impl ServiceTasks {
    pub(crate) fn insert(&self, task: Task) {
        let mut tasks = self.tasks.borrow_mut();
        if !tasks.contains(&task) {
            tasks.push(task);
        }
    }

    pub(crate) fn poll_pending(&self, dom: &mut VirtualDom, waker: &Waker) {
        if self.tasks.borrow().is_empty() {
            return;
        }
        // Queue scheduler notifications without running effects against unapplied DOM edits.
        dom.mark_dirty(ScopeId::ROOT);
        dom.process_events();
        let mut tasks = std::mem::take(&mut *self.tasks.borrow_mut());
        dom.in_runtime(|| tasks.retain(|task| task.poll_now().is_pending()));
        self.tasks.borrow_mut().extend(tasks);
        dom.process_events();
        let mut wake = self.wake.borrow_mut();
        if !self.tasks.borrow().is_empty() && wake.as_ref().is_none_or(|task| task.is_finished()) {
            let waker = waker.clone();
            *wake = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                waker.wake();
            }));
        }
    }

    pub(crate) fn stop_polling(&self) {
        if let Some(task) = self.wake.borrow_mut().take() {
            task.abort();
        }
    }
}

impl Drop for ServiceTasks {
    fn drop(&mut self) {
        self.stop_polling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus_core::{spawn, Element, VNode};
    use std::{cell::Cell, rc::Rc};

    fn app() -> Element {
        VNode::empty()
    }

    fn app_with_effect() -> Element {
        let effects = dioxus_core::consume_context::<Rc<Cell<usize>>>();
        dioxus_hooks::use_effect(move || effects.set(effects.get() + 1));
        VNode::empty()
    }

    #[tokio::test]
    async fn pending_dom_effects_are_not_run_by_the_service_poller() {
        let effects = Rc::new(Cell::new(0_usize));
        let mut dom = VirtualDom::new(app_with_effect);
        dom.provide_root_context(effects.clone());
        dom.rebuild_in_place();
        let services = ServiceTasks::default();
        let completed = Rc::new(Cell::new(false));
        let task = dom.in_scope(ScopeId::ROOT, || {
            spawn({
                let completed = completed.clone();
                async move {
                    completed.set(true);
                }
            })
        });
        services.insert(task);
        services.poll_pending(&mut dom, &futures_util::task::noop_waker());
        assert!(completed.get());
        assert_eq!(
            effects.get(),
            0,
            "effects must still wait for the DOM acknowledgement"
        );
    }

    #[tokio::test]
    async fn services_drain_messages_while_dom_acknowledgements_remain_pending() {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let services = ServiceTasks::default();
        let count = Rc::new(Cell::new(0));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = dom.in_scope(ScopeId::ROOT, || {
            spawn({
                let count = count.clone();
                async move {
                    while rx.recv().await.is_some() {
                        count.set(count.get() + 1);
                    }
                }
            })
        });
        services.insert(task);
        services.insert(task);
        let waker = futures_util::task::noop_waker();
        for _ in 0..1000 {
            tx.send(()).unwrap();
        }
        for _ in 0..20 {
            services.poll_pending(&mut dom, &waker);
            tokio::task::yield_now().await;
        }
        assert_eq!(count.get(), 1000);
        assert_eq!(services.tasks.borrow().len(), 1);
        drop(tx);
        services.poll_pending(&mut dom, &waker);
        assert!(services.tasks.borrow().is_empty());
        services.stop_polling();
        assert!(services.wake.borrow().is_none());
    }

    #[tokio::test]
    async fn cancelled_scoped_services_are_removed_without_polling_their_future() {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let services = ServiceTasks::default();
        let task = dom.in_scope(ScopeId::ROOT, || spawn(std::future::pending()));
        services.insert(task);
        dom.in_runtime(|| task.cancel());
        services.poll_pending(&mut dom, &futures_util::task::noop_waker());
        assert!(services.tasks.borrow().is_empty());
    }
}
