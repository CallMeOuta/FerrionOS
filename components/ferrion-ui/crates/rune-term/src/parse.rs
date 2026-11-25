#![forbid(unsafe_code)]

use vte::{Params, Perform};

use crate::color::{Attrs, TermColor};
use crate::screen::{Charset, Screen};

pub const MAX_OSC_BYTES: usize = 4096;

pub struct Parser {
    inner: vte::Parser<MAX_OSC_BYTES>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            inner: vte::Parser::new_with_size(),
        }
    }

    pub fn advance(&mut self, screen: &mut Screen, bytes: &[u8]) {
        self.inner.advance(screen, bytes);
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.backspace(),
            0x09 => self.tab_forward(1),
            0x0A..=0x0C => {
                if self.modes().newline_mode {
                    self.carriage_return();
                }
                self.line_feed();
            }
            0x0D => self.carriage_return(),
            0x0E => self.select_gl(true),
            0x0F => self.select_gl(false),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(cmd) = params.first() else {
            return;
        };
        if *cmd == b"52" {
            return;
        }
        if (*cmd == b"0" || *cmd == b"2")
            && let Some(title) = params.get(1)
        {
            self.set_title(title);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.line_feed(),
            ([], b'E') => self.next_line(),
            ([], b'H') => self.set_tab_stop(),
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([b'('], b'0') => self.set_charset(false, Charset::DecSpecialGraphics),
            ([b'('], b'B') => self.set_charset(false, Charset::Ascii),
            ([b')'], b'0') => self.set_charset(true, Charset::DecSpecialGraphics),
            ([b')'], b'B') => self.set_charset(true, Charset::Ascii),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let p0 = param_at(params, 0, 0);
        let p1 = param_at(params, 1, 0);
        match (intermediates, action) {
            ([], '@') => self.insert_blank_chars(p0.max(1)),
            ([], 'A') => self.move_up(p0.max(1)),
            ([], 'B') | ([], 'e') => self.move_down(p0.max(1)),
            ([], 'C') | ([], 'a') => self.move_right(p0.max(1)),
            ([], 'D') => self.move_left(p0.max(1)),
            ([], 'E') => {
                self.move_down(p0.max(1));
                self.carriage_return();
            }
            ([], 'F') => {
                self.move_up(p0.max(1));
                self.carriage_return();
            }
            ([], 'G') | ([], '`') => self.move_to_col(p0.max(1)),
            ([], 'H') | ([], 'f') => self.move_to(p0.max(1), p1.max(1)),
            ([], 'I') => self.tab_forward(p0.max(1)),
            ([], 'J') => self.erase_display(p0),
            ([], 'K') => self.erase_line(p0),
            ([], 'L') => self.insert_lines(p0.max(1)),
            ([], 'M') => self.delete_lines(p0.max(1)),
            ([], 'P') => self.delete_chars(p0.max(1)),
            ([], 'S') => self.scroll_up_region(p0.max(1), false),
            ([], 'T') => self.scroll_down_region(p0.max(1)),
            ([], 'X') => self.erase_chars(p0.max(1)),
            ([], 'Z') => self.tab_backward(p0.max(1)),
            ([], 'b') => self.repeat_last_char(p0.max(1)),
            ([], 'd') => self.move_to_row(p0.max(1)),
            ([], 'g') => self.clear_tab_stop(p0),
            ([], 'h') => {
                for sub in params.iter() {
                    if let Some(&mode) = sub.first() {
                        self.set_ansi_mode(mode, true);
                    }
                }
            }
            ([], 'l') => {
                for sub in params.iter() {
                    if let Some(&mode) = sub.first() {
                        self.set_ansi_mode(mode, false);
                    }
                }
            }
            ([b'?'], 'h') => {
                for sub in params.iter() {
                    if let Some(&mode) = sub.first() {
                        self.set_dec_mode(mode, true);
                    }
                }
            }
            ([b'?'], 'l') => {
                for sub in params.iter() {
                    if let Some(&mode) = sub.first() {
                        self.set_dec_mode(mode, false);
                    }
                }
            }
            ([], 'm') => apply_sgr(self, params),
            ([], 'n') => match p0 {
                5 => self.push_reply(b"\x1b[0n"),
                6 => {
                    let cur = self.cursor();
                    let reply = format!("\x1b[{};{}R", cur.row + 1, cur.col + 1);
                    self.push_reply(reply.as_bytes());
                }
                _ => {}
            },
            ([], 'r') => self.set_scroll_region(p0, p1),
            ([], 's') => self.save_cursor(),
            ([], 'u') => self.restore_cursor(),
            _ => {}
        }
    }
}

fn param_at(params: &Params, idx: usize, default: u16) -> u16 {
    params
        .iter()
        .nth(idx)
        .and_then(|sub| sub.first().copied())
        .unwrap_or(default)
}

