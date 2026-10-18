use crate::state::{update_state, JobView, UiState};
use gpui::{
    img, prelude::*, px, rgb, ClickEvent, Context, Entity, Image, ImageFormat, SharedString, Window,
};
use mpa_agent::{AgentMode, TargetSelection};
use mpa_domain::JobState;
use std::sync::Arc;

const INK: u32 = 0x1f2937;
const MUTED: u32 = 0x6b7280;
const LINE: u32 = 0xe5e7eb;
const PAPER: u32 = 0xffffff;
const WASH: u32 = 0xf3f4f6;
const ACCENT: u32 = 0x2563eb;
const GOOD: u32 = 0x15803d;
const BAD: u32 = 0xb91c1c;
const WARN: u32 = 0xb45309;
const WARN_BG: u32 = 0xfef3c7;

/// Single preview box: ~1/4 of the area each stacked image used to take
/// (the analysis thumbnails are ≤256px on their longest side). One page at a
/// time in a fixed box keeps the layout stable, so the list, the target picker
/// and the actions always stay on screen.
const PREVIEW_W: f32 = 140.;
const PREVIEW_H: f32 = 182.;

pub struct RootView {
    state: Entity<UiState>,
}

impl std::fmt::Debug for RootView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootView").finish_non_exhaustive()
    }
}

impl RootView {
    pub fn new(state: Entity<UiState>) -> Self {
        Self { state }
    }

    /// State handle for tests and shell wiring (the single ownership seam).
    pub fn state(&self) -> &Entity<UiState> {
        &self.state
    }
}

impl gpui::Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let st = self.state.read(cx);
        gpui::div()
            .flex_col()
            .w_full()
            .h_full()
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .child(header(self, st, cx))
            .child(notice_bar(self, st, cx))
            .child(
                gpui::div()
                    .flex()
                    .flex_1()
                    .gap_3()
                    .p_3()
                    .child(job_list(self, st, cx))
                    .child(detail(self, st, cx)),
            )
            .child(footer(self, st, cx))
    }
}

fn header(view: &RootView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    let (mode_label, mode_bg) = match st.mode {
        AgentMode::Active => ("ACTIVO — interceptando", GOOD),
        AgentMode::Suspended => ("SUSPENDIDO — todo pasa", MUTED),
    };
    let _ = &view.state;
    gpui::div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(rgb(LINE))
        .child(
            gpui::div()
                .text_lg()
                .child("MyPaper Print Agent".to_string()),
        )
        .child(
            gpui::div()
                .id("mode-toggle")
                .cursor_pointer()
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(mode_bg))
                .text_color(rgb(PAPER))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| s.toggle_mode(cx));
                }))
                .child(mode_label.to_string()),
        )
}

fn notice_bar(_view: &RootView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    match &st.notice {
        None => gpui::div().into_any_element(),
        Some(text) => gpui::div()
            .id("notice-dismiss")
            .cursor_pointer()
            .mx_3()
            .mt_2()
            .p_2()
            .rounded_md()
            .bg(rgb(WARN_BG))
            .text_color(rgb(INK))
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                update_state(&this.state, cx, |s, cx| s.dismiss_notice(cx));
            }))
            .child(format!("{text}  (clic para cerrar)"))
            .into_any_element(),
    }
}

fn job_list(_view: &RootView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    // Scrolls on its own too: a long queue must not push the detail pane (and
    // the footer) out of the window.
    let mut col = gpui::div()
        .id("job-list")
        .flex()
        .flex_col()
        .gap_2()
        .w(px(300.))
        .flex_shrink_0()
        .min_h_0()
        .overflow_y_scroll()
        .pr_1();
    col = col.child(
        gpui::div()
            .text_sm()
            .text_color(rgb(MUTED))
            .child(format!("En espera ({})", st.jobs.len())),
    );
    if st.jobs.is_empty() {
        col = col.child(
            gpui::div()
                .p_3()
                .rounded_md()
                .bg(rgb(WASH))
                .text_sm()
                .text_color(rgb(MUTED))
                .child("Sin impresiones en espera.\nEnvía algo a imprimir…".to_string()),
        );
    }
    for job in &st.jobs {
        let selected = st
            .selected
            .as_ref()
            .is_some_and(|(p, id)| p == &job.printer && *id == job.job_id);
        let printer = job.printer.clone();
        let job_id = job.job_id;
        let label = format!("#{job_id} {printer}");
        let state_label = match job.state {
            JobState::Paused => "pausado",
            JobState::Detected => "detectado",
            JobState::Released => "liberado",
            JobState::Cancelled => "cancelado",
        };
        col = col.child(
            gpui::div()
                .id(SharedString::from(format!("job-{}-{}", printer, job_id)))
                .cursor_pointer()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(if selected { ACCENT } else { LINE }))
                .bg(rgb(if selected { 0xeff6ff } else { PAPER }))
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| {
                        s.select(cx, printer.clone(), job_id)
                    });
                }))
                .child(gpui::div().text_sm().child(label))
                .child(gpui::div().text_xs().text_color(rgb(MUTED)).child(format!(
                    "{} · {state_label} · {} pág",
                    job.document,
                    job.pages.len()
                ))),
        );
    }
    col
}

