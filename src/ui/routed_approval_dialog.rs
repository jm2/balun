//! Explicit approval of one route-derived discovery proposal.

use adw::prelude::*;
use balun::controller::RoutedProposalState;
use balun::discovery::{RouteScope, RoutedProposalOriginSummary};

const CANCEL_RESPONSE: &str = "cancel";
const APPROVE_RESPONSE: &str = "approve";

/// Present the proposal the controller built, with the exact routes it
/// came from, and ask for approval. `on_approve` runs only for the approve
/// response; `on_closed` runs for every response or dismiss.
pub(crate) fn present(
    parent: &adw::ApplicationWindow,
    proposal: RoutedProposalState,
    origins: &[RoutedProposalOriginSummary],
    on_approve: impl Fn() + 'static,
    on_closed: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Search routes behind your tunnel?")
        .body(approval_body(proposal))
        .close_response(CANCEL_RESPONSE)
        .default_response(CANCEL_RESPONSE)
        .build();
    dialog.add_response(CANCEL_RESPONSE, "Cancel");
    dialog.add_response(APPROVE_RESPONSE, "Approve and search");
    dialog.set_response_appearance(APPROVE_RESPONSE, adw::ResponseAppearance::Suggested);

    let group = adw::PreferencesGroup::builder()
        .title("Routes that will be searched")
        .build();
    for origin in origins {
        let (title, subtitle) = origin_row(origin);
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable(false)
            .selectable(false)
            .build();
        group.add(&row);
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .build();
    content.append(&group);
    dialog.set_extra_child(Some(&content));

    dialog.connect_response(Some(APPROVE_RESPONSE), move |_, _| on_approve());
    dialog.connect_closed(move |_| on_closed());
    dialog.present(Some(parent));
}

/// Fixed copy naming the proposal's exact traffic budget.
pub(crate) fn approval_body(proposal: RoutedProposalState) -> String {
    let routes = if proposal.origin_count() == 1 {
        "one tunnel route".to_owned()
    } else {
        format!("{} tunnel routes", proposal.origin_count())
    };
    format!(
        "Balun will send bounded HDHomeRun discovery requests to {} private addresses behind {routes}: at most {} datagrams, {} per second, finishing within {} seconds. Your approval is remembered for exactly these routes and reused only for them; Balun never searches a range you did not approve.",
        proposal.candidate_count(),
        proposal.maximum_request_datagrams(),
        proposal.wire_datagrams_per_second(),
        proposal.overall_deadline_seconds(),
    )
}

/// One origin as a network title and an interface subtitle.
pub(crate) fn origin_row(origin: &RoutedProposalOriginSummary) -> (String, String) {
    let scopes = origin
        .scopes()
        .iter()
        .map(|scope| match scope {
            RouteScope::OnLink => "on-link",
            RouteScope::ViaGateway => "via a gateway",
            RouteScope::Other => "other scope",
        })
        .collect::<Vec<_>>()
        .join(", ");
    let subtitle = if scopes.is_empty() {
        format!("through {}", origin.interface_name())
    } else {
        format!("through {} ({scopes})", origin.interface_name())
    };
    (origin.network().to_string(), subtitle)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use balun::controller::RoutedApprovalToken;

    use super::*;

    #[test]
    fn approval_copy_states_the_exact_budget() {
        let proposal = RoutedProposalState::new(
            RoutedApprovalToken::new(1),
            4,
            8,
            64,
            16,
            Duration::from_secs(15),
            1,
        );
        let body = approval_body(proposal);
        assert!(body.contains("to 4 private addresses behind one tunnel route"));
        assert!(body.contains("at most 8 datagrams, 64 per second, finishing within 15 seconds"));
        assert!(body.contains("never searches a range you did not approve"));

        let two = RoutedProposalState::new(
            RoutedApprovalToken::new(1),
            300,
            600,
            64,
            16,
            Duration::from_secs(15),
            2,
        );
        assert!(approval_body(two).contains("behind 2 tunnel routes"));
    }
}
