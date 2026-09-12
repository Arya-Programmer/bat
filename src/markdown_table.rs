//! Rendering of GitHub-flavored Markdown tables.
//!
//! Markdown tables are only readable when their columns line up, which is
//! rarely true in the source file. [`TableRenderer`] buffers the lines of a
//! table until it has seen all of them, then rewrites every cell so that the
//! columns share a common width, and draws a full border around the result:
//!
//! ```text
//!                        ┌───────┬─────┐
//! | Name | Age |         │ Name  │ Age │
//! |------|----:|   ->    ╞═══════╪═════╡
//! | Alice | 30 |         │ Alice │  30 │
//!                        └───────┴─────┘
//! ```
//!
//! The border follows the shape that terminal table renderers have settled on
//! (`comfy-table`'s `UTF8_FULL`, `rich`'s `SQUARE`): light box-drawing lines
//! all around, with a heavier rule below the header row.
//!
//! The rewrite happens before syntax highlighting. The two border lines are the
//! only lines that do not exist in the file, and they are printed the way a
//! wrapped line is - as a continuation, with an empty gutter - so that they
//! claim no line number of their own.

use unicode_width::UnicodeWidthStr;

use crate::line_range::MaxBufferedLineNumber;

/// A line that is held back while we figure out whether it belongs to a table.
pub(crate) struct PendingLine {
    pub out_of_range: bool,
    pub line_number: usize,
    pub max_buffered_line_number: MaxBufferedLineNumber,
    /// Set for the border lines, which are drawn in addition to the lines of
    /// the file and therefore carry no line number of their own.
    pub continuation: bool,
    pub line: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Alignment {
    Left,
    Center,
    Right,
}

#[derive(Default)]
pub(crate) struct TableRenderer {
    /// The candidate header row, plus every line that followed it while we were
    /// still collecting the table.
    pending: Vec<PendingLine>,
    /// Whether `pending` is known to be a table (i.e. the delimiter row was seen).
    in_table: bool,
    /// Whether we are inside a fenced code block, where tables must be left alone.
    in_code_fence: bool,
}

impl TableRenderer {
    /// Feeds the next line and returns the lines that are ready to be printed.
    pub(crate) fn feed(&mut self, line: PendingLine) -> Vec<PendingLine> {
        let mut ready = Vec::new();
        self.feed_into(line, &mut ready);
        ready
    }

    /// Returns everything that is still buffered, at the end of the input.
    pub(crate) fn flush(&mut self) -> Vec<PendingLine> {
        self.in_table = false;
        let pending = std::mem::take(&mut self.pending);
        render(pending)
    }

