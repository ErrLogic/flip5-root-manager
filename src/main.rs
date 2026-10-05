//! Entry point: terminal setup, startup device probing (never the
//! workflow itself — CLAUDE.md section 30), and the main event loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

use flip5_root_manager::adb::{AdbClient, DryRunScenario, RealAdbClient};
use flip5_root_manager::app::App;
use flip5_root_manager::artifacts::PayloadRepository;
use flip5_root_manager::tui;
use flip5_root_manager::tui::events::map_key;

/// How often to re-probe ADB for device connect/disconnect while the TUI
/// is running (hotplug: no restart needed to notice a newly connected or
/// removed device). This only refreshes the header's connectivity
/// display — it never advances the workflow state machine, which still
/// only moves when the user presses Enter.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(1500);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let payload_repo_root = resolve_payload_repo_root()?;
    let payload_repo = PayloadRepository::new(payload_repo_root);
    let staging_dir = std::env::temp_dir().join("flip5-root-manager-staging");

    let adb: Arc<dyn AdbClient> = Arc::new(RealAdbClient::new());
    // Dry Run's failure-simulation scenario is a development/testing aid
    // only: off by default, and settable only via an environment
    // variable, never through the TUI itself.
    let dryrun_scenario = DryRunScenario::from_env();
    let mut app = App::new(adb, payload_repo, staging_dir, dryrun_scenario);
    // Read-only startup probe only; the workflow state machine itself
    // remains at `Disconnected` until the user explicitly presses Enter.
    app.probe_device_at_startup().await;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Resolves the read-only payload repository path (CLAUDE.md section 2).
/// Honors `FLIP5_PAYLOAD_REPO` if set, otherwise assumes the sibling
/// directory layout from the example workspace:
/// `workspace/{Root-My-Galaxy-Payloads, flip5-root-manager}`.
fn resolve_payload_repo_root() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("FLIP5_PAYLOAD_REPO") {
        return Ok(PathBuf::from(p));
    }
    std::env::current_dir()?
        .parent()
        .map(|p| p.join("Root-My-Galaxy-Payloads"))
        .ok_or_else(|| {
            anyhow::anyhow!("could not determine payload repository path; set FLIP5_PAYLOAD_REPO")
        })
}

async fn run_event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    let mut events = EventStream::new();
    // A persistent interval (not recreated each loop iteration) so the
    // hotplug poll fires on a steady ~1.5s cadence regardless of how
    // often the other branches below win (see `tokio::time::Interval`
    // semantics vs. a freshly-constructed `sleep`).
    let mut device_poll = tokio::time::interval(DEVICE_POLL_INTERVAL);
    device_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        terminal.draw(|f| tui::draw(f, app))?;
        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            maybe_event = events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event
                    && key.kind == KeyEventKind::Press
                {
                    app.handle_action(map_key(key.code));
                }
            }
            Some(event) = app.recv_runner_event() => {
                app.apply_runner_event(event);
            }
            _ = device_poll.tick() => {
                // Skip while a step is actively running, to avoid piling
                // extra `adb` calls on top of a long-running operation;
                // the next tick picks it back up a moment later.
                if !app.is_step_running() {
                    app.refresh_device_probe().await;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                // Periodic tick only, so elapsed-time displays keep
                // moving while a long-running step executes.
            }
        }

        app.poll_active_task().await;
        app.drain_runner_events();
        // Once the user has pressed Enter to start the workflow, keep it
        // moving on its own — see `App::auto_continue` for exactly when
        // this is (and isn't) allowed to act.
        app.auto_continue();
    }
}
