use tao::window::WindowId;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2ProcessFailedEventArgs2, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_REASON, COREWEBVIEW2_WEB_ERROR_STATUS,
};
use webview2_com::{NavigationCompletedEventHandler, ProcessFailedEventHandler};
use windows_core::{Interface, BOOL};
use wry::WebViewExtWindows;

pub(crate) fn install(webview: &wry::WebView, window_id: WindowId) {
    // WebView2 invokes these handlers on its owning UI thread and retains them.
    unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(core) => core,
            Err(error) => {
                tracing::warn!(?window_id, %error, "Could not attach WebView2 diagnostics");
                return;
            }
        };
        let process_failed = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                args.ProcessFailedKind(&mut kind)?;
                let mut reason = None;
                let mut exit_code = None;
                if let Ok(details) = args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
                    let mut value = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                    if details.Reason(&mut value).is_ok() {
                        reason = Some(value.0);
                    }
                    let mut value = 0;
                    if details.ExitCode(&mut value).is_ok() {
                        exit_code = Some(value);
                    }
                }
                tracing::error!(
                    ?window_id,
                    kind = kind.0,
                    ?reason,
                    ?exit_code,
                    "WebView2 process failed"
                );
            }
            Ok(())
        }));
        let mut token = 0;
        if let Err(error) = core.add_ProcessFailed(&process_failed, &mut token) {
            tracing::warn!(?window_id, %error, "Could not register WebView2 process diagnostics");
        }
        let navigation = NavigationCompletedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut success = BOOL::default();
                args.IsSuccess(&mut success)?;
                if success.as_bool() {
                    tracing::info!(?window_id, "WebView2 navigation completed");
                } else {
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                    args.WebErrorStatus(&mut status)?;
                    tracing::error!(?window_id, status = status.0, "WebView2 navigation failed");
                }
            }
            Ok(())
        }));
        if let Err(error) = core.add_NavigationCompleted(&navigation, &mut token) {
            tracing::warn!(?window_id, %error, "Could not register WebView2 navigation diagnostics");
        }
    }
}
