//! Interface state and how input events change it. No drawing, no terminal I/O.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::layout::Size;
use seqpeeq_core::{
    Alignment, Command, Conservation, Dataset, Error, Finished, Job, MsaCmd, NodeId, NodeSel,
    Progress, Session, Started, Summary, TreeCmd, TreeDoc, View, summary_block,
};

use crate::line::{History, LineEdit};
use crate::msa_view::{
    Cell, Colouring, MsaView, Pending, Region, Search, SelKind, Select, Shape, Typed, parse_goto,
};
use crate::tree_view::{TreeSearch, TreeView, common_ancestor};
use crate::ui;
use crate::ui::grid::{self, Hit};
use crate::zoom::{self, Scale};

pub enum Mode {
    Normal,
    /// Typing one line at the bottom of the screen.
    Prompt(Prompt),
    /// Typing into residues: letters are text, not commands.
    Edit(Typing),
    /// `f` was pressed: the next key is the character to fill with.
    FillChar,
    /// Waiting for `y` or anything else in answer to a question.
    Confirm(Confirm),
    Help,
}

/// How typed characters go into a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    /// In front of the cursor, shifting what follows (`i`).
    Insert,
    /// Over the residues (`R`).
    Replace,
}

/// What the user is being asked to confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Quitting while an alignment has edits not written to a file.
    Quit,
    /// Closing such an alignment.
    Close,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub line: LineEdit,
}

/// What a line being typed is for.
pub enum PromptKind {
    /// The `:` command line.
    Command,
    /// Where to move the cursor: a column, or a row and a column.
    Goto,
    /// A new name for the sequence in `row`.
    Rename { row: usize },
    /// A regular expression to look for in the sequence names.
    Search,
    /// A new name for the tip `node` of the visible tree.
    RenameTip { node: NodeId },
    /// The file to write the clade at `at` to.
    TreeOut { at: NodeSel },
    /// The file for that clade's sequences, or nothing.
    TreeSeqs { at: NodeSel, out: String },
}

impl PromptKind {
    /// What is shown in front of the text.
    pub fn lead(&self) -> &'static str {
        match self {
            PromptKind::Command => ":",
            PromptKind::Goto => "go to: ",
            PromptKind::Rename { .. } => "name: ",
            PromptKind::Search => "/",
            PromptKind::RenameTip { .. } => "name: ",
            PromptKind::TreeOut { .. } => "tree file: ",
            PromptKind::TreeSeqs { .. } => "sequence file (Enter for none): ",
        }
    }
}

/// Work for the event loop to hand to a worker thread.
pub enum Request {
    /// The slow part of a command.
    Run(Job),
    /// Count the columns of one state of an alignment.
    Conserve {
        /// The [`Dataset::id`] of the alignment.
        dataset: u64,
        view: View,
        progress: Arc<Progress>,
    },
    /// Tally every row of one state of an alignment for the overview.
    Summarise {
        /// The [`Dataset::id`] of the alignment.
        dataset: u64,
        view: View,
        /// The count of that state's columns.
        conserved: Conservation,
        progress: Arc<Progress>,
    },
}

/// Work that a worker thread has finished.
pub enum Done {
    Command(Finished),
    /// A count of columns; `None` if it was cancelled.
    Conservation {
        dataset: u64,
        result: Option<Conservation>,
    },
    /// A summary for the overview; `None` if it was cancelled.
    Summary {
        dataset: u64,
        result: Option<Summary>,
    },
}

/// Alignments of up to this many residues are counted for conservation at
/// once, which takes a few milliseconds; larger ones on a worker thread.
pub const INLINE_CELLS: usize = 4_000_000;

/// The command that is running in the background.
pub struct Busy {
    /// What it is doing, such as `opening a.fa`.
    pub label: String,
    pub progress: Arc<Progress>,
    /// Whether the visible tab should move to the current file when it ends.
    follow: bool,
    /// Whether it came from the queue rather than from the user just now.
    queued: bool,
}

pub struct App {
    pub session: Session,
    pub mode: Mode,
    /// The last outcome or error, shown on the bottom line.
    pub status: String,
    /// Index of the dataset whose tab is shown.
    pub active: usize,
    pub show_log: bool,
    pub quit: bool,
    /// The command running in the background, if any. Until it ends, every
    /// other command is refused.
    pub busy: Option<Busy>,
    /// Work waiting for the event loop to hand to a thread.
    pub outbox: Vec<Request>,
    /// Commands waiting for the session to be free, oldest first.
    pub queue: VecDeque<Command>,
    /// The size of the terminal.
    pub screen: Size,
    /// Where the user is in each open alignment, by [`Dataset::id`].
    ///
    /// [`Dataset::id`]: seqpeeq_core::Dataset::id
    pub views: HashMap<u64, MsaView>,
    /// Where the user is in each open tree, by [`Dataset::id`].
    pub trees: HashMap<u64, TreeView>,
    /// The largest alignment, in residues, to count on the spot.
    pub inline_cells: usize,
    /// The newest count of each alignment's columns, by [`Dataset::id`].
    pub conserved: HashMap<u64, Conservation>,
    /// The count running in the background: the dataset, the revision
    /// being counted, and its progress.
    pub counting: Option<(u64, u64, Arc<Progress>)>,
    /// The newest summary of each alignment, by [`Dataset::id`].
    pub summaries: HashMap<u64, Summary>,
    /// The summary being made in the background: the dataset, the
    /// revision, and its progress.
    pub summarising: Option<(u64, u64, Arc<Progress>)>,
    /// Lines entered on the command line.
    history: History,
    /// Whether the left button went down on the grid and is still held, so
    /// that mouse movement is a drag across it.
    dragging: bool,
}

impl App {
    pub fn new(session: Session) -> App {
        App {
            session,
            mode: Mode::Normal,
            status: String::new(),
            active: 0,
            show_log: false,
            quit: false,
            busy: None,
            outbox: Vec::new(),
            queue: VecDeque::new(),
            screen: Size::new(80, 24),
            views: HashMap::new(),
            trees: HashMap::new(),
            inline_cells: INLINE_CELLS,
            conserved: HashMap::new(),
            counting: None,
            summaries: HashMap::new(),
            summarising: None,
            history: History::default(),
            dragging: false,
        }
    }

    /// Add a command to run when the session is free.
    pub fn enqueue(&mut self, cmd: Command) {
        self.queue.push_back(cmd);
    }

    /// Called once per turn of the event loop: start queued commands while
    /// nothing is running.
    pub fn pump(&mut self) {
        self.sync_views();
        self.rescale();
        while self.busy.is_none() {
            let Some(cmd) = self.queue.pop_front() else {
                break;
            };
            let ran = self.run_command(cmd);
            if let Some(busy) = &mut self.busy {
                busy.queued = true;
            }
            self.report(ran, true);
        }
        self.refresh_conservation();
        self.refresh_summary();
    }

    /// Make sure the visible alignment has, or is getting, a summary of the
    /// state it is in, while its lines hold several sequences. It waits for
    /// a count of exactly that state.
    fn refresh_summary(&mut self) {
        let current = self
            .session
            .datasets()
            .get(self.active)
            .and_then(|dataset| {
                let (alignment, view) = (dataset.msa()?, self.views.get(&dataset.id)?);
                (view.zoom > 0
                    && alignment.is_aligned()
                    && summary_block(alignment.rows(), alignment.cols()).is_some())
                .then_some((dataset.id, alignment.revision()))
            });
        if let Some((id, revision, progress)) = &self.summarising
            && current != Some((*id, *revision))
        {
            progress.cancel();
            self.summarising = None;
        }
        let Some(dataset) = self.session.datasets().get(self.active) else {
            return;
        };
        let (Some(alignment), Some(view)) = (dataset.msa(), self.views.get(&dataset.id)) else {
            return;
        };
        // Drawn from at every level of the overview: a line's score, or a
        // cell of many columns, read straight from the file costs the
        // whole row on every frame.
        let too_many = summary_block(alignment.rows(), alignment.cols()).is_none();
        if view.zoom == 0 || !alignment.is_aligned() || too_many {
            return;
        }
        let (id, revision) = (dataset.id, alignment.revision());
        if self
            .summaries
            .get(&id)
            .is_some_and(|summary| summary.revision() == revision)
        {
            return;
        }
        let Some(conserved) = self
            .conserved
            .get(&id)
            .filter(|found| found.revision() == revision)
        else {
            return;
        };
        if alignment.rows().saturating_mul(alignment.cols()) <= self.inline_cells {
            if let Some(summary) = alignment.view().summary(conserved, &Progress::new()) {
                self.summaries.insert(id, summary);
            }
            return;
        }
        if let Some((summarising, of, progress)) = &self.summarising {
            if (*summarising, *of) == (id, revision) {
                return;
            }
            // A summary of a state that is no longer shown is wasted work.
            progress.cancel();
        }
        let progress = Arc::new(Progress::new());
        self.summarising = Some((id, revision, Arc::clone(&progress)));
        self.outbox.push(Request::Summarise {
            dataset: id,
            view: alignment.view().clone(),
            conserved: conserved.clone(),
            progress,
        });
    }

    /// Make sure the visible alignment has, or is getting, a count of its
    /// columns for the state it is in, if its colouring needs one.
    fn refresh_conservation(&mut self) {
        let Some(dataset) = self.session.datasets().get(self.active) else {
            return;
        };
        let (Some(alignment), Some(view)) = (dataset.msa(), self.views.get(&dataset.id)) else {
            return;
        };
        // The overview's colours and scores need the count, whatever the
        // grid's colouring.
        if (view.colouring != Colouring::Conserved && view.zoom == 0) || !alignment.is_aligned() {
            return;
        }
        let (id, revision) = (dataset.id, alignment.revision());
        if self
            .conserved
            .get(&id)
            .is_some_and(|found| found.revision() == revision)
        {
            return;
        }
        if alignment.rows().saturating_mul(alignment.cols()) <= self.inline_cells {
            if let Some(found) = alignment.view().conservation(&Progress::new()) {
                self.conserved.insert(id, found);
            }
            return;
        }
        if let Some((counting, of, progress)) = &self.counting {
            if (*counting, *of) == (id, revision) {
                return;
            }
            // A count of a state that is no longer shown is wasted work.
            progress.cancel();
        }
        let progress = Arc::new(Progress::new());
        self.counting = Some((id, revision, Arc::clone(&progress)));
        self.outbox.push(Request::Conserve {
            dataset: id,
            view: alignment.view().clone(),
            progress,
        });
    }

    /// The count to shade the visible alignment with, if there is one that
    /// fits it: of the state shown, or of an earlier state of the same
    /// width, which is close enough to show while the new count runs.
    pub fn conservation(&self) -> Option<&Conservation> {
        let dataset = self.session.datasets().get(self.active)?;
        let alignment = dataset.msa()?;
        self.conserved
            .get(&dataset.id)
            .filter(|found| found.cols() == alignment.cols())
    }

    /// The summary to draw the visible alignment's lines of several
    /// sequences from: of the state shown, or of an earlier state of the
    /// same shape, which is close enough to show while the new one is made.
    pub fn summary(&self) -> Option<&Summary> {
        let dataset = self.session.datasets().get(self.active)?;
        let alignment = dataset.msa()?;
        self.summaries.get(&dataset.id).filter(|summary| {
            summary.rows() == alignment.rows() && summary.cols() == alignment.cols()
        })
    }

    /// The zoom levels for the visible alignment on this screen. Only the
    /// grid when the alignment fits it whole, or is unaligned.
    pub fn levels(&self) -> Vec<Scale> {
        let (Some(aln), Some(grid), Some(overview)) = (
            self.shape(),
            ui::plain_shape(self),
            ui::overview_shape(self),
        ) else {
            return vec![Scale::ONE];
        };
        let aligned = self
            .alignment()
            .is_some_and(|(alignment, _)| alignment.is_aligned());
        let fits = aln.rows <= grid.rows && aln.cols <= grid.cols;
        if !aligned || fits {
            return vec![Scale::ONE];
        }
        zoom::levels(aln, overview, summary_block(aln.rows, aln.cols))
    }

    /// Keep the visible view's zoom level among the levels there are, and
    /// its scale the level's.
    fn rescale(&mut self) {
        let levels = self.levels();
        if let Some(view) = self.view_mut() {
            view.zoom = view.zoom.min(levels.len() - 1);
            let scale = levels[view.zoom];
            if view.scale != scale {
                view.scale = scale;
                view.snap();
            }
        }
    }

    /// `-`, `+` and `z`: go to another zoom level, centred on the cursor.
    fn zoom(&mut self, key: KeyCode) {
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let zooming_out = matches!(key, KeyCode::Char('-' | 'z'));
        if zooming_out && !alignment.is_aligned() {
            self.status = "error: the overview needs sequences of one length".to_string();
            return;
        }
        let last = self.levels().len() - 1;
        if zooming_out && last == 0 {
            self.status = "the whole alignment already fits on screen".to_string();
            return;
        }
        let zoom = match key {
            KeyCode::Char('-') => (view.zoom + 1).min(last),
            KeyCode::Char('z') if view.zoom == 0 => last,
            KeyCode::Char('z') => 0,
            _ => view.zoom.saturating_sub(1),
        };
        if zoom == view.zoom {
            return;
        }
        if let Some(view) = self.view_mut() {
            view.zoom = zoom;
        }
        self.rescale();
        let (Some(aln), screen) = (self.shape(), self.screen_shape()) else {
            return;
        };
        if let Some(view) = self.view_mut() {
            view.centre(screen, aln);
        }
    }

    /// Ask every job running in the background to stop.
    pub fn cancel_jobs(&self) {
        if let Some(busy) = &self.busy {
            busy.progress.cancel();
        }
        for (_, _, progress) in [&self.counting, &self.summarising].into_iter().flatten() {
            progress.cancel();
        }
    }

    /// Take in work a thread has finished.
    pub fn handle_done(&mut self, done: Done) {
        match done {
            Done::Command(finished) => {
                let Some(busy) = self.busy.take() else {
                    return;
                };
                let ended = self.session.finish(finished);
                // A queued open does not take the tab from a prompt, a
                // pending fill, a question or a typed run: those were aimed
                // at the file on screen and must land on it.
                let pending = !matches!(self.mode, Mode::Normal | Mode::Help);
                if ended.is_ok()
                    && busy.follow
                    && !(busy.queued && pending)
                    && let Some(current) = self.session.current()
                {
                    self.active = current;
                }
                self.report(ended.map(|outcome| Some(outcome.message)), busy.queued);
            }
            Done::Conservation { dataset, result } => {
                // A cancelled count has nothing to say; whatever cancelled
                // it has already asked for the count it wants.
                let Some(found) = result else {
                    return;
                };
                if self
                    .counting
                    .as_ref()
                    .is_some_and(|(id, of, _)| (*id, *of) == (dataset, found.revision()))
                {
                    self.counting = None;
                }
                // Kept only if the alignment is still in the state counted.
                let datasets = self.session.datasets().iter();
                let now = datasets
                    .filter(|open| open.id == dataset)
                    .find_map(Dataset::msa)
                    .map(Alignment::revision);
                if now == Some(found.revision()) {
                    self.conserved.insert(dataset, found);
                }
            }
            Done::Summary { dataset, result } => {
                let Some(summary) = result else {
                    return;
                };
                if self
                    .summarising
                    .as_ref()
                    .is_some_and(|(id, of, _)| (*id, *of) == (dataset, summary.revision()))
                {
                    self.summarising = None;
                }
                // Kept only if the alignment is still in the state summarised.
                let datasets = self.session.datasets().iter();
                let now = datasets
                    .filter(|open| open.id == dataset)
                    .find_map(Dataset::msa)
                    .map(Alignment::revision);
                if now == Some(summary.revision()) {
                    self.summaries.insert(dataset, summary);
                }
            }
        }
    }

    pub fn resize(&mut self, size: Size) {
        if size != self.screen {
            self.screen = size;
            self.reveal();
        }
    }

