//! A local report and an explicitly reviewed, redacted upstream issue.
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
    auth: Option<bool>,
    preview_request: Option<String>,
    draft: Option<IssueDraft>,
    scroll: usize,
    pan: usize,
    carry: agent_launcher::LauncherEsc,
    issue: Option<String>,
    node: Option<String>,
    pending: Option<(Instant, tokio::sync::oneshot::Receiver<Outcome>)>,
}

#[derive(Clone, serde::Deserialize)]
struct IssueDraft {
    title: String,
    body: String,
    #[serde(rename = "ref")]
    reference: String,
}

enum Outcome {
    Auth(bool),
    Draft(Result<IssueDraft, String>),
    Sent {
        issue: Option<String>,
        node: Option<String>,
        notice: String,
    },
}

async fn gh(args: &[String], body: Option<String>) -> (bool, String) {
    crate::backlog_write::run_command_in("gh".into(), args, body).await
}

async fn authenticated() -> bool {
    gh(
        &["auth", "status", "--active", "--hostname", "github.com"].map(str::to_owned),
        None,
    )
    .await
    .0
}

fn issue_url(text: &str) -> bool {
    text.strip_prefix("https://github.com/fnodotdev/fno/issues/")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

async fn publish(draft: &IssueDraft) -> Result<String, String> {
    let (ok, output) = gh(
        &[
            "issue".into(),
            "list".into(),
            "--repo".into(),
            "https://github.com/fnodotdev/fno".into(),
            "--state".into(),
            "all".into(),
            "--search".into(),
            format!("\"fno-feedback-ref: {}\" in:body", draft.reference),
            "--json".into(),
            "url".into(),
        ],
        None,
    )
    .await;
    if !ok {
        return Err(format!(
            "GitHub lookup failed: {}. Check gh auth status and repository issues before retrying.",
            crate::redact::redact_text(&output)
        ));
    }
    let matches = ok
        .then(|| serde_json::from_str::<Vec<serde_json::Value>>(&output).ok())
        .flatten()
        .ok_or(
            "GitHub lookup failed. Check gh auth status and repository issues before retrying.",
        )?;
    if let Some(url) = matches
        .iter()
        .filter_map(|v| v.get("url")?.as_str())
        .find(|url| issue_url(url))
    {
        return Ok(url.to_owned());
    }
    let (ok, output) = gh(
        &[
            "issue".into(),
            "create".into(),
            "--repo".into(),
            "https://github.com/fnodotdev/fno".into(),
            "--title".into(),
            draft.title.clone(),
            "--body-file".into(),
            "-".into(),
        ],
        Some(draft.body.clone()),
    )
    .await;
    if ok && issue_url(&output) {
        Ok(output)
    } else {
        Err(format!("GitHub returned no issue receipt: {}. Check fnodotdev/fno issues before retrying; the backlog report is kept.", crate::redact::redact_text(&output)))
    }
}

const PREVIEW_COLS: usize = 60;

fn code_room(term: (u16, u16)) -> usize {
    PREVIEW_COLS
        .min(
            (term.1 as usize)
                .saturating_sub(chrome::Chrome::FRAME_COLS * 2)
                .max(1),
        )
        // One leading space and two cells of air at the row's right edge.
        .saturating_sub(3)
}

fn code_window(line: &str, offset: usize, width: usize) -> String {
    let cols = chrome::str_cols(line);
    let tail = chrome::clip_tail(line, cols.saturating_sub(offset));
    let gap = cols
        .saturating_sub(chrome::str_cols(&tail) + offset)
        .min(width);
    chrome::clip(&format!("{}{tail}", " ".repeat(gap)), width)
}

fn review_lines(body: &str) -> impl Iterator<Item = (&str, bool)> {
    body.lines().scan(None::<usize>, |fence, line| {
        let trimmed = line.trim_start();
        let ticks = trimmed.bytes().take_while(|&c| c == b'`').count();
        let literal = fence.is_some() || ticks >= 3;
        if let Some(open) = *fence {
            if ticks >= open && trimmed[ticks..].trim().is_empty() {
                *fence = None;
            }
        } else if ticks >= 3 {
            *fence = Some(ticks);
        }
        Some((line, literal))
    })
}

fn code_block(text: &str) -> String {
    // A pane's own Markdown fences must not close the captured block.
    let ticks = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((ticks + 1).max(3));
    format!("{fence}text\n{text}\n{fence}")
}

impl Report {
    fn new(context: String, parent: Option<String>) -> Self {
        Self {
            field: InputField::new("", 240),
            feature: false,
            context,
            parent,
            notice: String::new(),
            auth: None,
            preview_request: None,
            draft: None,
            scroll: 0,
            pan: 0,
            carry: Default::default(),
            issue: None,
            node: None,
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

    fn enqueue(&mut self, work: impl std::future::Future<Output = Outcome> + Send + 'static) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending = Some((Instant::now() + Duration::from_millis(100), rx));
        tokio::spawn(async move {
            let _ = tx.send(work.await);
        });
    }

    fn prepare(&mut self, title: String) {
        let (_, body) = self.submission(&title);
        let kind = if self.feature { "feature" } else { "bug" };
        self.notice = "Preparing the redacted issue. Nothing has been sent.".into();
        self.enqueue(async move {
            let (ok, output) = crate::backlog_write::run_command(&[
                "do".into(), "feedback".into(), "--text".into(), body,
                "--kind".into(), kind.into(), "--json".into(), "--offline".into(),
            ], None).await;
            let draft = ok.then(|| serde_json::from_str::<IssueDraft>(&output).ok()).flatten()
                .filter(|d| !d.title.trim().is_empty() && !d.body.trim().is_empty()
                    && d.body.len() <= 65536 && !d.reference.is_empty()
                    && d.reference.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
                .ok_or_else(|| "The redacted draft could not be prepared or exceeds GitHub's body limit. Your text is kept. Shorten the report or run fno do feedback.".into());
            Outcome::Draft(draft)
        });
    }

    fn send(&mut self) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let (args, _) = self.submission(&draft.title);
        let body = draft.body.clone();
        let (mut issue, mut node) = (self.issue.clone(), self.node.clone());
        let auth = self.auth == Some(true);
        self.notice = "Filing the reviewed report…".into();
        self.enqueue(async move {
            let mut notices = Vec::new();
            if auth && issue.is_none() {
                match publish(&draft).await {
                    Ok(url) => issue = Some(url),
                    Err(error) => notices.push(error),
                }
            }
            if node.is_none() {
                let (ok, receipt) = crate::backlog_write::run_command(&args, Some(body)).await;
                if ok {
                    node = Some(receipt);
                } else {
                    notices.push(receipt);
                }
            }
            if let Some(url) = &issue {
                notices.push(url.clone());
            }
            if let Some(id) = &node {
                notices.push(format!("backlog {id}"));
            }
            if !auth {
                notices
                    .push("No gh authentication. Run gh auth login to file a GitHub issue.".into());
            }
            Outcome::Sent {
                issue,
                node,
                notice: notices.join(" · "),
            }
        });
    }

    fn popup(&self, term: (u16, u16)) -> Popup {
        let auth = match self.auth {
            None => "Checking gh authentication for fnodotdev/fno…",
            Some(true) => "Public issue: github.com/fnodotdev/fno. Copy: your fno backlog (its sharing settings apply).",
            Some(false) => {
                "No gh authentication for fnodotdev/fno. Run gh auth login. Backlog-only filing is available."
            }
        };
        let (rows, title, footer) = if let Some(draft) = &self.draft {
            let mut rows = vec![
                PopupRow::Text(auth.into()),
                PopupRow::Info {
                    label: "title".into(),
                    value: draft.title.clone(),
                },
            ];
            if !self.notice.is_empty() {
                rows.push(PopupRow::Text(self.notice.clone()));
            }
            rows.extend(review_lines(&draft.body).map(|(line, literal)| {
                if literal {
                    PopupRow::Text(code_window(line, self.pan, code_room(term)))
                } else if line.starts_with("## ") {
                    PopupRow::Header(line.into())
                } else {
                    PopupRow::Text(line.into())
                }
            }));
            (
                rows,
                "review GitHub issue",
                if self.pending.is_some() {
                    " filing…"
                } else if self.auth == Some(true) {
                    if self.issue.is_some() || self.node.is_some() {
                        " ↑↓ scroll · ←→ pan · Enter retry · Esc close"
                    } else {
                        " ↑↓ scroll · ←→ pan · Enter send · Esc edit"
                    }
                } else {
                    " ↑↓ scroll · ←→ pan · Enter backlog only · Esc edit"
                },
            )
        } else {
            let mut rows = vec![
                PopupRow::Header(format!("type: {}", if self.feature { "idea" } else { "bug" })),
                self.field.row("description"),
                PopupRow::Text("Includes focused pane text, focused row, build and up to 40 lines from client and server logs. Check the full preview for private details before sending.".into()),
                PopupRow::Text(auth.into()),
            ];
            if !self.notice.is_empty() {
                rows.push(PopupRow::Text(self.notice.clone()));
            }
            (
                rows,
                "report a problem",
                " Tab type · Enter review · Esc cancel",
            )
        };
        let mut popup = Popup::new(rows, Anchor::Center)
            .title(title)
            .footer(footer)
            .plain_body()
            .width_cap(PREVIEW_COLS);
        popup.scroll = self.scroll;
        popup
    }

    fn scroll_limit(&self, term: (u16, u16)) -> usize {
        let popup = self.popup(term);
        popup
            .render((u16::MAX, term.1))
            .lines
            .len()
            .saturating_sub(popup.render(term).lines.len())
    }

    fn pan_limit(&self, term: (u16, u16)) -> usize {
        self.draft
            .as_ref()
            .map(|draft| {
                review_lines(&draft.body)
                    .filter(|(_, literal)| *literal)
                    .map(|(line, _)| chrome::str_cols(line))
                    .max()
                    .unwrap_or(0)
                    .saturating_sub(code_room(term))
            })
            .unwrap_or(0)
    }

    fn complete(&mut self, result: Outcome) -> bool {
        self.pending = None;
        match result {
            Outcome::Auth(auth) => {
                self.auth = Some(auth);
                if let Some(title) = self.preview_request.take() {
                    self.prepare(title);
                }
            }
            Outcome::Draft(Ok(draft)) => {
                self.draft = Some(draft);
                self.scroll = 0;
                self.pan = 0;
                self.notice.clear();
            }
            Outcome::Draft(Err(error)) => {
                if self.auth.is_none() {
                    self.auth = Some(false);
                }
                self.notice = error;
            }
            Outcome::Sent {
                issue,
                node,
                notice,
            } => {
                self.scroll = 0;
                self.pan = 0;
                self.issue = issue;
                self.node = node;
                self.notice = notice;
                return self.node.is_some() && (self.auth == Some(false) || self.issue.is_some());
            }
        }
        false
    }
}

pub(super) fn open(view: &mut View) {
    let frame = view
        .frames
        .get(&view.layout.focus)
        .filter(|frame| frame.geometry_ok())
        .map(crate::vt::frame_text)
        .unwrap_or_else(|| "[focused pane text unavailable]".into());
    let agent = view
        .layout
        .agents
        .iter()
        .find(|a| view.row_hosts_pane(a, view.layout.focus));
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
    let context = format!("## Build\nversion: {}\nrevision: {}\ndirty: {}\n\n## Focused row\n{}\n\n## Frame\n{}\n\n## Server log\n{}\n\n## Client log\n{}", BUILD_VERSION, env!("FNO_MUX_GIT_REV"), env!("FNO_MUX_GIT_DIRTY"), row, code_block(&frame),
        code_block(&crate::mux_log::tail(&server_log, 40)), code_block(&crate::mux_log::tail(&crate::mux_log::client_path(), 40)));
    view.clear_peek();
    let mut report = Report::new(
        context,
        crate::config_defaults::lookup_key("mux.report_parent"),
    );
    report.enqueue(async { Outcome::Auth(authenticated().await) });
    view.report = Some(report);
}

pub(super) fn keys(view: &mut View, bytes: &[u8]) {
    use agent_launcher::LKey;
    let Some(report) = view.report.as_mut() else {
        return;
    };
    if report.pending.is_some() && report.auth.is_some() {
        return;
    }
    if report.draft.is_some() {
        let limit = report.scroll_limit(view.term);
        report.scroll = report.scroll.min(limit);
        let pan_limit = report.pan_limit(view.term);
        report.pan = report.pan.min(pan_limit);
        for key in report.carry.fold_carry(bytes, bytes.is_empty()) {
            match key {
                LKey::Esc => {
                    if report.issue.is_some() || report.node.is_some() {
                        view.report = None;
                        return;
                    }
                    report.draft = None;
                    report.scroll = 0;
                    report.pan = 0;
                    report.notice.clear();
                }
                LKey::Up => report.scroll = report.scroll.saturating_sub(1),
                LKey::Down => report.scroll = report.scroll.saturating_add(1).min(limit),
                LKey::Left => report.pan = report.pan.saturating_sub(8),
                LKey::Right => report.pan = report.pan.saturating_add(8).min(pan_limit),
                LKey::Home => {
                    report.scroll = 0;
                    report.pan = 0;
                }
                LKey::End => report.scroll = limit,
                LKey::Enter => {
                    report.send();
                    break;
                }
                _ => {}
            }
        }
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
                if report.auth.is_none() {
                    report.preview_request = Some(title);
                } else {
                    report.prepare(title);
                }
                break;
            }
            FieldEvent::Submit(_) => {
                report.notice = "Describe the problem before reviewing.".into()
            }
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
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Outcome::Draft(
                    Err("The report command stopped. Your text is kept.".into()),
                )),
                Err(_) => None,
            }
        });
    if let Some(result) = result {
        if let Some(report) = view.report.as_mut() {
            let close = report.complete(result);
            let notice = report.notice.clone();
            if close {
                view.report = None;
            }
            if !notice.is_empty() {
                view.set_notice(notice);
            }
        }
        true
    } else {
        false
    }
}

pub(super) async fn snapshot(view: &mut View, preview: bool) -> Result<(), String> {
    async fn settle(report: &mut Report) -> Result<(), String> {
        if let Some((_, result)) = report.pending.take() {
            report.complete(result.await.map_err(|_| "report task stopped")?);
        }
        Ok(())
    }
    open(view);
    if let Some(report) = view.report.as_mut() {
        settle(report).await?;
    }
    keys(view, b"Pane stopped responding.");
    if preview {
        keys(view, b"\r");
        if let Some(report) = view.report.as_mut() {
            settle(report).await?;
            if report.draft.is_none() {
                return Err(report.notice.clone());
            }
        }
    }
    Ok(())
}

pub(super) fn draw(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    if let Some(report) = &view.report {
        draw_popup_overlay(
            cells,
            rows,
            cols,
            &report.popup(view.term),
            view.term,
            &view.theme,
        );
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
