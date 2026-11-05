//! A mux-local report: snapshot first, type a description, file through the board door.
use super::{
    input_field::{FieldEvent, InputField},
    *,
};

pub(super) struct Report {
    field: InputField,
    feature: bool,
    context: String,
    parent: Option<String>,
    notice: String,
    pending: Option<(Instant, tokio::sync::oneshot::Receiver<(bool, String)>)>,
}

impl Report {
    fn new(context: String, parent: Option<String>) -> Self {
        Self {
            field: InputField::new("What went wrong?", 240),
            feature: false,
            context,
            parent,
            notice: "Includes this screen, focused row, build and recent logs.".into(),
            pending: None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(due, _)| *due)
    }

    fn submission(&self, title: &str) -> (Vec<String>, String) {
        let mut args: Vec<String> = [
            "backlog",
            "idea",
            title,
            "--type",
            if self.feature { "feature" } else { "bug" },
            "--source-kind",
            "operator_request",
            "--separate",
            "--difficulty",
            "medium",
            "--details-file",
            "-",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        if let Some(parent) = self.parent.as_ref().filter(|p| !p.trim().is_empty()) {
            args.extend(["--parent".into(), parent.clone()]);
        }
        (args, format!("{title}\n\n{}", self.context))
    }

    fn popup(&self) -> Popup {
        Popup::new(
            vec![
                PopupRow::Info {
                    label: "type".into(),
                    value: if self.feature { "idea" } else { "bug" }.into(),
                },
                self.field.row("description"),
                PopupRow::Text(self.notice.clone()),
            ],
            Anchor::Center,
        )
        .title("report a problem")
        .footer(if self.pending.is_some() {
            "filing…"
        } else {
            "Tab type  ·  Enter file  ·  Esc cancel"
        })
        .plain_body()
    }

    fn complete(&mut self, result: (bool, String)) -> bool {
        self.pending = None;
        self.notice = result.1;
        result.0
    }
}

pub(super) fn open(view: &mut View) {
    let frame = crate::vt::frame_text(&view.compose());
    let rows = view.display_rows();
    let agent = view
        .selector
        .and_then(|i| rows.get(i))
        .and_then(|row| match row {
            DisplayRow::Agent(a) => Some(*a),
            _ => None,
        })
        .or_else(|| {
            view.layout
                .agents
                .iter()
                .find(|a| view.row_hosts_pane(a, view.layout.focus))
        });
    let row = agent
        .map(|a| {
            format!(
                "name: {}\nsession: {}\nnode: {}",
                a.name,
                a.harness_session_id.as_deref().unwrap_or("unknown"),
                a.node.as_deref().unwrap_or("none")
            )
        })
        .unwrap_or_else(|| format!("pane: {}\nno agent row", view.layout.focus));
    let server_log = crate::proto::mux_dir().join(format!("{}.log", view.session));
    let context = format!("## Build\nversion: {}\nrevision: {}\ndirty: {}\n\n## Focused row\n{}\n\n## Frame\n```text\n{}\n```\n\n## Server log\n```text\n{}\n```\n\n## Client log\n```text\n{}\n```", BUILD_VERSION, env!("FNO_MUX_GIT_REV"), env!("FNO_MUX_GIT_DIRTY"), row, frame,
        crate::mux_log::tail(&server_log, 40), crate::mux_log::tail(&crate::mux_log::client_path(), 40));
    view.clear_peek();
    view.report = Some(Report::new(
        context,
        crate::config_defaults::lookup_key("mux.report_parent"),
    ));
}

pub(super) fn keys(view: &mut View, bytes: &[u8]) {
    let Some(report) = view.report.as_mut() else {
        return;
    };
    if report.pending.is_some() {
        return;
    }
    for event in report.field.feed(bytes) {
        match event {
            FieldEvent::Tab => report.feature = !report.feature,
            FieldEvent::Cancel => {
                view.report = None;
                return;
            }
            FieldEvent::Submit(title) if !title.is_empty() => {
                let (args, body) = report.submission(&title);
                let (tx, rx) = tokio::sync::oneshot::channel();
                report.pending = Some((Instant::now() + Duration::from_millis(100), rx));
                report.notice = "Filing report…".into();
                tokio::spawn(async move {
                    let _ = tx.send(crate::backlog_write::run_command(&args, Some(body)).await);
                });
                break;
            }
            FieldEvent::Submit(_) => report.notice = "Describe the problem before filing.".into(),
        }
    }
}

pub(super) fn poll(view: &mut View) -> bool {
    let result = view
        .report
        .as_mut()
        .and_then(|r| r.pending.as_mut())
        .and_then(|(due, rx)| {
            *due = Instant::now() + Duration::from_millis(100);
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some((
                    false,
                    "The report command stopped. Your text is kept.".into(),
                )),
                Err(_) => None,
            }
        });
    if let Some(result) = result {
        let notice = result.1.clone();
        if view.report.as_mut().is_some_and(|r| r.complete(result)) {
            view.report = None;
        }
        view.set_notice(notice);
        true
    } else {
        false
    }
}

pub(super) fn draw(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    if let Some(report) = &view.report {
        draw_popup_overlay(cells, rows, cols, &report.popup(), view.term, &view.theme);
    }
}

pub(super) fn deadline(view: &View) -> Option<Instant> {
    view.health
        .deadline()
        .into_iter()
        .chain(view.report.as_ref().and_then(Report::deadline))
        .min()
}

pub(super) fn tick(view: &mut View, compositor: &mut Compositor) -> Result<(), String> {
    view.health.record();
    if poll(view) {
        compositor
            .draw(&view.compose())
            .map_err(|e| format!("draw: {e}"))?;
    }
    Ok(())
}
