#![forbid(unsafe_code)]

use std::rc::Rc;

use rune::{Color, El, GridRow, LineCap, Shape, Span, TextStyle, grid};

use crate::color::{Attrs, Palette, TermColor, srgb_to_rune};
use crate::input::Selection;
use crate::screen::{Cell, Row, Screen};

#[derive(Clone, Copy, Debug)]
pub struct TermMetrics {
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl Default for TermMetrics {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            cell_w: 8.4,
            cell_h: 18.0,
        }
    }
}

struct CachedRow {
    generation: u64,
    cursor_col: Option<u16>,
    selection_cols: Option<(u16, u16)>,
    grid_row: Rc<GridRow>,
}

pub struct TermView {
    metrics: TermMetrics,
    cached_cols: u16,
    cached_rows: u16,
    cache: Vec<Option<CachedRow>>,
}

impl Default for TermView {
    fn default() -> Self {
        Self::new(TermMetrics::default())
    }
}

impl TermView {
    pub fn new(metrics: TermMetrics) -> Self {
        assert!(metrics.font_size > 0.0 && metrics.font_size.is_finite());
        assert!(metrics.cell_w > 0.0 && metrics.cell_w.is_finite());
        assert!(metrics.cell_h > 0.0 && metrics.cell_h.is_finite());
        Self {
            metrics,
            cached_cols: 0,
            cached_rows: 0,
            cache: Vec::new(),
        }
    }

    pub const fn metrics(&self) -> TermMetrics {
        self.metrics
    }

    pub fn build(&mut self, screen: &Screen, selection: Option<Selection>) -> El {
        let cols = screen.cols();
        let rows = screen.rows();
        if self.cached_cols != cols || self.cached_rows != rows {
            self.cached_cols = cols;
            self.cached_rows = rows;
            self.cache.clear();
            self.cache.resize_with(usize::from(rows), || None);
        }
        let cursor = screen.cursor();
        let show_cursor = cursor.visible && screen.viewport_offset() == 0;
        let palette = *screen.palette();
        let mut out_rows = Vec::with_capacity(usize::from(rows));
        for r in 0..rows {
            let row = screen.visible_row(r);
            let cursor_col = (show_cursor && cursor.row == r).then_some(cursor.col);
            let selection_cols = selection.and_then(|s| s.row_cols(r, cols));
            let reuse = self.cache[usize::from(r)].as_ref().is_some_and(|c| {
                c.generation == row.generation
                    && c.cursor_col == cursor_col
                    && c.selection_cols == selection_cols
            });
            if !reuse {
                let built = Rc::new(build_grid_row(
                    row,
                    cols,
                    &palette,
                    self.metrics,
                    cursor_col,
                    selection_cols,
                ));
                self.cache[usize::from(r)] = Some(CachedRow {
                    generation: row.generation,
                    cursor_col,
                    selection_cols,
                    grid_row: built,
                });
            }
            if let Some(cached) = &self.cache[usize::from(r)] {
                out_rows.push(Rc::clone(&cached.grid_row));
            }
        }
        let mut el = grid(cols, self.metrics.cell_w, self.metrics.cell_h, out_rows)
            .w(f32::from(cols) * self.metrics.cell_w)
            .h(f32::from(rows) * self.metrics.cell_h);
        if show_cursor {
            el = el.grid_caret(cursor.col, cursor.row);
        }
        el
    }
}