fn detail(view: &RootView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    let Some(job) = UiState::selected_job(&st.jobs, &st.selected) else {
        return gpui::div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(MUTED))
            .child("Selecciona una impresión de la lista".to_string());
    };
    gpui::div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .gap_2()
        .child(gpui::div().text_lg().child(format!(
            "#{} {} — {}",
            job.job_id, job.printer, job.document
        )))
        .child(analysis_line(job))
        // Bounded middle row: the page list scrolls inside it and the preview
        // sits next to it. Without `flex_1().min_h_0()` the scroll container
        // grows with its content instead of scrolling, and the actions below
        // get pushed out of the window.
        .child(
            gpui::div()
                .flex()
                .flex_1()
                .min_h_0()
                .gap_3()
                .child(pages_panel(job, st, cx))
                .child(preview_panel(job, st, cx)),
        )
        .child(summary_line(job))
        .child(target_picker(view, job, st, cx))
        .child(actions(view, job, cx))
}

fn analysis_line(job: &JobView) -> impl IntoElement {
    let mut col = gpui::div().flex_col().gap_1();
    match &job.analysis_error {
        Some(e) => {
            col = col.child(
                gpui::div()
                    .text_sm()
                    .text_color(rgb(BAD))
                    .child(format!("Análisis falló: {e}. Puedes cancelar o esperar.")),
            );
        }
        None if job.pages.is_empty() => {
            col = col.child(
                gpui::div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("Analizando cobertura…".to_string()),
            );
        }
        None => {}
    }
    for warning in &job.warnings {
        col = col.child(
            gpui::div()
                .text_sm()
                .text_color(rgb(WARN))
                .child(format!("Aviso: {warning}")),
        );
    }
    col
}

/// Compact, scrollable list of the job's pages. No images here: the single
/// preview pane shows the picked page, so the list stays readable and the row
/// height does not depend on the render.
fn pages_panel(job: &JobView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    let included = job.pages.iter().filter(|p| p.included).count();
    let mut col = gpui::div()
        .id(SharedString::from(format!(
            "pages-{}-{}",
            job.printer, job.job_id
        )))
        .flex()
        .flex_col()
        .gap_1()
        .w(px(260.))
        .flex_shrink_0()
        .min_h_0()
        .overflow_y_scroll()
        .pr_1();
    col = col.child(gpui::div().text_sm().text_color(rgb(MUTED)).child(format!(
        "Páginas ({included}/{} incluidas) · clic para previsualizar",
        job.pages.len()
    )));
    for page in &job.pages {
        let printer = job.printer.clone();
        let job_id = job.job_id;
        let page_no = page.page_no;
        let is_preview = st.selected_page == Some(page_no);
        let checkbox = if page.included { "[x]" } else { "[ ]" };
        let tier = page.tier_id.clone().unwrap_or_else(|| "—".into());
        let c = page.coverage;
        col = col.child(
            gpui::div()
                .id(SharedString::from(format!(
                    "page-{printer}-{job_id}-{page_no}"
                )))
                .flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(if is_preview { ACCENT } else { LINE }))
                .bg(rgb(if is_preview { 0xeff6ff } else { PAPER }))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| s.select_page(cx, page_no));
                }))
                .child(gpui::div().text_sm().child(checkbox.to_string()))
                .child(
                    gpui::div()
                        .flex()
                        .flex_col()
                        .child(format!("Página {page_no} · tier {tier}"))
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(coverage_line(c)),
                        ),
                ),
        );
    }
    col
}

