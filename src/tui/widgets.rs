//! Rendering for each panel (CLAUDE.md sections 9 and 11).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, DeviceProbe, Focus, RunMode};
use crate::artifacts::ArtifactSlot;
use crate::terminal::LineKind;
use crate::workflow::state::{TOTAL_REAL_STEPS, WORKFLOW_ORDER};
use crate::workflow::{WorkflowState, WorkflowStatus};

pub fn render_header(f: &mut Frame, area: Rect, app: &App) {
    // In Dry Run, the header must reflect the simulated device, not
    // whatever's (or isn't) really plugged in — a real "No device" next
    // to an active simulated run would read as contradictory.
    let (status_text, status_style) = if app.mode() == RunMode::DryRun {
        (
            "SIMULATED".to_string(),
            Style::default()
                .fg(Color::Rgb(255, 170, 0))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        match &app.device_probe {
            DeviceProbe::NotChecked => {
                ("Checking...".to_string(), Style::default().fg(Color::Gray))
            }
            DeviceProbe::NoDevice => ("No device".to_string(), Style::default().fg(Color::Red)),
            DeviceProbe::MultipleDevices(_) => (
                "Multiple devices".to_string(),
                Style::default().fg(Color::Red),
            ),
            DeviceProbe::AdbError(e) => {
                (format!("ADB error: {e}"), Style::default().fg(Color::Red))
            }
            DeviceProbe::Connected {
                compatible: true, ..
            } => (
                "READY".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            DeviceProbe::Connected {
                compatible: false, ..
            } => ("Incompatible".to_string(), Style::default().fg(Color::Red)),
        }
    };
    let title_line = Line::from(vec![
        Span::styled(
            " Flip5 Root Manager ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ".repeat(area.width.saturating_sub(40).max(1) as usize / 10 + 1)),
        Span::raw("Device: "),
        Span::styled(status_text, status_style),
    ]);
    let mode_line = match app.mode() {
        // Real Run isn't visually loud — it's the existing, unremarkable
        // default behavior. Dry Run must be unmistakable (requirement:
        // visually distinguishable from real execution).
        RunMode::Real => Line::from(Span::raw(" Mode: REAL RUN")),
        RunMode::DryRun => Line::from(Span::styled(
            " Mode: DRY RUN — no device-changing commands will run",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 170, 0))
                .add_modifier(Modifier::BOLD),
        )),
    };
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(
        Paragraph::new(vec![title_line, mode_line]).block(block),
        area,
    );
}

pub fn render_device(f: &mut Frame, area: Rect, app: &App) {
    let lines = if app.mode() == RunMode::DryRun {
        let sim_style = Style::default()
            .fg(Color::Rgb(255, 170, 0))
            .add_modifier(Modifier::BOLD);
        vec![
            Line::from(format!("Serial : {}", crate::adb::dryrun::DRYRUN_SERIAL)),
            Line::from(format!("Model  : {}", crate::device::EXPECTED_MODEL)),
            Line::from(format!("Build  : {}", crate::device::EXPECTED_BUILD)),
            Line::from(Span::styled("Status : Simulated", sim_style)),
        ]
    } else {
        render_device_lines(app)
    };
    let block = Block::default().title("Device").borders(Borders::ALL);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_device_lines(app: &App) -> Vec<Line<'static>> {
    match &app.device_probe {
        DeviceProbe::NotChecked => vec![Line::from("Probing for device...")],
        DeviceProbe::NoDevice => vec![Line::from(Span::styled(
            "No device connected.",
            Style::default().fg(Color::Red),
        ))],
        DeviceProbe::MultipleDevices(serials) => {
            vec![Line::from(Span::styled(
                format!("Multiple devices connected: {}", serials.join(", ")),
                Style::default().fg(Color::Red),
            ))]
        }
        DeviceProbe::AdbError(e) => vec![Line::from(Span::styled(
            format!("ADB error: {e}"),
            Style::default().fg(Color::Red),
        ))],
        DeviceProbe::Connected {
            serial,
            model,
            build,
            compatible,
            detail,
        } => {
            let mut lines = vec![Line::from(format!("Serial : {serial}"))];
            if *compatible {
                lines.push(Line::from(format!("Model  : {model}")));
                lines.push(Line::from(format!("Build  : {build}")));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("DeviceMismatch: {}", detail.clone().unwrap_or_default()),
                    Style::default().fg(Color::Red),
                )));
            }
            lines
        }
    }
}

