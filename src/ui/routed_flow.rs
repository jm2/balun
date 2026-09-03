//! Approval-on-demand flow for routed discovery, kept GTK-free.
//!
//! A click asks the controller to run the approved routed scan. Nothing is
//! sent until the store remembers an approval, so an unapproved proposal
//! comes back as a fixed failure; the flow then asks for a fresh proposal,
//! shows it once, and re-runs only after the user approves it. Every step is
//! driven by accepted snapshots, and only a user's click arms the prompt, so
//! a run that keeps failing can never loop back into the dialog by itself.

use balun::controller::{
    ApplicationSnapshot, DiscoveryFailure, DiscoveryKind, DiscoveryStatus, OperationGeneration,
    RoutedApprovalToken, RoutedProposalState, RoutedProposalStatus,
};

/// Where the flow stands between snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutedFlowStage {
    Idle,
    AwaitingProposal { generation: OperationGeneration },
    ShowingDialog(RoutedApprovalToken),
    Approving(RoutedApprovalToken),
    Rerunning,
}

/// What the window must do after one snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutedFlowAction {
    Nothing,
    Propose,
    ShowDialog(RoutedProposalState),
    Run,
}

#[derive(Debug)]
pub(crate) struct RoutedApprovalFlow {
    stage: RoutedFlowStage,
    armed: bool,
}

impl RoutedApprovalFlow {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            stage: RoutedFlowStage::Idle,
            armed: false,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn stage(&self) -> RoutedFlowStage {
        self.stage
    }

    /// The user asked for a routed search; one refusal may now open the dialog.
    pub(crate) const fn user_requested_run(&mut self) {
        self.stage = RoutedFlowStage::Idle;
        self.armed = true;
    }

    /// The user approved the shown proposal; the caller sends the approval.
    pub(crate) const fn dialog_approved(&mut self, token: RoutedApprovalToken) {
        self.stage = RoutedFlowStage::Approving(token);
    }

    /// The dialog closed without approval.
    pub(crate) const fn dialog_dismissed(&mut self) {
        if matches!(self.stage, RoutedFlowStage::ShowingDialog(_)) {
            self.stage = RoutedFlowStage::Idle;
        }
    }

