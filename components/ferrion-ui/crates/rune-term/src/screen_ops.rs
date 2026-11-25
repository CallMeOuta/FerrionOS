#![forbid(unsafe_code)]

use unicode_width::UnicodeWidthChar;

use crate::color::{Attrs, TermColor};
use crate::screen::{
    AltBackup, Cell, Charset, MAX_COLS, MAX_TITLE_BYTES, Row, SavedCursor, Screen, resize_row,
};

impl Screen {
    pub(crate) fn set_title(&mut self, bytes: &[u8]) {
        let slice = if bytes.len() > MAX_TITLE_BYTES {
            &bytes[..MAX_TITLE_BYTES]
        } else {
            bytes
        };
        self.title = String::from_utf8_lossy(slice).into_owned();
    }

    pub(crate) fn set_pen_fg(&mut self, fg: TermColor) {
        self.pen_fg = fg;
    }

    pub(crate) fn set_pen_bg(&mut self, bg: TermColor) {
        self.pen_bg = bg;
    }

    pub(crate) fn pen_attrs_mut(&mut self) -> &mut Attrs {
        &mut self.pen_attrs
    }

    pub(crate) fn reset_pen(&mut self) {
        self.pen_fg = TermColor::Default;
        self.pen_bg = TermColor::Default;
        self.pen_attrs = Attrs::NONE;
    }

    pub(crate) fn set_charset(&mut self, g1: bool, cs: Charset) {
        if g1 {
            self.charset_g1 = cs;
        } else {
            self.charset_g0 = cs;
        }
    }

    pub(crate) fn select_gl(&mut self, use_g1: bool) {
        self.gl_is_g1 = use_g1;
    }

    pub(crate) fn print_char(&mut self, raw: char) {
        let ch = self.map_charset(raw);
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            return;
        }
        let cell_w = if width >= 2 { 2u16 } else { 1u16 };
        if cell_w == 2 && self.cols < 2 {
            return;
        }
        if self.cursor.pending_wrap {
            if self.modes.auto_wrap {
                self.grid[usize::from(self.cursor.row)].wrapped = true;
                self.touch_row(self.cursor.row);
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
                self.line_feed();
            } else {
                self.cursor.pending_wrap = false;
            }
        }
        if cell_w == 2 && self.cursor.col + 1 >= self.cols {
            if self.modes.auto_wrap {
                let row = self.cursor.row;
                let col = self.cursor.col;
                self.clear_wide_overlap(row, col);
                self.grid[usize::from(row)].cells[usize::from(col)] = Cell::blank(self.pen_bg);
                self.grid[usize::from(row)].wrapped = true;
                self.touch_row(row);
                self.cursor.col = 0;
                self.line_feed();
            } else {
                return;
            }
        }
        if self.modes.insert_mode {
            self.insert_blank_chars(cell_w);
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        self.clear_wide_overlap(row, col);
        if cell_w == 2 {
            self.clear_wide_overlap(row, col + 1);
        }
        let cell = Cell {
            ch,
            width: cell_w as u8,
            fg: self.pen_fg,
            bg: self.pen_bg,
            attrs: self.pen_attrs,
        };
        self.grid[usize::from(row)].cells[usize::from(col)] = cell;
        if cell_w == 2 {
            self.grid[usize::from(row)].cells[usize::from(col + 1)] =
                Cell::spacer(self.pen_fg, self.pen_bg, self.pen_attrs);
        }
        self.touch_row(row);
        self.last_printed = Some(raw);
        if col + cell_w >= self.cols {
            if self.modes.auto_wrap {
                self.cursor.col = self.cols - 1;
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.col = self.cols - 1;
            }
        } else {
            self.cursor.col = col + cell_w;
        }
    }

