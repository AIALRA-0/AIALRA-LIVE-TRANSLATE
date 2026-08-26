//! Core state and privacy rules shared by every local adapter.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A session state transition is persisted before user-visible state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Ready,
    Recording,
    Degraded,
    Stopping,
    Completed,
    Failed,
    Archived,
}

impl SessionState {
    /// Rejecting illegal transitions prevents a late model result from reopening a stopped session.
    pub fn transition(self, next: Self) -> Result<Self, StateTransitionError> {
        let allowed = matches!(
            (self, next),
            (Self::Created, Self::Ready)
                | (Self::Ready, Self::Recording)
                | (Self::Recording, Self::Degraded)
                | (Self::Degraded, Self::Recording)
                | (Self::Recording | Self::Degraded, Self::Stopping)
                | (Self::Stopping, Self::Completed | Self::Failed)
                | (Self::Created | Self::Ready, Self::Failed)
                | (Self::Completed | Self::Failed, Self::Archived)
        );

        allowed.then_some(next).ok_or(StateTransitionError {
            current: self,
            next,
        })
    }
}

/// Cloud policy is checked on the server for every provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudEgressPolicy {
    LocalOnly,
    TextCloudAllowed,
    SelectedMultimodalCloudAllowed,
}

impl CloudEgressPolicy {
    /// Text leaves the machine only when the stored session policy explicitly allows it.
    pub fn allows_text(self) -> bool {
        matches!(
            self,
            Self::TextCloudAllowed | Self::SelectedMultimodalCloudAllowed
        )
    }

    /// Images require the narrowest explicit multimodal permission.
    pub fn allows_multimodal(self) -> bool {
        matches!(self, Self::SelectedMultimodalCloudAllowed)
    }
}

/// The error retains both states so logs can explain rejected control actions without course data.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("illegal session transition from {current:?} to {next:?}")]
pub struct StateTransitionError {
    pub current: SessionState,
    pub next: SessionState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_session_cannot_resume_recording() {
        // A completed course remains immutable unless a new session is created.
        let result = SessionState::Completed.transition(SessionState::Recording);
        assert!(result.is_err());
    }

    #[test]
    fn local_policy_denies_every_cloud_data_type() {
        // The default policy blocks both text and images at the service boundary.
        let policy = CloudEgressPolicy::LocalOnly;
        assert!(!policy.allows_text());
        assert!(!policy.allows_multimodal());
    }
}
