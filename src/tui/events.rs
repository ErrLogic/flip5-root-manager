//! Keyboard control mapping (CLAUDE.md section 10). Kept as a pure
//! function from `KeyCode` to `AppAction` so it is testable without a
//! terminal.

use crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    /// Confirm / execute: run the next applicable step, or confirm an
    /// artifact selection.
    Enter,
    /// Toggle between Real Run and Dry Run (only before the workflow has
    /// started).
    ToggleMode,
    /// Retry the current failed step.
    Retry,
    /// Stop the current running operation.
    Stop,
    /// Change focus between panels.
    ToggleFocus,
    /// Scroll logs up (PageUp).
    ScrollUp,
    /// Scroll logs down (PageDown).
    ScrollDown,
    /// Jump to log beginning (Home).
    ScrollToTop,
    /// Jump to log end (End) — re-enables auto-follow.
    ScrollToEnd,
    /// Clear the visible log panel.
    ClearLog,
    /// Decline the optional post-root integration (hybrid_mount/
    /// ViPER4Android-RE) once it's been offered.
    SkipOptionalIntegration,
    /// Quit the application.
    Quit,
    /// Key with no mapped action.
    None,
}

pub fn map_key(code: KeyCode) -> AppAction {
    match code {
        KeyCode::Enter => AppAction::Enter,
        KeyCode::Char('d') | KeyCode::Char('D') => AppAction::ToggleMode,
        KeyCode::Char('r') | KeyCode::Char('R') => AppAction::Retry,
        KeyCode::Char('s') | KeyCode::Char('S') => AppAction::Stop,
        KeyCode::Tab => AppAction::ToggleFocus,
        KeyCode::PageUp => AppAction::ScrollUp,
        KeyCode::PageDown => AppAction::ScrollDown,
        KeyCode::Home => AppAction::ScrollToTop,
        KeyCode::End => AppAction::ScrollToEnd,
        KeyCode::Char('c') | KeyCode::Char('C') => AppAction::ClearLog,
        KeyCode::Char('k') | KeyCode::Char('K') => AppAction::SkipOptionalIntegration,
        KeyCode::Char('q') | KeyCode::Char('Q') => AppAction::Quit,
        KeyCode::Esc => AppAction::Quit,
        _ => AppAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_keys() {
        assert_eq!(map_key(KeyCode::Enter), AppAction::Enter);
        assert_eq!(map_key(KeyCode::Char('d')), AppAction::ToggleMode);
        assert_eq!(map_key(KeyCode::Char('D')), AppAction::ToggleMode);
        assert_eq!(map_key(KeyCode::Char('r')), AppAction::Retry);
        assert_eq!(map_key(KeyCode::Char('S')), AppAction::Stop);
        assert_eq!(map_key(KeyCode::Char('q')), AppAction::Quit);
        assert_eq!(map_key(KeyCode::PageUp), AppAction::ScrollUp);
        assert_eq!(map_key(KeyCode::End), AppAction::ScrollToEnd);
    }

    #[test]
    fn unmapped_keys_are_none() {
        assert_eq!(map_key(KeyCode::Char('z')), AppAction::None);
        assert_eq!(map_key(KeyCode::F(5)), AppAction::None);
    }
}
