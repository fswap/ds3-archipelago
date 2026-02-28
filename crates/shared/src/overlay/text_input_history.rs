use std::collections::VecDeque;

use imgui::*;

/// The maximum number of messages to store in the history.
const MAX_LENGTH: usize = 500;

/// History for a single-line text input that's used repeatedly, such as a
/// messenger input or a command prompt.
#[derive(Default)]
pub struct TextInputHistory {
    /// The history of lines in a text input.
    history: VecDeque<String>,

    /// The current index into [history]. None means that the user hasn't
    /// scrolled into the history at all.
    cursor: Option<usize>,
}

impl TextInputHistory {
    /// Creates a new, empty history.
    pub fn new() -> Self {
        Default::default()
    }

    /// Adds `line` to this input's history.
    pub fn add(&mut self, line: String) {
        if self.history.len() >= MAX_LENGTH {
            self.history.pop_back();
        }
        self.history.push_front(line);
        self.cursor = None;
    }
}

impl InputTextCallbackHandler for &mut TextInputHistory {
    fn on_history(&mut self, dir: HistoryDirection, mut text: TextCallbackData) {
        if dir == HistoryDirection::Up {
            let cursor = self.cursor.map(|c| c + 1).unwrap_or_default();
            if let Some(line) = self.history.get(cursor) {
                text.clear();
                text.push_str(line);
                self.cursor = Some(cursor);
            }
        } else if let Some(mut cursor) = self.cursor
            && cursor > 0
        {
            cursor -= 1;
            self.cursor = Some(cursor);
            text.clear();
            text.push_str(&self.history[cursor]);
        } else {
            self.cursor = None;
        }
    }
}
