use gpui::{AppContext, TestAppContext};
use mpa_agent::{AgentCore, AgentMode, MockTickets};
use mpa_spool::MockSpool;
use mpa_ui::{RootView, UiState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn test_core() -> Arc<Mutex<AgentCore>> {
    Arc::new(Mutex::new(AgentCore::new(
        Arc::new(MockSpool::with_demo_jobs()),
        Arc::new(MockTickets::demo()),
        vec![mpa_domain::Tier {
            id: "t6".into(),
            min_coverage: 0.0,
            max_coverage: 50.0,
            active: true,
        }],
    )))
}

fn test_path() -> std::path::PathBuf {
    std::env::temp_dir().join("mpa-ui-flow-test-state.json")
}

#[gpui::test]
async fn held_job_renders_and_mirrors(cx: &mut TestAppContext) {
    let core = test_core();
    {
        let mut core = core.lock().unwrap();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.analyze_job("Mock-LaserJet", 41).unwrap();
    }
    let (view, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| UiState::new(Arc::clone(&core), test_path(), None));
        RootView::new(state)
    });
    // add_window_view parks a first frame: render ran without panicking.
    let jobs = view.update(cx, |view, cx| {
        let mut count = 0;
        view.state().update(cx, |state, cx| {
            state.refresh(cx);
            count = state.jobs.len();
        });
        count
    });
    assert_eq!(jobs, 1);
}

#[gpui::test]
async fn mode_toggle_flows_into_core_and_mirror(cx: &mut TestAppContext) {
    let core = test_core();
    let (view, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| UiState::new(Arc::clone(&core), test_path(), None));
        RootView::new(state)
    });
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| state.toggle_mode(cx));
    });
    assert_eq!(core.lock().unwrap().mode(), AgentMode::Suspended);
    let mode = view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| {
            state.refresh(cx);
            state.mode
        })
    });
    assert_eq!(mode, AgentMode::Suspended);
}

/// A ticket source that behaves like the paired HTTP one: slow and blocking.
struct SlowTickets(Duration);

impl mpa_agent::TicketSource for SlowTickets {
    fn list_open_tickets(&self) -> Result<Vec<mpa_agent::Ticket>, mpa_agent::TicketError> {
        std::thread::sleep(self.0);
        Ok(vec![])
    }
}

#[test]
fn opening_the_window_does_not_block_on_the_ticket_source() {
    let core = Arc::new(Mutex::new(AgentCore::new(
        Arc::new(MockSpool::with_demo_jobs()),
        Arc::new(SlowTickets(Duration::from_secs(3))),
        vec![],
    )));
    let started = std::time::Instant::now();
    let state = UiState::new(Arc::clone(&core), test_path(), None);
    let elapsed = started.elapsed();
    // The mirror is filled by the background refresher, never by the UI:
    // `pull()` must stay a pure in-memory read.
    assert!(state.tickets.is_empty());
    assert!(
        elapsed < Duration::from_millis(500),
        "UiState::new (the window's first frame) waited {elapsed:?} on the ticket source"
    );
}
