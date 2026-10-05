//! Terminal UI: presentation layer only. Reads [`crate::app::App`] state
//! and renders it; all mutation happens in `app`'s update methods
//! (CLAUDE.md section 32).

pub mod events;
pub mod layout;
pub mod widgets;

use ratatui::Frame;

use crate::app::App;

/// Renders every panel. Also records the Terminal Output and Workflow
/// panels' actual rendered viewport heights back onto `app`, so keyboard
/// scrolling (which happens outside of rendering, in response to key
/// events) always pages by the real current size instead of a guessed
/// constant — see `App::terminal_viewport_height` /
/// `App::workflow_viewport_height`.
pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = layout::compute(f.area());
    widgets::render_header(f, chunks.header, app);
    widgets::render_device(f, chunks.device, app);
    widgets::render_artifacts(f, chunks.artifacts, app);
    app.workflow_viewport_height = widgets::render_workflow(f, chunks.workflow, app);
    app.terminal_viewport_height = widgets::render_terminal(f, chunks.terminal, app);
    widgets::render_footer(f, chunks.footer, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::adb::AdbClient;
    use crate::adb::mock::{MockAdbClient, MockState};
    use crate::artifacts::{ArtifactSlot, PayloadRepository};

    fn render_to_string_sized(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_to_string(app: &mut App) -> String {
        render_to_string_sized(app, 160, 45)
    }

    async fn app_with(state: MockState) -> App {
        let adb: Arc<dyn AdbClient> = Arc::new(MockAdbClient::new(state));
        let staging = std::env::temp_dir().join(format!(
            "flip5-tui-render-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut app = App::new(
            adb,
            PayloadRepository::new("/tmp/flip5-tui-render-test-nonexistent-repo"),
            staging,
            crate::adb::DryRunScenario::AllSuccess,
        );
        app.probe_device_at_startup().await;
        app
    }

    #[tokio::test]
    async fn renders_without_panicking_when_no_device() {
        let mut app = app_with(MockState {
            device_state: None,
            ..Default::default()
        })
        .await;
        let out = render_to_string(&mut app);
        assert!(out.contains("No device"));
        assert!(out.contains("Workflow (0/13)"));
    }

    /// Finds the rendered line containing `label` and returns the glyph
    /// immediately to its left (the "✓"/"○"/"→"/"▶"/"✗" marker), skipping
    /// whitespace and the panel's box-drawing border character.
    fn marker_before(out: &str, label: &str) -> char {
        // The compact status line at the top of the panel may also
        // mention the label (e.g. "Next: Payload selection — press
        // Enter"); the checklist row itself is always the *last* line
        // containing the label.
        let line = out
            .lines()
            .rev()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("label {label:?} not found"));
        let idx = line.find(label).unwrap();
        line[..idx]
            .chars()
            .rev()
            .find(|c| !c.is_whitespace() && *c != '│')
            .unwrap_or(' ')
    }

    #[tokio::test]
    async fn pre_start_checklist_reflects_hotplug_probe_without_touching_workflow_state() {
        let mut app = app_with(MockState::default()).await;
        let out = render_to_string(&mut app);
        // Header reflects the new, more meaningful status text.
        assert!(out.contains("READY"));
        // Both detection steps show satisfied even though the workflow
        // itself has not started (CLAUDE.md section 30 — only Enter
        // starts it; this is display-only, verified separately in
        // app::tests).
        assert!(out.contains("Workflow (2/13)"));
        assert_eq!(marker_before(&out, "Device detection"), '✓');
        assert_eq!(marker_before(&out, "Device verification"), '✓');
        // The workflow has not actually started: "Payload selection"
        // must NOT be marked as the active/next step ("→"), even though
        // detection/verification show satisfied above it. It must read
        // as plain pending, exactly like every other step that hasn't
        // run yet.
        assert_eq!(
            marker_before(&out, "Payload selection"),
            '○',
            "Payload selection must not be marked active before Enter is pressed"
        );
        assert_eq!(
            app.runner.status(),
            crate::workflow::WorkflowStatus::Running(crate::workflow::WorkflowState::Disconnected)
        );
    }

    #[tokio::test]
    async fn after_the_workflow_actually_starts_payload_selection_becomes_the_active_next_step() {
        let mut app = app_with(MockState::default()).await;
        // Drive the real first two steps (what pressing Enter twice
        // ultimately does), rather than relying on the pre-start probe
        // shortcut.
        app.runner.detect_device().await.unwrap();
        app.runner.verify_device().await.unwrap();
        assert_eq!(
            app.runner.status(),
            crate::workflow::WorkflowStatus::Running(
                crate::workflow::WorkflowState::DeviceVerified
            )
        );

        let out = render_to_string(&mut app);
        assert_eq!(marker_before(&out, "Device detection"), '✓');
        assert_eq!(marker_before(&out, "Device verification"), '✓');
        assert_eq!(
            marker_before(&out, "Payload selection"),
            '→',
            "once the workflow has genuinely started, Payload selection must be the marked next step"
        );
    }

    #[tokio::test]
    async fn renders_without_panicking_when_failed() {
        let mut app = app_with(MockState {
            model: "SM-WRONG".to_string(),
            ..Default::default()
        })
        .await;
        app.runner.detect_device().await.unwrap();
        app.runner.verify_device().await.unwrap();
        let out = render_to_string(&mut app);
        assert!(out.contains("Failed"));
        assert!(out.contains("retry"));
    }

    #[tokio::test]
    async fn dry_run_mode_shows_a_simulated_device_everywhere() {
        let mut app = app_with(MockState {
            device_state: None,
            ..Default::default()
        })
        .await;
        app.toggle_mode();
        let out = render_to_string(&mut app);

        assert!(
            out.contains("Device: SIMULATED"),
            "header must show SIMULATED in Dry Run"
        );
        assert!(
            out.contains(crate::adb::dryrun::DRYRUN_SERIAL),
            "Device panel must show the simulated serial"
        );
        assert!(out.contains(crate::device::EXPECTED_MODEL));
        assert!(out.contains(crate::device::EXPECTED_BUILD));
        assert!(out.contains("Status : Simulated"));
        // The mode banner from the previous refinement is still present.
        assert!(out.contains("Mode: DRY RUN"));
    }

    #[tokio::test]
    async fn real_mode_never_shows_simulated_device_info() {
        let mut app = app_with(MockState::default()).await;
        let out = render_to_string(&mut app);
        assert!(!out.contains("SIMULATED"));
        assert!(!out.contains(crate::adb::dryrun::DRYRUN_SERIAL));
    }

    /// A render test against the real (read-only) payload repository so
    /// the artifact blocks actually resolve to `ArtifactSlot::Found`
    /// rather than `Missing`.
    async fn app_with_real_repo() -> App {
        let adb: Arc<dyn AdbClient> = Arc::new(MockAdbClient::new(MockState::default()));
        let staging = std::env::temp_dir().join(format!(
            "flip5-tui-render-artifacts-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let repo_root = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .join("Root-My-Galaxy-Payloads");
        App::new(
            adb,
            PayloadRepository::new(repo_root),
            staging,
            crate::adb::DryRunScenario::AllSuccess,
        )
    }

    #[tokio::test]
    async fn artifact_panel_never_shows_the_full_sha256() {
        let mut app = app_with_real_repo().await;
        let out = render_to_string(&mut app);

        assert!(out.contains("Payload lib"));
        assert!(out.contains("cve-2026-43499-app.so"));
        assert!(
            out.contains("86d8ff59"),
            "shortened hash must still be visible"
        );
        assert!(
            out.contains('\u{b7}'),
            "size and hash should be joined with a middle dot"
        );

        // Requirement: the full SHA-256 must never appear in the compact
        // panel, only the 8-char prefix — but it must still be preserved
        // internally (e.g. for real selection/verification use).
        if let ArtifactSlot::Found(info) = &app.payload_lib {
            assert!(
                !out.contains(info.sha256.as_str()),
                "the full SHA-256 must not be displayed in the compact artifacts panel"
            );
            assert_eq!(info.sha256.len(), 64);
        } else {
            panic!("expected the payload library artifact to be found against the real repo");
        }

        // Repo metadata stays readable.
        assert!(out.contains("Repo: Root-My-Galaxy-Payloads"));
    }

    #[tokio::test]
    async fn artifact_panel_uses_a_single_row_per_artifact_when_width_allows() {
        let mut app = app_with_real_repo().await;
        // The default (wide) test terminal: the left column is a fixed
        // width regardless of overall terminal width, and is comfortably
        // wide enough for the single-row format.
        let out = render_to_string(&mut app);
        let label_line = out.lines().find(|l| l.contains("Payload lib")).unwrap();
        assert!(
            label_line.contains("cve-2026-43499-app.so"),
            "at normal width, filename/size/hash should share the label's single row: {label_line:?}"
        );
    }

    #[tokio::test]
    async fn renders_without_panicking_at_a_very_narrow_terminal_width() {
        // The exact per-artifact single-row-vs-2-line responsive choice
        // is covered precisely (with a controlled inner width) by
        // `widgets::tests`; this just proves the whole TUI stays usable
        // — no panics, no divide-by-zero in the layout/scroll math — when
        // resized much smaller than normal (requirement: "the overall
        // TUI should remain usable when the terminal window is resized
        // smaller").
        let mut app = app_with_real_repo().await;
        // At this width the fixed-width left column may get compressed
        // enough that individual labels aren't guaranteed to be legible
        // — the point of this test is solely that rendering an extreme
        // size never panics (e.g. a width/height-based division or
        // subtraction underflowing).
        let out = render_to_string_sized(&mut app, 45, 20);
        assert!(!out.trim().is_empty());
    }

    #[tokio::test]
    async fn workflow_summary_stays_visible_even_when_the_list_is_scrolled_away() {
        let mut app = app_with(MockState::default()).await;
        app.runner.detect_device().await.unwrap();
        app.runner.verify_device().await.unwrap();
        // Force the workflow list scroll far from the top, as if the user
        // had manually scrolled down to inspect later (pending) steps.
        app.workflow_scroll.offset = 20;
        let out = render_to_string(&mut app);

        // The pinned summary (Current/Next/Remaining) must still be
        // present regardless of where the scrollable list is positioned.
        assert!(out.contains("Current: Device verification"));
        assert!(out.contains("Next:"));
        assert!(out.contains("Remaining:"));
    }

    #[tokio::test]
    async fn workflow_panel_shows_a_scroll_hint_when_steps_overflow_the_viewport() {
        let mut app = app_with(MockState::default()).await;
        // A short terminal compresses the workflow panel below the 16
        // rows needed to show every step without scrolling.
        let out = render_to_string_sized(&mut app, 160, 20);
        assert!(
            out.contains("more below") || out.contains("more above"),
            "expected a scroll hint when the step list overflows its viewport:\n{out}"
        );
        // The panel itself must never render steps outside its own
        // bordered box — i.e. the last visible step row must appear
        // before the terminal/footer panels' content in the buffer. This
        // is implicitly guaranteed by rendering into a fixed-size
        // TestBackend without panicking; an explicit marker check is a
        // light extra safety net.
        assert!(out.contains("Workflow ("));
    }

    #[tokio::test]
    async fn workflow_completion_summary_is_visible_without_scrolling() {
        use crate::artifacts::{ArtifactInfo, ArtifactKind};
        use std::path::PathBuf;

        fn fake(kind: ArtifactKind) -> ArtifactSlot {
            ArtifactSlot::Found(ArtifactInfo {
                kind,
                name: kind.display_name(),
                relative_path: kind.relative_path(),
                absolute_path: PathBuf::from("/dev/null"),
                size_bytes: 1,
                sha256: "0".repeat(64),
                repository_location: PathBuf::from("/dev/null"),
            })
        }

        let mut app = app_with(MockState::default()).await;
        let r = &app.runner;
        r.detect_device().await.unwrap();
        r.verify_device().await.unwrap();
        r.select_payload_artifacts(
            fake(ArtifactKind::PayloadLibrary),
            fake(ArtifactKind::PayloadRunner),
        )
        .unwrap();
        r.push_payload().await.unwrap();
        r.execute_payload().await.unwrap();
        r.select_kernelsu_artifact(fake(ArtifactKind::KernelSu))
            .unwrap();
        r.push_kernelsu().await.unwrap();
        r.stage_kernelsu().await.unwrap();
        r.load_kernelsu().await.unwrap();
        r.verify_kernelsu().await.unwrap();
        r.verify_root().await.unwrap();
        r.cleanup_mount().await.unwrap();
        r.complete().unwrap();
        assert!(app.runner.status().is_completed());

        let out = render_to_string(&mut app);
        assert!(out.contains("Workflow (13/13)"));
        assert!(out.contains("Completed: 13/13"));
        assert!(out.contains("Status: SUCCESS"));
        // The optional integration must be offered, never run
        // automatically just because the mandatory workflow completed.
        assert!(out.contains("Optional integrations"));
        assert!(out.contains("hybrid_mount"));

        // Ordering: the mandatory checklist (e.g. "Cleanup", the last
        // mandatory step) must render *before* "Optional integrations" —
        // not after it.
        let cleanup_pos = out.find("Cleanup").expect("Cleanup row must be present");
        let optional_pos = out
            .find("Optional integrations")
            .expect("Optional integrations heading must be present");
        assert!(
            cleanup_pos < optional_pos,
            "the mandatory checklist must come before Optional integrations, not after"
        );
    }
}