    /// The alignment on the visible tab and where the user is in it.
    pub fn alignment(&self) -> Option<(&Alignment, &MsaView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset.msa()?, self.views.get(&dataset.id)?))
    }

    /// The size of the alignment on the visible tab.
    fn shape(&self) -> Option<Shape> {
        let (alignment, _) = self.alignment()?;
        Some(Shape::of(alignment))
    }

    fn view_mut(&mut self) -> Option<&mut MsaView> {
        let dataset = self.session.datasets().get(self.active)?;
        self.views.get_mut(&dataset.id)
    }

    /// How many rows and columns of residues are on screen.
    fn screen_shape(&self) -> Shape {
        match ui::grid_of(self) {
            Some(layout) => ui::grid::screen_shape(&layout),
            None => Shape { rows: 0, cols: 0 },
        }
    }

    /// The tree on the visible tab and where the user is in it.
    pub fn tree(&self) -> Option<(&Dataset, &TreeDoc, &TreeView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset, dataset.tree()?, self.trees.get(&dataset.id)?))
    }

    fn tree_view_mut(&mut self) -> Option<(&TreeDoc, &mut TreeView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset.tree()?, self.trees.get_mut(&dataset.id)?))
    }

    /// The rows the tree view of dataset `id` has on screen.
    fn tree_height(&self, id: u64) -> Option<usize> {
        let doc = self
            .session
            .datasets()
            .iter()
            .find(|d| d.id == id)?
            .tree()?;
        let view = self.trees.get(&id)?;
        let screen = ratatui::layout::Rect::new(0, 0, self.screen.width, self.screen.height);
        let body = ui::regions(self, screen).body;
        Some(ui::tree::tree_layout(body, doc, view).rows.height as usize)
    }

    /// Keep one view per open tree, fitted to the screen and following
    /// every edit, undo and redo of its tree.
    fn sync_trees(&mut self) {
        let ids: Vec<u64> = self
            .session
            .datasets()
            .iter()
            .filter(|d| d.tree().is_some())
            .map(|d| d.id)
            .collect();
        self.trees.retain(|id, _| ids.contains(id));
        for id in ids {
            // The counts walk every tip: work them out again only when the
            // tree or its alignment has changed.
            let key = self.session.link_of(id).and_then(|aln| {
                let datasets = self.session.datasets();
                let tree = datasets.iter().find(|d| d.id == id)?.tree()?.revision();
                let msa = datasets.iter().find(|d| d.id == aln)?.msa()?.revision();
                Some((tree, aln, msa))
            });
            let link = match self.trees.get(&id) {
                Some(view) if view.link_key == key => view.link,
                _ => self.session.link_summary(id),
            };
            let fresh = !self.trees.contains_key(&id);
            let Some(doc) = self.session.datasets().iter().find(|d| d.id == id) else {
                continue;
            };
            let Some(doc) = doc.tree() else {
                continue;
            };
            if fresh {
                let mut view = TreeView::new(doc.tree());
                view.revision = doc.revision();
                self.trees.insert(id, view);
            }
            let height = self.tree_height(id).unwrap_or(1);
            let doc = self.session.datasets().iter().find(|d| d.id == id);
            let (Some(doc), Some(view)) = (doc.and_then(Dataset::tree), self.trees.get_mut(&id))
            else {
                continue;
            };
            view.link = link;
            view.link_key = key;
            let (tree, facts) = (doc.tree(), doc.facts());
            if fresh {
                // The cursor starts on the first row.
                view.refit(tree, facts, height);
                if let Some(first) = view.rows.first() {
                    view.cursor = first.node;
                }
                view.refit(tree, facts, height);
            } else if view.revision != doc.revision() {
                view.height = height;
                view.repair(tree, facts, doc.revision());
            } else if view.height != height {
                view.refit(tree, facts, height);
            }
        }
    }

    /// Scroll the visible alignment just enough to show its cursor.
    fn reveal(&mut self) {
        self.sync_trees();
        self.rescale();
        let screen = self.screen_shape();
        if let Some(view) = self.view_mut() {
            view.reveal(screen);
        }
    }

    /// Keep one view per open alignment, each inside its alignment: files
    /// are opened and closed, and edits can shrink what a view points into.
    fn sync_views(&mut self) {
        let mut open = Vec::new();
        for dataset in self.session.datasets() {
            if let Some(alignment) = dataset.msa() {
                let view = self.views.entry(dataset.id).or_insert_with(MsaView::new);
                view.clamp(Shape {
                    rows: alignment.rows(),
                    cols: alignment.cols(),
                });
                open.push(dataset.id);
            }
        }
        self.views.retain(|id, _| open.contains(id));
        self.sync_trees();
        self.conserved.retain(|id, _| open.contains(id));
        self.summaries.retain(|id, _| open.contains(id));
        for job in [&mut self.counting, &mut self.summarising] {
            if let Some((id, _, progress)) = job
                && !open.contains(id)
            {
                progress.cancel();
                *job = None;
            }
        }
    }

    /// Background work is running, so the screen should be redrawn soon to
    /// show progress.
    pub fn wants_tick(&self) -> bool {
        self.busy.is_some() || self.counting.is_some() || self.summarising.is_some()
    }

    /// Show how a command went. `None` means it is running in the
    /// background. A queued command never replaces an error on screen:
    /// several run in a row at start-up, and a failure among them must
    /// still be there to read when they are done.
    fn report(&mut self, ran: seqpeeq_core::Result<Option<String>>, queued: bool) {
        let text = match ran {
            Ok(Some(message)) => message,
            Ok(None) => match &self.busy {
                Some(busy) => format!("{}…", busy.label),
                None => return,
            },
            Err(Error::Cancelled) => "cancelled".to_string(),
            // Nothing more can be logged, so nothing more will be done;
            // say how to get the work out all the same.
            Err(error @ Error::Log { .. }) => {
                format!("error: {error}; :rescue FILE still writes the file")
            }
            Err(error) => format!("error: {error}"),
        };
        if !(queued && self.status.starts_with("error:")) {
            self.status = text;
        }
        self.sync_views();
    }

    pub fn handle_event(&mut self, event: &Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(text),
            Event::Resize(width, height) => self.resize(Size::new(*width, *height)),
            _ => {}
        }
        // Tabs, the log pane and the status line change the rows a tree has.
        self.sync_trees();
    }

    fn handle_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Normal if ctrl => {}
            Mode::Normal => match key.code {
                // Esc lets go of the selection if there is one; otherwise it
                // stops the work running in the background.
                KeyCode::Esc => {
                    if let Some((_, view)) = self.tree_view_mut()
                        && view.select.is_some()
                    {
                        view.select = None;
                    } else if let Some(view) = self.view_mut().filter(|view| view.select.is_some())
                    {
                        view.select = None;
                    } else if let Some(busy) = &self.busy {
                        busy.progress.cancel();
                        self.status = "cancelling…".to_string();
                    }
                }
                KeyCode::Char('q') => self.ask_quit(),
                KeyCode::Char(':') => self.open_prompt(PromptKind::Command, ""),
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('L') => self.show_log = !self.show_log,
                KeyCode::Tab => self.step_tab(1),
                KeyCode::BackTab => self.step_tab(-1),
                _ if self.tree().is_some() => self.tree_key(key),
                _ => self.grid_key(key),
            },
            Mode::Prompt(prompt) => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Backspace if prompt.line.text().is_empty() => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let Mode::Prompt(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
                    else {
                        unreachable!("the mode was just matched");
                    };
                    self.submit(prompt);
                }
                KeyCode::Up if matches!(prompt.kind, PromptKind::Command) => {
                    if let Some(older) = self.history.older(prompt.line.text()) {
                        prompt.line.set(older);
                    }
                }
                KeyCode::Down if matches!(prompt.kind, PromptKind::Command) => {
                    if let Some(newer) = self.history.newer() {
                        prompt.line.set(newer);
                    }
                }
                _ => {
                    prompt.line.key(key);
                }
            },
            Mode::Edit(_) if ctrl => {}
            Mode::Edit(Typing::Replace) => self.edit_key(key),
            Mode::Edit(Typing::Insert) => self.insert_key(key),
            Mode::FillChar => {
                self.mode = Mode::Normal;
                if let (KeyCode::Char(with), false) = (key.code, ctrl) {
                    self.fill(with);
                }
            }
            Mode::Confirm(confirm) => {
                let confirm = *confirm;
                self.mode = Mode::Normal;
                let yes = matches!(key.code, KeyCode::Char('y' | 'Y'));
                match confirm {
                    Confirm::Quit if yes => self.quit = true,
                    Confirm::Quit => self.status = "not quitting".to_string(),
                    Confirm::Close if yes => self.act(Command::Close),
                    Confirm::Close => self.status = "not closed".to_string(),
                }
            }
            Mode::Help => self.mode = Mode::Normal,
        }
    }

    /// The names of the open files that have edits not written to a
    /// file.
    pub fn unwritten(&self) -> Vec<String> {
        let titles = ui::tab_titles(self);
        let datasets = self.session.datasets().iter();
        datasets
            .zip(titles)
            .filter(|(dataset, _)| dataset.is_modified())
            .map(|(_, title)| title)
            .collect()
    }

    /// Quit, asking first if that would lose edits or stop a running job.
    fn ask_quit(&mut self) {
        if self.unwritten().is_empty() && self.busy.is_none() {
            self.quit = true;
        } else {
            self.mode = Mode::Confirm(Confirm::Quit);
        }
    }

    /// Run a command built from a gesture and show how it went.
    fn act(&mut self, cmd: Command) {
        let ran = self.run_command(cmd);
        self.report(ran, false);
        self.reveal();
    }

    /// Look for `pattern` in the sequence names and go to the first match
    /// at or after the cursor. A search that fails leaves the last one be.
    fn search(&mut self, pattern: &str) {
        if pattern.is_empty() {
            return;
        }
        if self.tree().is_some() {
            return self.tree_search(Some(pattern), true);
        }
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let search = match alignment.find_names(pattern) {
            Ok(matches) if matches.is_empty() => {
                self.status = format!("error: no sequence name matches '{pattern}'");
                return;
            }
            Ok(matches) => Search {
                pattern: pattern.to_string(),
                matches,
                revision: alignment.revision(),
                names: None,
            },
            Err(error) => {
                self.status = format!("error: {error}");
                return;
            }
        };
        let found = search.step(view.cursor.row, true, true);
        if let Some(view) = self.view_mut() {
            view.search = Some(search);
        }
        self.go_to_match(found);
    }

    /// `n` and `N`: go to the next or the previous match.
    fn search_step(&mut self, forward: bool) {
        if self.tree().is_some() {
            return self.tree_search(None, forward);
        }
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let Some(search) = &view.search else {
            self.status = "error: nothing to search for; press / first".to_string();
            return;
        };
        // Rows have moved if the alignment changed: look again.
        let fresh = if search.revision == alignment.revision() {
            None
        } else {
            Some(Search {
                pattern: search.pattern.clone(),
                matches: search.rows_in(alignment),
                revision: alignment.revision(),
                names: search.names.clone(),
            })
        };
        // Step from the match last gone to while the cursor is still on
        // its line, so that `n` moves on past it.
        let line = view.cursor.row..view.cursor.row + view.scale.rows;
        let row = match view.found {
            Some(at) if fresh.is_none() && line.contains(&at) => at,
            _ => view.cursor.row,
        };
        let Some(view) = self.view_mut() else {
            return;
        };
        if let Some(fresh) = fresh {
            view.search = Some(fresh);
        }
        let found = view
            .search
            .as_ref()
            .and_then(|search| search.step(row, forward, false));
        self.go_to_match(found);
    }

    /// Put the cursor on a match and say which one it is; or say there is
    /// none.
    fn go_to_match(&mut self, found: Option<(usize, usize)>) {
        let (Some((alignment, view)), Some(aln)) = (self.alignment(), self.shape()) else {
            return;
        };
        let Some(search) = &view.search else {
            return;
        };
        let Some((index, row)) = found else {
            self.status = format!("error: no sequence name matches '{}'", search.pattern);
            return;
        };
        self.status = format!(
            "match {} of {}: {}",
            index + 1,
            search.matches.len(),
            alignment.name(row)
        );
        if let Some(view) = self.view_mut() {
            view.move_to(Some(row), None, aln);
            view.found = Some(row);
        }
        self.reveal();
    }

    /// `:rescue FILE`: write the alignment out when the log has failed and
    /// no command can run any more.
    fn rescue(&mut self, file: &str) {
        if file.is_empty() {
            self.status = "error: rescue needs a file name".to_string();
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path = match (Path::new(file).strip_prefix("~"), home) {
            (Ok(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(file),
        };
        self.status = match self.session.rescue(self.active, &path) {
            Ok(outcome) => outcome.message,
            Err(error) => format!("error: {error}"),
        };
    }

    /// `i` or `R`: start inserting, or typing over residues.
    fn start_typing(&mut self, typing: Typing) {
        let Some((alignment, _)) = self.alignment() else {
            return;
        };
        // Refused here, rather than when the first run is entered, so that
        // nothing is typed only to be thrown away.
        let refusal = if !alignment.is_aligned() {
            Some("sequences differ in length, so columns cannot be selected".to_string())
        } else {
            self.busy
                .as_ref()
                .map(|busy| format!("still busy: {} (Esc cancels)", busy.label))
        };
        if let Some(refusal) = refusal {
            self.status = format!("error: {refusal}");
            return;
        }
        if let Some(view) = self.view_mut() {
            view.select = None;
            view.append = typing == Typing::Insert;
        }
        self.mode = Mode::Edit(typing);
    }

    /// Esc while typing: the run becomes a command, and the cursor goes
    /// back inside the alignment if it was past the end.
    fn stop_typing(&mut self) {
        self.commit_typed();
        self.mode = Mode::Normal;
        let aln = self.shape();
        if let Some(view) = self.view_mut() {
            view.append = false;
            if let Some(aln) = aln {
                view.clamp(aln);
            }
        }
        self.reveal();
    }

    /// A key while typing over residues.
    fn edit_key(&mut self, key: &KeyEvent) {
        let Some(aln) = self.shape() else {
            self.mode = Mode::Normal;
            return;
        };
        let Some(Cell { row, col }) = self.view_mut().map(|view| view.cursor) else {
            return;
        };
        // What to put where, and where the cursor goes then.
        let last = aln.cols.saturating_sub(1);
        let (at, byte, next) = match key.code {
            KeyCode::Esc => return self.stop_typing(),
            KeyCode::Char('>') => {
                self.status = "error: '>' cannot be typed into a sequence".to_string();
                return;
            }
            KeyCode::Char(c) if c.is_ascii_graphic() => (col, c as u8, (col + 1).min(last)),
            KeyCode::Delete => (col, b'-', (col + 1).min(last)),
            KeyCode::Backspace if col > 0 => (col - 1, b'-', col - 1),
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => {
                // Moving away ends the run; the same keys then move as usual.
                self.commit_typed();
                self.grid_key(key);
                return;
            }
            _ => return,
        };
        let grown = match self.view_mut().and_then(|view| view.typed.as_mut()) {
            Some(Pending::Replace(typed)) => typed.row == row && typed.put(at, byte),
            _ => false,
        };
        if !grown {
            // Not beside what was typed so far: that run is finished.
            self.commit_typed();
            if let Some(view) = self.view_mut() {
                view.typed = Some(Pending::Replace(Typed::new(row, at, byte)));
            }
        }
        if let Some(view) = self.view_mut() {
            view.cursor.col = next;
        }
        self.reveal();
    }

    /// A key while inserting: characters go in front of the cursor, and
    /// Delete and Backspace remove, each shifting the residues up to the
    /// next gap. A run of one kind of key is one command.
    fn insert_key(&mut self, key: &KeyEvent) {
        let Some(aln) = self.shape() else {
            self.mode = Mode::Normal;
            return;
        };
        let Some(view) = self.view_mut() else {
            return;
        };
        let Cell { row, col } = view.cursor;
        // The run as it stands, if it is on this row.
        let pending = view.typed.clone().filter(|pending| pending.row() == row);
        // The run this key makes, whether it carries on the one pending,
        // and where the cursor goes.
        let (next, carries_on, cursor) = match key.code {
            KeyCode::Esc => return self.stop_typing(),
            KeyCode::Char('>') => {
                self.status = "error: '>' cannot be typed into a sequence".to_string();
                return;
            }
            KeyCode::Char(c) if c.is_ascii_graphic() => match pending {
                Some(Pending::Insert {
                    col: start,
                    mut seq,
                    ..
                }) if start + seq.len() == col => {
                    seq.push(c as u8);
                    let next = Pending::Insert {
                        row,
                        col: start,
                        seq,
                        added: 0,
                    };
                    (next, true, col + 1)
                }
                _ => {
                    let seq = vec![c as u8];
                    let next = Pending::Insert {
                        row,
                        col,
                        seq,
                        added: 0,
                    };
                    (next, false, col + 1)
                }
            },
            KeyCode::Delete
                if col
                    >= aln.cols.saturating_add(match &pending {
                        Some(Pending::Insert { added, .. }) => *added,
                        _ => 0,
                    }) =>
            {
                self.status = "error: nothing under the cursor to remove".to_string();
                return;
            }
            KeyCode::Delete => match pending {
                Some(Pending::Remove {
                    col: start,
                    count,
                    back: false,
                    ..
                }) if start == col && start + count < aln.cols => {
                    let count = count + 1;
                    let next = Pending::Remove {
                        row,
                        col,
                        count,
                        back: false,
                    };
                    (next, true, col)
                }
                _ => {
                    let next = Pending::Remove {
                        row,
                        col,
                        count: 1,
                        back: false,
                    };
                    (next, false, col)
                }
            },
            // Backspace straight after letters not yet made a command
            // takes the last of them back.
            KeyCode::Backspace
                if matches!(&pending, Some(Pending::Insert { col: start, seq, .. })
                    if start + seq.len() == col) =>
            {
                let Some(Pending::Insert {
                    col: start,
                    mut seq,
                    ..
                }) = pending
                else {
                    return;
                };
                seq.pop();
                let next = (!seq.is_empty()).then_some(Pending::Insert {
                    row,
                    col: start,
                    seq,
                    added: 0,
                });
                self.set_typed(next, col - 1);
                return;
            }
            KeyCode::Backspace if col > 0 => {
                let carried = match pending {
                    Some(Pending::Remove {
                        col: start,
                        count,
                        back: true,
                        ..
                    }) if start == col => Some(count),
                    _ => None,
                };
                let next = Pending::Remove {
                    row,
                    col: col - 1,
                    count: carried.map_or(1, |count| count + 1),
                    back: true,
                };
                (next, carried.is_some(), col - 1)
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => {
                // Moving away ends the run; the same keys then move as usual.
                self.commit_typed();
                self.grid_key(key);
                return;
            }
            _ => return,
        };
        if !carries_on {
            self.commit_typed();
        }
        // Removing a gap that only gaps follow up to the next residue
        // changes nothing: logged, it would still take an undo step.
        if let Pending::Remove {
            row, col, count: 1, ..
        } = next
            && !carries_on
            && self.removes_nothing(row, col)
        {
            self.status = "error: nothing to remove: the row would stay the same".to_string();
            self.set_typed(None, cursor);
            return;
        }
        self.set_typed(Some(next), cursor);
    }

    /// Make `typed` the run pending, its added columns planned against the
    /// alignment as it now is, and put the cursor at `col`.
    fn set_typed(&mut self, typed: Option<Pending>, col: usize) {
        let typed = match (typed, self.alignment()) {
            (Some(Pending::Insert { row, col, seq, .. }), Some((alignment, _))) => {
                let added = alignment.view().plan_insert(row, col, &seq).added;
                Some(Pending::Insert {
                    row,
                    col,
                    seq,
                    added,
                })
            }
            (typed, _) => typed,
        };
        if let Some(view) = self.view_mut() {
            view.typed = typed;
            view.cursor.col = col;
        }
        self.reveal();
    }

    /// Whether removing the cell of `row` at `col` leaves the row as it is.
    fn removes_nothing(&self, row: usize, col: usize) -> bool {
        let Some((alignment, _)) = self.alignment() else {
            return true;
        };
        let view = alignment.view();
        let shift = view.plan_remove(row, col, 1);
        let mut now = Vec::with_capacity(shift.bytes.len());
        view.residues(row, col..col + shift.bytes.len(), &mut now);
        now == shift.bytes
    }

    /// Turn what has been typed into one command. If the command is
    /// refused, the typed characters are dropped and the reason is shown.
    fn commit_typed(&mut self) {
        if let Some(typed) = self.view_mut().and_then(|view| view.typed.take()) {
            self.act(Command::Msa(typed.command()));
        }
    }

    /// `d`: delete the selected sequences or columns; a block, or with no
    /// selection the cell under the cursor, is replaced with gaps.
    fn delete(&mut self) {
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let aln = Shape::of(alignment);
        let cmd = match view.region(aln) {
            Some(region @ Region { cols: None, .. }) => MsaCmd::DeleteRows {
                rows: region.flags().0,
            },
            Some(region @ Region { rows: None, .. }) => match region.flags().1 {
                Some(cols) => MsaCmd::DeleteCols { cols },
                None => return,
            },
            _ => {
                let (rows, cols) = view.target(aln).flags();
                MsaCmd::Fill {
                    rows,
                    cols,
                    with: '-',
                }
            }
        };
        self.act(Command::Msa(cmd));
    }

    /// `f` then a character: overwrite the selection, or the cell under the
    /// cursor, with it.
    fn fill(&mut self, with: char) {
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let (rows, cols) = view.target(Shape::of(alignment)).flags();
        self.act(Command::Msa(MsaCmd::Fill { rows, cols, with }));
    }

    /// `e`: open the command line holding the extract for the selection,
    /// ready for the file name.
    fn start_extract(&mut self) {
        let Some((alignment, view)) = self.alignment() else {
            return;
        };
        let mut line = "msa extract".to_string();
        if let Some(region) = view.region(Shape::of(alignment)) {
            let (rows, cols) = region.flags();
            if let Some(rows) = rows.rows {
                line.push_str(&format!(" --rows {rows}"));
            }
            if let Some(cols) = cols {
                line.push_str(&format!(" --cols {cols}"));
            }
        }
        line.push_str(" --out ");
        self.open_prompt(PromptKind::Command, &line);
    }

    /// Give the sequence in `row` the name that was typed, if it changed.
    fn rename(&mut self, row: usize, name: &str) {
        let Some((alignment, _)) = self.alignment() else {
            return;
        };
        let name = name.trim();
        if name.is_empty() || row >= alignment.rows() || name == alignment.name(row) {
            return;
        }
        let rows = Region {
            rows: Some(row..row + 1),
            cols: None,
        };
        self.act(Command::Msa(MsaCmd::Rename {
            rows: rows.flags().0,
            to: Some(name.to_string()),
            regex: None,
            replace: None,
        }));
    }

    /// A key that acts on the alignment on the visible tab.
    fn grid_key(&mut self, key: &KeyEvent) {
        let Some(aln) = self.shape() else {
            return;
        };
        let zoomed = self.alignment().is_some_and(|(_, view)| view.zoom > 0);
        match key.code {
            KeyCode::Char('i' | 'R' | 'r') if zoomed => {
                return self.status = "zoom in to edit".to_string();
            }
            KeyCode::Char('-' | '+' | '=' | 'z') | KeyCode::Enter => return self.zoom(key.code),
            KeyCode::Char('g') => return self.open_prompt(PromptKind::Goto, ""),
            KeyCode::Char('i') => return self.start_typing(Typing::Insert),
            KeyCode::Char('R') => return self.start_typing(Typing::Replace),
            KeyCode::Char('/') => return self.open_prompt(PromptKind::Search, ""),
            KeyCode::Char('n') => return self.search_step(true),
            KeyCode::Char('N') => return self.search_step(false),
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => return self.delete(),
            KeyCode::Char('f') => return self.mode = Mode::FillChar,
            KeyCode::Char('e') => return self.start_extract(),
            KeyCode::Char('u') => return self.act(Command::Undo),
            KeyCode::Char('U') => return self.act(Command::Redo),
            KeyCode::Char('r') => {
                let Some((alignment, view)) = self.alignment() else {
                    return;
                };
                let row = view.cursor.row;
                let name = alignment.name(row).to_string();
                return self.open_prompt(PromptKind::Rename { row }, &name);
            }
            _ => {}
        }
        let screen = self.screen_shape();
        let page = screen.rows.max(1) as isize;
        let width = screen.cols.max(1) as isize;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some(view) = self.view_mut() else {
            return;
        };
        match key.code {
            // Shift with an arrow scrolls a screen at a time, the cursor
            // going with it.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down if shift => {
                let (rows, cols) = match key.code {
                    KeyCode::Left => (0, -width),
                    KeyCode::Right => (0, width),
                    KeyCode::Up => (-page, 0),
                    _ => (page, 0),
                };
                view.scroll(rows, cols, aln, screen);
                view.move_by(rows, cols, aln);
            }
            KeyCode::Left | KeyCode::Char('h') => view.move_by(0, -1, aln),
            KeyCode::Right | KeyCode::Char('l') => view.move_by(0, 1, aln),
            KeyCode::Up | KeyCode::Char('k') => view.move_by(-1, 0, aln),
            KeyCode::Down | KeyCode::Char('j') => view.move_by(1, 0, aln),
            KeyCode::PageUp => view.move_by(-page, 0, aln),
            KeyCode::PageDown => view.move_by(page, 0, aln),
            KeyCode::Home => view.move_to(None, Some(0), aln),
            KeyCode::End => view.move_to(None, Some(usize::MAX), aln),
            KeyCode::Char(kind @ ('v' | 'V' | 'C')) => {
                view.select = Some(Select {
                    kind: match kind {
                        'v' => SelKind::Block,
                        'V' => SelKind::Rows,
                        _ => SelKind::Cols,
                    },
                    anchor: view.cursor,
                });
            }
            KeyCode::Char('c') => view.cycle_colouring(),
            KeyCode::Char('[') => view.widen_names(-1),
            KeyCode::Char(']') => view.widen_names(1),
            _ => return,
        }
        self.reveal();
    }

    /// Move the cursor to what was typed at the go-to prompt.
    fn go_to(&mut self, text: &str) {
        let Some(aln) = self.shape() else {
            return;
        };
        let target = parse_goto(text).and_then(|(row, col)| {
            if let Some(row) = row.filter(|&row| row >= aln.rows) {
                return Err(format!(
                    "row {} is past the last row ({})",
                    row + 1,
                    aln.rows
                ));
            }
            if let Some(col) = col.filter(|&col| col >= aln.cols) {
                return Err(format!(
                    "column {} is past the last column ({})",
                    col + 1,
                    aln.cols
                ));
            }
            Ok((row, col))
        });
        match target {
            Ok((row, col)) => {
                if let Some(view) = self.view_mut() {
                    view.move_to(row, col, aln);
                }
                self.reveal();
            }
            Err(message) => self.status = format!("error: {message}"),
        }
    }

    /// Scroll the alignment under the mouse wheel. The cursor stays put.
    fn wheel(&mut self, mouse: &MouseEvent) {
        const ROWS: isize = 3;
        const COLS: isize = 10;
        // Few terminals report sideways scrolling; many pass on the wheel
        // with Shift or Alt held instead. Some, Apple's Terminal among them,
        // pass on neither: there the wheel over the column numbers does.
        let over_ruler = ui::grid_of(self).is_some_and(|layout| mouse.row == layout.ruler.y);
        let sideways = over_ruler
            || mouse
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let (rows, cols) = match mouse.kind {
            MouseEventKind::ScrollUp if sideways => (0, -COLS),
            MouseEventKind::ScrollDown if sideways => (0, COLS),
            MouseEventKind::ScrollUp => (-ROWS, 0),
            MouseEventKind::ScrollDown => (ROWS, 0),
            MouseEventKind::ScrollLeft => (0, -COLS),
            MouseEventKind::ScrollRight => (0, COLS),
            _ => return,
        };
        let (Some(aln), screen) = (self.shape(), self.screen_shape()) else {
            return;
        };
        if let Some(view) = self.view_mut() {
            view.scroll(rows, cols, aln, screen);
        }
    }

    /// Pasted text goes into a line being typed and nowhere else: letters
    /// are commands outside a prompt, so a paste there could do anything.
    fn handle_paste(&mut self, text: &str) {
        let Mode::Prompt(prompt) = &mut self.mode else {
            self.status = "paste ignored: press : first to paste into the command line".to_string();
            return;
        };
        let (first, rest) = match text.find(['\n', '\r']) {
            Some(end) => (&text[..end], &text[end..]),
            None => (text, ""),
        };
        prompt.line.insert(first);
        if !rest.trim().is_empty() {
            self.status = "pasted text was cut at its first line break".to_string();
        }
    }

    /// Start typing a line, with `text` already on it.
    fn open_prompt(&mut self, kind: PromptKind, text: &str) {
        self.history.reset();
        self.mode = Mode::Prompt(Prompt {
            kind,
            line: LineEdit::new(text),
        });
    }

    /// Act on a line that was entered.
    fn submit(&mut self, prompt: Prompt) {
        match prompt.kind {
            PromptKind::Command => {
                self.history.push(prompt.line.text());
                self.run_line(prompt.line.text());
            }
            PromptKind::Goto => self.go_to(prompt.line.text()),
            PromptKind::Rename { row } => self.rename(row, prompt.line.text()),
            PromptKind::Search => self.search(prompt.line.text()),
            PromptKind::RenameTip { node } => self.rename_tip(node, prompt.line.text()),
            PromptKind::TreeOut { at } => self.tree_out(at, prompt.line.text()),
            PromptKind::TreeSeqs { at, out } => {
                let seqs = prompt.line.text().trim();
                let seqs = (!seqs.is_empty()).then(|| PathBuf::from(seqs));
                self.act(Command::Tree(TreeCmd::Extract {
                    at,
                    out: PathBuf::from(out),
                    seqs,
                    wrap: None,
                }));
            }
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        if matches!(self.mode, Mode::Edit(_)) {
            self.wheel(mouse);
            // A press ends the run being typed and puts the cursor there.
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                self.commit_typed();
                let (Some(layout), Some(aln)) = (ui::grid_of(self), self.shape()) else {
                    return;
                };
                if let Some(view) = self.view_mut()
                    && let Hit::Cell(cell) = grid::hit(&layout, view, aln, mouse.column, mouse.row)
                {
                    view.cursor = cell;
                }
            }
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let left_click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
        if left_click && mouse.row == 0 {
            let ranges = ui::tab_ranges(&ui::tab_titles(self), self.screen.width, self.active);
            if let Some(tab) = ranges.iter().position(|r| r.contains(&mouse.column)) {
                self.active = tab;
                return;
            }
        }
        if self.tree().is_some() {
            return self.tree_mouse(mouse);
        }
        self.wheel(mouse);
        self.grid_mouse(mouse);
    }

    /// A key on the tree tab.
    fn tree_key(&mut self, key: &KeyEvent) {
        let Some((doc, view)) = self.tree_view_mut() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let page = view.height.max(1) as isize;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => view.step(tree, facts, -1),
            KeyCode::Down | KeyCode::Char('j') => view.step(tree, facts, 1),
            KeyCode::PageUp => view.step(tree, facts, -page),
            KeyCode::PageDown => view.step(tree, facts, page),
            KeyCode::Left | KeyCode::Char('h') => view.go_parent(tree, facts),
            KeyCode::Right | KeyCode::Char('l') => view.go_child(tree, facts),
            KeyCode::Enter => view.toggle(tree, facts),
            KeyCode::Char('+' | '=') => view.zoom_in(tree, facts),
            KeyCode::Char('-') => view.zoom_out(tree, facts),
            KeyCode::Char('z') => view.zoom_whole(tree, facts),
            KeyCode::Char('b') => view.equal = !view.equal,
            KeyCode::Char('v') => view.select = Some(view.cursor),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search, ""),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Char('u') => self.act(Command::Undo),
            KeyCode::Char('U') => self.act(Command::Redo),
            KeyCode::Char('s') => self.act(Command::Tree(TreeCmd::Ladderize { reverse: false })),
            KeyCode::Char('d' | 'o' | 'r' | 'e') => self.tree_act(key.code),
            KeyCode::Char('a') => self.show_sequences(),
            _ => {}
        }
    }

    /// `d` `o` `r` `e`: act on the selected clade, or the cursor's.
    fn tree_act(&mut self, code: KeyCode) {
        let Some((_, doc, view)) = self.tree() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let n = view.select.unwrap_or(view.cursor);
        let at = tree.sel_for(n, facts);
        match code {
            KeyCode::Char('d') => self.act(Command::Tree(TreeCmd::Prune { at })),
            KeyCode::Char('o') => self.act(Command::Tree(TreeCmd::Reroot {
                at,
                outgroup: Vec::new(),
                midpoint: false,
            })),
            KeyCode::Char('r') if tree.is_tip(n) => {
                let name = tree.label(n).unwrap_or_default().to_string();
                self.open_prompt(PromptKind::RenameTip { node: n }, &name);
            }
            KeyCode::Char('r') => {
                self.status =
                    "error: r renames a tip; to rename many, :tree rename --regex … --replace …"
                        .to_string();
            }
            _ => {
                // The whole tree is written with no node named: a save.
                let at = if n == tree.root() {
                    NodeSel::default()
                } else {
                    at
                };
                self.open_prompt(PromptKind::TreeOut { at }, "");
            }
        }
    }

    /// `a`: show the linked alignment with the sequences of the selected
    /// clade (or the cursor's) marked as search matches.
    fn show_sequences(&mut self) {
        let Some((dataset, doc, view)) = self.tree() else {
            return;
        };
        let Some(linked) = self.session.link_of(dataset.id) else {
            self.status = "error: this tree is not linked; :tree link FILE".to_string();
            return;
        };
        let datasets = self.session.datasets();
        let Some(index) = datasets.iter().position(|d| d.id == linked) else {
            return;
        };
        let Some(alignment) = datasets[index].msa() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let n = view.select.unwrap_or(view.cursor);
        let name = |n| tree.label(n).unwrap_or_default();
        let tips = facts.tips(n) as usize;
        let names: std::collections::HashSet<String> = tree
            .preorder_from(n)
            .into_iter()
            .filter(|&n| tree.is_tip(n))
            .map(|n| name(n).to_string())
            .collect();
        let search = Search {
            pattern: format!(
                "tips of {} … {}",
                name(facts.first_tip(n)),
                name(facts.last_tip(n))
            ),
            matches: Vec::new(),
            revision: alignment.revision(),
            names: Some(Arc::new(names)),
        };
        let matches = search.rows_in(alignment);
        if matches.is_empty() {
            let file = datasets[index].path.file_name().unwrap_or_default();
            self.status = format!(
                "error: none of these {tips} tips has a sequence in {}",
                file.to_string_lossy()
            );
            return;
        }
        let found: std::collections::HashSet<&str> =
            matches.iter().map(|&row| alignment.name(row)).collect();
        let have = if tips == 1 { "tip has" } else { "tips have" };
        let message = format!(
            "{} of {tips} {have} a sequence; n N step through them",
            found.len()
        );
        let first = matches[0];
        self.active = index;
        if let Some(view) = self.views.get_mut(&linked) {
            view.search = Some(Search { matches, ..search });
        }
        self.go_to_match(Some((0, first)));
        self.status = message;
    }

    /// Give the tip `node` the name typed, if it changed.
    fn rename_tip(&mut self, node: NodeId, name: &str) {
        let Some((_, doc, _)) = self.tree() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let name = name.trim();
        if name.is_empty() || !facts.contains(node) || tree.label(node) == Some(name) {
            return;
        }
        let at = tree.sel_for(node, facts);
        self.act(Command::Tree(TreeCmd::Rename {
            at,
            to: Some(name.to_string()),
            regex: None,
            replace: None,
        }));
    }

    /// The tree file for `e` was typed: write it, or first ask for a
    /// sequence file when the tree is linked.
    fn tree_out(&mut self, at: NodeSel, out: &str) {
        let out = out.trim();
        if out.is_empty() {
            return;
        }
        let linked = self
            .tree()
            .is_some_and(|(dataset, _, _)| self.session.link_of(dataset.id).is_some());
        if linked {
            let out = out.to_string();
            return self.open_prompt(PromptKind::TreeSeqs { at, out }, "");
        }
        self.act(Command::Tree(TreeCmd::Extract {
            at,
            out: PathBuf::from(out),
            seqs: None,
            wrap: None,
        }));
    }

    /// Search the tree's tip names: a new `pattern` from the cursor on, or
    /// (with `None`) the next or previous match, searching again if the
    /// tree has changed. The cursor goes to the match, its path opened.
    fn tree_search(&mut self, pattern: Option<&str>, forward: bool) {
        let Some((doc, view)) = self.tree_view_mut() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let again = match (pattern, &view.search) {
            (Some(pattern), _) => Some(pattern.to_string()),
            (None, Some(search)) if search.revision != doc.revision() => {
                Some(search.pattern.clone())
            }
            (None, Some(_)) => None,
            (None, None) => {
                self.status = "error: nothing to search for; press / first".to_string();
                return;
            }
        };
        if let Some(pattern) = again {
            match TreeSearch::find(doc, &pattern) {
                Ok(search) if search.matches.is_empty() => {
                    self.status = format!("error: no tip name matches '{pattern}'");
                    return;
                }
                Ok(search) => view.search = Some(search),
                Err(error) => {
                    self.status = format!("error: {error}");
                    return;
                }
            }
        }
        let search = view.search.as_ref().expect("a search");
        let Some((index, tip)) = search.next(facts, view.cursor, forward, pattern.is_some()) else {
            return;
        };
        let total = search.matches.len();
        if common_ancestor(tree, facts, view.zoom, tip) != view.zoom {
            view.zoom = tree.root();
        }
        view.cursor = tip;
        view.found = Some(tip);
        view.refit(tree, facts, view.height);
        self.status = format!(
            "match {} of {total}: {}",
            index + 1,
            tree.label(tip).unwrap_or_default()
        );
    }

    /// The mouse on the tree tab: click selects, right-click or
    /// Shift-click extends, the wheel moves the cursor three rows.
    fn tree_mouse(&mut self, mouse: &MouseEvent) {
        const ROWS: isize = 3;
        let screen = ratatui::layout::Rect::new(0, 0, self.screen.width, self.screen.height);
        let body = ui::regions(self, screen).body;
        let Some((doc, view)) = self.tree_view_mut() else {
            return;
        };
        let (tree, facts) = (doc.tree(), doc.facts());
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let extend = match mouse.kind {
            MouseEventKind::ScrollUp => return view.step(tree, facts, -ROWS),
            MouseEventKind::ScrollDown => return view.step(tree, facts, ROWS),
            MouseEventKind::Down(MouseButton::Left) => shift,
            MouseEventKind::Down(MouseButton::Right) => true,
            _ => return,
        };
        let layout = ui::tree::tree_layout(body, doc, view);
        let Some(n) = ui::tree::hit(&layout, doc, view, mouse.column, mouse.row) else {
            return;
        };
        if extend {
            let from = view.select.unwrap_or(view.cursor);
            view.select = Some(common_ancestor(tree, facts, from, n));
        } else {
            view.select = Some(n);
            view.cursor = n;
            view.refit(tree, facts, view.height);
        }
    }

    /// The mouse on the alignment: press to place the cursor or to select a
    /// sequence or column, drag to select, right-click to extend.
    fn grid_mouse(&mut self, mouse: &MouseEvent) {
        let (Some(layout), Some(aln)) = (ui::grid_of(self), self.shape()) else {
            return;
        };
        let screen = grid::screen_shape(&layout);
        let was_dragging = self.dragging;
        let Some(view) = self.view_mut() else {
            return;
        };
        let at = grid::hit(&layout, view, aln, mouse.column, mouse.row);
        // Many terminals keep Shift-click for their own selection, so the
        // right button extends too.
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let mut dragging = was_dragging;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if !shift => {
                dragging = at != Hit::Outside;
                match at {
                    Hit::Cell(cell) => {
                        view.cursor = cell;
                        view.select = None;
                    }
                    Hit::Name(row) => {
                        view.cursor.row = row;
                        view.select = Some(Select {
                            kind: SelKind::Rows,
                            anchor: view.cursor,
                        });
                    }
                    Hit::Ruler(col) => {
                        view.cursor.col = col;
                        view.select = Some(Select {
                            kind: SelKind::Cols,
                            anchor: view.cursor,
                        });
                    }
                    Hit::Outside => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right) => {
                let (kind, to) = match at {
                    Hit::Cell(cell) => (SelKind::Block, cell),
                    Hit::Name(row) => (SelKind::Rows, Cell { row, ..view.cursor }),
                    Hit::Ruler(col) => (SelKind::Cols, Cell { col, ..view.cursor }),
                    Hit::Outside => return,
                };
                view.select.get_or_insert(Select {
                    kind,
                    anchor: view.cursor,
                });
                view.cursor = to;
            }
            MouseEventKind::Drag(MouseButton::Left) if was_dragging => {
                let kind = view.select.map_or(SelKind::Block, |select| select.kind);
                let cells = layout.cells;
                if cells.is_empty() {
                    return;
                }
                // Past an edge, the screen first moves one step that way.
                // There is nothing right of the cells to drag onto, so their
                // last column stands in for "further right".
                let (x, y) = (mouse.column, mouse.row);
                let down = match () {
                    _ if kind == SelKind::Cols => 0,
                    _ if y < cells.top() => -1,
                    _ if y >= cells.bottom() => 1,
                    _ => 0,
                };
                let right = match () {
                    _ if kind == SelKind::Rows => 0,
                    _ if x < cells.left() => -1,
                    _ if x + 1 >= cells.right() => 1,
                    _ => 0,
                };
                view.scroll(down, right, aln, screen);
                // Then the cursor goes to the cell nearest the pointer.
                let column = x.clamp(cells.left(), cells.right() - 1) - cells.x;
                let line = y.clamp(cells.top(), cells.bottom() - 1) - cells.y;
                let to = Cell {
                    row: (view.top + line as usize * view.scale.rows)
                        .min(aln.rows.saturating_sub(1)),
                    col: (view.left + column as usize * view.scale.cols)
                        .min(aln.cols.saturating_sub(1)),
                };
                view.select.get_or_insert(Select {
                    kind,
                    anchor: view.cursor,
                });
                match kind {
                    SelKind::Block => view.cursor = to,
                    SelKind::Rows => view.cursor.row = to.row,
                    SelKind::Cols => view.cursor.col = to.col,
                }
                view.snap();
            }
            MouseEventKind::Up(MouseButton::Left) => dragging = false,
            _ => {}
        }
        self.dragging = dragging;
    }

    /// Move `step` tabs right (negative: left), wrapping at the ends.
    fn step_tab(&mut self, step: isize) {
        let count = self.session.datasets().len() as isize;
        if count > 0 {
            self.active = (self.active as isize + step).rem_euclid(count) as usize;
            self.reveal();
        }
    }

    /// Run a command on behalf of the user looking at the visible tab. Every
    /// change to the data goes through here.
    ///
    /// Returns the outcome's message, or `None` if the command is slow and
    /// is now running in the background ([`App::busy`] says what it is).
    pub(crate) fn run_command(&mut self, cmd: Command) -> seqpeeq_core::Result<Option<String>> {
        if let Some(busy) = &self.busy {
            return Err(Error::Invalid(format!(
                "still busy: {} (Esc cancels)",
                busy.label
            )));
        }
        let chooses_file = matches!(cmd, Command::Open { .. } | Command::Use { .. });
        // Data commands apply to the session's current file. If that is not
        // the tab on screen, say so first, so the log shows which file each
        // command changed.
        if !chooses_file
            && !self.session.datasets().is_empty()
            && self.session.current() != Some(self.active)
        {
            self.session.execute(Command::Use {
                dataset: self.active + 1,
            })?;
        }
        let follow = chooses_file || cmd == Command::Close;
        // What these remove or bring back, a selection may have pointed at.
        let unselects = matches!(
            cmd,
            Command::Undo
                | Command::Redo
                | Command::Msa(MsaCmd::DeleteRows { .. } | MsaCmd::DeleteCols { .. })
        );
        match self.session.start(cmd)? {
            Started::Done(outcome) => {
                if unselects && let Some(view) = self.view_mut() {
                    view.select = None;
                }
                if follow {
                    self.active = self.session.current().unwrap_or(0);
                }
                Ok(Some(outcome.message))
            }
            Started::Running(job) => {
                self.busy = Some(Busy {
                    label: job.label().to_string(),
                    progress: job.progress(),
                    follow,
                    queued: false,
                });
                self.outbox.push(Request::Run(job));
                Ok(None)
            }
        }
    }

    /// Run one line typed on the command line.
    fn run_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        // Leaving is an interface action, not a change to the data: not logged.
        match line {
            "q" | "quit" => return self.ask_quit(),
            "q!" | "quit!" => return self.quit = true,
            "rescue" => return self.rescue(""),
            _ => {}
        }
        // Not a command either: commands are logged, and this is for when
        // the log cannot be written.
        if let Some(file) = line.strip_prefix("rescue ") {
            return self.rescue(file.trim());
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let parsed = Command::parse_line(line).map(|cmd| expand_home(cmd, home.as_deref()));
        // Closing a file throws its edits away, so ask first. The file
        // closed is the tab on screen, which run_command makes current.
        let datasets = self.session.datasets();
        let unwritten = datasets.get(self.active).is_some_and(|d| d.is_modified());
        if matches!(parsed, Ok(Command::Close)) && unwritten {
            self.mode = Mode::Confirm(Confirm::Close);
            return;
        }
        let ran = parsed.and_then(|cmd| self.run_command(cmd));
        self.report(ran, false);
        self.reveal();
    }
}

/// Put the home directory in place of a leading `~` in the path a typed
/// command names, so that the log holds a path that means the same thing
/// to whoever replays it.
fn expand_home(mut cmd: Command, home: Option<&Path>) -> Command {
    let path = match &mut cmd {
        Command::Open { path, .. } => path,
        Command::Msa(MsaCmd::Extract { out, .. }) => out,
        _ => return cmd,
    };
    if let (Some(home), Ok(rest)) = (home, path.strip_prefix("~")) {
        *path = if rest.as_os_str().is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tab_ranges;
    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use seqpeeq_core::Session;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn key(c: char) -> Event {
        press(KeyCode::Char(c))
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_event(&key(c));
        }
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn paste(text: &str) -> Event {
        Event::Paste(text.to_string())
    }

    /// The text of the prompt being typed into, if one is open.
    fn typed(app: &App) -> Option<&str> {
        match &app.mode {
            Mode::Prompt(prompt) => Some(prompt.line.text()),
            _ => None,
        }
    }

    /// Do, on this thread, the work the loop would hand to worker threads.
    fn settle(app: &mut App) {
        loop {
            app.pump();
            let Some(request) = app.outbox.pop() else {
                break;
            };
            match request {
                Request::Run(job) => app.handle_done(Done::Command(job.run())),
                Request::Conserve {
                    dataset,
                    view,
                    progress,
                } => {
                    let result = view.conservation(&progress);
                    app.handle_done(Done::Conservation { dataset, result });
                }
                Request::Summarise {
                    dataset,
                    view,
                    conserved,
                    progress,
                } => {
                    let result = view.summary(&conserved, &progress);
                    app.handle_done(Done::Summary { dataset, result });
                }
            }
        }
    }

    fn enter(app: &mut App) {
        app.handle_event(&press(KeyCode::Enter));
        settle(app);
    }

    fn click(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A fresh app plus a temp dir holding `a.fa` and `t.nwk`.
    fn setup() -> (App, TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (fa, nwk) = (dir.path().join("a.fa"), dir.path().join("t.nwk"));
        fs::write(&fa, ">a\nA").unwrap();
        fs::write(&nwk, "(a,b);").unwrap();
        (App::new(Session::new(None)), dir, fa, nwk)
    }

    /// An app with both files open; the second is active.
    fn with_two_tabs() -> (App, TempDir) {
        let (mut app, dir, fa, nwk) = setup();
        for path in [fa, nwk] {
            type_str(&mut app, &format!(":open {}", path.display()));
            enter(&mut app);
        }
        (app, dir)
    }

    #[test]
    fn colon_command_opens_file() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.session.datasets().len(), 1);
        assert!(app.status.starts_with("opened "), "{}", app.status);
    }

    #[test]
    fn bad_command_shows_one_line_error() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":bogus");
        enter(&mut app);
        assert!(
            app.status.starts_with("error: ") && !app.status.contains('\n'),
            "{}",
            app.status
        );
        assert!(app.session.history().is_empty());
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn quit_paths() {
        let (mut app, ..) = setup();
        app.handle_event(&key('q'));
        assert!(app.quit);

        for typed in [":q", ":quit"] {
            let (mut app, ..) = setup();
            type_str(&mut app, typed);
            assert!(!app.quit);
            enter(&mut app);
            assert!(app.quit, "{typed}");
            assert!(app.session.history().is_empty());
        }

        let (mut app, ..) = setup();
        type_str(&mut app, ":open");
        app.handle_event(&ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn q_inside_command_buffer_is_text() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":open q");
        assert!(!app.quit);
        assert_eq!(typed(&app), Some("open q"));
    }

    #[test]
    fn escape_and_backspace_leave_command_mode() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":ab");
        app.handle_event(&press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));

        type_str(&mut app, ":");
        app.handle_event(&press(KeyCode::Backspace));
        assert!(matches!(app.mode, Mode::Normal));

        type_str(&mut app, ":é");
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(typed(&app), Some(""));
    }

    #[test]
    fn tabs_cycle_and_wrap() {
        let (mut app, ..) = setup();
        app.handle_event(&press(KeyCode::Tab));
        app.handle_event(&press(KeyCode::BackTab));
        assert_eq!(app.active, 0);

        let (mut app, _dir) = with_two_tabs();
        app.active = 0;
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(app.active, 1);
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(app.active, 0);
        app.handle_event(&press(KeyCode::BackTab));
        assert_eq!(app.active, 1);
    }

    #[test]
    fn new_open_becomes_active() {
        let (app, _dir) = with_two_tabs();
        assert_eq!(app.active, 1);
    }

    #[test]
    fn click_selects_tab() {
        let titles = ["a.fa".to_string(), "t.nwk".to_string()];
        assert_eq!(tab_ranges(&titles, 80, 0), [0..6, 7..14]);

        let (mut app, _dir) = with_two_tabs();
        app.active = 0;
        app.handle_event(&click(9, 1));
        assert_eq!(app.active, 0, "a click below the tab bar selects nothing");
        app.handle_event(&click(6, 0));
        assert_eq!(app.active, 0, "a click on the separator selects nothing");
        app.handle_event(&click(9, 0));
        assert_eq!(app.active, 1);
        app.handle_event(&click(0, 0));
        assert_eq!(app.active, 0);
    }

    #[test]
    fn toggles_log_pane() {
        let (mut app, ..) = setup();
        app.handle_event(&key('L'));
        assert!(app.show_log);
        app.handle_event(&key('L'));
        assert!(!app.show_log);
    }

    #[test]
    fn help_closes_on_any_key() {
        let (mut app, ..) = setup();
        app.handle_event(&key('?'));
        assert!(matches!(app.mode, Mode::Help));
        app.handle_event(&key('x'));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn key_release_is_ignored() {
        let (mut app, ..) = setup();
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_event(&Event::Key(release));
        assert!(!app.quit);
    }

    const TRIO: &str = ">s1\nACGT-ACGTA\n>s2\nACGTTACG-A\n>s3 third one\nAC--TACGTA\n";

    /// An app with two ten-column alignments open; the second is visible.
    fn with_two_alignments() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        for name in ["a.fa", "b.fa"] {
            let path = dir.path().join(name);
            fs::write(&path, TRIO).unwrap();
            type_str(&mut app, &format!(":open {}", path.display()));
            enter(&mut app);
        }
        (app, dir)
    }

    fn cols(app: &App, dataset: usize) -> usize {
        app.session.datasets()[dataset].msa().unwrap().cols()
    }

    #[test]
    fn a_typed_command_applies_to_the_visible_tab() {
        let (mut app, _dir) = with_two_alignments();
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!((app.active, app.session.current()), (0, Some(1)));

        type_str(&mut app, ":msa delete-cols --cols 1");
        enter(&mut app);
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            ["use 1", "msa delete-cols --cols 1"]
        );
        assert_eq!((cols(&app, 0), cols(&app, 1)), (9, 10));

        // The file is now current, so no second `use` is needed.
        let before = app.session.history().len();
        type_str(&mut app, ":msa delete-cols --cols 1");
        enter(&mut app);
        assert_eq!(app.session.history().len(), before + 1);
        assert_eq!((cols(&app, 0), cols(&app, 1)), (8, 10));
    }

    #[test]
    fn typed_use_moves_the_visible_tab() {
        let (mut app, _dir) = with_two_alignments();
        assert_eq!(app.active, 1);
        type_str(&mut app, ":use 1");
        enter(&mut app);
        assert_eq!((app.active, app.status.as_str()), (0, "using 1: a.fa"));
        type_str(&mut app, ":use 9");
        enter(&mut app);
        assert_eq!(app.status, "error: there is no file 9 (2 are open)");
        assert_eq!(app.active, 0);
    }

    #[test]
    fn opening_a_file_does_not_log_a_use_first() {
        let (mut app, dir) = with_two_alignments();
        app.handle_event(&press(KeyCode::Tab));
        let path = dir.path().join("a.fa");
        type_str(&mut app, &format!(":open {}", path.display()));
        enter(&mut app);
        assert_eq!(app.session.history().len(), 3);
        assert_eq!(app.active, 2);
    }

    #[test]
    fn a_paste_outside_a_prompt_does_nothing() {
        let (mut app, _dir) = with_two_alignments();
        let before = app.session.history().len();
        app.handle_event(&paste("dd\nq\n:msa delete-rows --rows 1\n"));
        assert!(!app.quit && matches!(app.mode, Mode::Normal));
        assert_eq!(app.session.history().len(), before);
        assert_eq!(
            app.status,
            "paste ignored: press : first to paste into the command line"
        );
        app.mode = Mode::Help;
        app.handle_event(&paste("q"));
        assert!(!app.quit && matches!(app.mode, Mode::Help));
    }

    #[test]
    fn a_paste_into_the_command_line_stops_at_the_first_line_break() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":open ");
        app.handle_event(&paste("/data/x.fa\nq\n"));
        assert_eq!(typed(&app), Some("open /data/x.fa"));
        assert_eq!(app.status, "pasted text was cut at its first line break");
        assert!(app.session.history().is_empty(), "nothing ran");

        // A line break at the very end is not worth a warning.
        let (mut app, ..) = setup();
        type_str(&mut app, ":");
        app.handle_event(&paste("undo\r\n"));
        assert_eq!((typed(&app), app.status.as_str()), (Some("undo"), ""));
    }

    #[test]
    fn the_command_line_is_edited_in_place_and_remembers_lines() {
        let (mut app, ..) = setup();
        type_str(&mut app, ":opn x");
        for _ in 0..3 {
            app.handle_event(&press(KeyCode::Left));
        }
        type_str(&mut app, "e");
        assert_eq!(typed(&app), Some("open x"));
        enter(&mut app);
        assert!(app.status.starts_with("error: "), "{}", app.status);
        type_str(&mut app, ":");
        app.handle_event(&press(KeyCode::Up));
        assert_eq!(typed(&app), Some("open x"));
        app.handle_event(&press(KeyCode::Down));
        assert_eq!(typed(&app), Some(""));
        // Leaving part-way through the history does not carry over.
        app.handle_event(&press(KeyCode::Up));
        app.handle_event(&press(KeyCode::Esc));
        type_str(&mut app, ":");
        app.handle_event(&ctrl('a'));
        app.handle_event(&press(KeyCode::Up));
        assert_eq!(typed(&app), Some("open x"));
    }

    #[test]
    fn an_empty_command_line_is_not_an_error() {
        let (mut app, ..) = setup();
        app.status = "kept".to_string();
        type_str(&mut app, ":  ");
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status, "kept");
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        use std::path::Path;
        let home = Path::new("/home/u");
        let open = |path: &str| Command::Open {
            path: path.into(),
            kind: None,
        };
        assert_eq!(
            expand_home(open("~/x.fa"), Some(home)),
            open("/home/u/x.fa")
        );
        assert_eq!(expand_home(open("~"), Some(home)), open("/home/u"));
        assert_eq!(
            expand_home(open("~user/x.fa"), Some(home)),
            open("~user/x.fa")
        );
        assert_eq!(expand_home(open("a~/x.fa"), Some(home)), open("a~/x.fa"));
        assert_eq!(expand_home(open("~/x.fa"), None), open("~/x.fa"));
        let extract = Command::parse_line("msa extract --out ~/o.fa").unwrap();
        assert_eq!(
            expand_home(extract, Some(home)).to_line().unwrap(),
            "msa extract --out /home/u/o.fa"
        );
    }

    #[test]
    fn an_open_runs_in_the_background() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "opening a.fa…");
        assert!(app.wants_tick() && app.session.datasets().is_empty());
        assert_eq!(app.outbox.len(), 1);
        settle(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        assert!(!app.wants_tick() && app.busy.is_none());
        assert_eq!((app.session.datasets().len(), app.active), (1, 0));
    }

    #[test]
    fn commands_are_refused_while_a_job_runs() {
        let (mut app, dir) = with_two_alignments();
        type_str(
            &mut app,
            &format!(":open {}", dir.path().join("a.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        let before = app.session.history().len();
        type_str(&mut app, ":msa delete-cols --cols 1");
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "error: still busy: opening a.fa (Esc cancels)");
        assert_eq!(app.session.history().len(), before);
        assert_eq!(
            cols(&app, 1),
            10,
            "the refused command was not kept for later"
        );
        settle(&mut app);
        assert_eq!(cols(&app, 1), 10);
        assert_eq!(app.session.datasets().len(), 3);
        assert_eq!(app.active, 2, "the file that was opened is shown");
    }

    #[test]
    fn escape_cancels_the_running_job() {
        let (mut app, _dir, fa, _) = setup();
        type_str(&mut app, &format!(":open {}", fa.display()));
        app.handle_event(&press(KeyCode::Enter));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(app.status, "cancelling…");
        settle(&mut app);
        assert_eq!(app.status, "cancelled");
        assert!(app.session.datasets().is_empty() && app.busy.is_none());
    }

    #[test]
    fn queued_opens_run_one_after_another_and_an_error_stays_visible() {
        let (mut app, dir, fa, nwk) = setup();
        let bad = dir.path().join("bad.fa");
        fs::write(&bad, "this is not fasta").unwrap();
        for path in [&fa, &bad, &nwk] {
            app.enqueue(Command::Open {
                path: path.clone(),
                kind: None,
            });
        }
        settle(&mut app);
        assert_eq!(app.session.datasets().len(), 2);
        assert!(
            app.status.starts_with("error: ") && app.status.contains("bad.fa"),
            "{}",
            app.status
        );
        assert!(app.queue.is_empty());
        // What the user does next is reported as usual.
        type_str(&mut app, ":use 1");
        enter(&mut app);
        assert_eq!(app.status, "using 1: a.fa");
    }

    /// a.fa is open and shown; b.fa is queued and its open has started.
    fn with_b_loading() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(80, 12));
        let (a, b) = (dir.path().join("a.fa"), dir.path().join("b.fa"));
        fs::write(&a, TRIO).unwrap();
        fs::write(&b, TRIO).unwrap();
        open_file(&mut app, &a);
        app.enqueue(Command::Open {
            path: b,
            kind: None,
        });
        app.pump();
        assert!(app.busy.is_some());
        (app, dir)
    }

    #[test]
    fn a_queued_open_does_not_take_the_tab_from_a_pending_gesture() {
        // A rename prompt started on a.fa renames a.fa.
        let (mut app, _dir) = with_b_loading();
        type_str(&mut app, "jjr");
        settle(&mut app);
        assert_eq!(app.session.datasets().len(), 2);
        assert_eq!(app.active, 0, "the tab stays on a.fa");
        for _ in 0.."s3 third one".len() {
            app.handle_event(&press(KeyCode::Backspace));
        }
        type_str(&mut app, "renamed");
        enter(&mut app);
        assert_eq!(app.session.datasets()[0].msa().unwrap().name(2), "renamed");
        assert_eq!(
            app.session.datasets()[1].msa().unwrap().name(2),
            "s3 third one"
        );

        // So does a fill waiting for its character.
        let (mut app, _dir) = with_b_loading();
        type_str(&mut app, "f");
        settle(&mut app);
        type_str(&mut app, "N");
        settle(&mut app);
        assert_eq!(app.active, 0);
        assert!(app.session.datasets()[0].msa().unwrap().is_modified());
        assert!(!app.session.datasets()[1].msa().unwrap().is_modified());

        // With nothing pending, the newly opened file is shown, as before.
        let (mut app, _dir) = with_b_loading();
        settle(&mut app);
        assert_eq!(app.active, 1);
    }

    #[test]
    fn a_tab_chosen_during_an_extract_stays_chosen() {
        let (mut app, dir) = with_two_alignments();
        type_str(
            &mut app,
            &format!(":msa extract --out {}", dir.path().join("o.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(app.status, "writing o.fa…");
        app.handle_event(&press(KeyCode::Tab));
        settle(&mut app);
        assert!(app.status.starts_with("wrote "), "{}", app.status);
        assert_eq!(app.active, 0);
    }

    use crate::msa_view::{Cell, Colouring, MsaView};
    use crate::ui::grid::{GridLayout, cursor_position};
    use ratatui::layout::Size;

    /// An app showing the 5 x 60 fixture on a 70 x 9 screen.
    fn with_grid() -> App {
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(70, 9));
        app.enqueue(Command::Open {
            path: "tests/data/grid.fa".into(),
            kind: None,
        });
        settle(&mut app);
        app
    }

    /// The view of the alignment on the visible tab.
    fn view(app: &App) -> &MsaView {
        app.alignment().expect("an alignment is shown").1
    }

    fn view_of(app: &App) -> &MsaView {
        view(app)
    }

    fn active_layout(app: &App) -> GridLayout {
        ui::grid_of(app).expect("an alignment is shown")
    }

    fn wheel(kind: MouseEventKind, modifiers: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 30,
            row: 3,
            modifiers,
        })
    }

    /// 40 sequences x 300 columns: too wide and too tall for 80 x 12.
    fn big() -> String {
        (0..40)
            .map(|i| format!(">s{i}\n{}\n", "ACGT".repeat(75)))
            .collect()
    }

    #[test]
    fn the_wheel_over_the_column_numbers_scrolls_sideways() {
        // For terminals that never pass on sideways scrolling, Apple's
        // Terminal among them.
        let (mut app, _dir) = with_text(&big());
        let ruler = active_layout(&app).ruler;
        let over_ruler = (ruler.x + 5, ruler.y);
        app.handle_event(&mouse(MouseEventKind::ScrollDown, over_ruler, NONE));
        assert_eq!((view(&app).left, view(&app).top), (10, 0));
        app.handle_event(&mouse(MouseEventKind::ScrollUp, over_ruler, NONE));
        assert_eq!((view(&app).left, view(&app).top), (0, 0));
        // Over the sequences the wheel still scrolls rows.
        let over_cells = (ruler.x + 5, ruler.y + 3);
        app.handle_event(&mouse(MouseEventKind::ScrollDown, over_cells, NONE));
        assert_eq!((view(&app).left, view(&app).top), (0, 3));
    }

    #[test]
    fn shift_arrows_move_a_screen_at_a_time() {
        let (mut app, _dir) = with_text(&big());
        let screen = app.screen_shape();
        let shifted = |code| Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT));
        app.handle_event(&shifted(KeyCode::Right));
        assert_eq!(view(&app).cursor.col, screen.cols);
        assert_eq!(view(&app).left, screen.cols, "the next screen of columns");
        app.handle_event(&shifted(KeyCode::Down));
        assert_eq!(view(&app).cursor.row, screen.rows);
        app.handle_event(&shifted(KeyCode::Left));
        app.handle_event(&shifted(KeyCode::Up));
        assert_eq!(view(&app).cursor, Cell { row: 0, col: 0 });
        // Unshifted, the arrows still move one cell.
        app.handle_event(&press(KeyCode::Right));
        assert_eq!(view(&app).cursor.col, 1);
    }

    #[test]
    fn keys_move_the_cursor_and_the_screen_follows() {
        let mut app = with_grid();
        for c in "lllj".chars() {
            app.handle_event(&key(c));
        }
        assert_eq!(view(&app).cursor, Cell { row: 1, col: 3 });
        app.handle_event(&key('h'));
        app.handle_event(&key('k'));
        app.handle_event(&press(KeyCode::Right));
        app.handle_event(&press(KeyCode::Down));
        assert_eq!(view(&app).cursor, Cell { row: 1, col: 3 });
        app.handle_event(&press(KeyCode::End));
        assert_eq!(view(&app).cursor.col, 59);
        assert_eq!(
            view(&app).left,
            17,
            "43 columns fit; the last one is on screen"
        );
        app.handle_event(&press(KeyCode::Home));
        assert_eq!((view(&app).cursor.col, view(&app).left), (0, 0));
        app.handle_event(&press(KeyCode::PageDown));
        assert_eq!(view(&app).cursor.row, 4, "stops at the last row");
        app.handle_event(&press(KeyCode::PageUp));
        assert_eq!(view(&app).cursor.row, 0);
    }

    #[test]
    fn go_to_moves_or_explains() {
        let mut app = with_grid();
        type_str(&mut app, "g3,50");
        enter(&mut app);
        assert_eq!(view(&app).cursor, Cell { row: 2, col: 49 });
        assert_eq!(view(&app).left, 7);
        for (typed, status) in [
            ("g99,1", "error: row 99 is past the last row (5)"),
            ("g999", "error: column 999 is past the last column (60)"),
            (
                "gx",
                "error: go to: give a column (27), a row and column (12,27) or a row (12,)",
            ),
        ] {
            type_str(&mut app, typed);
            enter(&mut app);
            assert_eq!(app.status, status);
            assert_eq!(view(&app).cursor, Cell { row: 2, col: 49 });
        }
        assert_eq!(app.session.history().len(), 1, "moving is not logged");
    }

    #[test]
    fn the_wheel_scrolls_and_view_keys_change_only_the_view() {
        let mut app = with_grid();
        app.handle_event(&wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE));
        assert_eq!((view(&app).left, view(&app).cursor.col), (10, 0));
        app.handle_event(&wheel(MouseEventKind::ScrollUp, KeyModifiers::SHIFT));
        assert_eq!(view(&app).left, 0);
        app.handle_event(&wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE));
        assert_eq!(
            view(&app).top,
            0,
            "five rows fit, so there is nothing to scroll"
        );
        app.handle_event(&key(']'));
        app.handle_event(&key('c'));
        assert_eq!(
            (view(&app).name_width, view(&app).colouring),
            (28, Colouring::Residue)
        );
        assert_eq!(app.session.history().len(), 1);
    }

    #[test]
    fn each_file_keeps_its_own_place_and_closing_forgets_it() {
        let (mut app, _dir) = with_two_alignments();
        app.handle_event(&key('l'));
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(view(&app).cursor, Cell::default());
        app.handle_event(&press(KeyCode::Tab));
        assert_eq!(view(&app).cursor, Cell { row: 0, col: 1 });
        type_str(&mut app, ":close");
        enter(&mut app);
        assert_eq!(app.views.len(), 1);
        assert_eq!(app.active, 0);
    }

    #[test]
    fn shrinking_the_terminal_keeps_the_cursor_on_screen() {
        let mut app = with_grid();
        app.handle_event(&press(KeyCode::End));
        app.handle_event(&Event::Resize(40, 6));
        let l = active_layout(&app);
        assert!(cursor_position(&l, view(&app)).is_some());
    }

    #[test]
    fn grid_keys_are_harmless_without_an_alignment() {
        let (mut app, ..) = setup();
        for c in "hjklgc[]".chars() {
            app.handle_event(&key(c));
        }
        app.handle_event(&wheel(MouseEventKind::ScrollDown, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    }

    use crate::msa_view::Region;
    use ratatui::layout::Rect;

    fn open_file(app: &mut App, path: &std::path::Path) {
        app.enqueue(Command::Open {
            path: path.to_path_buf(),
            kind: None,
        });
        settle(app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
    }

    /// An app showing `text`, saved as `a.fa`, on an 80 x 12 screen.
    fn with_text(text: &str) -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.fa");
        fs::write(&path, text).unwrap();
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(80, 12));
        open_file(&mut app, &path);
        (app, dir)
    }

    fn with_trio() -> (App, TempDir) {
        with_text(TRIO)
    }

    /// The newest line of the log.
    fn last(app: &App) -> &str {
        app.session.history().last().unwrap()
    }

    /// One whole row of the visible alignment.
    fn seq(app: &App, row: usize) -> String {
        let (alignment, _) = app.alignment().unwrap();
        let mut out = Vec::new();
        alignment.residues(row, 0..alignment.cols(), &mut out);
        String::from_utf8(out).unwrap()
    }

    /// Where on screen a cell is.
    fn at(app: &App, row: usize, col: usize) -> (u16, u16) {
        let (cells, view) = (active_layout(app).cells, view(app));
        (
            cells.x + (col - view.left) as u16,
            cells.y + (row - view.top) as u16,
        )
    }

    fn name_at(app: &App, row: usize) -> (u16, u16) {
        (
            active_layout(app).names.x + 1,
            at(app, row, view(app).left).1,
        )
    }

    fn ruler_at(app: &App, col: usize) -> (u16, u16) {
        (at(app, view(app).top, col).0, active_layout(app).ruler.y)
    }

    fn mouse(kind: MouseEventKind, (column, row): (u16, u16), modifiers: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        })
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const LEFT: MouseButton = MouseButton::Left;

    #[test]
    fn keys_select_and_d_does_the_right_thing_for_each_kind() {
        // (keys typed, command logged, sequence 1 afterwards)
        for (keys, logged, first) in [
            ("jVd", "msa delete-rows --rows 2", "ACGT-ACGTA"),
            ("jVjd", "msa delete-rows --rows 2-3", "ACGT-ACGTA"),
            ("lllCld", "msa delete-cols --cols 4-5", "ACGACGTA"),
            (
                "lvljd",
                "msa fill --rows 1-2 --cols 2-3 --with=-",
                "A--T-ACGTA",
            ),
            ("d", "msa fill --rows 1 --cols 1 --with=-", "-CGT-ACGTA"),
            ("fN", "msa fill --rows 1 --cols 1 --with N", "NCGT-ACGTA"),
            ("VfN", "msa fill --rows 1 --with N", "NNNNNNNNNN"),
            ("llClfx", "msa fill --cols 3-4 --with x", "ACxx-ACGTA"),
        ] {
            let (mut app, _dir) = with_trio();
            type_str(&mut app, keys);
            assert_eq!(last(&app), logged, "{keys}");
            assert_eq!(seq(&app, 0), first, "{keys}");
            assert!(!app.status.starts_with("error"), "{keys}: {}", app.status);
        }
    }

    #[test]
    fn delete_and_backspace_are_d() {
        for code in [KeyCode::Delete, KeyCode::Backspace] {
            let (mut app, _dir) = with_trio();
            type_str(&mut app, "V");
            app.handle_event(&press(code));
            assert_eq!(last(&app), "msa delete-rows --rows 1");
        }
    }

    #[test]
    fn the_selection_goes_after_a_deletion_and_stays_after_a_fill() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjVd");
        assert_eq!(view(&app).select, None);
        assert_eq!(
            view(&app).cursor.row,
            1,
            "the cursor was on the row that went"
        );
        type_str(&mut app, "vlfN");
        assert!(view(&app).select.is_some());
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(view(&app).select, None);
    }

    #[test]
    fn undo_redo_and_their_refusals() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "u");
        assert_eq!(app.status, "error: nothing to undo");
        type_str(&mut app, "du");
        assert_eq!((last(&app), seq(&app, 0).as_str()), ("undo", "ACGT-ACGTA"));
        type_str(&mut app, "U");
        assert_eq!((last(&app), seq(&app, 0).as_str()), ("redo", "-CGT-ACGTA"));
    }

    #[test]
    fn fill_waits_for_a_character_and_escape_cancels() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "f");
        assert!(matches!(app.mode, Mode::FillChar));
        app.handle_event(&press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.session.history().len(), 1);
        type_str(&mut app, "f>");
        assert!(
            app.status.starts_with("error: the fill character"),
            "{}",
            app.status
        );
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn rename_edits_the_name_in_a_prompt() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjr");
        assert_eq!(typed(&app), Some("s3 third one"));
        app.handle_event(&ctrl('u'));
        type_str(&mut app, "new name");
        enter(&mut app);
        assert_eq!(last(&app), "msa rename --rows 3 --to 'new name'");
        let before = app.session.history().len();
        type_str(&mut app, "r");
        enter(&mut app);
        type_str(&mut app, "r");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(
            app.session.history().len(),
            before,
            "unchanged or cancelled: nothing logged"
        );
    }

    #[test]
    fn extract_opens_the_command_line_filled_in() {
        for (keys, line) in [
            ("e", "msa extract --out "),
            ("jVe", "msa extract --rows 2 --out "),
            ("lCle", "msa extract --cols 2-3 --out "),
            ("vjle", "msa extract --rows 1-2 --cols 1-2 --out "),
        ] {
            let (mut app, _dir) = with_trio();
            type_str(&mut app, keys);
            assert_eq!(typed(&app), Some(line), "{keys}");
        }
        let (mut app, dir) = with_trio();
        let out = dir.path().join("part.fa");
        type_str(&mut app, "vjle");
        type_str(&mut app, out.to_str().unwrap());
        enter(&mut app);
        assert_eq!(fs::read_to_string(&out).unwrap(), ">s1\nAC\n>s2\nAC\n");
    }

    #[test]
    fn the_mouse_selects_blocks_sequences_and_columns() {
        let (mut app, _dir) = with_trio();
        app.handle_event(&mouse(MouseEventKind::Down(LEFT), at(&app, 0, 2), NONE));
        assert_eq!(
            (view(&app).cursor, view(&app).select),
            (Cell { row: 0, col: 2 }, None)
        );
        app.handle_event(&mouse(MouseEventKind::Drag(LEFT), at(&app, 1, 4), NONE));
        app.handle_event(&mouse(MouseEventKind::Up(LEFT), at(&app, 1, 4), NONE));
        assert_eq!(
            view(&app).region(Shape::of(app.alignment().unwrap().0)),
            Some(Region {
                rows: Some(0..2),
                cols: Some(2..5)
            })
        );

        app.handle_event(&mouse(MouseEventKind::Down(LEFT), name_at(&app, 1), NONE));
        assert_eq!(
            view(&app).region(Shape::of(app.alignment().unwrap().0)),
            Some(Region {
                rows: Some(1..2),
                cols: None
            })
        );
        app.handle_event(&mouse(MouseEventKind::Drag(LEFT), name_at(&app, 2), NONE));
        assert_eq!(
            view(&app).region(Shape::of(app.alignment().unwrap().0)),
            Some(Region {
                rows: Some(1..3),
                cols: None
            })
        );
        app.handle_event(&mouse(MouseEventKind::Up(LEFT), name_at(&app, 2), NONE));

        app.handle_event(&mouse(MouseEventKind::Down(LEFT), ruler_at(&app, 3), NONE));
        app.handle_event(&mouse(MouseEventKind::Drag(LEFT), ruler_at(&app, 6), NONE));
        assert_eq!(
            view(&app).region(Shape::of(app.alignment().unwrap().0)),
            Some(Region {
                rows: None,
                cols: Some(3..7)
            })
        );
        type_str(&mut app, "d");
        assert_eq!(last(&app), "msa delete-cols --cols 4-7");
    }

    #[test]
    fn right_click_and_shift_click_extend() {
        for (button, modifiers) in [
            (MouseButton::Right, NONE),
            (MouseButton::Left, KeyModifiers::SHIFT),
        ] {
            let (mut app, _dir) = with_trio();
            app.handle_event(&mouse(MouseEventKind::Down(LEFT), at(&app, 0, 1), NONE));
            app.handle_event(&mouse(MouseEventKind::Up(LEFT), at(&app, 0, 1), NONE));
            app.handle_event(&mouse(
                MouseEventKind::Down(button),
                at(&app, 2, 3),
                modifiers,
            ));
            assert_eq!(
                view(&app).region(Shape::of(app.alignment().unwrap().0)),
                Some(Region {
                    rows: Some(0..3),
                    cols: Some(1..4)
                })
            );
        }
    }

    #[test]
    fn clicks_that_miss_the_alignment_do_nothing() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "lv");
        let before = (view(&app).cursor, view(&app).select);
        let regions = ui::regions(&app, Rect::new(0, 0, 80, 12));
        let below = (at(&app, 2, 0).0, at(&app, 2, 0).1 + 2);
        let right = (at(&app, 0, 9).0 + 5, at(&app, 0, 9).1);
        for position in [
            below,
            right,
            (3, regions.info.y),
            (3, regions.bottom.y),
            (79, 11),
        ] {
            for kind in [
                MouseEventKind::Down(LEFT),
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::Drag(LEFT),
                MouseEventKind::Up(LEFT),
            ] {
                app.handle_event(&mouse(kind, position, NONE));
            }
        }
        assert_eq!((view(&app).cursor, view(&app).select), before);
        // During a prompt the mouse is ignored altogether.
        let inside = at(&app, 2, 2);
        type_str(&mut app, ":");
        app.handle_event(&mouse(MouseEventKind::Down(LEFT), inside, NONE));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(view(&app).cursor, before.0);
        // And a tiny screen has no cells to hit.
        app.handle_event(&Event::Resize(5, 3));
        for kind in [MouseEventKind::Down(LEFT), MouseEventKind::Drag(LEFT)] {
            app.handle_event(&mouse(kind, (4, 2), NONE));
        }
    }

    #[test]
    fn a_drag_past_the_edge_is_clamped_and_scrolls() {
        let mut app = with_grid(); // 60 columns, 43 on screen
        app.handle_event(&mouse(MouseEventKind::Down(LEFT), at(&app, 0, 40), NONE));
        // Column 69 is the last one on a 70-column screen; row 7 is the
        // information row.
        app.handle_event(&mouse(MouseEventKind::Drag(LEFT), (69, 7), NONE));
        assert_eq!(view(&app).left, 1, "one step to the right");
        assert_eq!(
            view(&app).cursor,
            Cell { row: 4, col: 43 },
            "the cell now under the pointer, last sequence"
        );
        assert_eq!(
            view(&app).region(Shape::of(app.alignment().unwrap().0)),
            Some(Region {
                rows: Some(0..5),
                cols: Some(40..44)
            })
        );
    }

    #[test]
    fn actions_name_rows_and_columns_that_still_exist() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjV");
        type_str(&mut app, ":msa delete-rows --rows 2-3");
        enter(&mut app);
        assert_eq!((view(&app).cursor.row, view(&app).select), (0, None));
        type_str(&mut app, "d");
        assert_eq!(last(&app), "msa fill --rows 1 --cols 1 --with=-");
        type_str(&mut app, "uu");
        type_str(&mut app, "jjV:close");
        enter(&mut app); // both edits were undone, so there is no question
        assert!(app.session.datasets().is_empty() && app.alignment().is_none());
        type_str(&mut app, "dfNuUevVCr"); // no alignment: every key is harmless
        assert!(!app.quit && matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn gestures_apply_to_the_visible_tab_and_are_refused_while_busy() {
        let (mut app, dir) = with_two_alignments();
        app.handle_event(&press(KeyCode::Tab));
        type_str(&mut app, "d");
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            ["use 1", "msa fill --rows 1 --cols 1 --with=-"]
        );

        type_str(
            &mut app,
            &format!(":msa extract --out {}", dir.path().join("o.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        let before = app.session.history().len();
        type_str(&mut app, "d");
        type_str(&mut app, "u");
        assert_eq!(app.status, "error: still busy: writing o.fa (Esc cancels)");
        assert_eq!(app.session.history().len(), before);
        settle(&mut app);
        assert_eq!(
            seq(&app, 0),
            "-CGT-ACGTA",
            "the refused undo was not applied later"
        );
    }

    #[test]
    fn column_actions_on_an_unaligned_file_are_explained() {
        let (mut app, _dir) = with_text(">a\nAC\n>b\nACGT\n");
        type_str(&mut app, "d");
        assert_eq!(
            app.status,
            "error: sequences differ in length, so columns cannot be selected"
        );
        type_str(&mut app, "jVd");
        assert_eq!(last(&app), "msa delete-rows --rows 2");
    }

    #[test]
    fn quitting_while_a_file_is_being_written_asks_first() {
        let (mut app, dir) = with_trio();
        let out = dir.path().join("o.fa");
        type_str(&mut app, &format!(":msa extract --out {}", out.display()));
        // Enter and `q` arrive in the same turn, before any thread starts.
        app.handle_event(&press(KeyCode::Enter));
        type_str(&mut app, "q");
        assert!(!app.quit, "the write would be lost");
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)));
        assert_eq!(
            crate::ui::bottom_text(&app),
            "Still writing o.fa; quitting stops it. Quit anyway? (y/n)"
        );
        // The write ends while the question is still open.
        settle(&mut app);
        assert!(out.exists());
        assert_eq!(crate::ui::bottom_text(&app), "Quit? (y/n)");
        type_str(&mut app, "n");
        // Once it has ended and nothing is unwritten, `q` just quits.
        type_str(&mut app, "q");
        assert!(app.quit);
    }

    #[test]
    fn quitting_or_closing_with_an_edited_tree_asks_first() {
        let (mut app, _dir) = with_two_tabs(); // the tree's tab is active
        settle(&mut app);
        type_str(&mut app, ":tree rename --tip a --to x");
        enter(&mut app);
        settle(&mut app);
        assert_eq!(app.status, "renamed 'a' to 'x'");
        type_str(&mut app, "q");
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)) && !app.quit);
        type_str(&mut app, "n");
        type_str(&mut app, ":close");
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Close)));
    }

    #[test]
    fn closing_asks_about_the_tab_on_screen_not_the_last_file_used() {
        let (mut app, _dir, fa, nwk) = setup();
        type_str(&mut app, &format!(":open {}", nwk.display()));
        enter(&mut app);
        settle(&mut app);
        type_str(&mut app, ":tree rename --tip a --to x");
        enter(&mut app);
        settle(&mut app);
        type_str(&mut app, &format!(":open {}", fa.display()));
        enter(&mut app);
        settle(&mut app);
        app.handle_event(&press(KeyCode::BackTab));
        assert_eq!(app.active, 0, "the edited tree is on screen");
        type_str(&mut app, ":close");
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Close)));
        assert_eq!(app.session.datasets().len(), 2);
    }

    #[test]
    fn quitting_and_closing_ask_when_edits_are_unwritten() {
        let (mut app, dir) = with_trio();
        type_str(&mut app, "q");
        assert!(app.quit, "nothing was changed, so no question");

        let (mut app, _dir) = with_trio();
        type_str(&mut app, "dq");
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)) && !app.quit);
        type_str(&mut app, "n");
        assert_eq!(app.status, "not quitting");
        type_str(&mut app, ":close");
        enter(&mut app);
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Close)));
        type_str(&mut app, "x");
        assert_eq!(
            (app.status.as_str(), app.session.datasets().len()),
            ("not closed", 1)
        );
        type_str(&mut app, ":close");
        enter(&mut app);
        type_str(&mut app, "y");
        assert!(app.session.datasets().is_empty());
        assert_eq!(last(&app), "close");

        // Writing it out is enough to quit without a question.
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "d");
        type_str(
            &mut app,
            &format!(
                ":msa extract --out {}",
                dir.path().join("saved.fa").display()
            ),
        );
        enter(&mut app);
        type_str(&mut app, "q");
        assert!(app.quit);

        let (mut app, _dir) = with_trio();
        type_str(&mut app, "d:q");
        enter(&mut app);
        assert!(!app.quit);
        type_str(&mut app, "y");
        assert!(app.quit);
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "d:q!");
        enter(&mut app);
        assert!(app.quit);
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "d");
        app.handle_event(&ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn typing_overwrites_and_is_logged_as_one_command() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jlllR");
        assert!(matches!(app.mode, Mode::Edit(Typing::Replace)));
        let before = app.session.history().len();
        type_str(&mut app, "NN");
        assert_eq!(app.session.history().len(), before, "not a command yet");
        assert_eq!(view(&app).cursor, Cell { row: 1, col: 5 });
        app.handle_event(&press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(last(&app), "msa overwrite --row 2 --col 4 --seq NN");
        assert_eq!(seq(&app, 1), "ACGNNACG-A");
        type_str(&mut app, "u");
        assert_eq!(
            seq(&app, 1),
            "ACGTTACG-A",
            "one step of undo takes back the whole run"
        );
    }

    #[test]
    fn backspace_and_delete_put_gaps() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "lllllR");
        app.handle_event(&press(KeyCode::Backspace));
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(view(&app).cursor.col, 3);
        type_str(&mut app, "A");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa overwrite --row 1 --col 4 --seq A-");

        let (mut app, _dir) = with_trio();
        type_str(&mut app, "R");
        app.handle_event(&press(KeyCode::Backspace));
        app.handle_event(&press(KeyCode::Delete));
        app.handle_event(&press(KeyCode::Delete));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa overwrite --row 1 --col 1 --seq=--");
    }

    #[test]
    fn moving_away_ends_the_run() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "RA");
        app.handle_event(&press(KeyCode::Down));
        type_str(&mut app, "C");
        app.handle_event(&press(KeyCode::Esc));
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            [
                "msa overwrite --row 1 --col 1 --seq A",
                "msa overwrite --row 2 --col 2 --seq C",
            ]
        );
        // A click ends it too, and moves the cursor.
        type_str(&mut app, "RG");
        let to = at(&app, 2, 7);
        app.handle_event(&mouse(MouseEventKind::Down(LEFT), to, NONE));
        assert_eq!(last(&app), "msa overwrite --row 2 --col 3 --seq G");
        assert_eq!(view(&app).cursor, Cell { row: 2, col: 7 });
        assert!(matches!(app.mode, Mode::Edit(Typing::Replace)));
    }

    #[test]
    fn letters_are_text_while_typing_and_nothing_typed_logs_nothing() {
        let (mut app, _dir) = with_trio();
        let before = app.session.history().len();
        type_str(&mut app, "R");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(app.session.history().len(), before);
        type_str(&mut app, "Rdquvf:");
        assert!(!app.quit && matches!(app.mode, Mode::Edit(Typing::Replace)));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa overwrite --row 1 --col 1 --seq dquvf:");
        type_str(&mut app, "R>");
        assert_eq!(app.status, "error: '>' cannot be typed into a sequence");
        app.handle_event(&paste("ACGT"));
        assert_eq!(view(&app).typed, None, "a paste never reaches the grid");
        app.handle_event(&ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn typing_at_the_last_column_stays_there() {
        let (mut app, _dir) = with_trio();
        app.handle_event(&press(KeyCode::End));
        type_str(&mut app, "RAC");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa overwrite --row 1 --col 10 --seq C");
    }

    #[test]
    fn typing_is_refused_where_it_cannot_work() {
        let (mut app, _dir) = with_text(">a\nAC\n>b\nACGT\n");
        type_str(&mut app, "R");
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status,
            "error: sequences differ in length, so columns cannot be selected"
        );

        let (mut app, dir) = with_trio();
        type_str(
            &mut app,
            &format!(":msa extract --out {}", dir.path().join("o.fa").display()),
        );
        app.handle_event(&press(KeyCode::Enter));
        type_str(&mut app, "R");
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status, "error: still busy: writing o.fa (Esc cancels)");
        settle(&mut app);
    }

    /// 4 sequences x 201 columns: does not fit 80 columns; the rows do.
    fn wide() -> String {
        let row = |unit: &str| unit.repeat(50) + "A";
        format!(
            ">a\n{}\n>b\n{}\n>c\n{}\n>d\n{}\n",
            row("ACGT"),
            row("ACGT"),
            row("AC--"),
            row("TTTT")
        )
    }

    /// 40 sequences x 20 columns: the columns fit; the rows do not.
    fn tall() -> String {
        (0..40)
            .map(|i| {
                let row = if i == 7 {
                    "TTTTTTTTTTTTTTTTTTTT"
                } else {
                    "ACGTACGTACGTACGTACGT"
                };
                format!(">n{i}\n{row}\n")
            })
            .collect()
    }

    #[test]
    fn a_shrunk_line_shows_its_lowest_score_once_summarised() {
        let (mut app, _dir) = with_text(&tall());
        type_str(&mut app, "z");
        settle(&mut app);
        let summary = app.summary().expect("summarised at once: it is small");
        assert_eq!(summary.row(7).score(), Some(25));
        assert!(app.outbox.is_empty() && app.summarising.is_none());
    }

    #[test]
    fn large_alignments_are_summarised_in_the_background_and_old_results_dropped() {
        let (mut app, _dir) = with_text(&tall());
        app.inline_cells = 0;
        type_str(&mut app, "z");
        settle(&mut app); // count, then summary, both through the outbox
        assert!(app.summary().is_some() && app.summarising.is_none() && !app.wants_tick());
        let first = app.summary().unwrap().revision();
        type_str(&mut app, "fN"); // a new state of the same shape
        assert!(app.summary().is_some(), "the old picture stays meanwhile");

        // The count of the new state, then its summary, which comes back
        // only after the alignment has moved on again.
        app.pump();
        let Some(Request::Conserve {
            dataset,
            view: counted,
            progress,
        }) = app.outbox.pop()
        else {
            panic!("a count was asked for")
        };
        let result = counted.conservation(&progress);
        app.handle_done(Done::Conservation { dataset, result });
        app.pump();
        let Some(Request::Summarise {
            dataset,
            view,
            conserved,
            progress,
        }) = app.outbox.pop()
        else {
            panic!("a summary was asked for")
        };
        assert!(app.wants_tick(), "a summary is running");
        let stale = view.summary(&conserved, &progress);
        type_str(&mut app, "fA");
        app.pump();
        assert!(progress.is_cancelled(), "the old summary is cancelled");
        assert!(app.summarising.is_none(), "the old job is cleared");
        let stale_revision = stale.as_ref().unwrap().revision();
        app.handle_done(Done::Summary {
            dataset,
            result: stale,
        });
        let kept = app.summary().unwrap().revision();
        assert_eq!(kept, first, "the stale summary is dropped");
        assert_ne!(kept, stale_revision);

        type_str(&mut app, "z:close");
        enter(&mut app);
        type_str(&mut app, "y");
        settle(&mut app);
        assert!(app.session.datasets().is_empty());
        assert!(app.summaries.is_empty() && app.summarising.is_none());
    }

    #[test]
    fn n_and_shift_n_visit_every_match_in_the_overview() {
        let (mut app, _dir) = with_text(&tall());
        type_str(&mut app, "z");
        settle(&mut app);
        assert!(view(&app).scale.rows > 1, "lines hold several sequences");
        type_str(&mut app, "/n1");
        enter(&mut app);
        let mut seen = vec![app.status.clone()];
        for _ in 0..3 {
            type_str(&mut app, "n");
            seen.push(app.status.clone());
        }
        assert_eq!(
            seen,
            [
                "match 1 of 11: n1",
                "match 2 of 11: n10",
                "match 3 of 11: n11",
                "match 4 of 11: n12"
            ]
        );
        type_str(&mut app, "N");
        assert_eq!(app.status, "match 3 of 11: n11");
    }

    #[test]
    fn the_overview_is_summarised_at_one_sequence_per_line_too() {
        // Drawing a line's score or a wide cell straight from the file
        // reads the whole row, every frame.
        let (mut app, _dir) = with_text(&wide());
        type_str(&mut app, "-");
        settle(&mut app);
        assert_eq!(view(&app).scale.rows, 1);
        let summary = app.summary().expect("summarised");
        assert_eq!(
            summary.row(3).score(),
            Some(25),
            "all T: every fourth column"
        );
    }

    #[test]
    fn a_zoom_key_that_changes_no_level_leaves_the_view_where_it_is() {
        let (mut app, _dir) = with_text(&wide());
        type_str(&mut app, &"l".repeat(120));
        let left = view(&app).left;
        assert!(left > 0);
        type_str(&mut app, "+");
        assert_eq!(view(&app).left, left, "+ at the grid");
        type_str(&mut app, "z");
        let (zoom, left) = (view(&app).zoom, view(&app).left);
        type_str(&mut app, "-");
        assert_eq!(
            (view(&app).zoom, view(&app).left),
            (zoom, left),
            "- at the last level"
        );
    }

    #[test]
    fn quitting_cancels_every_background_job() {
        let (mut app, _dir) = with_text(TRIO);
        let jobs: Vec<_> = (0..2).map(|_| Arc::new(Progress::new())).collect();
        app.counting = Some((1, 1, Arc::clone(&jobs[0])));
        app.summarising = Some((1, 1, Arc::clone(&jobs[1])));
        app.cancel_jobs();
        assert!(jobs.iter().all(|job| job.is_cancelled()));
    }

    #[test]
    fn a_delete_that_would_change_nothing_is_refused_and_logs_nothing() {
        let (mut app, _dir) = with_text(TRIO);
        type_str(&mut app, "fN");
        app.handle_event(&press(KeyCode::End));
        type_str(&mut app, "i");
        for _ in 0..5 {
            app.handle_event(&press(KeyCode::Delete));
        }
        assert!(app.status.contains("nothing to remove"), "{}", app.status);
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(shown(&app, 0), "NCGT-ACGT-");
        type_str(&mut app, "uu");
        assert_eq!(shown(&app, 0), "ACGT-ACGTA", "two undos reach the fill");
    }

    #[test]
    fn a_no_op_backspace_still_moves_left_without_logging() {
        let (mut app, _dir) = with_text(">a\nA--T\n");
        type_str(&mut app, "lli");
        let before = app.session.history().len();
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(view(&app).cursor.col, 1);
        assert_eq!(shown(&app, 0), "A--T");
        assert_eq!(app.session.history().len(), before);
    }

    #[test]
    fn minus_plus_and_z_step_through_the_levels() {
        let (mut app, _dir) = with_text(&wide());
        let levels = app.levels();
        assert_eq!(levels.last(), Some(&Scale { rows: 1, cols: 8 }));
        type_str(&mut app, "-");
        assert_eq!(
            (view(&app).zoom, view(&app).scale),
            (1, Scale { rows: 1, cols: 2 })
        );
        type_str(&mut app, "z");
        assert_eq!(
            view(&app).zoom,
            0,
            "from any overview level back to the grid"
        );
        type_str(&mut app, "z");
        assert_eq!(view(&app).zoom, levels.len() - 1);
        type_str(&mut app, "-");
        assert_eq!(view(&app).zoom, levels.len() - 1, "no further out");
        type_str(&mut app, "+");
        assert_eq!(view(&app).zoom, levels.len() - 2);
        app.handle_event(&press(KeyCode::Enter));
        assert_eq!(view(&app).zoom, levels.len() - 3);
        type_str(&mut app, "=");
        assert_eq!(view(&app).zoom, levels.len() - 4);
        type_str(&mut app, "+");
        assert_eq!((view(&app).zoom, view(&app).scale), (0, Scale::ONE));
    }

    #[test]
    fn a_file_that_fits_or_is_unaligned_has_no_overview() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "-");
        assert_eq!(
            (view(&app).zoom, app.status.as_str()),
            (0, "the whole alignment already fits on screen")
        );
        let (mut app, _dir) = with_text(">a\nAC\n>b\nACGT\n");
        type_str(&mut app, "z");
        assert_eq!(
            app.status,
            "error: the overview needs sequences of one length"
        );
    }

    #[test]
    fn editing_keys_are_refused_in_the_overview() {
        let (mut app, _dir) = with_text(&wide());
        type_str(&mut app, "-");
        for key in ["i", "R", "r"] {
            app.status.clear();
            type_str(&mut app, key);
            assert!(matches!(app.mode, Mode::Normal), "{key}");
            assert_eq!(app.status, "zoom in to edit", "{key}");
        }
    }

    #[test]
    fn a_selection_in_the_overview_names_real_columns() {
        let (mut app, _dir) = with_text(&wide());
        type_str(&mut app, "-Cld"); // two blocks of 2 columns
        assert_eq!(last(&app), "msa delete-cols --cols 1-4");
        app.handle_event(&press(KeyCode::End));
        type_str(&mut app, "Cd"); // the last block holds one column
        assert_eq!(last(&app), "msa delete-cols --cols 197");
    }

    #[test]
    fn the_level_follows_the_screen_and_the_alignment() {
        let (mut app, _dir) = with_text(&wide());
        type_str(&mut app, "z");
        app.resize(Size::new(400, 40));
        assert_eq!(
            (view(&app).zoom, view(&app).scale),
            (0, Scale::ONE),
            "it all fits now"
        );
        app.resize(Size::new(80, 12));
        type_str(&mut app, "z:msa delete-cols --cols 50-201");
        enter(&mut app);
        assert_eq!(view(&app).zoom, 0, "49 columns fit the grid");
        assert!(view(&app).cursor.col < 49);
        for (w, h) in [(5, 3), (1, 1), (0, 0)] {
            app.resize(Size::new(w, h));
            type_str(&mut app, "-z+");
        }
    }

    #[test]
    fn typing_starts_without_a_selection() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "VR");
        assert_eq!(view(&app).select, None);
    }

    /// A row as it is drawn: the alignment with the pending run's preview
    /// over it.
    fn shown(app: &App, row: usize) -> String {
        let mut text = seq(app, row).into_bytes();
        let (alignment, view) = app.alignment().unwrap();
        if let Some(pending) = &view.typed {
            let (at, start, bytes) = pending.preview(alignment.view());
            if at == row {
                // A run may reach past the last column, into ones it adds.
                text.resize(text.len().max(start + bytes.len()), b'-');
                for (offset, byte) in bytes.into_iter().enumerate() {
                    text[start + offset] = byte;
                }
            }
        }
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn insert_mode_shifts_and_logs_one_insert() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjli");
        assert!(matches!(app.mode, Mode::Edit(Typing::Insert)));
        let before = app.session.history().len();
        type_str(&mut app, "NN");
        assert_eq!(app.session.history().len(), before, "not a command yet");
        assert_eq!(view(&app).cursor, Cell { row: 2, col: 3 });
        assert_eq!(shown(&app, 2), "ANNCTACGTA");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa insert --row 3 --col 2 --seq NN");
        assert_eq!(seq(&app, 2), "ANNCTACGTA", "the preview was the result");
        type_str(&mut app, "u");
        assert_eq!(seq(&app, 2), "AC--TACGTA");
    }

    #[test]
    fn delete_and_backspace_runs_are_one_remove_each() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjlllli");
        app.handle_event(&press(KeyCode::Delete));
        app.handle_event(&press(KeyCode::Delete));
        assert_eq!(
            (view(&app).cursor.col, shown(&app, 2)),
            (4, "AC--CGTA--".to_string())
        );
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa remove --row 3 --col 5 --count 2");

        let (mut app, _dir) = with_trio();
        type_str(&mut app, "llllllli");
        app.handle_event(&press(KeyCode::Backspace));
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(view(&app).cursor.col, 5);
        app.handle_event(&press(KeyCode::Delete)); // another kind of key ends the run
        app.handle_event(&press(KeyCode::Esc));
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            [
                "msa remove --row 1 --col 6 --count 2",
                "msa remove --row 1 --col 6 --count 1"
            ]
        );
    }

    #[test]
    fn insert_mode_adds_at_the_end_of_a_row_as_one_insert() {
        let (mut app, _dir) = with_trio();
        app.handle_event(&press(KeyCode::End));
        type_str(&mut app, "i");
        app.handle_event(&press(KeyCode::Right));
        assert_eq!(view(&app).cursor.col, 10, "one past the last column");
        let before = app.session.history().len();
        type_str(&mut app, "TG");
        assert_eq!(app.session.history().len(), before, "not a command yet");
        assert_eq!(view(&app).cursor.col, 12, "past the columns the run adds");
        assert_eq!(shown(&app, 0), "ACGT-ACGTATG");
        app.handle_event(&press(KeyCode::Delete));
        assert_eq!(app.status, "error: nothing under the cursor to remove");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa insert --row 1 --col 11 --seq TG");
        assert_eq!(
            (seq(&app, 0), seq(&app, 1)),
            ("ACGT-ACGTATG".into(), "ACGTTACG-A--".into())
        );
        assert_eq!(view(&app).cursor.col, 11, "back inside the alignment");
        type_str(&mut app, "u");
        assert_eq!(seq(&app, 0), "ACGT-ACGTA", "one undo takes the whole run");
    }

    #[test]
    fn delete_can_remove_a_residue_after_pending_added_columns() {
        let (mut app, _dir) = with_text(">a\nACGT\n");
        app.handle_event(&press(KeyCode::End));
        type_str(&mut app, "iXX");
        assert_eq!(shown(&app, 0), "ACGXXT");
        app.handle_event(&press(KeyCode::Delete));
        assert_ne!(app.status, "error: nothing under the cursor to remove");
        assert_eq!(shown(&app, 0), "ACGXX-");
        app.handle_event(&press(KeyCode::Esc));
        let history = app.session.history();
        assert_eq!(
            history[history.len() - 2..],
            [
                "msa insert --row 1 --col 4 --seq XX",
                "msa remove --row 1 --col 6 --count 1",
            ]
        );
        assert_eq!(seq(&app, 0), "ACGXX-");
    }

    #[test]
    fn letters_typed_past_the_gaps_of_a_row_are_one_insert() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "i");
        let before = app.session.history().len();
        type_str(&mut app, "XYZ");
        assert_eq!(app.session.history().len(), before, "not a command yet");
        assert_eq!(shown(&app, 0), "XYZACGTACGTA");
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa insert --row 1 --col 1 --seq XYZ");
        assert_eq!(seq(&app, 1), "ACGTTACG-A--");
        type_str(&mut app, "u");
        assert_eq!(seq(&app, 0), "ACGT-ACGTA");
    }

    #[test]
    fn backspace_takes_back_letters_not_yet_made_a_command() {
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjliNX");
        let before = app.session.history().len();
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(app.session.history().len(), before, "no command");
        assert_eq!(
            (view(&app).cursor.col, shown(&app, 2)),
            (2, "ANC-TACGTA".to_string())
        );
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(last(&app), "msa insert --row 3 --col 2 --seq N");
        assert_eq!(seq(&app, 2), "ANC-TACGTA");
        type_str(&mut app, "u");
        assert_eq!(seq(&app, 2), "AC--TACGTA");

        // Taking back every letter leaves nothing to make a command of;
        // a Backspace after that removes as usual.
        let (mut app, _dir) = with_trio();
        type_str(&mut app, "jjliNX");
        let before = app.session.history().len();
        app.handle_event(&press(KeyCode::Backspace));
        app.handle_event(&press(KeyCode::Backspace));
        assert_eq!(view(&app).cursor.col, 1);
        assert!(view(&app).typed.is_none());
        app.handle_event(&press(KeyCode::Backspace));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(app.session.history().len(), before + 1);
        assert_eq!(last(&app), "msa remove --row 3 --col 1 --count 1");
    }

    use seqpeeq_core::{Conservation, Progress};

    /// Open a file holding `text` without asking for anything further: the
    /// open is run to its end, and the loop is not turned again.
    fn open_text(app: &mut App, dir: &TempDir, name: &str, text: &str) {
        let path = dir.path().join(name);
        fs::write(&path, text).unwrap();
        app.enqueue(Command::Open { path, kind: None });
        app.pump();
        let Some(Request::Run(job)) = app.outbox.pop() else {
            panic!("an alignment opens in the background")
        };
        app.handle_done(Done::Command(job.run()));
    }

    /// An app that counts every alignment in the background, however small,
    /// showing the 4 x 6 example.
    fn with_background_counting() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(80, 12));
        app.inline_cells = 0;
        open_text(
            &mut app,
            &dir,
            "c.fa",
            ">1\nACGT-A\n>2\nACGA-C\n>3\nACTT-G\n>4\nAGGTTT\n",
        );
        (app, dir)
    }

    #[test]
    fn small_alignments_are_counted_at_once() {
        let (mut app, _dir) = with_trio();
        assert_eq!(app.inline_cells, INLINE_CELLS);
        assert!(app.outbox.is_empty() && !app.wants_tick());
        assert_eq!(app.conservation().map(Conservation::cols), Some(10));
        type_str(&mut app, "Cd");
        app.pump();
        assert_eq!(app.conservation().map(Conservation::cols), Some(9));
        assert!(app.outbox.is_empty());
    }

    #[test]
    fn large_alignments_are_counted_in_the_background() {
        let (mut app, _dir) = with_background_counting();
        assert!(app.conservation().is_none());
        app.pump();
        assert!(matches!(app.outbox.as_slice(), [Request::Conserve { .. }]));
        assert!(app.wants_tick());
        app.pump();
        assert_eq!(
            app.outbox.len(),
            1,
            "one count per state, however often asked"
        );
        let (_, right) = ui::info_text(&app);
        assert!(right.starts_with("counting columns"), "{right}");
        settle(&mut app);
        assert_eq!(app.conservation().map(Conservation::cols), Some(6));
        assert!(app.counting.is_none() && !app.wants_tick());
    }

    #[test]
    fn an_old_count_shades_only_while_the_width_is_unchanged() {
        let (mut app, _dir) = with_background_counting();
        settle(&mut app);
        type_str(&mut app, "fN"); // same width
        assert!(
            app.conservation().is_some(),
            "the old shading stays while the new count runs"
        );
        settle(&mut app);
        let id = app.session.datasets()[0].id;
        assert_eq!(
            app.conserved[&id].revision(),
            app.alignment().unwrap().0.revision()
        );
        type_str(&mut app, "Cd"); // one column fewer
        assert!(
            app.conservation().is_none(),
            "the old count no longer lines up"
        );
        settle(&mut app);
        assert_eq!(app.conservation().map(Conservation::cols), Some(5));
    }

    #[test]
    fn a_result_for_a_state_that_has_gone_is_dropped() {
        let (mut app, _dir) = with_background_counting();
        app.pump();
        let Some(Request::Conserve {
            dataset,
            view: counted,
            progress,
        }) = app.outbox.pop()
        else {
            panic!("a count was asked for")
        };
        type_str(&mut app, "Cd"); // the alignment moves on
        let result = counted.conservation(&Progress::new());
        app.handle_done(Done::Conservation { dataset, result });
        assert!(app.conserved.is_empty());
        assert!(progress.is_cancelled() || app.counting.is_none());
        type_str(&mut app, ":close");
        enter(&mut app);
        type_str(&mut app, "y");
        settle(&mut app);
        assert!(app.session.datasets().is_empty());
        assert!(
            app.conserved.is_empty() && app.counting.is_none(),
            "nothing is kept for a closed file"
        );
    }

    #[test]
    fn a_new_state_cancels_the_count_of_the_old_one() {
        let (mut app, _dir) = with_background_counting();
        app.pump();
        let Some(Request::Conserve {
            progress: first, ..
        }) = app.outbox.pop()
        else {
            panic!("a count was asked for")
        };
        type_str(&mut app, "fN");
        app.pump();
        assert!(first.is_cancelled());
        assert_eq!(app.outbox.len(), 1);
        // The cancelled count comes back empty and disturbs nothing.
        let dataset = app.session.datasets()[0].id;
        app.handle_done(Done::Conservation {
            dataset,
            result: None,
        });
        assert!(app.counting.is_some());
    }

    #[test]
    fn other_colourings_and_other_tabs_need_no_count() {
        let (mut app, _dir) = with_background_counting();
        app.handle_event(&key('c'));
        app.pump();
        assert!(app.outbox.is_empty());
    }

    #[test]
    fn search_finds_names_and_steps_through_them() {
        let mut app = with_grid();
        type_str(&mut app, "/_a1$");
        enter(&mut app);
        assert_eq!(app.status, "match 1 of 4: Homo_sapiens_A1");
        type_str(&mut app, "nn");
        assert_eq!(
            (view(&app).cursor.row, app.status.as_str()),
            (3, "match 3 of 4: Pongo_abelii_A1")
        );
        type_str(&mut app, "nn");
        assert_eq!(view(&app).cursor.row, 0, "wraps past the last match");
        type_str(&mut app, "N");
        assert_eq!(view(&app).cursor.row, 4);
        assert_eq!(app.session.history().len(), 1, "searching is not logged");
    }

    #[test]
    fn search_refusals_keep_the_earlier_search() {
        let mut app = with_grid();
        type_str(&mut app, "n");
        assert_eq!(app.status, "error: nothing to search for; press / first");
        type_str(&mut app, "/gorilla");
        enter(&mut app);
        assert_eq!(view(&app).cursor.row, 2);
        for (typed, status) in [
            ("/(", "error: bad regular expression '('"),
            ("/zebra", "error: no sequence name matches 'zebra'"),
        ] {
            type_str(&mut app, typed);
            enter(&mut app);
            assert_eq!(app.status, status);
            assert_eq!(view(&app).search.as_ref().unwrap().pattern, "gorilla");
        }
        // An empty search line changes nothing.
        type_str(&mut app, "/");
        enter(&mut app);
        assert_eq!(view(&app).search.as_ref().unwrap().pattern, "gorilla");
    }

    #[test]
    fn matches_are_found_again_after_the_rows_change() {
        let mut app = with_grid();
        type_str(&mut app, "/pan|macaca");
        enter(&mut app);
        assert_eq!(view(&app).cursor.row, 1);
        type_str(&mut app, ":msa delete-rows --rows 1-2");
        enter(&mut app);
        type_str(&mut app, "n");
        assert_eq!(app.status, "match 1 of 1: Macaca_mulatta_A1");
        assert_eq!(view(&app).cursor.row, 2);
        type_str(&mut app, ":msa delete-rows --rows 3");
        enter(&mut app);
        type_str(&mut app, "n");
        assert_eq!(app.status, "error: no sequence name matches 'pan|macaca'");
        assert!(view(&app).cursor.row < 2);
    }

    /// An app showing TRIO whose log has room for the `open` and no more.
    fn with_trio_and_a_full_log() -> (App, TempDir) {
        use seqpeeq_core::SessionLog;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.fa");
        fs::write(&path, TRIO).unwrap();
        let open = Command::Open {
            path: path.clone(),
            kind: None,
        };
        // How much of a log one `open` of this file takes.
        let probe = dir.path().join("probe.log");
        let mut session = Session::new(Some(SessionLog::new(probe.clone())));
        session.execute(open).unwrap();
        drop(session);
        let used = fs::metadata(&probe).unwrap().len() as usize;

        let log = SessionLog::with_limit(dir.path().join("s.log"), used + 10);
        let mut app = App::new(Session::new(Some(log)));
        app.resize(Size::new(80, 12));
        open_file(&mut app, &path);
        (app, dir)
    }

    #[test]
    fn rescue_writes_the_file_on_screen() {
        use seqpeeq_core::SessionLog;
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.fa"), dir.path().join("b.fa"));
        fs::write(&a, TRIO).unwrap();
        fs::write(&b, TRIO).unwrap();
        // Edit a.fa, then open b.fa, which becomes current.
        let script = |log: SessionLog| {
            let mut app = App::new(Session::new(Some(log)));
            app.resize(Size::new(80, 12));
            open_file(&mut app, &a);
            type_str(&mut app, "fT");
            open_file(&mut app, &b);
            app
        };
        let probe = dir.path().join("probe.log");
        drop(script(SessionLog::new(probe.clone())));
        let used = fs::metadata(&probe).unwrap().len() as usize;
        let mut app = script(SessionLog::with_limit(dir.path().join("s.log"), used + 10));

        // Back to a.fa; the `use 1` that `d` needs cannot be logged.
        app.handle_event(&press(KeyCode::Tab));
        type_str(&mut app, "d");
        assert!(
            app.status.contains("cannot write session log"),
            "{}",
            app.status
        );
        assert_eq!((app.active, app.session.current()), (0, Some(1)));

        let out = dir.path().join("saved.fa");
        type_str(&mut app, &format!(":rescue {}", out.display()));
        enter(&mut app);
        assert!(app.status.contains("from a.fa"), "{}", app.status);
        assert!(
            fs::read_to_string(&out)
                .unwrap()
                .starts_with(">s1\nTCGT-ACGTA\n"),
            "the file on screen, with its edit"
        );
    }

    #[test]
    fn rescue_is_offered_and_works_when_the_log_fails() {
        let (mut app, dir) = with_trio_and_a_full_log();
        type_str(&mut app, "d");
        assert!(
            app.status.starts_with("error: cannot write session log"),
            "{}",
            app.status
        );
        assert!(
            app.status.ends_with("; :rescue FILE still writes the file"),
            "{}",
            app.status
        );
        assert_eq!(seq(&app, 0), "ACGT-ACGTA", "the edit did not stand");
        let out = dir.path().join("saved.fa");
        type_str(&mut app, &format!(":rescue {}", out.display()));
        enter(&mut app);
        assert!(app.status.contains("not logged"), "{}", app.status);
        assert_eq!(fs::read_to_string(&out).unwrap(), TRIO);

        let (mut app, dir) = with_trio();
        type_str(
            &mut app,
            &format!(":rescue {}", dir.path().join("x.fa").display()),
        );
        enter(&mut app);
        assert_eq!(
            app.status,
            "error: the session log has not failed; use msa extract --out PATH"
        );
        type_str(&mut app, ":rescue");
        enter(&mut app);
        assert_eq!(app.status, "error: rescue needs a file name");
    }

    #[test]
    fn a_click_selects_the_tab_drawn_there_when_the_bar_is_scrolled() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Session::new(None));
        for n in 1..=6 {
            let path = dir.path().join(format!("file{n}.fa"));
            fs::write(&path, TRIO).unwrap();
            open_file(&mut app, &path);
        }
        app.resize(Size::new(25, 10));
        app.active = 4;
        // Tabs 4, 5 and part of 6 are drawn; tab 4 starts at the left edge.
        app.handle_event(&click(12, 0));
        assert_eq!(app.active, 4);
        app.handle_event(&click(0, 0));
        assert_eq!(app.active, 3);
    }

    // The tree tab.

    /// An app showing `t.nwk` holding `text`, on an 80 x 24 screen.
    fn with_tree(text: &str) -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let nwk = dir.path().join("t.nwk");
        fs::write(&nwk, text).unwrap();
        let mut app = App::new(Session::new(None));
        type_str(&mut app, &format!(":open {}", nwk.display()));
        enter(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        (app, dir)
    }

    const T6: &str = "((A,B),(C,(D,E)),F);";

    fn tree_view(app: &App) -> &TreeView {
        app.tree().expect("a tree is shown").2
    }

    fn shown_tree(app: &App) -> &seqpeeq_core::Tree {
        app.tree().expect("a tree is shown").1.tree()
    }

    fn tip_of(app: &App, name: &str) -> NodeId {
        crate::tree_view::tests::tip(shown_tree(app), name)
    }

    fn parent_of(app: &App, name: &str) -> NodeId {
        shown_tree(app).parent(tip_of(app, name)).unwrap()
    }

    fn root_of(app: &App) -> NodeId {
        shown_tree(app).root()
    }

    /// The text of the tree's rows on screen.
    fn rows_of(app: &App) -> Vec<String> {
        let (_, doc, view) = app.tree().unwrap();
        view.rows
            .iter()
            .map(|row| ui::tree::row_text(doc, view, row))
            .collect()
    }

    fn tree_layout_of(app: &App) -> ui::tree::TreeLayout {
        let (_, doc, view) = app.tree().unwrap();
        let screen = ratatui::layout::Rect::new(0, 0, app.screen.width, app.screen.height);
        ui::tree::tree_layout(ui::regions(app, screen).body, doc, view)
    }

    /// Click the tip `name` with `button`.
    fn click_node(app: &mut App, name: &str, modifiers: KeyModifiers, button: MouseButton) {
        let n = tip_of(app, name);
        let layout = tree_layout_of(app);
        let (_, doc, view) = app.tree().unwrap();
        let at = ui::tree::node_position(&layout, doc, view, n).expect("on screen");
        app.handle_event(&mouse(MouseEventKind::Down(button), at, modifiers));
    }

    /// The screen's rows as text, trailing spaces removed.
    fn render_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let row: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                row.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn a_tree_tab_moves_opens_and_zooms_with_keys() {
        let (mut app, _dir) = with_tree(T6);
        let (c, cde) = (tip_of(&app, "C"), parent_of(&app, "C"));
        assert_eq!(
            tree_view(&app).cursor,
            tip_of(&app, "A"),
            "on the first row"
        );
        type_str(&mut app, "jj");
        assert_eq!(tree_view(&app).cursor, c);
        type_str(&mut app, "h");
        assert_eq!(tree_view(&app).cursor, cde);
        enter(&mut app);
        assert!(tree_view(&app).closed.contains(&cde));
        assert!(rows_of(&app).contains(&"▸ 3 tips  C … E".to_string()));
        enter(&mut app);
        type_str(&mut app, "+");
        assert_eq!(tree_view(&app).shown, cde);
        type_str(&mut app, "-");
        assert_eq!(tree_view(&app).shown, root_of(&app));
        type_str(&mut app, "+z");
        assert_eq!(tree_view(&app).shown, root_of(&app));
        type_str(&mut app, "b");
        assert!(tree_view(&app).equal);
        type_str(&mut app, "l");
        assert_eq!(tree_view(&app).cursor, c, "back down to where h came from");
    }

    #[test]
    fn clicks_select_extend_and_escape_clears() {
        let (mut app, _dir) = with_tree(T6);
        click_node(&mut app, "A", NONE, LEFT);
        assert_eq!(tree_view(&app).select, Some(tip_of(&app, "A")));
        click_node(&mut app, "B", KeyModifiers::SHIFT, LEFT);
        assert_eq!(tree_view(&app).select, Some(parent_of(&app, "A")));
        click_node(&mut app, "C", NONE, MouseButton::Right);
        assert_eq!(tree_view(&app).select, Some(root_of(&app)));
        app.handle_event(&press(KeyCode::Esc));
        assert_eq!(tree_view(&app).select, None);
        type_str(&mut app, "v");
        assert_eq!(tree_view(&app).select, Some(tree_view(&app).cursor));
    }

    #[test]
    fn the_wheel_moves_the_cursor_three_rows() {
        let (mut app, _dir) = with_tree(T6);
        app.handle_event(&wheel(MouseEventKind::ScrollDown, NONE));
        assert_eq!(tree_view(&app).cursor, tip_of(&app, "D"));
    }

    #[test]
    fn the_tab_shows_the_tree_header_and_the_cursor_in_the_information_row() {
        let (app, _dir) = with_tree("((A:1,B:2)90:3,C:1);");
        let screen = render_rows(&app, 80, 24);
        assert!(screen[1].contains("t.nwk · 3 tips"), "{screen:?}");
        let (left, right) = ui::info_text(&app);
        assert_eq!((left.as_str(), right.as_str()), ("A · length 1", ""));
    }

    #[test]
    fn the_tree_fits_the_rows_the_screen_leaves_it() {
        let (mut app, _dir) = with_tree(T6);
        app.resize(Size::new(80, 8));
        let rows = tree_layout_of(&app).rows.height as usize;
        assert_eq!(tree_view(&app).height, rows);
        assert!(tree_view(&app).rows.len() <= rows.max(3));
    }

    /// `/pattern` and Enter.
    fn search_for(app: &mut App, pattern: &str) {
        type_str(app, &format!("/{pattern}"));
        enter(app);
    }

    /// `:line` and Enter.
    fn run_typed(app: &mut App, line: &str) {
        type_str(app, &format!(":{line}"));
        enter(app);
    }

    #[test]
    fn search_finds_tips_opens_the_path_and_steps_through_matches() {
        let (mut app, _dir) = with_tree("((a1,b),((c,a2),(d,(e,a3))));");
        // Tabs, header, 3 rows, information row, bottom line: the open
        // message, with a long temp path, would take two.
        app.status.clear();
        app.resize(Size::new(80, 7));
        assert_eq!(tree_view(&app).height, 3);
        search_for(&mut app, "A");
        assert_eq!(app.status, "match 1 of 3: a1");
        type_str(&mut app, "n");
        assert_eq!(
            (app.status.as_str(), tree_view(&app).cursor),
            ("match 2 of 3: a2", tip_of(&app, "a2"))
        );
        assert!(
            rows_of(&app).contains(&"a2".to_string()),
            "its path is open"
        );
        type_str(&mut app, "n");
        assert_eq!(app.status, "match 3 of 3: a3");
        type_str(&mut app, "n");
        assert_eq!(app.status, "match 1 of 3: a1");
        type_str(&mut app, "N");
        assert_eq!(app.status, "match 3 of 3: a3");
        run_typed(&mut app, "tree prune --tip a2");
        type_str(&mut app, "n");
        assert_eq!(
            app.status, "match 1 of 2: a1",
            "the tree changed: searched again"
        );
        search_for(&mut app, "zzz");
        assert_eq!(app.status, "error: no tip name matches 'zzz'");
        search_for(&mut app, "(");
        assert_eq!(app.status, "error: bad regular expression '('");
    }

    #[test]
    fn a_match_outside_the_zoomed_clade_shows_the_whole_tree() {
        let (mut app, _dir) = with_tree(T6);
        type_str(&mut app, "+"); // the cursor is on A: zoom to A and B
        assert_eq!(tree_view(&app).zoom, parent_of(&app, "A"));
        search_for(&mut app, "^E$");
        assert_eq!(tree_view(&app).zoom, root_of(&app));
        assert_eq!(tree_view(&app).cursor, tip_of(&app, "E"));
    }

    fn tree_doc(app: &App) -> &TreeDoc {
        app.tree().expect("a tree is shown").1
    }

    /// Put the cursor on `n` and refit, as moving there would.
    fn put_cursor_on(app: &mut App, n: NodeId) {
        let (doc, view) = app.tree_view_mut().unwrap();
        view.cursor = n;
        view.refit(doc.tree(), doc.facts(), view.height);
    }

    fn put_cursor(app: &mut App, name: &str) {
        let n = tip_of(app, name);
        put_cursor_on(app, n);
    }

    fn put_cursor_root(app: &mut App) {
        let n = root_of(app);
        put_cursor_on(app, n);
    }

    fn put_cursor_row(app: &mut App, row: usize) {
        let n = tree_view(app).rows[row].node;
        put_cursor_on(app, n);
    }

    /// The cursor to the smallest clade holding `a` and `b`, then `v`.
    fn select_node(app: &mut App, a: &str, b: &str) {
        let (doc, _) = app.tree_view_mut().unwrap();
        let (tree, facts) = (doc.tree(), doc.facts());
        let n = common_ancestor(
            tree,
            facts,
            crate::tree_view::tests::tip(tree, a),
            crate::tree_view::tests::tip(tree, b),
        );
        put_cursor_on(app, n);
        type_str(app, "v");
    }

    fn prompt_lead(app: &App) -> &'static str {
        match &app.mode {
            Mode::Prompt(prompt) => prompt.kind.lead(),
            _ => panic!("no prompt is open"),
        }
    }

    fn prompt_text(app: &App) -> String {
        format!("{}{}", prompt_lead(app), typed(app).unwrap())
    }

    fn clear_prompt_and_type(app: &mut App, text: &str) {
        for _ in 0..typed(app).unwrap().chars().count() {
            app.handle_event(&press(KeyCode::Backspace));
        }
        type_str(app, text);
    }

    /// Open `a.fa` holding `fasta`, go back to the tree and link them.
    fn link_alignment(app: &mut App, dir: &TempDir, fasta: &str) {
        let fa = dir.path().join("a.fa");
        fs::write(&fa, fasta).unwrap();
        run_typed(app, &format!("open {}", fa.display()));
        run_typed(app, "use 1");
        run_typed(app, &format!("tree link {}", fa.display()));
        assert!(app.status.starts_with("linked "), "{}", app.status);
    }

    /// The text of `tree extract` as logged.
    fn extract_line(at: NodeSel, out: PathBuf, seqs: Option<PathBuf>) -> String {
        Command::Tree(TreeCmd::Extract {
            at,
            out,
            seqs,
            wrap: None,
        })
        .to_line()
        .unwrap()
    }

    /// The tree as the app has it now, written to a file and read back.
    fn saved_newick(app: &mut App, dir: &TempDir) -> String {
        let out = dir.path().join("now.nwk");
        run_typed(app, &format!("tree extract --out {}", out.display()));
        fs::read_to_string(out).unwrap()
    }

    /// Run the app's logged lines in a new session and write its tree.
    fn replay_into_fresh_session(app: &App, dir: &TempDir) -> String {
        let mut session = Session::new(None);
        for line in app.session.history() {
            session.execute(Command::parse_line(line).unwrap()).unwrap();
        }
        let out = dir.path().join("replayed.nwk");
        let line = format!("tree extract --out {}", out.display());
        session
            .execute(Command::parse_line(&line).unwrap())
            .unwrap();
        fs::read_to_string(out).unwrap()
    }

    #[test]
    fn keys_act_on_the_selection_or_the_cursor_and_are_logged() {
        let (mut app, _dir) = with_tree("((A,B),(C,D),E);");
        select_node(&mut app, "A", "B");
        type_str(&mut app, "d");
        assert_eq!(last(&app), "tree prune --mrca A --mrca B");
        assert!(tree_doc(&app).facts().contains(tree_view(&app).cursor));
        put_cursor(&mut app, "C");
        type_str(&mut app, "o");
        assert_eq!(last(&app), "tree reroot --tip C");
        type_str(&mut app, "s");
        assert_eq!(last(&app), "tree ladderize");
        type_str(&mut app, "u");
        assert_eq!(last(&app), "undo");
        type_str(&mut app, "U");
        assert_eq!(last(&app), "redo");
    }

    #[test]
    fn rename_and_extract_ask_in_the_bottom_line() {
        let (mut app, dir) = with_tree("((A,B),(C,D));");
        put_cursor(&mut app, "A");
        type_str(&mut app, "r");
        assert_eq!(prompt_text(&app), "name: A");
        clear_prompt_and_type(&mut app, "x y");
        enter(&mut app);
        assert_eq!(last(&app), "tree rename --tip A --to 'x y'");
        put_cursor_root(&mut app);
        type_str(&mut app, "r");
        assert_eq!(
            app.status,
            "error: r renames a tip; to rename many, :tree rename --regex … --replace …"
        );
        type_str(&mut app, "e");
        assert_eq!(prompt_lead(&app), "tree file: ");
        let out = dir.path().join("o.nwk");
        type_str(&mut app, &out.display().to_string());
        enter(&mut app);
        assert_eq!(last(&app), extract_line(NodeSel::default(), out, None));
        link_alignment(&mut app, &dir, ">C\nAA\n>D\nCC\n");
        select_node(&mut app, "C", "D");
        type_str(&mut app, "e");
        let (out, seqs) = (dir.path().join("c.nwk"), dir.path().join("c.fa"));
        type_str(&mut app, &out.display().to_string());
        enter(&mut app);
        assert_eq!(prompt_lead(&app), "sequence file (Enter for none): ");
        type_str(&mut app, &seqs.display().to_string());
        enter(&mut app);
        let at = NodeSel {
            mrca: vec!["C".to_string(), "D".to_string()],
            ..NodeSel::default()
        };
        assert_eq!(last(&app), extract_line(at, out, Some(seqs.clone())));
        assert!(seqs.exists());
    }

    #[test]
    fn shared_names_are_acted_on_by_number_and_replay_to_the_same_tree() {
        let (mut app, dir) = with_tree("((A,A),(B,C));");
        put_cursor_row(&mut app, 1); // the second A
        type_str(&mut app, "d");
        assert_eq!(last(&app), "tree prune --node 4");
        let replayed = replay_into_fresh_session(&app, &dir);
        assert_eq!(replayed, saved_newick(&mut app, &dir));
        assert_eq!(replayed, "(A,(B,C));\n");
    }

    #[test]
    fn an_edit_that_removes_the_cursor_or_shown_clade_leaves_a_working_view() {
        let (mut app, _dir) = with_tree("((A,B),(C,(D,E)),F);");
        put_cursor(&mut app, "D");
        type_str(&mut app, "h+"); // show (D,E)
        put_cursor(&mut app, "D");
        type_str(&mut app, "d");
        let facts = tree_doc(&app).facts();
        let view = tree_view(&app);
        assert!(facts.contains(view.cursor) && facts.contains(view.shown));
        assert!(!view.rows.is_empty());
        type_str(&mut app, "u");
        let facts = tree_doc(&app).facts();
        let view = tree_view(&app);
        assert!(facts.contains(view.cursor) && facts.contains(view.shown));
        assert!(!view.rows.is_empty());
    }

    /// Show the tree's tab again: it is the first file.
    fn to_tree_tab(app: &mut App) {
        run_typed(app, "use 1");
        assert!(app.tree().is_some());
    }

    #[test]
    fn a_marks_the_clades_sequences_in_the_linked_alignment() {
        let (mut app, dir) = with_tree("((A,B),(C,X));");
        link_alignment(&mut app, &dir, ">A\nAA\n>B\nCC\n>C\nGG\n>D\nTT\n");
        select_node(&mut app, "C", "X");
        type_str(&mut app, "a");
        let (alignment, view) = app.alignment().expect("the alignment's tab is visible");
        assert_eq!(view.search.as_ref().unwrap().matches, [2]);
        assert_eq!(view.cursor.row, 2);
        assert_eq!(alignment.name(2), "C");
        assert_eq!(
            app.status,
            "1 of 2 tips have a sequence; n N step through them"
        );
        run_typed(&mut app, "msa rename --rows 3 --to Q");
        type_str(&mut app, "n");
        assert_eq!(
            app.status,
            "error: no sequence name matches 'tips of C … X'"
        );
        to_tree_tab(&mut app);
        select_node(&mut app, "A", "B");
        type_str(&mut app, "a");
        assert_eq!(view_of(&app).search.as_ref().unwrap().matches, [0, 1]);
        run_typed(&mut app, "close");
        type_str(&mut app, "y");
        to_tree_tab(&mut app);
        type_str(&mut app, "a");
        assert_eq!(
            app.status,
            "error: this tree is not linked; :tree link FILE"
        );
    }

    #[test]
    fn a_says_when_no_tip_of_the_clade_has_a_sequence() {
        let (mut app, dir) = with_tree("((A,B),(Y,Z));");
        link_alignment(&mut app, &dir, ">A\nAA\n>B\nCC\n");
        select_node(&mut app, "Y", "Z");
        type_str(&mut app, "a");
        assert_eq!(
            app.status,
            "error: none of these 2 tips has a sequence in a.fa"
        );
        assert!(app.tree().is_some(), "the tree's tab stays");
    }

    #[test]
    fn the_link_counts_are_worked_out_again_only_when_something_changed() {
        let (mut app, dir) = with_tree("((A,B),(C,X));");
        link_alignment(&mut app, &dir, ">A\nAA\n>B\nCC\n>C\nGG\n");
        let key = tree_view(&app).link_key;
        assert!(key.is_some());
        assert_eq!(tree_view(&app).link.unwrap().matched, 3);
        type_str(&mut app, "jjk");
        assert_eq!(tree_view(&app).link_key, key, "moving changes nothing");
        run_typed(&mut app, "use 2");
        run_typed(&mut app, "msa rename --rows 1 --to Q");
        let view = &app.trees[&app.session.datasets()[0].id];
        assert_ne!(view.link_key, key, "the alignment changed");
        assert_eq!(view.link.unwrap().matched, 2);
    }

    #[test]
    #[ignore = "timing; run with: cargo test --release -p seqpeeq -- --ignored keys_on"]
    fn keys_on_a_large_linked_tree_stay_fast() {
        use std::time::Instant;
        let n = 1_000_000;
        let mut text = String::with_capacity(n * 12);
        for i in 0..n - 1 {
            text.push_str(&format!("(t{i},"));
        }
        text.push_str(&format!("t{}", n - 1));
        text.push_str(&")".repeat(n - 1));
        text.push(';');
        let (mut app, dir) = with_tree(&text);
        let fasta: String = (0..1000).map(|i| format!(">t{i}\nACGT\n")).collect();
        link_alignment(&mut app, &dir, &fasta);
        for tab in ["use 1", "use 2"] {
            run_typed(&mut app, tab);
            let mut slowest = std::time::Duration::ZERO;
            for _ in 0..20 {
                let started = Instant::now();
                type_str(&mut app, "j");
                slowest = slowest.max(started.elapsed());
            }
            println!("{tab}: slowest key {slowest:?}");
            assert!(slowest.as_millis() < 10, "{tab}: {slowest:?}");
        }
    }
}