    /// Feed one accepted snapshot and learn what to do next.
    pub(crate) fn observe(&mut self, snapshot: &ApplicationSnapshot) -> RoutedFlowAction {
        let discovery = snapshot.discovery();
        let proposal = snapshot.routed().proposal();
        match self.stage {
            RoutedFlowStage::Idle => {
                let refused = discovery.kind() == DiscoveryKind::Routed
                    && discovery.status()
                        == DiscoveryStatus::Failed(DiscoveryFailure::RoutedNotApproved);
                if refused && self.armed {
                    self.armed = false;
                    self.stage = RoutedFlowStage::AwaitingProposal {
                        generation: discovery.generation(),
                    };
                    RoutedFlowAction::Propose
                } else {
                    RoutedFlowAction::Nothing
                }
            }
            RoutedFlowStage::AwaitingProposal { .. } => match proposal {
                RoutedProposalStatus::Proposed(state) if state.approved() => {
                    self.stage = RoutedFlowStage::Rerunning;
                    RoutedFlowAction::Run
                }
                RoutedProposalStatus::Proposed(state) => {
                    self.stage = RoutedFlowStage::ShowingDialog(state.token());
                    RoutedFlowAction::ShowDialog(state)
                }
                RoutedProposalStatus::Failed(_) => {
                    self.stage = RoutedFlowStage::Idle;
                    RoutedFlowAction::Nothing
                }
                RoutedProposalStatus::None | RoutedProposalStatus::Proposing => {
                    RoutedFlowAction::Nothing
                }
            },
            RoutedFlowStage::ShowingDialog(_) => RoutedFlowAction::Nothing,
            RoutedFlowStage::Approving(token) => match proposal {
                RoutedProposalStatus::Proposed(state)
                    if state.token() == token && state.approved() =>
                {
                    self.stage = RoutedFlowStage::Rerunning;
                    RoutedFlowAction::Run
                }
                RoutedProposalStatus::Proposed(state) if state.token() != token => {
                    self.stage = RoutedFlowStage::Idle;
                    RoutedFlowAction::Nothing
                }
                RoutedProposalStatus::Failed(_) | RoutedProposalStatus::None => {
                    self.stage = RoutedFlowStage::Idle;
                    RoutedFlowAction::Nothing
                }
                RoutedProposalStatus::Proposed(_) | RoutedProposalStatus::Proposing => {
                    RoutedFlowAction::Nothing
                }
            },
            RoutedFlowStage::Rerunning => {
                if discovery.kind() == DiscoveryKind::Routed
                    && discovery.status() != DiscoveryStatus::Idle
                {
                    self.stage = RoutedFlowStage::Idle;
                }
                RoutedFlowAction::Nothing
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use balun::controller::{
        DiscoveryState, RoutedAvailability, RoutedDiscoveryState, SelectedLineupState,
        SnapshotRevision,
    };

    use super::*;

    fn token() -> RoutedApprovalToken {
        RoutedApprovalToken::new(5)
    }

    fn proposal(approved: bool) -> RoutedProposalState {
        RoutedProposalState::new(token(), 4, 8, 64, 16, Duration::from_secs(15), 1)
            .with_approved(approved)
    }

    fn snapshot(
        revision: u64,
        discovery: DiscoveryState,
        proposal: RoutedProposalStatus,
    ) -> ApplicationSnapshot {
        ApplicationSnapshot::new(
            SnapshotRevision::new(revision),
            discovery.generation(),
            OperationGeneration::INITIAL,
            discovery,
            [],
            None,
            SelectedLineupState::unselected(OperationGeneration::INITIAL),
        )
        .unwrap()
        .with_routed(RoutedDiscoveryState::new(
            RoutedAvailability::Available,
            proposal,
            None,
        ))
    }

    fn refused(generation: u64) -> DiscoveryState {
        DiscoveryState::failed_for(
            OperationGeneration::new(generation),
            DiscoveryKind::Routed,
            DiscoveryFailure::RoutedNotApproved,
        )
    }

    #[test]
    fn a_refusal_prompts_only_after_the_user_asked_and_only_once() {
        let mut flow = RoutedApprovalFlow::new();
        assert_eq!(
            flow.observe(&snapshot(1, refused(1), RoutedProposalStatus::None)),
            RoutedFlowAction::Nothing,
            "nothing the user did not ask for opens a dialog"
        );

        flow.user_requested_run();
        assert_eq!(
            flow.observe(&snapshot(2, refused(2), RoutedProposalStatus::None)),
            RoutedFlowAction::Propose
        );
        assert_eq!(
            flow.observe(&snapshot(3, refused(2), RoutedProposalStatus::Proposing)),
            RoutedFlowAction::Nothing
        );
        assert_eq!(
            flow.observe(&snapshot(
                4,
                refused(2),
                RoutedProposalStatus::Proposed(proposal(false))
            )),
            RoutedFlowAction::ShowDialog(proposal(false))
        );
        assert_eq!(flow.stage(), RoutedFlowStage::ShowingDialog(token()));

        flow.dialog_dismissed();
        assert_eq!(flow.stage(), RoutedFlowStage::Idle);
        assert_eq!(
            flow.observe(&snapshot(5, refused(3), RoutedProposalStatus::None)),
            RoutedFlowAction::Nothing,
            "a later refusal does not reopen the dialog by itself"
        );
    }

    #[test]
    fn approval_reruns_once_the_controller_confirms_it() {
        let mut flow = RoutedApprovalFlow::new();
        flow.user_requested_run();
        flow.observe(&snapshot(1, refused(1), RoutedProposalStatus::None));
        flow.observe(&snapshot(
            2,
            refused(1),
            RoutedProposalStatus::Proposed(proposal(false)),
        ));
        flow.dialog_approved(token());
        assert_eq!(
            flow.observe(&snapshot(
                3,
                refused(1),
                RoutedProposalStatus::Proposed(proposal(false))
            )),
            RoutedFlowAction::Nothing
        );
        assert_eq!(
            flow.observe(&snapshot(
                4,
                refused(1),
                RoutedProposalStatus::Proposed(proposal(true))
            )),
            RoutedFlowAction::Run
        );
        assert_eq!(flow.stage(), RoutedFlowStage::Rerunning);
        let refreshing =
            DiscoveryState::refreshing_for(OperationGeneration::new(2), DiscoveryKind::Routed);
        assert_eq!(
            flow.observe(&snapshot(
                5,
                refreshing,
                RoutedProposalStatus::Proposed(proposal(true))
            )),
            RoutedFlowAction::Nothing
        );
        assert_eq!(flow.stage(), RoutedFlowStage::Idle);
    }

    #[test]
    fn a_changed_or_failed_proposal_abandons_the_approval() {
        let mut flow = RoutedApprovalFlow::new();
        flow.user_requested_run();
        flow.observe(&snapshot(1, refused(1), RoutedProposalStatus::None));
        assert_eq!(
            flow.observe(&snapshot(
                2,
                refused(1),
                RoutedProposalStatus::Failed(DiscoveryFailure::RoutedNoCandidates)
            )),
            RoutedFlowAction::Nothing
        );
        assert_eq!(flow.stage(), RoutedFlowStage::Idle);

        flow.user_requested_run();
        flow.observe(&snapshot(3, refused(2), RoutedProposalStatus::None));
        flow.observe(&snapshot(
            4,
            refused(2),
            RoutedProposalStatus::Proposed(proposal(false)),
        ));
        flow.dialog_approved(token());
        let other = RoutedProposalState::new(
            RoutedApprovalToken::new(6),
            4,
            8,
            64,
            16,
            Duration::from_secs(15),
            1,
        );
        assert_eq!(
            flow.observe(&snapshot(
                5,
                refused(2),
                RoutedProposalStatus::Proposed(other)
            )),
            RoutedFlowAction::Nothing
        );
        assert_eq!(flow.stage(), RoutedFlowStage::Idle);

        flow.user_requested_run();
        flow.observe(&snapshot(6, refused(3), RoutedProposalStatus::None));
        assert_eq!(
            flow.observe(&snapshot(
                7,
                refused(3),
                RoutedProposalStatus::Proposed(proposal(true))
            )),
            RoutedFlowAction::Run,
            "a proposal approved earlier in the session runs without a dialog"
        );
    }
}