/// Humanized byte count (e.g. `130.0KB`, `4.7MB`) — more compact and more
/// readable at a glance than a raw byte count, important now that each
/// artifact has to fit on one line.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / MB)
    }
}

/// Renders one artifact as compactly as the available `inner_width`
/// allows: a single row when everything fits, otherwise a 2-line block
/// (type label, then `filename · size · hash` indented below it). Either
/// way the artifact type stays visually distinct from the filename, and
/// only a shortened hash is ever shown — the full relative path and full
/// SHA-256 stay on `ArtifactInfo` (see `crate::artifacts`) for real use
/// (selection, verification), never truncated there.
fn artifact_block(label: &str, slot: &ArtifactSlot, inner_width: usize) -> Vec<Line<'static>> {
    match slot {
        ArtifactSlot::Found(info) => {
            // The filename alone (not the full relative path) is enough
            // to identify the artifact at a glance once size + hash are
            // shown too, and keeps it from dominating the line.
            let filename = std::path::Path::new(info.relative_path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| info.relative_path.to_string());
            let size = format_size(info.size_bytes);
            let hash8 = &info.sha256[..8.min(info.sha256.len())];

            let single_row = format!("{label} \u{b7} {filename} \u{b7} {size} \u{b7} {hash8}");
            if single_row.chars().count() + 2 <= inner_width {
                vec![Line::from(vec![
                    Span::styled("✓ ", Style::default().fg(Color::Green)),
                    Span::raw(single_row),
                ])]
            } else {
                vec![
                    Line::from(vec![
                        Span::styled("✓ ", Style::default().fg(Color::Green)),
                        Span::raw(label.to_string()),
                    ]),
                    Line::from(format!("  {filename} \u{b7} {size} \u{b7} {hash8}")),
                ]
            }
        }
        ArtifactSlot::Missing { expected_path, .. } => vec![
            Line::from(vec![
                Span::styled("✗ ", Style::default().fg(Color::Red)),
                Span::raw(label.to_string()),
            ]),
            Line::from(Span::styled(
                format!("  MISSING: {}", expected_path.display()),
                Style::default().fg(Color::Red),
            )),
        ],
    }
}

pub fn render_artifacts(f: &mut Frame, area: Rect, app: &App) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::with_capacity(10);
    lines.extend(artifact_block("Payload lib", &app.payload_lib, inner_width));
    lines.extend(artifact_block(
        "Payload runner",
        &app.payload_runner_artifact,
        inner_width,
    ));
    lines.extend(artifact_block(
        "KernelSU",
        &app.kernelsu_artifact,
        inner_width,
    ));
    if let Some(git) = &app.git_info {
        let repo_name = app
            .payload_repo
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| app.payload_repo.root.display().to_string());
        lines.push(Line::from(format!("Repo: {repo_name}")));
        lines.push(Line::from(format!(
            "{} \u{b7} {}",
            &git.commit[..8.min(git.commit.len())],
            if git.is_dirty { "dirty" } else { "clean" }
        )));
    }
    let block = Block::default()
        .title("Artifacts (read-only)")
        .borders(Borders::ALL);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The pinned summary block shown at the top of the Workflow panel,