    pub(crate) fn repeat_last_char(&mut self, count: u16) {
        let Some(ch) = self.last_printed else {
            return;
        };
        let n = count.clamp(1, MAX_COLS);
        for _ in 0..n {
            self.print_char(ch);
        }
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor.pending_wrap {
            self.cursor.pending_wrap = false;
            return;
        }
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        }
    }

    pub(crate) fn tab_forward(&mut self, count: u16) {
        let steps = count.clamp(1, MAX_COLS);
        self.cursor.pending_wrap = false;
        let mut col = self.cursor.col;
        for _ in 0..steps {
            let mut next = col + 1;
            while next < self.cols && !self.tab_stops[usize::from(next)] {
                next += 1;
            }
            col = next.min(self.cols - 1);
            if col == self.cols - 1 {
                break;
            }
        }
        self.cursor.col = col;
    }

    pub(crate) fn tab_backward(&mut self, count: u16) {
        let steps = count.clamp(1, MAX_COLS);
        self.cursor.pending_wrap = false;
        let mut col = self.cursor.col;
        for _ in 0..steps {
            if col == 0 {
                break;
            }
            let mut prev = col - 1;
            while prev > 0 && !self.tab_stops[usize::from(prev)] {
                prev -= 1;
            }
            col = prev;
        }
        self.cursor.col = col;
    }

    pub(crate) fn set_tab_stop(&mut self) {
        self.tab_stops[usize::from(self.cursor.col)] = true;
    }

    pub(crate) fn clear_tab_stop(&mut self, mode: u16) {
        match mode {
            0 => self.tab_stops[usize::from(self.cursor.col)] = false,
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }

    pub(crate) fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn line_feed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up_region(1, true);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    pub(crate) fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down_region(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    pub(crate) fn next_line(&mut self) {
        self.carriage_return();
        self.line_feed();
    }

    pub(crate) fn move_up(&mut self, count: u16) {
        self.cursor.pending_wrap = false;
        let n = count.max(1);
        let top = if self.cursor.row >= self.scroll_top {
            self.scroll_top
        } else {
            0
        };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(top);
    }

    pub(crate) fn move_down(&mut self, count: u16) {
        self.cursor.pending_wrap = false;
        let n = count.max(1);
        let bottom = if self.cursor.row <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows - 1
        };
        self.cursor.row = self.cursor.row.saturating_add(n).min(bottom);
    }

    pub(crate) fn move_right(&mut self, count: u16) {
        self.cursor.pending_wrap = false;
        let n = count.max(1);
        self.cursor.col = self.cursor.col.saturating_add(n).min(self.cols - 1);
    }

    pub(crate) fn move_left(&mut self, count: u16) {
        let n = count.max(1);
        if self.cursor.pending_wrap {
            self.cursor.pending_wrap = false;
            if n > 1 {
                self.cursor.col = self.cursor.col.saturating_sub(n - 1);
            }
            return;
        }
        self.cursor.col = self.cursor.col.saturating_sub(n);
    }

    pub(crate) fn move_to_col(&mut self, col_1based: u16) {
        self.cursor.pending_wrap = false;
        let col = col_1based.saturating_sub(1).min(self.cols - 1);
        self.cursor.col = col;
    }

    pub(crate) fn move_to_row(&mut self, row_1based: u16) {
        self.cursor.pending_wrap = false;
        let base = if self.modes.origin_mode {
            self.scroll_top
        } else {
            0
        };
        let limit = if self.modes.origin_mode {
            self.scroll_bottom
        } else {
            self.rows - 1
        };
        self.cursor.row = base.saturating_add(row_1based.saturating_sub(1)).min(limit);
    }

    pub(crate) fn move_to(&mut self, row_1based: u16, col_1based: u16) {
        self.move_to_row(row_1based);
        self.move_to_col(col_1based);
    }

    pub(crate) fn erase_display(&mut self, mode: u16) {
        let blank = Cell::blank(self.pen_bg);
        match mode {
            0 => {
                self.erase_line(0);
                for r in (self.cursor.row + 1)..self.rows {
                    self.clear_row(r, blank);
                }
            }
            1 => {
                for r in 0..self.cursor.row {
                    self.clear_row(r, blank);
                }
                self.erase_line(1);
            }
            2 => {
                for r in 0..self.rows {
                    self.clear_row(r, blank);
                }
            }
            3 => {
                self.scrollback.clear();
                self.viewport_offset = 0;
            }
            _ => {}
        }
    }

    pub(crate) fn erase_line(&mut self, mode: u16) {
        let row = self.cursor.row;
        let col = self.cursor.col;
        let blank = Cell::blank(self.pen_bg);
        match mode {
            0 => {
                self.clear_wide_overlap(row, col);
                for c in col..self.cols {
                    self.grid[usize::from(row)].cells[usize::from(c)] = blank;
                }
                self.grid[usize::from(row)].wrapped = false;
                self.touch_row(row);
            }
            1 => {
                self.clear_wide_overlap(row, col);
                for c in 0..=col {
                    self.grid[usize::from(row)].cells[usize::from(c)] = blank;
                }
                self.touch_row(row);
            }
            2 => {
                self.clear_row(row, blank);
            }
            _ => {}
        }
    }

    pub(crate) fn erase_chars(&mut self, count: u16) {
        let n = count.max(1);
        let row = self.cursor.row;
        let start = self.cursor.col;
        let end = start.saturating_add(n).min(self.cols);
        let blank = Cell::blank(self.pen_bg);
        if start < end {
            self.clear_wide_overlap(row, start);
            self.clear_wide_overlap(row, end - 1);
            for c in start..end {
                self.grid[usize::from(row)].cells[usize::from(c)] = blank;
            }
            self.touch_row(row);
        }
    }

    pub(crate) fn insert_blank_chars(&mut self, count: u16) {
        let n = usize::from(count.max(1)).min(usize::from(self.cols - self.cursor.col));
        let row = self.cursor.row;
        let start = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        self.clear_wide_overlap(row, self.cursor.col);
        let cells = &mut self.grid[usize::from(row)].cells;
        for c in (start + n..cols).rev() {
            cells[c] = cells[c - n];
        }
        let blank = Cell::blank(self.pen_bg);
        for cell in cells.iter_mut().take(start + n).skip(start) {
            *cell = blank;
        }
        if cells[cols - 1].width == 2 {
            cells[cols - 1] = blank;
        }
        self.grid[usize::from(row)].wrapped = false;
        self.touch_row(row);
    }

    pub(crate) fn delete_chars(&mut self, count: u16) {
        let n = usize::from(count.max(1)).min(usize::from(self.cols - self.cursor.col));
        let row = self.cursor.row;
        let start = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        self.clear_wide_overlap(row, self.cursor.col);
        if start + n < cols {
            self.clear_wide_overlap(row, (start + n) as u16);
        }
        let cells = &mut self.grid[usize::from(row)].cells;
        for c in start..(cols - n) {
            cells[c] = cells[c + n];
        }
        let blank = Cell::blank(self.pen_bg);
        for cell in cells.iter_mut().skip(cols - n) {
            *cell = blank;
        }
        self.grid[usize::from(row)].wrapped = false;
        self.touch_row(row);
    }

    pub(crate) fn insert_lines(&mut self, count: u16) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
        let top = usize::from(self.cursor.row);
        let bot = usize::from(self.scroll_bottom);
        let n = usize::from(count.max(1)).min(bot - top + 1);
        for _ in 0..n {
            self.grid.remove(bot);
            let row_gen = self.alloc_gen();
            self.grid
                .insert(top, Row::blank(self.cols, self.pen_bg, row_gen));
        }
        self.mark_range_dirty(self.cursor.row, self.scroll_bottom);
    }

    pub(crate) fn delete_lines(&mut self, count: u16) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
        let top = usize::from(self.cursor.row);
        let bot = usize::from(self.scroll_bottom);
        let n = usize::from(count.max(1)).min(bot - top + 1);
        for _ in 0..n {
            self.grid.remove(top);
            let row_gen = self.alloc_gen();
            self.grid
                .insert(bot, Row::blank(self.cols, self.pen_bg, row_gen));
        }
        self.mark_range_dirty(self.cursor.row, self.scroll_bottom);
    }

    pub(crate) fn scroll_up_region(&mut self, count: u16, record_scrollback: bool) {
        let top = usize::from(self.scroll_top);
        let bot = usize::from(self.scroll_bottom);
        let n = usize::from(count.max(1)).min(bot - top + 1);
        let save_sb = record_scrollback
            && !self.modes.alt_screen
            && self.scroll_top == 0
            && self.scroll_bottom == self.rows - 1;
        for _ in 0..n {
            let removed = self.grid.remove(top);
            if save_sb {
                self.push_scrollback(removed);
            }
            let row_gen = self.alloc_gen();
            self.grid
                .insert(bot, Row::blank(self.cols, self.pen_bg, row_gen));
        }
        self.mark_range_dirty(self.scroll_top, self.scroll_bottom);
    }

    pub(crate) fn scroll_down_region(&mut self, count: u16) {
        let top = usize::from(self.scroll_top);
        let bot = usize::from(self.scroll_bottom);
        let n = usize::from(count.max(1)).min(bot - top + 1);
        for _ in 0..n {
            self.grid.remove(bot);
            let row_gen = self.alloc_gen();
            self.grid
                .insert(top, Row::blank(self.cols, self.pen_bg, row_gen));
        }
        self.mark_range_dirty(self.scroll_top, self.scroll_bottom);
    }

    pub(crate) fn set_scroll_region(&mut self, top_1based: u16, bottom_1based: u16) {
        let top = if top_1based == 0 { 0 } else { top_1based - 1 };
        let bottom = if bottom_1based == 0 {
            self.rows - 1
        } else {
            (bottom_1based - 1).min(self.rows - 1)
        };
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.move_to(1, 1);
        }
    }

    pub(crate) fn save_cursor(&mut self) {
        let saved = SavedCursor {
            col: self.cursor.col,
            row: self.cursor.row,
            fg: self.pen_fg,
            bg: self.pen_bg,
            attrs: self.pen_attrs,
            origin_mode: self.modes.origin_mode,
            auto_wrap: self.modes.auto_wrap,
            pending_wrap: self.cursor.pending_wrap,
            charset_g0: self.charset_g0,
            charset_g1: self.charset_g1,
            gl_is_g1: self.gl_is_g1,
        };
        if self.modes.alt_screen {
            self.saved_alt = Some(saved);
        } else {
            self.saved_primary = Some(saved);
        }
    }

    pub(crate) fn restore_cursor(&mut self) {
        let saved = if self.modes.alt_screen {
            self.saved_alt
        } else {
            self.saved_primary
        };
        if let Some(s) = saved {
            self.cursor.col = s.col.min(self.cols - 1);
            self.cursor.row = s.row.min(self.rows - 1);
            self.cursor.pending_wrap = s.pending_wrap && self.cursor.col == self.cols - 1;
            self.pen_fg = s.fg;
            self.pen_bg = s.bg;
            self.pen_attrs = s.attrs;
            self.modes.origin_mode = s.origin_mode;
            self.modes.auto_wrap = s.auto_wrap;
            self.charset_g0 = s.charset_g0;
            self.charset_g1 = s.charset_g1;
            self.gl_is_g1 = s.gl_is_g1;
        }
    }

    pub(crate) fn set_ansi_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            4 => self.modes.insert_mode = enabled,
            20 => self.modes.newline_mode = enabled,
            _ => {}
        }
    }

    pub(crate) fn set_dec_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 => self.modes.app_cursor_keys = enabled,
            6 => {
                self.modes.origin_mode = enabled;
                self.move_to(1, 1);
            }
            7 => self.modes.auto_wrap = enabled,
            25 => self.cursor.visible = enabled,
            47 | 1047 => {
                if enabled {
                    self.enter_alt_screen(false);
                } else {
                    self.leave_alt_screen(false);
                }
            }
            1048 => {
                if enabled {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if enabled {
                    self.enter_alt_screen(true);
                } else {
                    self.leave_alt_screen(true);
                }
            }
            2004 => self.modes.bracketed_paste = enabled,
            _ => {}
        }
    }

    fn enter_alt_screen(&mut self, save_cursor: bool) {
        if self.modes.alt_screen {
            return;
        }
        if save_cursor {
            self.save_cursor();
        }
        let mut alt_grid = Vec::with_capacity(usize::from(self.rows));
        for _ in 0..self.rows {
            let row_gen = self.alloc_gen();
            alt_grid.push(Row::blank(self.cols, TermColor::Default, row_gen));
        }
        let primary_grid = std::mem::replace(&mut self.grid, alt_grid);
        self.alt_backup = Some(AltBackup {
            grid: primary_grid,
            cursor: self.cursor,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
        });
        self.modes.alt_screen = true;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.cursor.pending_wrap = false;
        self.viewport_offset = 0;
    }

    fn leave_alt_screen(&mut self, restore_cursor: bool) {
        if !self.modes.alt_screen {
            return;
        }
        self.modes.alt_screen = false;
        if let Some(backup) = self.alt_backup.take() {
            self.grid = backup.grid;
            for row in &mut self.grid {
                resize_row(row, self.cols);
            }
            while self.grid.len() < usize::from(self.rows) {
                let row_gen = self.alloc_gen();
                self.grid
                    .push(Row::blank(self.cols, TermColor::Default, row_gen));
            }
            self.grid.truncate(usize::from(self.rows));
            self.cursor = backup.cursor;
            self.cursor.col = self.cursor.col.min(self.cols - 1);
            self.cursor.row = self.cursor.row.min(self.rows - 1);
            self.scroll_top = backup.scroll_top.min(self.rows - 1);
            self.scroll_bottom = backup.scroll_bottom.min(self.rows - 1);
            if self.scroll_top >= self.scroll_bottom {
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
            }
        }
        if restore_cursor {
            self.restore_cursor();
        }
        self.mark_all_dirty();
    }

    fn map_charset(&self, ch: char) -> char {
        let active = if self.gl_is_g1 {
            self.charset_g1
        } else {
            self.charset_g0
        };
        if active == Charset::DecSpecialGraphics {
            dec_special_char(ch)
        } else {
            ch
        }
    }

    fn clear_row(&mut self, row: u16, blank: Cell) {
        let r = &mut self.grid[usize::from(row)];
        r.cells.fill(blank);
        r.wrapped = false;
        self.touch_row(row);
    }

    fn clear_wide_overlap(&mut self, row: u16, col: u16) {
        let cells = &mut self.grid[usize::from(row)].cells;
        let idx = usize::from(col);
        if cells[idx].width == 0 && idx > 0 {
            cells[idx - 1] = Cell::blank(cells[idx - 1].bg);
            cells[idx] = Cell::blank(cells[idx].bg);
        } else if cells[idx].width == 2 && idx + 1 < cells.len() {
            cells[idx + 1] = Cell::blank(cells[idx + 1].bg);
        }
    }
}

fn dec_special_char(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => ch,
    }
}
