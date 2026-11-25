#![forbid(unsafe_code)]

use rune::{Key, KeyEvent, Mods};

use crate::screen::Screen;

pub const MAX_PASTE_BYTES: usize = 65_536;

pub fn encode_key(ev: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = ev.mods;
    match &ev.key {
        Key::Enter => Some(vec![b'\r']),
        Key::Backspace => Some(if m.alt { vec![0x1b, 0x7f] } else { vec![0x7f] }),
        Key::Tab => Some(if m.shift {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        }),
        Key::Escape => Some(vec![0x1b]),
        Key::Up => Some(encode_cursor_key(b'A', m, app_cursor)),
        Key::Down => Some(encode_cursor_key(b'B', m, app_cursor)),
        Key::Right => Some(encode_cursor_key(b'C', m, app_cursor)),
        Key::Left => Some(encode_cursor_key(b'D', m, app_cursor)),
        Key::Home => Some(encode_cursor_key(b'H', m, app_cursor)),
        Key::End => Some(encode_cursor_key(b'F', m, app_cursor)),
        Key::Insert => Some(encode_tilde_key(2, m)),
        Key::Delete => Some(encode_tilde_key(3, m)),
        Key::PageUp => Some(encode_tilde_key(5, m)),
        Key::PageDown => Some(encode_tilde_key(6, m)),
        Key::Function(n) => encode_function_key(*n, m),
        Key::Command(c) => encode_ctrl_char(*c, m.alt),
        Key::Text(s) => {
            if s.is_empty() {
                return None;
            }
            if m.ctrl
                && !m.alt
                && s.chars().count() == 1
                && let Some(ch) = s.chars().next()
                && let Some(bytes) = encode_ctrl_char(ch, false)
            {
                return Some(bytes);
            }
            let mut out = Vec::with_capacity(s.len() + usize::from(m.alt));
            if m.alt && !m.ctrl {
                out.push(0x1b);
            }
            out.extend_from_slice(s.as_bytes());
            Some(out)
        }
        _ => None,
    }
}

pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let cleaned = text.replace("\x1b[201~", "");
    let raw = if cleaned.len() > MAX_PASTE_BYTES {
        &cleaned.as_bytes()[..MAX_PASTE_BYTES]
    } else {
        cleaned.as_bytes()
    };
    if !bracketed {
        return raw.to_vec();
    }
    let mut out = Vec::with_capacity(raw.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(raw);
    out.extend_from_slice(b"\x1b[201~");
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Selection {
    pub fn ordered(self) -> ((u16, u16), (u16, u16)) {
        if (self.start.1, self.start.0) <= (self.end.1, self.end.0) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn row_cols(self, row: u16, cols: u16) -> Option<(u16, u16)> {
        let ((sc, sr), (ec, er)) = self.ordered();
        if row < sr || row > er {
            return None;
        }
        let left = if row == sr { sc.min(cols) } else { 0 };
        let right = if row == er {
            ec.saturating_add(1).min(cols)
        } else {
            cols
        };
        (right > left).then_some((left, right))
    }
}

pub fn extract_selection(screen: &Screen, sel: Selection) -> String {
    let ((_, sr), (_, er)) = sel.ordered();
    let max_row = er.min(screen.rows().saturating_sub(1));
    let mut out = String::new();
    for r in sr..=max_row {
        if let Some((left, right)) = sel.row_cols(r, screen.cols()) {
            let row = screen.visible_row(r);
            let mut line = String::new();
            for c in left..right {
                let cell = row.cells[usize::from(c)];
                if cell.width > 0 {
                    line.push(cell.ch);
                }
            }
            if !row.wrapped {
                out.push_str(line.trim_end_matches(' '));
            } else {
                out.push_str(&line);
            }
            if r < max_row && !row.wrapped {
                out.push('\n');
            }
        }
    }
    out
}

fn csi_mod(m: Mods) -> Option<u8> {
    let bits = u8::from(m.shift) | (u8::from(m.alt) << 1) | (u8::from(m.ctrl) << 2);
    (bits != 0).then_some(bits + 1)
}

fn encode_cursor_key(code: u8, m: Mods, app_cursor: bool) -> Vec<u8> {
    if let Some(mod_param) = csi_mod(m) {
        format!("\x1b[1;{mod_param}{}", code as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', code]
    } else {
        vec![0x1b, b'[', code]
    }
}

fn encode_tilde_key(num: u8, m: Mods) -> Vec<u8> {
    if let Some(mod_param) = csi_mod(m) {
        format!("\x1b[{num};{mod_param}~").into_bytes()
    } else {
        format!("\x1b[{num}~").into_bytes()
    }
}

fn encode_function_key(n: u8, m: Mods) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let suffix = (*b"PQRS")[usize::from(n - 1)];
            if let Some(mod_param) = csi_mod(m) {
                Some(format!("\x1b[1;{mod_param}{}", suffix as char).into_bytes())
            } else {
                Some(vec![0x1b, b'O', suffix])
            }
        }
        5 => Some(encode_tilde_key(15, m)),
        6 => Some(encode_tilde_key(17, m)),
        7 => Some(encode_tilde_key(18, m)),
        8 => Some(encode_tilde_key(19, m)),
        9 => Some(encode_tilde_key(20, m)),
        10 => Some(encode_tilde_key(21, m)),
        11 => Some(encode_tilde_key(23, m)),
        12 => Some(encode_tilde_key(24, m)),
        _ => None,
    }
}

fn encode_ctrl_char(ch: char, alt: bool) -> Option<Vec<u8>> {
    let lower = ch.to_ascii_lowercase();
    let ctrl = match lower {
        'a'..='z' => (lower as u8) - b'a' + 1,
        '@' | ' ' => 0x00,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        _ => return None,
    };
    if alt {
        Some(vec![0x1b, ctrl])
    } else {
        Some(vec![ctrl])
    }
}