fn build_grid_row(
    row: &Row,
    cols: u16,
    palette: &Palette,
    metrics: TermMetrics,
    cursor_col: Option<u16>,
    selection_cols: Option<(u16, u16)>,
) -> GridRow {
    let base_style = TextStyle::new(metrics.font_size, 400.0)
        .mono()
        .lh(metrics.cell_h);
    let sel_bg = srgb_to_rune([58, 92, 148]);
    let cur_bg = srgb_to_rune(palette.default_fg);
    let cur_fg = srgb_to_rune(palette.default_bg);

    let mut line_text = String::with_capacity(usize::from(cols));
    let mut spans: Vec<Span> = Vec::new();
    let mut pinned: Vec<(u16, u16, String, TextStyle, Color)> = Vec::new();
    let mut fills: Vec<(u16, u16, Color)> = Vec::new();
    let mut shape = Shape::new();

    let mut col = 0u16;
    while col < cols {
        let cell = row.cells[usize::from(col)];
        let width = u16::from(cell.width.max(1));
        let in_sel = selection_cols.is_some_and(|(l, r)| col >= l && col < r);
        let is_cursor = cursor_col == Some(col);
        let (mut fg, mut bg, explicit_bg) = resolve_paint_colors(cell, palette);
        if is_cursor {
            fg = cur_fg;
            bg = cur_bg;
        } else if in_sel {
            bg = sel_bg;
        }
        if is_cursor || in_sel || explicit_bg {
            push_fill(&mut fills, col, (col + width).min(cols), bg);
        }
        if cell.width == 0 {
            line_text.push(' ');
            col += 1;
            continue;
        }
        let visible_ch = if cell.attrs.contains(Attrs::HIDDEN) {
            ' '
        } else {
            cell.ch
        };
        let is_ascii_narrow = width == 1 && visible_ch.is_ascii() && visible_ch >= ' ';
        if is_ascii_narrow {
            let start = line_text.len();
            line_text.push(visible_ch);
            let end = line_text.len();
            if visible_ch != ' ' || cell.attrs.contains(Attrs::UNDERLINE) {
                let mut sp = Span::new(start, end).color(fg);
                if cell.attrs.contains(Attrs::BOLD) {
                    sp = sp.bold();
                }
                if cell.attrs.contains(Attrs::ITALIC) {
                    sp = sp.italic();
                }
                if cell.attrs.contains(Attrs::UNDERLINE) {
                    sp = sp.underline(fg);
                }
                push_span(&mut spans, sp);
            }
        } else {
            for _ in 0..width {
                line_text.push(' ');
            }
            if visible_ch != ' ' {
                let mut cell_style = base_style;
                if cell.attrs.contains(Attrs::BOLD) {
                    cell_style.weight = 600.0;
                }
                if cell.attrs.contains(Attrs::ITALIC) {
                    cell_style = cell_style.italic();
                }
                pinned.push((col, width, visible_ch.to_string(), cell_style, fg));
            }
        }
        let x0 = f32::from(col) * metrics.cell_w;
        let x1 = f32::from((col + width).min(cols)) * metrics.cell_w;
        if cell.attrs.contains(Attrs::DOUBLE_UNDERLINE) {
            let y1 = metrics.cell_h - 3.0;
            let y2 = metrics.cell_h - 1.0;
            shape = shape.line([x0, y1], [x1, y1], 1.0, LineCap::Butt, fg).line(
                [x0, y2],
                [x1, y2],
                1.0,
                LineCap::Butt,
                fg,
            );
        }
        if cell.attrs.contains(Attrs::STRIKETHROUGH) {
            let ym = metrics.cell_h * 0.5;
            shape = shape.line([x0, ym], [x1, ym], 1.0, LineCap::Butt, fg);
        }
        col += width;
    }

    let mut grow = GridRow::new(line_text, spans, base_style).wraps(row.wrapped);
    for (s, e, color) in fills {
        grow = grow.fill(s, e, color);
    }
    for (c, w, text, style, color) in pinned {
        grow = grow.cell(c, w, text, style, color);
    }
    if !shape.is_empty() {
        grow = grow.shape(shape);
    }
    grow
}

fn resolve_paint_colors(cell: Cell, palette: &Palette) -> (Color, Color, bool) {
    let mut fg_rgb = palette.resolve_fg(cell.fg);
    let mut bg_rgb = palette.resolve_bg(cell.bg);
    let mut explicit_bg = cell.bg != TermColor::Default;
    if cell.attrs.contains(Attrs::INVERSE) {
        std::mem::swap(&mut fg_rgb, &mut bg_rgb);
        explicit_bg = true;
    }
    if cell.attrs.contains(Attrs::DIM) {
        fg_rgb = [fg_rgb[0] / 2, fg_rgb[1] / 2, fg_rgb[2] / 2];
    }
    (srgb_to_rune(fg_rgb), srgb_to_rune(bg_rgb), explicit_bg)
}

fn push_fill(fills: &mut Vec<(u16, u16, Color)>, start: u16, end: u16, color: Color) {
    if let Some(last) = fills.last_mut()
        && last.1 == start
        && last.2 == color
    {
        last.1 = end;
        return;
    }
    fills.push((start, end, color));
}

fn push_span(spans: &mut Vec<Span>, span: Span) {
    if let Some(last) = spans.last_mut()
        && last.end == span.start
        && last.weight == span.weight
        && last.italic == span.italic
        && last.color == span.color
        && last.underline == span.underline
    {
        last.end = span.end;
        return;
    }
    spans.push(span);
}
