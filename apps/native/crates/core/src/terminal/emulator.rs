//! The VT core: `alacritty_terminal`'s `Term` driven by vte's `Processor`. Bytes go in, a
//! snapshot of the visible rows comes out with every color already resolved. The app shares one
//! per session with its network task behind a [`FairMutex`], so output is parsed off the UI
//! thread and the UI locks it only to read what it paints.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::Selection;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};
use super::Grid;

pub use alacritty_terminal::index::{Point as GridPoint, Side};
pub use alacritty_terminal::sync::FairMutex;
pub use alacritty_terminal::selection::SelectionType;
pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::Rgb;

const SCROLLBACK_LINES: usize = 5_000;

struct Size(Grid);

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.0.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.0.rows as usize
    }

    fn columns(&self) -> usize {
        self.0.cols as usize
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(event);
    }
}

/// The colors a program sees when it asks (OSC 4/10/11) and the defaults cells resolve to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellView {
    pub c: char,
    pub fg: Rgb,
    /// `None` paints nothing over the terminal background.
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub wide: bool,
    pub spacer: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
    Hollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorView {
    pub row: usize,
    pub col: usize,
    pub style: CursorStyle,
    pub wide: bool,
}

/// What one frame paints, copied out under the lock.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<CellView>,
    pub cursor: Option<CursorView>,
    pub display_offset: usize,
    pub history: usize,
}

impl Snapshot {
    pub fn row(&self, row: usize) -> &[CellView] {
        &self.cells[row * self.cols..(row + 1) * self.cols]
    }
}

pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    theme: ThemeColors,
    cell_size: (f32, f32),
}

impl Emulator {
    pub fn new(grid: Grid, theme: ThemeColors) -> Self {
        let events = Listener::default();
        let config = Config { scrolling_history: SCROLLBACK_LINES, ..Config::default() };
        Emulator {
            term: Term::new(config, &Size(grid), events.clone()),
            parser: Processor::new(),
            events,
            theme,
            cell_size: (7., 16.),
        }
    }