/// The only preview: the picked page, its coverage, and the include toggle for
/// that page (so the list rows stay a single click target).
fn preview_panel(job: &JobView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    let mut col = gpui::div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .items_center()
        .gap_2()
        .p_2()
        .rounded_md()
        .bg(rgb(WASH));
    let Some(page) = st.preview_page(job) else {
        return col.child(
            gpui::div()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("Sin páginas analizadas todavía".to_string()),
        );
    };
    let printer = job.printer.clone();
    let job_id = job.job_id;
    let page_no = page.page_no;
    let included = page.included;
    let tier = page.tier_id.clone().unwrap_or_else(|| "—".into());
    let c = page.coverage;
    let thumb: Arc<Vec<u8>> = Arc::clone(&page.thumb);
    col = col
        .child(
            gpui::div()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(format!("Previsualización · página {page_no}")),
        )
        .child(preview_image(&thumb))
        .child(
            gpui::div()
                .text_sm()
                .child(format!("Página {page_no} · tier {tier}")),
        )
        .child(
            gpui::div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(coverage_line(c)),
        )
        .child(
            gpui::div()
                .id(SharedString::from(format!(
                    "include-{printer}-{job_id}-{page_no}"
                )))
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(if included { GOOD } else { LINE }))
                .text_color(rgb(if included { GOOD } else { MUTED }))
                .text_sm()
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| {
                        s.toggle_page(cx, printer.clone(), job_id, page_no)
                    });
                }))
                .child(if included {
                    "Incluida — clic para excluir".to_string()
                } else {
                    "Excluida — clic para incluir".to_string()
                }),
        );
    col
}

/// Coverage as one short line. The long form (`C=18.5 M=6.0 Y=0.0 K=62.0 ·
/// color=18.5%`) gets clipped inside the 260px page-list column.
fn coverage_line(c: mpa_domain::Coverage) -> String {
    format!(
        "C{:.1} M{:.1} Y{:.1} K{:.1} · color {:.1}%",
        c.c,
        c.m,
        c.y,
        c.k,
        c.color_coverage()
    )
}

/// One comfortable preview box. The generated thumbnails are ≤256px on their
/// longest side; this box (and `ObjectFit::Contain`) keeps any page — letter,
/// A4, photo — inside it without stretching or pushing the layout around.
fn preview_image(thumb: &Arc<Vec<u8>>) -> impl IntoElement {
    if thumb.is_empty() {
        return gpui::div()
            .w(px(PREVIEW_W))
            .h(px(PREVIEW_H))
            .rounded_md()
            .bg(rgb(PAPER))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(rgb(MUTED))
            .child("s/v".to_string());
    }
    gpui::div()
        .w(px(PREVIEW_W))
        .h(px(PREVIEW_H))
        .rounded_md()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(PAPER))
        .child(
            img(Arc::new(Image::from_bytes(
                ImageFormat::Png,
                (**thumb).clone(),
            )))
            .w_full()
            .h_full()
            .object_fit(gpui::ObjectFit::Contain),
        )
}

fn summary_line(job: &JobView) -> impl IntoElement {
    let parts: Vec<String> = job
        .tier_counts()
        .into_iter()
        .map(|(t, n)| format!("{t}×{n}"))
        .collect();
    let text = if parts.is_empty() {
        "Sin páginas incluidas".to_string()
    } else {
        format!("Resumen: {}", parts.join(" + "))
    };
    gpui::div().text_sm().child(text)
}

fn target_picker(
    view: &RootView,
    job: &JobView,
    st: &UiState,
    cx: &Context<RootView>,
) -> impl IntoElement {
    let mut row = gpui::div().flex().flex_wrap().items_center().gap_2();
    row = row.child(
        gpui::div()
            .text_sm()
            .text_color(rgb(MUTED))
            .child("Destino:".to_string()),
    );
    row = row.child(target_button(
        view,
        job,
        &TargetSelection::Unassigned,
        "Sin asignar",
        cx,
    ));
    for ticket in &st.tickets {
        row = row.child(target_button(
            view,
            job,
            &TargetSelection::Ticket(ticket.id.clone()),
            &format!("#{0} ({1})", ticket.ticket_number, ticket.cashier),
            cx,
        ));
    }
    row = row.child(target_button(
        view,
        job,
        &TargetSelection::NewDraft,
        "Nuevo borrador",
        cx,
    ));
    row
}

