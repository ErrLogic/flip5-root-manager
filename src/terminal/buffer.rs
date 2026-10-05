//! In-memory terminal/log buffer backing the TUI's output panel
//! (CLAUDE.md section 11): timestamps, command start/end markers, exit
//! codes, scrolling, and auto-follow.

use chrono::Local;
use std::time::Duration;

use crate::adb::CommandKind;
use crate::terminal::scroll::ScrollState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Info,
    HostCommand,
    DeviceCommand,
    RootCommand,
    Stdout,
    Stderr,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub timestamp: chrono::DateTime<Local>,
    pub kind: LineKind,
    pub text: String,
}

/// Strips ANSI escape sequences and other control bytes from text before
/// it's stored for display.
///
/// Captured process output (the payload runner, KernelSU's loader, etc.)
/// may contain colour codes, cursor-movement sequences, or bare carriage
/// returns used for single-line progress updates. Ratatui renders a
/// `LogLine`'s text as literal character cells; when the crossterm
/// backend later writes those cells to the *real* terminal, any raw ESC/
/// `\r`/control bytes embedded in them are interpreted by that terminal
/// as real cursor-control input, not display data — which is what
/// produces overlapping, garbled lines in the output panel (CLAUDE.md
/// section 11: command output must always be cleanly observable, not
/// merely "usually fine"). This only removes bytes with no printable
/// representation; it never drops or rewrites the actual output text.
pub fn sanitize_for_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: `ESC [ ... <final-byte>` (e.g. SGR colour codes,
                // cursor movement). Consume through the final byte so no
                // stray digits/brackets leak into the text.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() || next == '~' {
                            break;
                        }
                    }
                }
                // OSC: `ESC ] ... BEL` or `ESC ] ... ESC \`.
                Some(']') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Short two-byte escape (e.g. `ESC c` reset) — consume
                // the one following byte.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            // Bare carriage return: a single-line progress-style
            // overwrite with no following newline in this chunk. Drop it
            // rather than let it move the real cursor back to column 0.
            '\r' => {}
            // Backspace and other non-printable C0/DEL control bytes.
            '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// A scrolling, auto-following log buffer. New lines are appended via
