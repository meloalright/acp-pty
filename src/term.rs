//! Headless terminal emulation.
//!
//! Instead of blindly stripping ANSI escapes (which mangles `\r` overwrites,
//! cursor moves, ZLE redraws, progress bars, …), feed the raw PTY byte stream
//! into a real in-memory terminal and read back the rendered text.
//!
//! Streaming model that fits the append-style IM preview: the cursor's current
//! row is the *active* line (a prompt, or a line still being rewritten — e.g. a
//! `\r` progress bar). Everything strictly above the cursor is finalized, so we
//! emit those rows as they complete and never emit the active line. The active
//! line is used only for prompt/settle detection.

const ROWS: u16 = 1000;
const COLS: u16 = 80;

pub struct TermRenderer {
    parser: vt100::Parser,
    emitted: usize,
}

impl TermRenderer {
    pub fn new() -> Self {
        Self {
            parser: vt100::Parser::new(ROWS, COLS, 0),
            emitted: 0,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// True when a full-screen program has switched to the alternate screen
    /// (vim, htop, opencode's TUI, …). Such programs redraw a fixed grid in
    /// place and never finalize lines, so streaming them is meaningless.
    pub fn in_alt_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    fn cursor_row(&self) -> usize {
        self.parser.screen().cursor_position().0 as usize
    }

    /// The active (cursor) line, trimmed — a prompt, or a line still being
    /// rewritten. Used for prompt/settle detection; never emitted as output.
    pub fn current_line(&self) -> String {
        let row = self.cursor_row();
        self.parser
            .screen()
            .rows(0, COLS)
            .nth(row)
            .unwrap_or_default()
            .trim_end()
            .to_string()
    }

    /// True if there are finalized rows above the cursor not yet emitted.
    pub fn pending(&self) -> bool {
        self.cursor_row() > self.emitted
    }

    /// Take newly finalized rows (those strictly above the cursor) since the
    /// last call, joined with newlines. The active line is excluded.
    pub fn take_completed(&mut self) -> String {
        let cur = self.cursor_row();
        if cur <= self.emitted {
            return String::new();
        }
        let rows: Vec<String> = self.parser.screen().rows(0, COLS).take(cur).collect();
        let out: Vec<&str> = rows[self.emitted..cur]
            .iter()
            .map(|r| r.trim_end())
            .collect();
        self.emitted = cur;
        let mut s = out.join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        s
    }

    /// Drop everything up to the cursor without emitting (startup/init noise).
    pub fn skip_to_cursor(&mut self) {
        self.emitted = self.cursor_row();
    }
}
