#![forbid(unsafe_code)]

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::color::{Attrs, Palette, TermColor};

pub const MAX_COLS: u16 = 512;
pub const MAX_ROWS: u16 = 256;
pub const MAX_SCROLLBACK: usize = 10_000;
pub const MAX_TITLE_BYTES: usize = 1024;
pub const MAX_REPLY_BYTES: usize = 1024;
pub const DEFAULT_TAB_WIDTH: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    Ascii,
    DecSpecialGraphics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub fg: TermColor,
    pub bg: TermColor,
    pub attrs: Attrs,
}

impl Cell {
    pub const fn blank(bg: TermColor) -> Self {
        Self {
            ch: ' ',
            width: 1,
            fg: TermColor::Default,
            bg,
            attrs: Attrs::NONE,
        }
    }

    pub const fn spacer(fg: TermColor, bg: TermColor, attrs: Attrs) -> Self {
        Self {
            ch: '\0',
            width: 0,
            fg,
            bg,
            attrs,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
    pub generation: u64,
}

impl Row {
    pub fn blank(cols: u16, bg: TermColor, generation: u64) -> Self {
        Self {
            cells: vec![Cell::blank(bg); usize::from(cols)],
            wrapped: false,
            generation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub pending_wrap: bool,
    pub visible: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            pending_wrap: false,
            visible: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SavedCursor {
    pub col: u16,
    pub row: u16,
    pub fg: TermColor,
    pub bg: TermColor,
    pub attrs: Attrs,
    pub origin_mode: bool,
    pub auto_wrap: bool,
    pub pending_wrap: bool,
    pub charset_g0: Charset,
    pub charset_g1: Charset,
    pub gl_is_g1: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub auto_wrap: bool,
    pub origin_mode: bool,
    pub app_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub insert_mode: bool,
    pub newline_mode: bool,
    pub alt_screen: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            auto_wrap: true,
            origin_mode: false,
            app_cursor_keys: false,
            bracketed_paste: false,
            insert_mode: false,
            newline_mode: false,
            alt_screen: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AltBackup {
    pub grid: Vec<Row>,
    pub cursor: Cursor,
    pub scroll_top: u16,
    pub scroll_bottom: u16,
}

#[derive(Clone, Debug)]
pub struct Screen {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) grid: Vec<Row>,
    pub(crate) scrollback: VecDeque<Row>,
    pub(crate) max_scrollback: usize,
    pub(crate) scroll_top: u16,
    pub(crate) scroll_bottom: u16,
    pub(crate) tab_stops: Vec<bool>,
    pub(crate) cursor: Cursor,
    pub(crate) pen_fg: TermColor,
    pub(crate) pen_bg: TermColor,
    pub(crate) pen_attrs: Attrs,
    pub(crate) saved_primary: Option<SavedCursor>,
    pub(crate) saved_alt: Option<SavedCursor>,
    pub(crate) alt_backup: Option<AltBackup>,
    pub(crate) modes: Modes,
    pub(crate) charset_g0: Charset,
    pub(crate) charset_g1: Charset,
    pub(crate) gl_is_g1: bool,
    pub(crate) last_printed: Option<char>,
    pub(crate) title: String,
    pub(crate) reply_buf: Vec<u8>,
    pub(crate) palette: Palette,
    pub(crate) next_gen: u64,
    pub(crate) viewport_offset: usize,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, MAX_SCROLLBACK)
    }

    pub fn with_scrollback(cols: u16, rows: u16, max_scrollback: usize) -> Self {
        assert!((1..=MAX_COLS).contains(&cols));
        assert!((1..=MAX_ROWS).contains(&rows));
        let capped_scrollback = max_scrollback.min(MAX_SCROLLBACK);
        let mut grid = Vec::with_capacity(usize::from(rows));
        for r in 0..rows {
            grid.push(Row::blank(cols, TermColor::Default, u64::from(r) + 1));
        }
        Self {
            cols,
            rows,
            grid,
            scrollback: VecDeque::new(),
            max_scrollback: capped_scrollback,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tab_stops: default_tab_stops(cols),
            cursor: Cursor::default(),
            pen_fg: TermColor::Default,
            pen_bg: TermColor::Default,
            pen_attrs: Attrs::NONE,
            saved_primary: None,
            saved_alt: None,
            alt_backup: None,
            modes: Modes::default(),
            charset_g0: Charset::Ascii,
            charset_g1: Charset::Ascii,
            gl_is_g1: false,
            last_printed: None,
            title: String::new(),
            reply_buf: Vec::new(),
            palette: Palette::XTERM_DARK,
            next_gen: u64::from(rows) + 1,
            viewport_offset: 0,
        }
    }

    pub const fn cols(&self) -> u16 {
        self.cols
    }

    pub const fn rows(&self) -> u16 {
        self.rows
    }

    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub const fn modes(&self) -> Modes {
        self.modes
    }

    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.mark_all_dirty();
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub const fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    pub fn scroll_viewport(&mut self, delta: isize) {
        if self.modes.alt_screen {
            self.viewport_offset = 0;
            return;
        }
        let max_off = self.scrollback.len();
        if delta >= 0 {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(delta as usize)
                .min(max_off);
        } else {
            self.viewport_offset = self.viewport_offset.saturating_sub(delta.unsigned_abs());
        }
    }

    pub fn visible_row(&self, screen_row: u16) -> &Row {
        assert!(screen_row < self.rows);
        if self.viewport_offset == 0 || self.modes.alt_screen {
            return &self.grid[usize::from(screen_row)];
        }
        let sb_len = self.scrollback.len();
        let top_abs = sb_len.saturating_sub(self.viewport_offset);
        let abs_idx = top_abs + usize::from(screen_row);
        if abs_idx < sb_len {
            &self.scrollback[abs_idx]
        } else {
            let grid_idx = (abs_idx - sb_len).min(usize::from(self.rows) - 1);
            &self.grid[grid_idx]
        }
    }

    pub fn grid_row(&self, row: u16) -> &Row {
        assert!(row < self.rows);
        &self.grid[usize::from(row)]
    }

    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_buf)
    }

    pub fn push_reply(&mut self, bytes: &[u8]) {
        let avail = MAX_REPLY_BYTES.saturating_sub(self.reply_buf.len());
        let take = bytes.len().min(avail);
        self.reply_buf.extend_from_slice(&bytes[..take]);
    }

    pub fn reset(&mut self) {
        self.grid.clear();
        for _ in 0..self.rows {
            let row_gen = self.alloc_gen();
            self.grid
                .push(Row::blank(self.cols, TermColor::Default, row_gen));
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.tab_stops = default_tab_stops(self.cols);
        self.cursor = Cursor::default();
        self.pen_fg = TermColor::Default;
        self.pen_bg = TermColor::Default;
        self.pen_attrs = Attrs::NONE;
        self.saved_primary = None;
        self.saved_alt = None;
        self.alt_backup = None;
        self.modes = Modes::default();
        self.charset_g0 = Charset::Ascii;
        self.charset_g1 = Charset::Ascii;
        self.gl_is_g1 = false;
        self.last_printed = None;
        self.viewport_offset = 0;
    }

    pub fn resize(&mut self, new_cols: u16, new_rows: u16) {
        assert!((1..=MAX_COLS).contains(&new_cols));
        assert!((1..=MAX_ROWS).contains(&new_rows));
        if new_cols == self.cols && new_rows == self.rows {
            return;
        }
        let old_rows = self.rows;
        if new_rows < old_rows {
            let drop_needed = usize::from(old_rows - new_rows);
            let bottom_blank = usize::from(old_rows - 1 - self.cursor.row);
            let scroll_up = drop_needed.saturating_sub(bottom_blank);
            for _ in 0..scroll_up {
                let top = self.grid.remove(0);
                if !self.modes.alt_screen {
                    self.push_scrollback(top);
                }
            }
            self.grid.truncate(usize::from(new_rows));
            self.cursor.row = self.cursor.row.saturating_sub(scroll_up as u16);
        } else if new_rows > old_rows {
            let grow = usize::from(new_rows - old_rows);
            for _ in 0..grow {
                let row_gen = self.alloc_gen();
                self.grid
                    .push(Row::blank(self.cols, TermColor::Default, row_gen));
            }
        }
        for row in &mut self.grid {
            resize_row(row, new_cols);
            self.next_gen = self.next_gen.wrapping_add(1);
            row.generation = self.next_gen;
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.scroll_top = 0;
        self.scroll_bottom = new_rows - 1;
        self.tab_stops = default_tab_stops(new_cols);
        self.cursor.col = self.cursor.col.min(new_cols - 1);
        self.cursor.row = self.cursor.row.min(new_rows - 1);
        self.cursor.pending_wrap = false;
        self.viewport_offset = self.viewport_offset.min(self.scrollback.len());
    }

    pub fn text_lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(usize::from(self.rows));
        for r in 0..self.rows {
            let mut s = String::new();
            for cell in &self.grid[usize::from(r)].cells {
                if cell.width > 0 {
                    s.push(cell.ch);
                }
            }
            out.push(s.trim_end_matches(' ').to_owned());
        }
        out
    }

    pub fn to_snapshot_json(&self) -> Value {
        let mut lines = Vec::with_capacity(usize::from(self.rows));
        for r in 0..self.rows {
            let row = &self.grid[usize::from(r)];
            let mut cells = Vec::with_capacity(usize::from(self.cols));
            for cell in &row.cells {
                let ch_str = if cell.width == 0 {
                    String::new()
                } else {
                    cell.ch.to_string()
                };
                cells.push(json!({
                    "ch": ch_str,
                    "w": cell.width,
                    "fg": self.palette.resolve_fg(cell.fg),
                    "bg": self.palette.resolve_bg(cell.bg),
                    "attrs": cell.attrs.names(),
                }));
            }
            lines.push(Value::Array(cells));
        }
        json!({
            "cols": self.cols,
            "rows": self.rows,
            "cursor": [self.cursor.col, self.cursor.row],
            "pending_wrap": self.cursor.pending_wrap,
            "cursor_visible": self.cursor.visible,
            "alt_screen": self.modes.alt_screen,
            "bracketed_paste": self.modes.bracketed_paste,
            "title": self.title,
            "scrollback_len": self.scrollback.len(),
            "text_lines": self.text_lines(),
            "lines": lines,
        })
    }

    pub(crate) fn alloc_gen(&mut self) -> u64 {
        self.next_gen = self.next_gen.wrapping_add(1);
        self.next_gen
    }

    pub(crate) fn touch_row(&mut self, row: u16) {
        let row_gen = self.alloc_gen();
        self.grid[usize::from(row)].generation = row_gen;
    }

    pub(crate) fn mark_range_dirty(&mut self, start: u16, end: u16) {
        for r in start..=end {
            self.touch_row(r);
        }
    }

    pub(crate) fn mark_all_dirty(&mut self) {
        for r in 0..self.rows {
            self.touch_row(r);
        }
    }

    pub(crate) fn push_scrollback(&mut self, row: Row) {
        if self.max_scrollback == 0 {
            return;
        }
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
        if self.viewport_offset > 0 {
            self.viewport_offset = (self.viewport_offset + 1).min(self.scrollback.len());
        }
    }
}

pub(crate) fn default_tab_stops(cols: u16) -> Vec<bool> {
    (0..cols)
        .map(|c| c > 0 && c % DEFAULT_TAB_WIDTH == 0)
        .collect()
}

pub(crate) fn resize_row(row: &mut Row, new_cols: u16) {
    let target = usize::from(new_cols);
    if row.cells.len() > target {
        row.cells.truncate(target);
        if let Some(last) = row.cells.last_mut()
            && last.width == 2
        {
            *last = Cell::blank(last.bg);
        }
    } else if row.cells.len() < target {
        row.cells.resize(target, Cell::blank(TermColor::Default));
    }
}
