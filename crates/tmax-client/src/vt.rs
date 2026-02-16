use vte::{Params, Parser, Perform};

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

pub struct VtScreen {
    parser: Parser,
    state: ScreenState,
}

impl VtScreen {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            parser: Parser::new(),
            state: ScreenState::new(cols, rows),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.state.rows)
            .map(|row| self.state.line(row))
            .collect()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.state.resize(cols, rows);
    }

    pub fn load_snapshot(&mut self, cols: u16, rows: u16, lines: &[String]) {
        self.state.resize(usize::from(cols), usize::from(rows));
        self.state.clear_all();
        for (row, line) in lines.iter().enumerate().take(self.state.rows) {
            for (col, ch) in line.chars().enumerate().take(self.state.cols) {
                let idx = self.state.index(col, row);
                self.state.cells[idx].ch = ch;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VtScreen;

    #[test]
    fn plain_text_and_newline_render() {
        let mut screen = VtScreen::new(10, 3);
        screen.feed(b"hello\nworld");
        let lines = screen.lines();
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], "world");
    }

    #[test]
    fn cursor_move_escape_is_applied() {
        let mut screen = VtScreen::new(8, 3);
        screen.feed(b"abcd\x1b[2;3HZX");
        let lines = screen.lines();
        assert_eq!(lines[0], "abcd");
        assert_eq!(lines[1], "  ZX");
    }
}