    fn feed_into(&mut self, line: PendingLine, ready: &mut Vec<PendingLine>) {
        let content = content_of(&line.line);

        // Four spaces of indentation start a code block, whose contents are
        // shown verbatim.
        if !self.in_table && content.starts_with("    ") {
            ready.extend(self.flush());
            ready.push(line);
            return;
        }

        if is_code_fence(content) {
            self.in_code_fence = !self.in_code_fence;
            ready.extend(self.flush());
            ready.push(line);
            return;
        }

        if self.in_code_fence {
            ready.push(line);
            return;
        }

        if self.in_table {
            // A table ends at the first line that is not a row.
            if split_row(content).is_some() {
                self.pending.push(line);
            } else {
                ready.extend(self.flush());
                ready.push(line);
            }
            return;
        }

        match self.pending.pop() {
            // We are holding a candidate header row: this line decides whether
            // it really was one.
            Some(header) => {
                if delimiter_row(content).is_some_and(|d| d.len() == cell_count(&header)) {
                    self.in_table = true;
                    self.pending.push(header);
                    self.pending.push(line);
                } else {
                    ready.push(header);
                    self.feed_into(line, ready);
                }
            }
            None if split_row(content).is_some() => self.pending.push(line),
            None => ready.push(line),
        }
    }
}

/// The number of cells in a line that is known to be a table row.
fn cell_count(line: &PendingLine) -> usize {
    split_row(content_of(&line.line)).map_or(0, |cells| cells.len())
}

/// Strips the line terminator, which is re-attached after rendering.
fn content_of(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// The line terminator of `line`, which is empty for an unterminated last line.
fn ending_of(line: &str) -> &str {
    &line[content_of(line).len()..]
}

fn is_code_fence(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Splits a table row into its trimmed cells, or returns `None` if the line
/// cannot be a table row.
fn split_row(content: &str) -> Option<Vec<String>> {
    let trimmed = content.trim();
    if !trimmed.contains('|') {
        return None;
    }

    let mut cells = vec![String::new()];
    let mut escaped = false;
    for c in trimmed.chars() {
        match c {
            _ if escaped => {
                escaped = false;
                cells.last_mut()?.push(c);
            }
            '\\' => escaped = true,
            '|' => cells.push(String::new()),
            _ => cells.last_mut()?.push(c),
        }
    }

    // Leading and trailing pipes are optional and do not introduce a cell.
    if trimmed.starts_with('|') {
        cells.remove(0);
    }
    if trimmed.ends_with('|') && !cells.is_empty() {
        cells.pop();
    }

    let cells: Vec<String> = cells.iter().map(|cell| cell.trim().to_string()).collect();
    (!cells.is_empty()).then_some(cells)
}

/// Parses the `---|:--:|---:` row that separates the header from the body.
fn delimiter_row(content: &str) -> Option<Vec<Alignment>> {
    let cells = split_row(content)?;
    cells
        .iter()
        .map(|cell| {
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                return None;
            }
            Some(match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Alignment::Center,
                (false, true) => Alignment::Right,
                _ => Alignment::Left,
            })
        })
        .collect()
}

/// Rewrites the buffered lines of a table and surrounds them with a border.
/// Anything that turned out not to be a table is returned unchanged.
fn render(mut pending: Vec<PendingLine>) -> Vec<PendingLine> {
    let Some(alignments) = pending
        .get(1)
        .and_then(|line| delimiter_row(content_of(&line.line)))
    else {
        return pending;
    };

    let rows: Vec<Vec<String>> = pending
        .iter()
        .map(|line| split_row(content_of(&line.line)).unwrap_or_default())
        .collect();

    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .enumerate()
                // The delimiter row only contributes its minimum width.
                .map(|(index, cells)| match cells.get(column) {
                    _ if index == 1 => 3,
                    Some(cell) => cell.width(),
                    None => 0,
                })
                .max()
                .unwrap_or(3)
        })
        .collect();

    let indent: String = content_of(&pending[0].line)
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let rule = |line: char, left: char, joint: char, right: char| {
        let columns: Vec<String> = widths
            .iter()
            .map(|width| line.to_string().repeat(width + 2))
            .collect();
        format!("{indent}{left}{}{right}", columns.join(&joint.to_string()))
    };

    // The last line of the file may not be terminated, in which case it is the
    // bottom border that has to stay unterminated.
    let last_ending = ending_of(&pending[pending.len() - 1].line).to_string();
    let inner_ending = if last_ending.is_empty() {
        "\n"
    } else {
        &last_ending
    };

    for (index, (line, cells)) in pending.iter_mut().zip(&rows).enumerate() {
        let ending = if index == rows.len() - 1 {
            inner_ending
        } else {
            ending_of(&line.line)
        };
        let rendered = if index == 1 {
            rule('═', '╞', '╪', '╡')
        } else {
            let columns: Vec<String> = widths
                .iter()
                .enumerate()
                .map(|(column, width)| {
                    let cell = cells.get(column).map_or("", String::as_str);
                    let alignment = alignments.get(column).copied().unwrap_or(Alignment::Left);
                    format!(" {} ", pad(cell, *width, alignment))
                })
                .collect();
            format!("{indent}│{}│", columns.join("│"))
        };
        line.line = format!("{rendered}{ending}");
    }

    // A border line is attached to the row it touches, so that it is skipped
    // and highlighted along with that row.
    let top = border(&pending[0], rule('─', '┌', '┬', '┐'), inner_ending);
    let bottom = border(
        &pending[pending.len() - 1],
        rule('─', '└', '┴', '┘'),
        &last_ending,
    );

    pending.insert(0, top);
    pending.push(bottom);
    pending
}

/// Builds one of the two border lines that are drawn around a table.
fn border(neighbour: &PendingLine, rendered: String, ending: &str) -> PendingLine {
    PendingLine {
        out_of_range: neighbour.out_of_range,
        line_number: neighbour.line_number,
        max_buffered_line_number: neighbour.max_buffered_line_number,
        continuation: true,
        line: format!("{rendered}{ending}"),
    }
}

