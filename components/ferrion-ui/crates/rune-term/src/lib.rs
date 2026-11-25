pub mod color;
pub mod input;
pub mod parse;
pub mod pty;
pub mod screen;
mod screen_ops;
pub mod view;

pub use color::{Attrs, Palette, TermColor, srgb_to_rune};
pub use input::{MAX_PASTE_BYTES, Selection, encode_key, encode_paste, extract_selection};
pub use parse::{MAX_OSC_BYTES, Parser};
pub use pty::{
    MAX_PENDING_CHUNKS, MAX_WRITE_BYTES, Pty, PtyConfig, PtyError, READ_BUF_BYTES,
    SHUTDOWN_JOIN_TIMEOUT, read_stdin_winsize,
};
pub use screen::{
    Cell, Charset, Cursor, DEFAULT_TAB_WIDTH, MAX_COLS, MAX_REPLY_BYTES, MAX_ROWS, MAX_SCROLLBACK,
    MAX_TITLE_BYTES, Modes, Row, Screen,
};
pub use view::{TermMetrics, TermView};

pub struct Terminal {
    screen: Screen,
    parser: Parser,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            screen: Screen::new(cols, rows),
            parser: Parser::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.screen.resize(cols, rows);
    }
}
