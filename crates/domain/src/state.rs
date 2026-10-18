use thiserror::Error;

/// Lifecycle of a print job as seen by the agent. The actual spool control
/// (pause/resume/cancel) happens in the `spool` crate; this machine only
/// tracks the analysis intent so illegal flows are unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Detected,
    Paused,
    Released,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEvent {
    PausedByAgent,
    Resumed,
    Cancelled,
    ReprintedSubset,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("illegal transition: {event:?} from {state:?}")]
pub struct TransitionError {
    pub state: JobState,
    pub event: JobEvent,
}

pub fn transition(state: JobState, event: JobEvent) -> Result<JobState, TransitionError> {
    let next = match (state, event) {
        (JobState::Detected, JobEvent::PausedByAgent) => JobState::Paused,
        (JobState::Detected, JobEvent::Cancelled) => JobState::Cancelled,
        (JobState::Paused, JobEvent::Resumed) => JobState::Released,
        (JobState::Paused, JobEvent::Cancelled) => JobState::Cancelled,
        // Operator unchecked pages: original job dies, a filtered subset prints.
        (JobState::Paused, JobEvent::ReprintedSubset) => JobState::Released,
        _ => return Err(TransitionError { state, event }),
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_detect_pause_release() {
        let s = transition(JobState::Detected, JobEvent::PausedByAgent).unwrap();
        assert_eq!(s, JobState::Paused);
        let s = transition(s, JobEvent::Resumed).unwrap();
        assert_eq!(s, JobState::Released);
    }

    #[test]
    fn cancel_from_detected_or_paused() {
        assert_eq!(
            transition(JobState::Detected, JobEvent::Cancelled).unwrap(),
            JobState::Cancelled
        );
        assert_eq!(
            transition(JobState::Paused, JobEvent::Cancelled).unwrap(),
            JobState::Cancelled
        );
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(transition(JobState::Detected, JobEvent::Resumed).is_err());
        assert!(transition(JobState::Released, JobEvent::Resumed).is_err());
        assert!(transition(JobState::Cancelled, JobEvent::Cancelled).is_err());
    }
}
