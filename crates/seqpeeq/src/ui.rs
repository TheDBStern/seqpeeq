//! Drawing. Reads the [`App`](crate::app::App) state and never changes it.

use std::ops::Range;

use crate::app::App;

/// The tab label of each open dataset: its file name.
pub fn tab_titles(app: &App) -> Vec<String> {
    app.session
        .datasets()
        .iter()
        .map(|dataset| match dataset.path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => dataset.path.display().to_string(),
        })
        .collect()
}

/// The columns each tab occupies in the tab bar. A tab is drawn as its title
/// with one space either side, and tabs are separated by one `│` column.
/// Drawing and click handling both use this, so they cannot disagree.
pub fn tab_ranges(titles: &[String]) -> Vec<Range<u16>> {
    let mut start = 0u16;
    titles
        .iter()
        .map(|title| {
            let width = title.chars().count() as u16 + 2;
            let range = start..start.saturating_add(width);
            start = range.end.saturating_add(1);
            range
        })
        .collect()
}