fn pad(cell: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(cell.width());
    match alignment {
        Alignment::Left => format!("{cell}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{cell}", " ".repeat(padding)),
        Alignment::Center => format!(
            "{}{cell}{}",
            " ".repeat(padding / 2),
            " ".repeat(padding - padding / 2)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_all(input: &str) -> String {
        let mut renderer = TableRenderer::default();
        let mut output = Vec::new();
        for (index, line) in input.lines().enumerate() {
            output.extend(renderer.feed(PendingLine {
                out_of_range: false,
                line_number: index + 1,
                max_buffered_line_number: MaxBufferedLineNumber::Final(index + 1),
                continuation: false,
                line: format!("{line}\n"),
            }));
        }
        output.extend(renderer.flush());
        output.into_iter().map(|line| line.line).collect()
    }

    #[test]
    fn renders_a_table() {
        assert_eq!(
            render_all("| Name | Age |\n|---|---|\n| Alice | 30 |\n"),
            concat!(
                "┌───────┬─────┐\n",
                "│ Name  │ Age │\n",
                "╞═══════╪═════╡\n",
                "│ Alice │ 30  │\n",
                "└───────┴─────┘\n",
            )
        );
    }

    #[test]
    fn renders_a_table_without_outer_pipes() {
        assert_eq!(
            render_all("Name | Age\n--- | ---\nAlice | 30\n"),
            concat!(
                "┌───────┬─────┐\n",
                "│ Name  │ Age │\n",
                "╞═══════╪═════╡\n",
                "│ Alice │ 30  │\n",
                "└───────┴─────┘\n",
            )
        );
    }

    #[test]
    fn respects_alignment() {
        assert_eq!(
            render_all("| a | b | c |\n|:---|:---:|---:|\n| 1 | 2 | 3 |\n"),
            concat!(
                "┌─────┬─────┬─────┐\n",
                "│ a   │  b  │   c │\n",
                "╞═════╪═════╪═════╡\n",
                "│ 1   │  2  │   3 │\n",
                "└─────┴─────┴─────┘\n",
            )
        );
    }

    #[test]
    fn keeps_track_of_wide_characters() {
        assert_eq!(
            render_all("| a | b |\n|---|---|\n| 日本 | x |\n"),
            concat!(
                "┌──────┬─────┐\n",
                "│ a    │ b   │\n",
                "╞══════╪═════╡\n",
                "│ 日本 │ x   │\n",
                "└──────┴─────┘\n",
            )
        );
    }

    #[test]
    fn keeps_line_numbers_and_endings() {
        let mut renderer = TableRenderer::default();
        let mut output = Vec::new();
        for (index, line) in ["| a |\r\n", "|---|\r\n", "| 1 |\r\n"].iter().enumerate() {
            output.extend(renderer.feed(PendingLine {
                out_of_range: index == 0,
                line_number: index + 10,
                max_buffered_line_number: MaxBufferedLineNumber::Final(12),
                continuation: false,
                line: line.to_string(),
            }));
        }
        output.extend(renderer.flush());

        // The two border lines are attached to the rows they touch.
        assert_eq!(output.len(), 5);
        assert!(output[0].continuation);
        assert!(output[0].out_of_range);
        assert_eq!(output[0].line_number, 10);
        assert!(!output[1].continuation);
        assert_eq!(output[1].line_number, 10);
        assert_eq!(output[3].line_number, 12);
        assert!(output[4].continuation);
        assert_eq!(output[4].line_number, 12);
        assert!(output.iter().all(|line| line.line.ends_with("\r\n")));
    }

    #[test]
    fn leaves_everything_else_alone() {
        let text = concat!(
            "# Title\n",
            "\n",
            "a | b without a delimiter row\n",
            "some | text\n",
            "\n",
            "```\n",
            "| a | b |\n",
            "|---|---|\n",
            "```\n",
            "\n",
            "    | a | b |\n",
            "    |---|---|\n",
        );
        assert_eq!(render_all(text), text);
    }

    #[test]
    fn handles_ragged_rows_and_escaped_pipes() {
        assert_eq!(
            render_all("| a | b |\n|---|---|\n| 1 |\n| x \\| y | 2 |\n"),
            concat!(
                "┌───────┬─────┐\n",
                "│ a     │ b   │\n",
                "╞═══════╪═════╡\n",
                "│ 1     │     │\n",
                "│ x | y │ 2   │\n",
                "└───────┴─────┘\n",
            )
        );
    }
}
