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
            mode: mpa_domain::PrintMode::Color,
            min_coverage: 0.0,
            max_coverage: 50.0,
            active: true,
        }],
    )))
}

fn test_path() -> std::path::PathBuf {
    std::env::temp_dir().join("mpa-ui-flow-test-state.json")
}

// The demo capture fixture only exists off Windows: on Windows the real spool
// path needs a job that actually sits on a printer.
#[cfg(not(windows))]
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
        RootView::new(state, cx.focus_handle())
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
        RootView::new(state, cx.focus_handle())
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

// --- Fase 5: ticket selection ---------------------------------------------

fn demo_tickets() -> Vec<mpa_agent::Ticket> {
    vec![
        mpa_agent::Ticket {
            id: "order-101".into(),
            ticket_number: 101,
            cashier: "Ana".into(),
            total: 85.50,
        },
        mpa_agent::Ticket {
            id: "order-102".into(),
            ticket_number: 102,
            cashier: "Luis".into(),
            total: 12.00,
        },
    ]
}

#[test]
fn the_ticket_filter_matches_number_and_cashier() {
    let core = test_core();
    let mut state = UiState::new(Arc::clone(&core), test_path(), None);
    state.tickets = demo_tickets();

    // No filter: every open ticket is offered.
    assert_eq!(state.filtered_tickets().len(), 2);

    // Exact number.
    state.ticket_filter = "101".into();
    let hit = state.filtered_tickets();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].ticket_number, 101);

    // Cashier, case-insensitive.
    state.ticket_filter = "ana".into();
    assert_eq!(state.filtered_tickets().len(), 1);
    assert_eq!(state.filtered_tickets()[0].cashier, "Ana");

    // Partial number still narrows (the operator types what they see).
    state.ticket_filter = "10".into();
    assert_eq!(state.filtered_tickets().len(), 2);

    // No match: an empty list, which the UI reports instead of hiding.
    state.ticket_filter = "999".into();
    assert!(state.filtered_tickets().is_empty());

    // Whitespace is not a query.
    state.ticket_filter = "   ".into();
    assert_eq!(state.filtered_tickets().len(), 2);
}

#[gpui::test]
async fn typing_and_escape_drive_the_ticket_filter(cx: &mut TestAppContext) {
    let core = test_core();
    let (view, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| UiState::new(Arc::clone(&core), test_path(), None));
        RootView::new(state, cx.focus_handle())
    });
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, _| {
            state.tickets = demo_tickets();
        });
    });
    // "1", "0", "2" → the filter reads 102.
    for ch in ["1", "0", "2"] {
        view.update(cx, |view, cx| {
            view.state()
                .update(cx, |state, cx| state.ticket_filter_key(cx, ch, Some(ch)));
        });
    }
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, _| {
            assert_eq!(state.ticket_filter, "102");
            assert_eq!(state.filtered_tickets().len(), 1);
        });
    });

    // Backspace removes one character, Escape clears the field.
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| {
            state.ticket_filter_key(cx, "backspace", None)
        });
    });
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, _| {
            assert_eq!(state.ticket_filter, "10");
            assert_eq!(state.filtered_tickets().len(), 2);
        });
    });
    view.update(cx, |view, cx| {
        view.state()
            .update(cx, |state, cx| state.ticket_filter_key(cx, "escape", None));
    });
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, _| {
            assert!(state.ticket_filter.is_empty());
            assert_eq!(state.filtered_tickets().len(), 2);
        });
    });

    // Control characters (arrows, Enter) must not pollute the query.
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| {
            state.ticket_filter_key(cx, "enter", Some("\n"))
        });
    });
    view.update(cx, |view, cx| {
        view.state()
            .update(cx, |state, _| assert!(state.ticket_filter.is_empty()));
    });
}

/// A job held with its analysis already in, without touching the spooler: the
/// demo capture fixture is not available on Windows.
fn adopt_analyzed_job(core: &Arc<Mutex<AgentCore>>, job_id: u32, document: &str) {
    let mut core = core.lock().unwrap();
    core.adopt_job(mpa_agent::WatchedJob {
        printer: "Mock-LaserJet".into(),
        job_id,
        document: document.into(),
        state: mpa_domain::JobState::Paused,
        pages: vec![mpa_agent::WatchedPage {
            page_no: 1,
            coverage: mpa_domain::Coverage::new(0.0, 0.0, 0.0, 12.0),
            tier_id: Some("t6".into()),
            included: true,
            thumbnail_png: Vec::new(),
            area: None,
        }],
        analysis_error: None,
        warnings: Vec::new(),
        target: mpa_agent::TargetSelection::Unassigned,
        format: None,
        spool_path: None,
        print_mode: mpa_domain::PrintMode::Bw,
        preset: None,
    });
}

#[gpui::test]
async fn a_reprint_of_the_same_document_keeps_its_ticket(cx: &mut TestAppContext) {
    let core = test_core();
    {
        let mut core = core.lock().unwrap();
        core.set_cached_tickets(demo_tickets());
    }
    adopt_analyzed_job(&core, 41, "apunte.docx");
    let (view, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| UiState::new(Arc::clone(&core), test_path(), None));
        RootView::new(state, cx.focus_handle())
    });
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| state.refresh(cx));
    });
    // The operator assigns the analysis to ticket 101.
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| {
            state.select_target(
                cx,
                "Mock-LaserJet".into(),
                41,
                mpa_agent::TargetSelection::Ticket("order-101".into()),
            )
        });
    });

    // The same document is printed again: the second job inherits the choice.
    adopt_analyzed_job(&core, 42, "apunte.docx");
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| state.refresh(cx));
    });
    let second = core
        .lock()
        .unwrap()
        .snapshot()
        .into_iter()
        .find(|j| j.job_id == 42)
        .expect("second job");
    assert_eq!(
        second.target,
        mpa_agent::TargetSelection::Ticket("order-101".into()),
        "the remembered destination was not restored for the same document"
    );

    // A DIFFERENT document is not dragged into the same ticket.
    adopt_analyzed_job(&core, 43, "otro-documento.docx");
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| state.refresh(cx));
    });
    let other = core
        .lock()
        .unwrap()
        .snapshot()
        .into_iter()
        .find(|j| j.job_id == 43)
        .expect("other document job");
    assert_eq!(
        other.target,
        mpa_agent::TargetSelection::Unassigned,
        "a different document must not inherit the destination"
    );

    // Once the ticket closes, the memory must NOT attach anything to it.
    {
        let mut core = core.lock().unwrap();
        core.set_cached_tickets(vec![]);
    }
    adopt_analyzed_job(&core, 44, "apunte.docx");
    view.update(cx, |view, cx| {
        view.state().update(cx, |state, cx| state.refresh(cx));
    });
    let fourth = core
        .lock()
        .unwrap()
        .snapshot()
        .into_iter()
        .find(|j| j.job_id == 44)
        .expect("fourth job");
    assert_eq!(
        fourth.target,
        mpa_agent::TargetSelection::Unassigned,
        "a closed ticket must not be re-attached from memory"
    );
}
