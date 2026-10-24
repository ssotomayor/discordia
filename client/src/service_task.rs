pub fn spawn(future: impl std::future::Future<Output = ()> + 'static) -> dioxus::core::Task {
    let task = dioxus::prelude::spawn(future);
    #[cfg(target_os = "android")]
    crate::platform::window().keep_alive_task(task);
    task
}