/// [`LogBuffer::push`]; while `auto_follow` is true the view tracks the
/// bottom of the buffer, and any manual scroll disables it until the user
/// explicitly scrolls back down to the bottom (not merely presses a key —
/// see [`LogBuffer::page_down`]).
///
/// The scroll offset is always kept within `[0, max_scroll]` (see
/// [`ScrollState`]) for whatever viewport height is passed in, so the
/// view can never be positioned past the last line.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: Vec<LogLine>,
    scroll: ScrollState,
    auto_follow: bool,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            scroll: ScrollState::default(),
            auto_follow: true,
        }
    }

    pub fn push(&mut self, kind: LineKind, text: impl Into<String>) {
        self.lines.push(LogLine {
            timestamp: Local::now(),
            kind,
            text: sanitize_for_display(&text.into()),
        });
    }

    pub fn push_info(&mut self, text: impl Into<String>) {
        self.push(LineKind::Info, text);
    }

    /// A positive confirmation (model/build verified, rule applied,
    /// device connected and compatible, workflow completed, ...).
    pub fn push_success(&mut self, text: impl Into<String>) {
        self.push(LineKind::Success, text);
    }

    /// Something the user should notice but that isn't itself a failure
    /// (no device connected, multiple devices, cancellation requested,
    /// ...).
    pub fn push_warning(&mut self, text: impl Into<String>) {
        self.push(LineKind::Warning, text);
    }

    /// A failure outside of a captured command's own stderr/exit code —
    /// e.g. a workflow step failing, an ADB error while probing, a
    /// device mismatch.
    pub fn push_error(&mut self, text: impl Into<String>) {
        self.push(LineKind::Error, text);
    }

    /// Marks the start of a command, distinguishing host/device/root
    /// command boundaries per CLAUDE.md section 13.
    pub fn push_command_start(&mut self, kind: CommandKind, display: &str) {
        let line_kind = match kind {
            CommandKind::Host => LineKind::HostCommand,
            CommandKind::Device => LineKind::DeviceCommand,
            CommandKind::RootDevice => LineKind::RootCommand,
        };
        let prefix = match kind {
            CommandKind::Host => "$ (host)",
            CommandKind::Device => "$ (device)",
            CommandKind::RootDevice => "$ (root)",
        };
        self.push(line_kind, format!("{prefix} {display}"));
    }

    pub fn push_stdout(&mut self, text: impl Into<String>) {
        self.push(LineKind::Stdout, format!("stdout: {}", text.into()));
    }

    pub fn push_stderr(&mut self, text: impl Into<String>) {
        self.push(LineKind::Stderr, format!("stderr: {}", text.into()));
    }

    pub fn push_exit(&mut self, exit_code: Option<i32>, elapsed: Duration) {
        match exit_code {
            Some(code) => self.push(
                if code == 0 {
                    LineKind::Success
                } else {
                    LineKind::Error
                },
                format!("exit: {code} ({:.1}s)", elapsed.as_secs_f64()),
            ),
            None => self.push(
                LineKind::Error,
                format!("exit: unknown ({:.1}s)", elapsed.as_secs_f64()),
            ),
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll = ScrollState::default();
        self.auto_follow = true;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn all(&self) -> &[LogLine] {
        &self.lines
    }

    pub fn auto_follow(&self) -> bool {
        self.auto_follow
    }

    /// Whether the view is currently showing the newest line, for a
    /// panel of `viewport_height` rows — true either because auto-follow
    /// is on, or because a manual scroll has (re)reached the bottom.
    pub fn is_at_bottom(&self, viewport_height: usize) -> bool {
        self.auto_follow || self.scroll.is_at_bottom(self.lines.len(), viewport_height)
    }

    /// Returns the window of lines that should be visible for a panel of
    /// `viewport_height` rows, given current scroll position. Never
    /// returns a window positioned past the last line — when the buffer
    /// has fewer lines than the viewport, this simply returns all of
    /// them (the caller renders however many rows that is; no blank
    /// lines are synthesized here).
    pub fn visible_window(&self, viewport_height: usize) -> &[LogLine] {
        if self.lines.is_empty() || viewport_height == 0 {
            return &[];
        }
        let top = if self.auto_follow {
            ScrollState::max_scroll(self.lines.len(), viewport_height)
        } else {
            self.scroll
                .offset
                .min(ScrollState::max_scroll(self.lines.len(), viewport_height))
        };
        let end = (top + viewport_height).min(self.lines.len());
        &self.lines[top..end]
    }

    /// Scrolls up by one viewport/page. Leaves auto-follow (if it was
    /// active, the page starts from the current bottom).
    pub fn page_up(&mut self, viewport_height: usize) {
        if self.auto_follow {
            self.scroll
                .jump_to_bottom(self.lines.len(), viewport_height);
        }
        self.scroll.page_up(self.lines.len(), viewport_height);
        self.auto_follow = false;
    }

    /// Scrolls down by one viewport/page. A no-op while already at the
    /// bottom (whether via auto-follow or a prior scroll); resumes
    /// auto-follow once this reaches the bottom, per CLAUDE.md-refinement
    /// requirement 7 ("When the user scrolls back to the bottom, resume
    /// auto-following").
    pub fn page_down(&mut self, viewport_height: usize) {
        if self.auto_follow {
            return;
        }
        self.scroll.page_down(self.lines.len(), viewport_height);
        if self.scroll.is_at_bottom(self.lines.len(), viewport_height) {
            self.auto_follow = true;
        }
    }

    pub fn jump_to_top(&mut self) {
        self.scroll.jump_to_top();
        self.auto_follow = false;
    }

    pub fn jump_to_bottom(&mut self) {
        self.auto_follow = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_ansi_color_codes() {
        assert_eq!(
            sanitize_for_display("\x1b[1;31mred bold\x1b[0m plain"),
            "red bold plain"
        );
    }

    #[test]
    fn sanitize_drops_bare_carriage_returns() {
        // A progress-bar-style single-line update: without this, the \r
        // would later be written to the real terminal and move its
        // cursor back to column 0, overwriting earlier output.
        assert_eq!(
            sanitize_for_display("attempt 1\rattempt 2"),
            "attempt 1attempt 2"
        );
    }

    #[test]
    fn sanitize_strips_osc_sequences() {
        assert_eq!(
            sanitize_for_display("\x1b]0;window title\x07visible text"),
            "visible text"
        );
    }

    #[test]
    fn sanitize_keeps_ordinary_text_byte_for_byte() {
        let plain = "[+] p0*profslide source mode=trace0 caller=ffffffc00817db44";
        assert_eq!(sanitize_for_display(plain), plain);
    }

    #[test]
    fn pushed_lines_are_sanitized() {
        let mut buf = LogBuffer::new();
        buf.push_stdout("\x1b[32mok\x1b[0m\rdone");
        assert_eq!(buf.all()[0].text, "stdout: okdone");
    }

    #[test]
    fn status_helpers_tag_lines_with_the_right_kind() {
        let mut buf = LogBuffer::new();
        buf.push_info("neutral");
        buf.push_success("good");
        buf.push_warning("heads up");
        buf.push_error("bad");
        let kinds: Vec<LineKind> = buf.all().iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                LineKind::Info,
                LineKind::Success,
                LineKind::Warning,
                LineKind::Error
            ]
        );
    }

    #[test]
    fn auto_follow_shows_most_recent_lines() {
        let mut buf = LogBuffer::new();
        for i in 0..10 {
            buf.push_info(format!("line {i}"));
        }
        let window = buf.visible_window(3);
        assert_eq!(window.len(), 3);
        assert_eq!(window[0].text, "line 7");
        assert_eq!(window[2].text, "line 9");
    }

    #[test]
    fn manual_scroll_disables_auto_follow() {
        let mut buf = LogBuffer::new();
        for i in 0..10 {
            buf.push_info(format!("line {i}"));
        }
        buf.jump_to_top();
        assert!(!buf.auto_follow());
        let window = buf.visible_window(3);
        assert_eq!(window[0].text, "line 0");

        buf.jump_to_bottom();
        assert!(buf.auto_follow());
    }

    #[test]
    fn clear_empties_buffer_and_resets_scroll() {
        let mut buf = LogBuffer::new();
        buf.push_info("hello");
        buf.jump_to_top();
        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.auto_follow());
    }

    fn filled(n: usize) -> LogBuffer {
        let mut buf = LogBuffer::new();
        for i in 0..n {
            buf.push_info(format!("line {i}"));
        }
        buf
    }

    #[test]
    fn empty_buffer_shows_nothing_and_is_at_bottom() {
        let buf = LogBuffer::new();
        assert!(buf.visible_window(20).is_empty());
        assert!(buf.is_at_bottom(20));
    }

    #[test]
    fn buffer_smaller_than_viewport_shows_everything_with_no_blank_padding() {
        let buf = filled(5);
        let window = buf.visible_window(20);
        // No fake blank lines inserted — the caller just gets fewer rows
        // than the viewport height.
        assert_eq!(window.len(), 5);
        assert_eq!(window[0].text, "line 0");
        assert_eq!(window[4].text, "line 4");
        assert!(buf.is_at_bottom(20));
    }

    #[test]
    fn buffer_exactly_equal_to_viewport_shows_everything() {
        let buf = filled(20);
        let window = buf.visible_window(20);
        assert_eq!(window.len(), 20);
        assert_eq!(window[0].text, "line 0");
        assert_eq!(window[19].text, "line 19");
        assert!(buf.is_at_bottom(20));
    }

    #[test]
    fn buffer_larger_than_viewport_auto_follow_shows_the_tail() {
        // Matches the documented example: content=100, viewport=20 ->
        // visible window is lines 80..100 (0-indexed), i.e. "line 80".."line 99".
        let buf = filled(100);
        let window = buf.visible_window(20);
        assert_eq!(window.len(), 20);
        assert_eq!(window[0].text, "line 80");
        assert_eq!(window[19].text, "line 99");
    }

    #[test]
    fn page_down_from_top_advances_by_one_viewport() {
        let mut buf = filled(100);
        buf.jump_to_top();
        buf.page_down(20);
        let window = buf.visible_window(20);
        assert_eq!(window[0].text, "line 20");
        assert!(!buf.is_at_bottom(20));
    }

    #[test]
    fn repeated_page_down_stops_exactly_at_the_bottom_with_no_blank_area() {
        let mut buf = filled(100);
        buf.jump_to_top();
        for _ in 0..20 {
            buf.page_down(20);
        }
        let window = buf.visible_window(20);
        assert_eq!(
            window.len(),
            20,
            "must still show a full page, never blank trailing rows"
        );
        assert_eq!(window[0].text, "line 80");
        assert_eq!(window[19].text, "line 99");
        assert!(buf.is_at_bottom(20));
        // And auto-follow must have resumed.
        assert!(buf.auto_follow());
    }

    #[test]
    fn page_up_from_bottom_goes_up_by_one_viewport() {
        let mut buf = filled(100);
        buf.page_up(20);
        let window = buf.visible_window(20);
        assert_eq!(window[0].text, "line 60");
    }

    #[test]
    fn repeated_page_up_stops_exactly_at_the_top() {
        let mut buf = filled(100);
        for _ in 0..20 {
            buf.page_up(20);
        }
        let window = buf.visible_window(20);
        assert_eq!(window[0].text, "line 0");
    }

    #[test]
    fn new_log_while_at_bottom_stays_at_bottom() {
        let mut buf = filled(50);
        assert!(buf.is_at_bottom(20));
        buf.push_info("new line");
        let window = buf.visible_window(20);
        assert_eq!(window.last().unwrap().text, "new line");
    }

    #[test]
    fn new_log_while_scrolled_up_preserves_the_users_position() {
        let mut buf = filled(100);
        buf.jump_to_top();
        let before = buf.visible_window(20)[0].text.clone();
        for i in 0..5 {
            buf.push_info(format!("extra {i}"));
        }
        let after = buf.visible_window(20)[0].text.clone();
        assert_eq!(
            before, after,
            "scrolled-up position must not jump when new output arrives"
        );
        assert!(!buf.is_at_bottom(20));
    }

    #[test]
    fn returning_to_bottom_after_scrolling_up_resumes_auto_follow() {
        let mut buf = filled(100);
        buf.jump_to_top();
        assert!(!buf.is_at_bottom(20));
        buf.jump_to_bottom();
        assert!(buf.is_at_bottom(20));
        assert!(buf.auto_follow());
        buf.push_info("newest");
        assert_eq!(buf.visible_window(20).last().unwrap().text, "newest");
    }

    #[test]
    fn clearing_resets_scroll_and_bottom_state() {
        let mut buf = filled(100);
        buf.jump_to_top();
        buf.clear();
        assert!(buf.is_empty());
        assert!(buf.is_at_bottom(20));
        assert!(buf.auto_follow());
    }
}