fn target_button(
    _view: &RootView,
    job: &JobView,
    target: &TargetSelection,
    label: &str,
    cx: &Context<RootView>,
) -> impl IntoElement {
    let printer = job.printer.clone();
    let job_id = job.job_id;
    let target = target.clone();
    let label = label.to_string();
    let active = job.target == target;
    gpui::div()
        .id(SharedString::from(format!(
            "target-{printer}-{job_id}-{label}"
        )))
        .cursor_pointer()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(if active { ACCENT } else { LINE }))
        .bg(rgb(if active { 0xeff6ff } else { PAPER }))
        .text_sm()
        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
            update_state(&this.state, cx, |s, cx| {
                s.select_target(cx, printer.clone(), job_id, target.clone())
            });
        }))
        .child(label)
}

fn actions(_view: &RootView, job: &JobView, cx: &Context<RootView>) -> impl IntoElement {
    let printer = job.printer.clone();
    let job_id = job.job_id;
    let can_complete = job.state == JobState::Paused
        && !job.pages.is_empty()
        && job.pages.iter().any(|p| p.included)
        && job.target != TargetSelection::Unassigned;
    let partial = !job.pages.iter().all(|p| p.included);
    let complete_label = if partial {
        "Completar (cancela original; reimprime Fase 4)"
    } else {
        "Completar e imprimir"
    };
    let mut row = gpui::div().flex().items_center().gap_2();
    if can_complete {
        let printer_c = printer.clone();
        row = row.child(
            gpui::div()
                .id(SharedString::from(format!("complete-{printer}-{job_id}")))
                .cursor_pointer()
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(GOOD))
                .text_color(rgb(PAPER))
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| {
                        s.complete(cx, printer_c.clone(), job_id)
                    });
                }))
                .child(complete_label.to_string()),
        );
    } else {
        row = row.child(
            gpui::div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child("Para completar: incluye ≥1 página y elige destino.".to_string()),
        );
    }
    if job.state == JobState::Paused {
        row = row.child(
            gpui::div()
                .id(SharedString::from(format!("cancel-{printer}-{job_id}")))
                .cursor_pointer()
                .px_4()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(BAD))
                .text_color(rgb(BAD))
                .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| {
                        s.cancel(cx, printer.clone(), job_id)
                    });
                }))
                .child("Cancelar".to_string()),
        );
    }
    if job.state == JobState::Released {
        row = row.child(
            gpui::div()
                .text_sm()
                .text_color(rgb(GOOD))
                .child("Liberado — borrador en cola de envío.".to_string()),
        );
    }
    row
}

fn footer(_view: &RootView, st: &UiState, cx: &Context<RootView>) -> impl IntoElement {
    let mut row = gpui::div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_1()
        .border_t_1()
        .border_color(rgb(LINE))
        .text_xs()
        .text_color(rgb(MUTED))
        .child(format!(
            "Borradores en cola: {} ({} sin enviar)",
            st.outbox_len, st.outbox_pending
        ));
    let mut buttons = gpui::div().flex().items_center().gap_2();
    if let Some(version) = &st.update_available {
        let label = format!("Instalar {version}");
        buttons = buttons.child(
            gpui::div()
                .id("apply-update")
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_md()
                .bg(rgb(GOOD))
                .text_color(rgb(PAPER))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| s.apply_update(cx));
                }))
                .child(label),
        );
    } else {
        buttons = buttons.child(
            gpui::div()
                .id("check-update")
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(LINE))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    update_state(&this.state, cx, |s, cx| s.check_updates(cx));
                }))
                .child("Actualización".to_string()),
        );
    }
    buttons = buttons.child(
        gpui::div()
            .id("sync-now")
            .cursor_pointer()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(ACCENT))
            .text_color(rgb(PAPER))
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                update_state(&this.state, cx, |s, cx| s.sync_now(cx));
            }))
            .child("Sincronizar / reimprimir".to_string()),
    );
    row = row.child(buttons);
    row
}