/// *always* visible regardless of how the step list below it is
/// scrolled (requirement: the user must always be able to tell how many
/// steps are done, what's current/next, how many remain, and whether
/// anything failed, without scrolling).
/// Renders the single "Optional integrations" row for the post-root
/// hybrid_mount/ViPER4Android-RE integration. This only ever appears
/// after the mandatory root workflow has reached `Completed` — never
/// before, and never as part of the mandatory checklist itself.
fn optional_integration_lines(app: &App) -> Vec<Line<'static>> {
    use crate::workflow::OptionalIntegrationStatus;
    let line = match app.runner.hybrid_mount_status() {
        OptionalIntegrationStatus::NotOffered => Line::from(vec![
            Span::styled("○ ", Style::default().fg(Color::DarkGray)),
            Span::raw("hybrid_mount / ViPER4Android configuration"),
        ]),
        // Root is already done; this is purely optional from here. Spell
        // that out right on the row (not just in the footer/terminal) so
        // it can never be mistaken for a required remaining step.
        OptionalIntegrationStatus::AwaitingChoice => {
            return vec![
                Line::from(vec![
                    Span::styled(
                        "○ ",
                        Style::default()
                            .fg(Color::Rgb(255, 170, 0))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "hybrid_mount / ViPER4Android configuration — optional",
                        Style::default()
                            .fg(Color::Rgb(255, 170, 0))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    "  Enter = configure it now   K = finish here (exploit already succeeded)",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
        }
        OptionalIntegrationStatus::Skipped => Line::from(Span::styled(
            "○ hybrid_mount integration skipped",
            Style::default().fg(Color::DarkGray),
        )),
        OptionalIntegrationStatus::NotInstalled => Line::from(Span::styled(
            "- hybrid_mount not installed; integration skipped",
            Style::default().fg(Color::DarkGray),
        )),
        OptionalIntegrationStatus::Running => Line::from(Span::styled(
            "▶ hybrid_mount / ViPER4Android configuration (running...)",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        OptionalIntegrationStatus::Succeeded => Line::from(Span::styled(
            "✓ hybrid_mount / ViPER4Android configured",
            Style::default().fg(Color::Green),
        )),
        OptionalIntegrationStatus::Failed { detail } => {
            return vec![
                Line::from(Span::styled(
                    format!("✗ hybrid_mount integration failed: {detail}"),
                    Style::default().fg(Color::Red),
                )),
                Line::from(Span::styled(
                    "(root workflow still succeeded)",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
        }
    };
    vec![line]
}

fn workflow_summary_lines(
    app: &App,
    status: &WorkflowStatus,
    display_idx: usize,
) -> Vec<Line<'static>> {
    let remaining = TOTAL_REAL_STEPS.saturating_sub(display_idx);

    // A step is actively executing: show the prominent, reverse-video
    // "current step" line (name, attempt count, elapsed time) in place
    // of the plain Current/Next/Remaining triplet.
    if let Some((active_state, started)) = app.active_step {
        let (attempts, max) = app.runner.retry_info(active_state);
        let elapsed = started.elapsed().as_secs();
        return vec![
            Line::from(Span::styled(
                format!(
                    "▶ {}   Attempt {attempts}/{max}   {:02}:{:02}",
                    active_state.label(),
                    elapsed / 60,
                    elapsed % 60
                ),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
    }

    match status {
        // Note: the "Optional integrations" / hybrid_mount line is
        // deliberately *not* added here. The pinned summary's whole
        // purpose is a short, always-visible status strip — it is not
        // where step-like content belongs. hybrid_mount's status is
        // appended after the mandatory checklist instead (see
        // `render_workflow`), matching the original mockup's order:
        // mandatory steps first, then the completion banner, then the
        // optional integration last.
        WorkflowStatus::Running(WorkflowState::Completed) => vec![
            Line::from(Span::styled(
                "✓ Root workflow completed",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!("Completed: {TOTAL_REAL_STEPS}/{TOTAL_REAL_STEPS}")),
            Line::from(Span::styled(
                "Status: SUCCESS",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
        ],
        WorkflowStatus::Failed { failure, .. } => vec![
            Line::from(Span::styled(
                format!("✗ Failed: {} — press R to retry", failure.label()),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!("Current: {}", WORKFLOW_ORDER[display_idx].label())),
            Line::from(format!("Remaining: {remaining}")),
        ],
        WorkflowStatus::Running(WorkflowState::Disconnected) => {
            let hint = match &app.device_probe {
                DeviceProbe::Connected {
                    compatible: true, ..
                } => "Device ready — press Enter to begin",
                _ => "Waiting for a compatible device — press Enter once connected",
            };
            vec![
                Line::from(Span::styled(hint, Style::default().fg(Color::Gray))),
                Line::from(""),
            ]
        }
        WorkflowStatus::Running(_) => {
            let next_label = WORKFLOW_ORDER
                .get(display_idx + 1)
                .map(|s| s.label())
                .unwrap_or("-");
            vec![
                Line::from(format!("Current: {}", WORKFLOW_ORDER[display_idx].label())),
                Line::from(format!("Next: {next_label}")),
                Line::from(format!("Remaining: {remaining}")),
            ]
        }
    }
}

/// One rendered step-list row (marker + label, already styled). Built in
/// full every render (only 16 items — negligible cost) and then sliced
/// down to whatever window `workflow_scroll` currently points at, so the
/// list can never render outside the panel.
fn workflow_step_rows(
    app: &App,
    status: &WorkflowStatus,
    display_idx: usize,
) -> Vec<Line<'static>> {
    let failed_target = match status {
        WorkflowStatus::Failed { last_good, failure } => {
            Some((last_good.normal_next(), failure.clone()))
        }
        WorkflowStatus::Running(_) => None,
    };
    // Only once the workflow has actually started (left
    // `Running(Disconnected)`) may a step be marked "→ next"/"▶ active" —
    // before that, the pre-start hotplug shortcut in `display_idx` may
    // legitimately show detection/verification as already satisfied, but
    // nothing is actually running, so every other step must stay a plain
    // "○", including "Payload selection".
    let workflow_started = !matches!(status, WorkflowStatus::Running(WorkflowState::Disconnected));

    WORKFLOW_ORDER
        .iter()
        .enumerate()
        .filter(|(_, state)| {
            **state != WorkflowState::Disconnected && **state != WorkflowState::Completed
        })
        .map(|(idx, state)| {
            let is_failed_target = matches!(&failed_target, Some((Some(t), _)) if t == state);
            let is_active = workflow_started && app.is_step_running() && idx == display_idx + 1;
            let (marker, style) = if idx <= display_idx {
                ("✓", Style::default().fg(Color::Green))
            } else if is_failed_target {
                ("✗", Style::default().fg(Color::Red))
            } else if is_active {
                // Reverse-video across the whole line so the currently
                // executing step is unmistakable, distinct from "→".
                (
                    "▶",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if workflow_started && idx == display_idx + 1 {
                (
                    "→",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("○", Style::default().fg(Color::DarkGray))
            };
            Line::from(Span::styled(format!("{marker} {}", state.label()), style))
        })
        .collect()
}

/// Renders the Workflow panel: a pinned summary (always fully visible)
/// followed by a bounded, independently scrollable step list. Returns the
/// step list's viewport height in rows, so `tui::draw` can record it for
/// PageUp/PageDown/Home/End to page by the actual rendered size.
pub fn render_workflow(f: &mut Frame, area: Rect, app: &App) -> usize {
    let status = app.runner.status();
    let display_idx = app.effective_progress();

    let summary = workflow_summary_lines(app, &status, display_idx);
    let rows = workflow_step_rows(app, &status, display_idx);

    let inner_height = area.height.saturating_sub(2) as usize; // minus borders
    let list_viewport = inner_height.saturating_sub(summary.len());

    let max_scroll = crate::terminal::ScrollState::max_scroll(rows.len(), list_viewport);
    let top = app.workflow_scroll.offset.min(max_scroll);
    let end = (top + list_viewport).min(rows.len());

    let mut lines = summary;
    if list_viewport > 0 {
        lines.extend_from_slice(&rows[top..end]);
    }

    // A failure's detail text is appended as part of the scrollable list
    // content (it can be long/wrap) rather than the pinned summary, which
    // stays a fixed, predictable size.
    if let WorkflowStatus::Failed { failure, .. } = &status
        && let Some(detail) = failure.detail()
        && end >= rows.len()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            detail.to_string(),
            Style::default().fg(Color::Red),
        )));
    }

    // The optional, post-root hybrid_mount/ViPER4Android-RE integration
    // comes *after* the mandatory checklist, never before it — matching
    // the order root actually happens in: 13 mandatory steps, then root
    // completion, then (only once offered) the optional integration.
    // Shown once the mandatory list is fully scrolled into view, same
    // gating as the failure detail above.
    if matches!(status, WorkflowStatus::Running(WorkflowState::Completed)) && end >= rows.len() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Optional integrations",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.extend(optional_integration_lines(app));
    }

    let focused = app.focus == Focus::Workflow;
    let more_below = end < rows.len();
    let more_above = top > 0;
    let scroll_hint = match (more_above, more_below) {
        (true, true) => " [more above/below]",
        (true, false) => " [more above]",
        (false, true) => " [more below]",
        (false, false) => "",
    };
    let shown_progress = display_idx.min(TOTAL_REAL_STEPS);
    let title = format!("Workflow ({shown_progress}/{TOTAL_REAL_STEPS}){scroll_hint}");
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        });
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    list_viewport
}

fn line_style(kind: LineKind) -> Style {
    match kind {
        LineKind::Info => Style::default().fg(Color::Gray),
        LineKind::HostCommand => Style::default().fg(Color::Cyan),
        LineKind::DeviceCommand => Style::default().fg(Color::Blue),
        LineKind::RootCommand => Style::default().fg(Color::Magenta),
        LineKind::Stdout => Style::default().fg(Color::White),
        LineKind::Stderr => Style::default().fg(Color::Yellow),
        LineKind::Success => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        LineKind::Warning => Style::default().fg(Color::Rgb(255, 170, 0)),
        LineKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

/// Renders the Terminal Output panel. Returns the viewport height in rows
/// that was actually used, so `tui::draw` can record it for
/// PageUp/PageDown/Home/End to page by the real rendered size rather than
/// a guessed constant.
pub fn render_terminal(f: &mut Frame, area: Rect, app: &App) -> usize {
    let height = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .log
        .visible_window(height)
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                format!("[{}] {}", l.timestamp.format("%H:%M:%S"), l.text),
                line_style(l.kind),
            ))
        })
        .collect();

    let focused = app.focus == Focus::Log;
    let follow = if app.log.is_at_bottom(height) {
        ""
    } else {
        " [scroll]"
    };
    let block = Block::default()
        .title(format!("Terminal Output{follow}"))
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        });
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    height
}

pub fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    // The hint names whichever mode `D` would switch *to*, so it always
    // reads as an instruction rather than a static label.
    let toggle_to = match app.mode() {
        RunMode::Real => "Dry Run",
        RunMode::DryRun => "Real Run",
    };
    // Terminal Output and the Workflow step list scroll independently
    // now, so the footer names which one PgUp/PgDn/Home/End currently
    // apply to (whichever panel has focus — its border is also
    // highlighted cyan).
    let focus_name = match app.focus {
        Focus::Log => "Terminal",
        Focus::Workflow => "Workflow",
    };
    let text = if app.runner.hybrid_mount_status()
        == crate::workflow::OptionalIntegrationStatus::AwaitingChoice
    {
        "[Enter] Configure hybrid_mount/ViPER4Android (optional)  [K] Finish — exploit already succeeded  [Tab] Focus  [PgUp/PgDn] Scroll  [C] Clear  [Q] Quit"
            .to_string()
    } else {
        format!(
            "[Enter] Run  [D] {toggle_to}  [R] Retry  [S] Stop  [Tab] Focus: Terminal/Workflow (now {focus_name})  [PgUp/PgDn/Home/End] Scroll  [C] Clear  [Q] Quit"
        )
    };
    let mut spans = vec![Span::raw(text)];
    if let Some(err) = &app.last_error {
        spans = vec![Span::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )];
    }
    let block = Block::default().borders(Borders::ALL);
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactInfo, ArtifactKind};
    use std::path::PathBuf;

    fn lines_to_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn fake_found(relative_path: &'static str, size_bytes: u64) -> ArtifactSlot {
        ArtifactSlot::Found(ArtifactInfo {
            kind: ArtifactKind::PayloadLibrary,
            name: "test",
            relative_path,
            absolute_path: PathBuf::from("/dev/null"),
            size_bytes,
            sha256: "86d8ff59156eecabbbc36368c83b9728f7a75e51cabbcc02e250cd89bb885c4".to_string(),
            repository_location: PathBuf::from("/dev/null"),
        })
    }

    #[test]
    fn format_size_matches_expected_units() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(133_018), "129.9 KB");
        assert_eq!(format_size(4_928_297), "4.7 MB");
    }

    #[test]
    fn artifact_block_uses_a_single_row_when_it_fits() {
        let slot = fake_found("build/x/cve-2026-43499-app.so", 133_018);
        let lines = artifact_block("Payload lib", &slot, 120);
        let rendered = lines_to_strings(&lines);
        assert_eq!(
            rendered.len(),
            1,
            "should collapse to one row when width allows: {rendered:?}"
        );
        assert!(rendered[0].contains("Payload lib"));
        assert!(rendered[0].contains("cve-2026-43499-app.so"));
        assert!(rendered[0].contains("129.9 KB"));
        assert!(rendered[0].contains("86d8ff59"));
        // The four fields must be separated by middle dots, in the exact
        // preferred order: label · filename · size · hash.
        assert_eq!(
            rendered[0],
            "✓ Payload lib \u{b7} cve-2026-43499-app.so \u{b7} 129.9 KB \u{b7} 86d8ff59"
        );
        // Never the full 64-char hash.
        assert!(
            !rendered[0]
                .contains("86d8ff59156eecabbbc36368c83b9728f7a75e51cabbcc02e250cd89bb885c4")
        );
    }

    #[test]
    fn artifact_block_falls_back_to_two_lines_when_narrow() {
        let slot = fake_found("build/x/cve-2026-43499-app.so", 133_018);
        let lines = artifact_block("Payload lib", &slot, 20);
        let rendered = lines_to_strings(&lines);
        assert_eq!(
            rendered.len(),
            2,
            "should wrap to a compact block when width is tight: {rendered:?}"
        );
        assert!(rendered[0].contains("Payload lib"));
        assert!(
            !rendered[0].contains("cve-2026-43499-app.so"),
            "filename must not share the label row here"
        );
        assert!(rendered[1].contains("cve-2026-43499-app.so"));
        assert!(rendered[1].contains("129.9 KB"));
        assert!(rendered[1].contains("86d8ff59"));
    }

    #[test]
    fn artifact_block_handles_a_long_filename_without_truncating_ambiguously() {
        let slot = fake_found(
            "build/x/a-very-long-and-descriptive-artifact-filename-indeed.bin",
            10,
        );
        // Even at a generous width this won't collapse to one row because
        // the filename alone is long; it must still show the *entire*
        // filename on the second line rather than cutting it short.
        let lines = artifact_block("Payload lib", &slot, 40);
        let rendered = lines_to_strings(&lines);
        assert_eq!(rendered.len(), 2);
        assert!(rendered[1].contains("a-very-long-and-descriptive-artifact-filename-indeed.bin"));
    }

    #[test]
    fn artifact_block_missing_is_never_mistaken_for_found() {
        let slot = ArtifactSlot::Missing {
            kind: ArtifactKind::PayloadLibrary,
            expected_path: PathBuf::from("/x/y.so"),
        };
        let lines = artifact_block("Payload lib", &slot, 120);
        let rendered = lines_to_strings(&lines);
        assert!(rendered.iter().any(|l| l.contains("MISSING")));
    }
}
