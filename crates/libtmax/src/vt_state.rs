use vte::{Params, Parser, Perform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    ch: char,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ' }
    }
}

#[derive(Debug)]
struct ScreenState {
    cols: usize,
    rows: usize,
    cursor_x: usize,
    cursor_y: usize,
    cells: Vec<Cell>,
}

impl ScreenState {
    fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            cells: vec![Cell::default(); cols * rows],
        }
    }

    fn index(&self, x: usize, y: usize) -> usize {
        y * self.cols + x
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_x >= self.cols {
            self.line_feed();
        }
        let idx = self.index(self.cursor_x, self.cursor_y);
        self.cells[idx].ch = ch;
        self.cursor_x += 1;
    }

    fn carriage_return(&mut self) {
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        self.cursor_x = self.cursor_x.saturating_sub(1);
    }

    fn line_feed(&mut self) {
        self.cursor_x = 0;
        if self.cursor_y + 1 < self.rows {
            self.cursor_y += 1;
            return;
        }
        self.scroll_up();
    }

    fn scroll_up(&mut self) {
        for row in 1..self.rows {
            for col in 0..self.cols {
                let src = self.index(col, row);
                let dst = self.index(col, row - 1);
                self.cells[dst] = self.cells[src];
            }
        }
        for col in 0..self.cols {
            let idx = self.index(col, self.rows - 1);
            self.cells[idx] = Cell::default();
        }
    }

    fn clear_all(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::default();
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    fn clear_line_to_end(&mut self) {
        for col in self.cursor_x..self.cols {
            let idx = self.index(col, self.cursor_y);
            self.cells[idx] = Cell::default();
        }
    }

    fn move_cursor(&mut self, row: usize, col: usize) {
        self.cursor_y = row.min(self.rows.saturating_sub(1));
        self.cursor_x = col.min(self.cols.saturating_sub(1));
    }

    fn line(&self, row: usize) -> String {
        let mut line = String::with_capacity(self.cols);
        let start = row * self.cols;
        let end = start + self.cols;
        for cell in &self.cells[start..end] {
            line.push(cell.ch);
        }
        while line.ends_with(' ') {
            line.pop();
        }
        line
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut next = vec![Cell::default(); cols * rows];
        let copy_rows = rows.min(self.rows);
        let copy_cols = cols.min(self.cols);

        for row in 0..copy_rows {
            for col in 0..copy_cols {
                let old_idx = self.index(col, row);
                let new_idx = row * cols + col;
                next[new_idx] = self.cells[old_idx];
            }
        }

        self.cols = cols;
        self.rows = rows;
        self.cells = next;
        self.cursor_x = self.cursor_x.min(cols.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(rows.saturating_sub(1));
    }
}

impl Perform for ScreenState {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.line_feed(),
            b'\r' => self.carriage_return(),
            0x08 => self.backspace(),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match action {
            'H' | 'f' => {
                let row = param_or(params, 0, 1).saturating_sub(1);
                let col = param_or(params, 1, 1).saturating_sub(1);
                self.move_cursor(row, col);
            }
            'J' => self.clear_all(),
            'K' => self.clear_line_to_end(),
            _ => {}
        }
    }
}

fn param_or(params: &Params, index: usize, default: usize) -> usize {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first())
        .map(|v| usize::from(*v))
        .unwrap_or(default)
}

pub struct VtState {
    parser: Parser,
    state: ScreenState,
}

impl VtState {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: Parser::new(),
            state: ScreenState::new(usize::from(cols), usize::from(rows)),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.state.resize(usize::from(cols), usize::from(rows));
    }

    pub fn snapshot(&self) -> VtSnapshot {
        let mut lines = Vec::with_capacity(self.state.rows);
        for row in 0..self.state.rows {
            lines.push(self.state.line(row));
        }
        VtSnapshot {
            cols: u16::try_from(self.state.cols).unwrap_or(u16::MAX),
            rows: u16::try_from(self.state.rows).unwrap_or(u16::MAX),
            lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VtState;

    #[test]
    fn parses_plain_and_cursor_sequences() {
        let mut vt = VtState::new(10, 3);
        vt.feed(b"hello\n");
        vt.feed(b"\x1b[2;3HXY");
        let snap = vt.snapshot();
        assert_eq!(snap.lines[0], "hello");
        assert_eq!(snap.lines[1], "  XY");
    }

    #[test]
    fn deterministic_replay_matches_single_pass() {
        let sequence = b"alpha\nbeta\r\n\x1b[2;3HZZ";

        let mut one_pass = VtState::new(12, 4);
        one_pass.feed(sequence);
        let one_snapshot = one_pass.snapshot();

        let mut replay = VtState::new(12, 4);
        for chunk in sequence.chunks(2) {
            replay.feed(chunk);
        }
        let replay_snapshot = replay.snapshot();

        assert_eq!(one_snapshot, replay_snapshot);
    }
}
