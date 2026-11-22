use std::sync::Arc;

use norte_core::{Digest, Result};
use tokio::sync::Mutex;

use super::cdp::{CdpSession, connect_to};
use super::error::is_connection_error;
use super::limits::BrowserLimits;
use super::output::BrowserOutput;
use super::page::{finish_page, page_action, update_snapshot};
use super::request::{BrowserAction, BrowserRequest};
use super::tabs::{resolve_current_tab, tab_action};

/// Runs one browser request against the host-owned connection.
pub(super) async fn execute_browser_action(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    client: &reqwest::Client,
    cdp_slot: &Arc<Mutex<Option<CdpSession>>>,
    current_slot: &Arc<Mutex<Option<String>>>,
    snapshot_slot: &Arc<Mutex<Option<String>>>,
) -> Result<BrowserOutput> {
    let digest = Digest::of(&serde_json::to_vec(request)?);
    if let Some(output) =
        tab_action(request, limits, client, cdp_slot, current_slot, &digest).await?
    {
        return Ok(output);
    }

    let tab = resolve_current_tab(client, limits, current_slot).await?;
    *current_slot.lock().await = Some(tab.id.clone());
    let bring_to_front = matches!(&request.action, BrowserAction::Navigate { .. });
    let session = connect_to(cdp_slot, limits, &tab, bring_to_front).await?;
    let result = run_on_session(request, limits, client, &session, snapshot_slot, digest).await;
    match result {
        Err(error) if is_connection_error(&error) && is_read_only(&request.action) => {
            *cdp_slot.lock().await = None;
            let fresh = connect_to(cdp_slot, limits, &tab, false).await?;
            run_on_session(request, limits, client, &fresh, snapshot_slot, digest).await
        }
        other => other,
    }
}

async fn run_on_session(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    client: &reqwest::Client,
    session: &CdpSession,
    snapshot_slot: &Arc<Mutex<Option<String>>>,
    digest: Digest,
) -> Result<BrowserOutput> {
    let outcome = page_action(request, limits, client, session, snapshot_slot).await?;
    update_snapshot(snapshot_slot, &outcome).await;
    finish_page(session, digest, outcome, limits).await
}

/// Actions that are safe to repeat after the connection is re-established.
fn is_read_only(action: &BrowserAction) -> bool {
    matches!(
        action,
        BrowserAction::ExtractDom { .. }
            | BrowserAction::ReadPage { .. }
            | BrowserAction::Screenshot
            | BrowserAction::Wait { .. }
            | BrowserAction::Scroll { .. }
    )
}
