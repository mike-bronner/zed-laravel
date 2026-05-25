//! LSP `$/progress` plumbing for the project-warming pipeline.
//!
//! When the LSP boots into a Laravel project of any meaningful size, the
//! user sees several seconds of silence before find-references / rename
//! and friends start returning useful results. The pattern cache is
//! warming up, but there's no signal in the editor — the LSP just looks
//! frozen. This module fixes that by emitting the standard LSP
//! `window/workDoneProgress/create` request followed by `$/progress`
//! notifications, which Zed (and any other LSP client) renders as a
//! status-bar progress indicator with title + message + filled bar.
//!
//! Lifecycle is start → report* → end, modeled as a single-owner type
//! that ends the progress in `Drop` if you forget. Errors from the
//! client are silently ignored: progress UI is non-essential and we
//! never want a missing client capability to break warming itself.

use std::time::{Duration, Instant};

use lsp_types::notification::Progress as ProgressNotification;
use lsp_types::request::WorkDoneProgressCreate;
use lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};
use tower_lsp::Client;

/// Token identifier for our indexing progress. A constant string is fine —
/// only one indexing progress is in flight at a time per LSP instance.
const PROGRESS_TOKEN: &str = "laravel-lsp/indexing";

/// Minimum interval between `$/progress` report notifications. Faster
/// than this and we'd just be spamming the editor for sub-frame updates
/// the user can't see anyway. Slower and the bar feels jumpy.
const REPORT_THROTTLE: Duration = Duration::from_millis(150);

/// Active progress handle. `report` is throttled so call sites don't
/// need to be careful about update frequency. `end` consumes self; drop
/// without ending also ends (with a fallback message) so a panic in the
/// middle of warming doesn't leave a stale progress bar.
pub struct IndexingProgress {
    client: Client,
    /// Set to false after `end` runs so `Drop` doesn't double-end.
    active: bool,
    last_report: Instant,
}

impl IndexingProgress {
    /// Create the progress token on the client and emit the `Begin`
    /// notification with both the persistent `title` and an initial
    /// `message`. Returns `None` if the client doesn't honour the create
    /// request — in that case the caller proceeds without UI.
    ///
    /// Passing the initial message into `Begin` (rather than firing a
    /// separate `Report` immediately after) matters: there's a real
    /// observable gap between `Begin` and the first follow-up report
    /// because intervening work (actor calls, config lookups) runs
    /// between them. Without an initial message, the status-bar entry
    /// shows just the title (e.g. "Laravel") for that gap, which looks
    /// like the LSP is stuck.
    pub async fn begin(
        client: Client,
        title: impl Into<String>,
        initial_message: impl Into<String>,
    ) -> Option<Self> {
        let token = NumberOrString::String(PROGRESS_TOKEN.to_string());
        let title = title.into();
        let initial_message = initial_message.into();

        // Ask the client to allocate the progress token. Some clients
        // (older ones) don't support this; we'd rather skip the UI than
        // fail warming.
        if client
            .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .await
            .is_err()
        {
            return None;
        }

        client
            .send_notification::<ProgressNotification>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title,
                        cancellable: Some(false),
                        message: Some(initial_message),
                        // Percentage is sent on the wire but the message
                        // text is deliberately free of any "(X%)" suffix.
                        // The LSP `percentage` field is a separate numeric
                        // channel — clients that render a progress bar
                        // use it for fill, clients that don't are free
                        // to ignore it. Either way, the message stays
                        // clean for the narrow status bar.
                        percentage: Some(0),
                    },
                )),
            })
            .await;

        Some(Self {
            client,
            active: true,
            last_report: Instant::now(),
        })
    }

    /// Send an incremental update. Calls within `REPORT_THROTTLE` of the
    /// previous report are dropped — pass `force=true` to bypass the
    /// throttle (use this for phase transitions you want guaranteed to
    /// land, e.g. "Discovering files" → "Indexing files").
    pub async fn report(
        &mut self,
        message: impl Into<String>,
        percentage: Option<u32>,
        force: bool,
    ) {
        if !self.active {
            return;
        }
        if !force && self.last_report.elapsed() < REPORT_THROTTLE {
            return;
        }
        self.last_report = Instant::now();

        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: NumberOrString::String(PROGRESS_TOKEN.to_string()),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                    WorkDoneProgressReport {
                        cancellable: Some(false),
                        message: Some(message.into()),
                        percentage,
                    },
                )),
            })
            .await;
    }

    /// Finalize the progress. The status bar entry disappears after the
    /// brief `message` flash. Consumes self.
    pub async fn end(mut self, message: impl Into<String>) {
        if !self.active {
            return;
        }
        self.active = false;
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: NumberOrString::String(PROGRESS_TOKEN.to_string()),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(message.into()),
                })),
            })
            .await;
    }
}

impl Drop for IndexingProgress {
    /// Safety net: if the warming pipeline panics or returns early
    /// without calling `end`, we still need to clear the status-bar
    /// entry. Spawn a fire-and-forget task because `Drop` can't await.
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let client = self.client.clone();
        tokio::spawn(async move {
            client
                .send_notification::<ProgressNotification>(ProgressParams {
                    token: NumberOrString::String(PROGRESS_TOKEN.to_string()),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd {
                            message: Some("Indexing interrupted.".into()),
                        },
                    )),
                })
                .await;
        });
    }
}