    pub fn set_theme(&mut self, theme: ThemeColors) {
        self.theme = theme;
    }

    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        self.cell_size = (width, height);
    }

    /// Parses live PTY output and returns the replies the program asked for (cursor position,
    /// device attributes, colors), which belong on the PTY.
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        self.parser.advance(&mut self.term, bytes);
        self.take_replies()
    }

    /// Starts over from a session's history. Replies are dropped: they answered queries from
    /// a program that has long moved on, and writing them would type garbage into the shell.
    pub fn reset(&mut self, history: &str) {
        let grid = self.grid();
        *self = Emulator { cell_size: self.cell_size, ..Emulator::new(grid, self.theme) };
        self.parser.advance(&mut self.term, history.as_bytes());
        self.take_replies();
    }

    /// Prints a client-side note into the grid without answering anything.
    pub fn print_local(&mut self, text: &str) {
        self.parser.advance(&mut self.term, text.as_bytes());
        self.take_replies();
    }

    /// When an unfinished synchronized update (DEC 2026) must be flushed.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    pub fn flush_sync(&mut self) -> String {
        self.parser.stop_sync(&mut self.term);
        self.take_replies()
    }

    fn take_replies(&mut self) -> String {
        let events = std::mem::take(&mut *self.events.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        let mut replies = String::new();
        for event in events {
            match event {
                Event::PtyWrite(text) => replies.push_str(&text),
                Event::ColorRequest(index, format) => {
                    let color = self.term.colors()[index].unwrap_or_else(|| self.default_color(index));
                    replies.push_str(&format(color));
                }
                Event::TextAreaSizeRequest(format) => {
                    let (cell_width, cell_height) = self.cell_size;
                    let grid = self.grid();
                    replies.push_str(&format(WindowSize {
                        num_lines: grid.rows,
                        num_cols: grid.cols,
                        cell_width: cell_width as u16,
                        cell_height: cell_height as u16,
                    }));
                }
                _ => {}
            }
        }
        replies
    }

    pub fn grid(&self) -> Grid {
        Grid::new(self.term.columns() as u16, self.term.screen_lines() as u16)
    }

    pub fn resize(&mut self, grid: Grid) {
        if grid != self.grid() {
            self.term.resize(Size(grid));
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// The grid point under a visible cell; rows count from the top of the viewport.
    pub fn point_at(&self, row: usize, col: usize) -> Point {
        let last_col = self.term.columns().saturating_sub(1);
        Point::new(Line(row as i32 - self.display_offset() as i32), Column(col.min(last_col)))
    }

    pub fn start_selection(&mut self, kind: SelectionType, point: Point, side: Side) {
        self.term.selection = Some(Selection::new(kind, point, side));
    }

    pub fn update_selection(&mut self, point: Point, side: Side) {
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn select_all(&mut self) {
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), self.term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        self.term.selection = Some(selection);
    }

    pub fn has_selection(&self) -> bool {
        self.selection_text().is_some()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|text| !text.is_empty())
    }

    /// What a wheel step means: input for a program that tracks the mouse (SGR reports) or owns
    /// the alternate screen (arrow keys, as xterm sends), or `None` to scroll the scrollback.
    /// Positive `lines` scroll up.
    pub fn wheel_input(&self, lines: i32, row: usize, col: usize) -> Option<String> {
        let mode = self.mode();
        let count = lines.unsigned_abs() as usize;
        if count == 0 {
            return None;
        }
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
            let button = if lines > 0 { 64 } else { 65 };
            return Some(format!("\x1b[<{button};{};{}M", col + 1, row + 1).repeat(count));
        }
        if mode.contains(TermMode::ALT_SCREEN) {
            let app = mode.contains(TermMode::APP_CURSOR);
            let arrow = match (lines > 0, app) {
                (true, false) => "\x1b[A",
                (true, true) => "\x1bOA",
                (false, false) => "\x1b[B",
                (false, true) => "\x1bOB",
            };
            return Some(arrow.repeat(count));
        }
        None
    }

    /// Focus reports for programs that asked for them (DEC 1004).
    pub fn focus_input(&self, focused: bool) -> Option<&'static str> {
        self.mode().contains(TermMode::FOCUS_IN_OUT).then_some(if focused { "\x1b[I" } else { "\x1b[O" })
    }

    pub fn snapshot(&self, out: &mut Snapshot) {
        let content = self.term.renderable_content();
        let (cols, rows) = (self.term.columns(), self.term.screen_lines());
        out.cols = cols;
        out.rows = rows;
        out.display_offset = content.display_offset;
        out.history = self.term.grid().history_size();
        out.cells.clear();
        let blank = CellView {
            c: ' ',
            fg: self.theme.foreground,
            bg: None,
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            strikeout: false,
            wide: false,
            spacer: false,
            selected: false,
        };
        out.cells.resize(cols * rows, blank);
        let offset = content.display_offset as i32;
        let colors = content.colors;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            if row < 0 || row as usize >= rows || indexed.point.column.0 >= cols {
                continue;
            }
            let cell = indexed.cell;
            let flags = cell.flags;
            let mut fg = self.resolve(cell.fg, colors);
            let mut bg = match cell.bg {
                Color::Named(NamedColor::Background) => None,
                other => Some(self.resolve(other, colors)),
            };
            if flags.contains(Flags::INVERSE) {
                let background = bg.unwrap_or(self.theme.background);
                bg = Some(fg);
                fg = background;
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg.unwrap_or(self.theme.background);
            }
            let selected = content.selection.is_some_and(|range| range.contains(indexed.point));
            out.cells[row as usize * cols + indexed.point.column.0] = CellView {
                c: if flags.contains(Flags::HIDDEN) { ' ' } else { cell.c },
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                dim: flags.contains(Flags::DIM),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikeout: flags.contains(Flags::STRIKEOUT),
                wide: flags.contains(Flags::WIDE_CHAR),
                spacer: flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                selected,
            };
        }
        let cursor = content.cursor;
        let row = cursor.point.line.0 + offset;
        out.cursor = (cursor.shape != CursorShape::Hidden && row >= 0 && (row as usize) < rows).then(|| CursorView {
            row: row as usize,
            col: cursor.point.column.0.min(cols.saturating_sub(1)),
            style: match cursor.shape {
                CursorShape::Beam => CursorStyle::Beam,
                CursorShape::Underline => CursorStyle::Underline,
                CursorShape::HollowBlock => CursorStyle::Hollow,
                _ => CursorStyle::Block,
            },
            wide: self.term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR),
        });
    }

    fn resolve(&self, color: Color, colors: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(index) => colors[index as usize].unwrap_or_else(|| self.default_color(index as usize)),
            Color::Named(named) => colors[named].unwrap_or_else(|| self.default_color(named as usize)),
        }
    }

    /// The theme's color for a palette slot, `NamedColor` slot, or xterm's 256-color index.
    fn default_color(&self, index: usize) -> Rgb {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let theme = &self.theme;
        match index {
            0..=15 => theme.ansi[index],
            16..=231 => {
                let n = index - 16;
                Rgb { r: CUBE[n / 36], g: CUBE[(n / 6) % 6], b: CUBE[n % 6] }
            }
            232..=255 => {
                let level = 8 + 10 * (index - 232) as u8;
                Rgb { r: level, g: level, b: level }
            }
            _ if index == NamedColor::Background as usize => theme.background,
            _ if index == NamedColor::Cursor as usize => theme.cursor,
            _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) => {
                theme.ansi[index - NamedColor::DimBlack as usize]
            }
            _ => theme.foreground,
        }
    }
}