fn apply_sgr(screen: &mut Screen, params: &Params) {
    if params.is_empty() {
        screen.reset_pen();
        return;
    }
    let mut iter = params.iter();
    while let Some(sub) = iter.next() {
        let code = sub.first().copied().unwrap_or(0);
        if sub.len() > 1 {
            apply_sgr_subparams(screen, sub);
            continue;
        }
        match code {
            0 => screen.reset_pen(),
            1 => screen.pen_attrs_mut().insert(Attrs::BOLD),
            2 => screen.pen_attrs_mut().insert(Attrs::DIM),
            3 => screen.pen_attrs_mut().insert(Attrs::ITALIC),
            4 => {
                screen.pen_attrs_mut().remove(Attrs::DOUBLE_UNDERLINE);
                screen.pen_attrs_mut().insert(Attrs::UNDERLINE);
            }
            7 => screen.pen_attrs_mut().insert(Attrs::INVERSE),
            8 => screen.pen_attrs_mut().insert(Attrs::HIDDEN),
            9 => screen.pen_attrs_mut().insert(Attrs::STRIKETHROUGH),
            21 => {
                screen.pen_attrs_mut().remove(Attrs::UNDERLINE);
                screen.pen_attrs_mut().insert(Attrs::DOUBLE_UNDERLINE);
            }
            22 => {
                screen.pen_attrs_mut().remove(Attrs::BOLD);
                screen.pen_attrs_mut().remove(Attrs::DIM);
            }
            23 => screen.pen_attrs_mut().remove(Attrs::ITALIC),
            24 => {
                screen.pen_attrs_mut().remove(Attrs::UNDERLINE);
                screen.pen_attrs_mut().remove(Attrs::DOUBLE_UNDERLINE);
            }
            27 => screen.pen_attrs_mut().remove(Attrs::INVERSE),
            28 => screen.pen_attrs_mut().remove(Attrs::HIDDEN),
            29 => screen.pen_attrs_mut().remove(Attrs::STRIKETHROUGH),
            30..=37 => screen.set_pen_fg(TermColor::Indexed((code - 30) as u8)),
            38 => {
                if let Some(color) = parse_extended_semicolon(&mut iter) {
                    screen.set_pen_fg(color);
                }
            }
            39 => screen.set_pen_fg(TermColor::Default),
            40..=47 => screen.set_pen_bg(TermColor::Indexed((code - 40) as u8)),
            48 => {
                if let Some(color) = parse_extended_semicolon(&mut iter) {
                    screen.set_pen_bg(color);
                }
            }
            49 => screen.set_pen_bg(TermColor::Default),
            90..=97 => screen.set_pen_fg(TermColor::Indexed((code - 90 + 8) as u8)),
            100..=107 => screen.set_pen_bg(TermColor::Indexed((code - 100 + 8) as u8)),
            _ => {}
        }
    }
}

fn apply_sgr_subparams(screen: &mut Screen, sub: &[u16]) {
    let code = sub[0];
    match code {
        4 => {
            screen.pen_attrs_mut().remove(Attrs::UNDERLINE);
            screen.pen_attrs_mut().remove(Attrs::DOUBLE_UNDERLINE);
            match sub.get(1).copied().unwrap_or(1) {
                0 => {}
                2 => screen.pen_attrs_mut().insert(Attrs::DOUBLE_UNDERLINE),
                _ => screen.pen_attrs_mut().insert(Attrs::UNDERLINE),
            }
        }
        38 | 48 => {
            let color = match sub.get(1).copied() {
                Some(5) => sub
                    .get(2)
                    .copied()
                    .map(|idx| TermColor::Indexed((idx & 0xFF) as u8)),
                Some(2) if sub.len() >= 6 => Some(TermColor::Rgb(
                    (sub[3] & 0xFF) as u8,
                    (sub[4] & 0xFF) as u8,
                    (sub[5] & 0xFF) as u8,
                )),
                Some(2) if sub.len() >= 5 => Some(TermColor::Rgb(
                    (sub[2] & 0xFF) as u8,
                    (sub[3] & 0xFF) as u8,
                    (sub[4] & 0xFF) as u8,
                )),
                _ => None,
            };
            if let Some(c) = color {
                if code == 38 {
                    screen.set_pen_fg(c);
                } else {
                    screen.set_pen_bg(c);
                }
            }
        }
        _ => {}
    }
}

fn parse_extended_semicolon<'a>(iter: &mut impl Iterator<Item = &'a [u16]>) -> Option<TermColor> {
    let mode = iter.next()?.first().copied().unwrap_or(0);
    match mode {
        5 => {
            let idx = iter.next()?.first().copied().unwrap_or(0);
            Some(TermColor::Indexed((idx & 0xFF) as u8))
        }
        2 => {
            let r = iter.next()?.first().copied().unwrap_or(0);
            let g = iter.next()?.first().copied().unwrap_or(0);
            let b = iter.next()?.first().copied().unwrap_or(0);
            Some(TermColor::Rgb(
                (r & 0xFF) as u8,
                (g & 0xFF) as u8,
                (b & 0xFF) as u8,
            ))
        }
        _ => None,
    }
}