/// The cell under a pointer offset from the grid's top-left corner, and which half of it was
/// hit. Offsets outside the grid clamp to its edge and force the side, so a drag that leaves
/// the pane still extends to the row or column it left through.
pub fn hit_cell(x: f32, y: f32, cell_width: f32, line_height: f32, cols: usize, rows: usize) -> (usize, usize, Side) {
    let usable = |v: f32| v.is_finite() && v > 0.;
    if cols == 0 || rows == 0 || !usable(cell_width) || !usable(line_height) {
        return (0, 0, Side::Left);
    }
    let (x, y) = (if x.is_finite() { x } else { 0. }, if y.is_finite() { y } else { 0. });
    let mut side = if x.max(0.) % cell_width > cell_width / 2. { Side::Right } else { Side::Left };
    let raw_col = (x / cell_width).floor();
    let col = if raw_col > (cols - 1) as f32 {
        side = Side::Right;
        cols - 1
    } else {
        raw_col.max(0.) as usize
    };
    let raw_row = (y / line_height).floor();
    let row = if raw_row > (rows - 1) as f32 {
        side = Side::Right;
        rows - 1
    } else if raw_row < 0. {
        side = Side::Left;
        0
    } else {
        raw_row as usize
    };
    (row, col, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> ThemeColors {
        let gray = |v| Rgb { r: v, g: v, b: v };
        ThemeColors { foreground: gray(240), background: gray(0), cursor: gray(200), ansi: [gray(1); 16] }
    }

    fn text(emulator: &Emulator, row: usize) -> String {
        let mut snapshot = Snapshot::default();
        emulator.snapshot(&mut snapshot);
        snapshot.row(row).iter().filter(|cell| !cell.spacer).map(|cell| cell.c).collect::<String>().trim_end().to_owned()
    }

    #[test]
    fn output_lands_in_the_grid_and_queries_get_replies() {
        let mut emulator = Emulator::new(Grid::new(20, 4), theme());
        assert_eq!(emulator.feed(b"one\r\ntwo\x1b[6n"), "\x1b[2;4R");
        assert_eq!((text(&emulator, 0), text(&emulator, 1)), ("one".into(), "two".into()));
        assert_eq!(emulator.feed(b"\x1b]11;?\x07"), "\x1b]11;rgb:0000/0000/0000\x07");
    }

    #[test]
    fn reset_replays_history_without_replying() {
        let mut emulator = Emulator::new(Grid::new(20, 4), theme());
        emulator.feed(b"old");
        emulator.reset("fresh\x1b[6n");
        assert_eq!(text(&emulator, 0), "fresh");
        assert_eq!(emulator.feed(b""), "");
    }

    #[test]
    fn inverse_and_hidden_cells_resolve_their_colors() {
        let mut emulator = Emulator::new(Grid::new(10, 2), theme());
        emulator.feed(b"\x1b[31;7mI\x1b[0m\x1b[8mH\x1b[0m\x1b[38;5;196mR");
        let mut snapshot = Snapshot::default();
        emulator.snapshot(&mut snapshot);
        let row = snapshot.row(0);
        assert_eq!((row[0].fg, row[0].bg), (theme().background, Some(theme().ansi[1])));
        assert_eq!(row[1].c, ' ');
        assert_eq!(row[2].fg, Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn selection_and_scrollback() {
        let mut emulator = Emulator::new(Grid::new(10, 3), theme());
        for line in 1..=6 {
            emulator.feed(format!("line{line}\r\n").as_bytes());
        }
        emulator.scroll(2);
        assert_eq!(text(&emulator, 0), "line3");
        let start = emulator.point_at(0, 0);
        emulator.start_selection(SelectionType::Simple, start, Side::Left);
        emulator.update_selection(emulator.point_at(0, 4), Side::Right);
        assert_eq!(emulator.selection_text().as_deref(), Some("line3"));
        emulator.scroll_to_bottom();
        assert_eq!(emulator.display_offset(), 0);
        emulator.select_all();
        assert!(emulator.selection_text().is_some_and(|all| all.starts_with("line1")));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut emulator = Emulator::new(Grid::new(10, 2), theme());
        emulator.feed("宽w".as_bytes());
        let mut snapshot = Snapshot::default();
        emulator.snapshot(&mut snapshot);
        let row = snapshot.row(0);
        assert!(row[0].wide && row[1].spacer && row[2].c == 'w');
        assert_eq!(snapshot.cursor.map(|cursor| cursor.col), Some(3));
    }
    #[test]
    fn pointer_maps_to_cells_and_clamps_with_a_forced_side() {
        assert_eq!(hit_cell(25., 45., 10., 20., 8, 4), (2, 2, Side::Left));
        assert_eq!(hit_cell(26., 0., 10., 20., 8, 4), (0, 2, Side::Right));
        assert_eq!(hit_cell(9_999., 0., 10., 20., 8, 4), (0, 7, Side::Right));
        assert_eq!(hit_cell(1., 9_999., 10., 20., 8, 4), (3, 0, Side::Right));
        assert_eq!(hit_cell(75., -50., 10., 20., 8, 4), (0, 7, Side::Left));
        assert_eq!(hit_cell(f32::NAN, 5., 0., 20., 8, 4), (0, 0, Side::Left));
    }

    #[test]
    fn wheel_goes_to_mouse_programs_and_the_alternate_screen() {
        let mut emulator = Emulator::new(Grid::new(20, 5), theme());
        assert_eq!(emulator.wheel_input(2, 0, 0), None);
        emulator.feed(b"\x1b[?1049h");
        assert_eq!(emulator.wheel_input(-2, 0, 0).as_deref(), Some("\x1b[B\x1b[B"));
        emulator.feed(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(emulator.wheel_input(1, 2, 3).as_deref(), Some("\x1b[<64;4;3M"));
        assert_eq!(emulator.focus_input(true), None);
        emulator.feed(b"\x1b[?1004h");
        assert_eq!(emulator.focus_input(false), Some("\x1b[O"));
    }
}
