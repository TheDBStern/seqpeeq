//! The BAM tab's side of the app: one view per open BAM, and the reading
//! it hands to worker threads.

use super::*;
use crate::bam_view::{ContigList, ContigSort, ReadId, pass_bin, serves, zoom_levels};
use crate::ui::bam::BamHit;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use seqpeeq_core::bam::parse_region;
use std::time::{Duration, Instant};

/// More selected names than this go to a names file beside the log.
const NAMES_ON_THE_LINE: usize = 20;

/// A jump reading the BAM in the background.
pub struct Jumping {
    pub dataset: u64,
    /// The contig, the position it started from, and which way and to
    /// what kind of stretch.
    pub asked: (usize, u64, Direction, Stretch),
    pub progress: Arc<Progress>,
}

/// What a jump that found nothing says.
fn no_more(want: Stretch, dir: Direction, contig: &str) -> String {
    match (want, dir) {
        (Stretch::Covered, Direction::Forward) => format!("no more covered stretches on {contig}"),
        (Stretch::Empty, Direction::Forward) => {
            format!("no more stretches without coverage on {contig}")
        }
        (Stretch::Covered, Direction::Back) => {
            format!("no covered stretch before here on {contig}")
        }
        (Stretch::Empty, Direction::Back) => {
            format!("no stretch without coverage before here on {contig}")
        }
    }
}

/// The `g` line, holding `text`.
fn goto_line(text: &str) -> Mode {
    Mode::Prompt(Prompt::new(PromptKind::Goto, LineEdit::new(text)))
}

/// A contig's name as typed in the `g` line: in braces when it holds a
/// `:`, so a position can follow it.
fn quoted(name: &str) -> String {
    match name.contains(':') {
        true => format!("{{{name}}}"),
        false => name.to_string(),
    }
}

/// Whether `name` starts with `typed`, in any case.
fn starts_with_any_case(name: &str, typed: &str) -> bool {
    let mut name = name.chars().flat_map(char::to_lowercase);
    typed
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| name.next() == Some(c))
}

/// The longest start of `first` that each of `rest` shares, in any case.
fn common_prefix<'a>(first: &'a str, rest: &[&str]) -> &'a str {
    let mut end = first.len();
    for other in rest {
        // Where they differ, or where `other` runs out, counted in
        // characters: the same letter in another case may take more or
        // fewer bytes.
        let mut theirs = other.chars();
        let shared = first
            .char_indices()
            .find(|&(_, a)| {
                theirs
                    .next()
                    .is_none_or(|b| !a.to_lowercase().eq(b.to_lowercase()))
            })
            .map_or(first.len(), |(i, _)| i);
        end = end.min(shared);
    }
    &first[..end]
}

/// How often a drag held past the edge of the reads scrolls them.
const DRAG_EVERY: Duration = Duration::from_millis(50);

/// How many columns a drag held past a side scrolls each time.
const DRAG_COLS: i64 = 4;

/// A drag across a BAM's reads, from a press on them until the button is
/// let go.
#[derive(Debug, Clone)]
pub struct BamDrag {
    /// The BAM it is on.
    pub dataset: u64,
    /// The first and last positions of the column pressed.
    pub anchor: (u64, u64),
    /// The row of reads pressed, counted from the first row.
    pub anchor_row: usize,
    /// The read pressed, to find its row again once the reads are packed
    /// afresh.
    pub anchor_read: Option<ReadId>,
    /// Where the pointer is, held inside the reads: a column and a row
    /// counted from the first shown.
    pub at: (usize, usize),
    /// Which side of the reads the pointer is past, if any: -1, 0 or 1
    /// sideways and up or down.
    pub edge: (i8, i8),
    /// Reads selected before the reads were last read afresh.
    pub kept: Vec<ReadId>,
    /// When it last scrolled the reads.
    pub scrolled: Instant,
}

/// Select what `drag` covers in `view`: the reads touching the rectangle
/// from where it started to where the pointer is, and those it kept.
fn drag_select(view: &mut BamView, drag: &BamDrag) {
    let span = view.span_of(drag.at.0);
    let to = view.top + drag.at.1;
    let positions = (drag.anchor.0.min(span.0), drag.anchor.1.max(span.1));
    let rows = drag.anchor_row.min(to)..=drag.anchor_row.max(to);
    let mut selected = drag.kept.clone();
    let mut seen: HashSet<ReadId> = selected.iter().cloned().collect();
    for read in view.touched(positions, rows) {
        if seen.insert(read.clone()) {
            selected.push(read);
        }
    }
    view.selected = selected;
}

/// A window being read in the background.
pub struct Reading {
    pub dataset: u64,
    pub generation: u64,
    pub want: Want,
    /// Whether a reference was attached when it was asked for.
    pub with_reference: bool,
    pub progress: Arc<Progress>,
}

impl App {
    /// The BAM on the visible tab and where the user is in it.
    pub fn bam(&self) -> Option<(&Dataset, &BamDoc, &BamView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset, dataset.bam()?, self.bams.get(&dataset.id)?))
    }

    /// The screen area a BAM tab is drawn in.
    fn bam_body(&self) -> ratatui::layout::Rect {
        let screen = ratatui::layout::Rect::new(0, 0, self.screen.width, self.screen.height);
        ui::regions(self, screen).body
    }

    /// Keep one view per open BAM, sized to the screen, and ask for what
    /// the visible one needs: its contig's coverage pass, and the window
    /// around it.
    /// Close the contig list once its file is no longer on the tab shown.
    fn close_stale_contig_list(&mut self) {
        let active = self.session.datasets().get(self.active).map(|d| d.id);
        if let Mode::Contigs(list) = &self.mode
            && Some(list.dataset) != active
        {
            self.mode = Mode::Normal;
        }
    }

    pub(super) fn sync_bams(&mut self) {
        self.close_stale_contig_list();
        let open: Vec<u64> = self
            .session
            .datasets()
            .iter()
            .filter(|d| d.bam().is_some_and(|b| !b.source().contigs().is_empty()))
            .map(|d| d.id)
            .collect();
        self.bams.retain(|id, _| open.contains(id));
        self.passes.retain(|(id, _), _| open.contains(id));
        if let Some((id, _, progress)) = &self.passing
            && !open.contains(id)
        {
            progress.cancel();
            self.passing = None;
        }
        if let Some(reading) = self.reading.take_if(|r| !open.contains(&r.dataset)) {
            reading.progress.cancel();
        }
        if let Some(jumping) = self.jumping.take_if(|j| !open.contains(&j.dataset)) {
            jumping.progress.cancel();
        }
        self.searches.retain(|id, s| {
            let keep = open.contains(id);
            if let (false, Some((_, progress))) = (keep, &s.running) {
                progress.cancel();
            }
            keep
        });
        self.gathering.retain(|(id, _, progress)| {
            let keep = open.contains(id);
            if !keep {
                progress.cancel();
            }
            keep
        });
        let body = self.bam_body();
        let stacked = self.stacked_reads(body);
        let mut fresh = Vec::new();
        for dataset in self.session.datasets() {
            let Some(doc) = dataset.bam().filter(|_| open.contains(&dataset.id)) else {
                continue;
            };
            let contigs = doc.source().contigs();
            if !self.bams.contains_key(&dataset.id) {
                fresh.push(dataset.id);
            }
            let view = self.bams.entry(dataset.id).or_insert_with(|| {
                let first = contigs
                    .iter()
                    .position(|c| c.mapped.is_some_and(|m| m > 0))
                    .unwrap_or(0);
                BamView::new(first, contigs[first].length, body.width as usize)
            });
            for over in view.over.iter_mut().chain(view.parked.iter_mut()) {
                if let crate::bam_view::Over::Axis(axis) = over {
                    let layout = ui::read_axis::axis_layout(body, axis, contigs);
                    axis.resize(layout.bars.width as usize, layout.bars.height as usize);
                }
            }
            let length = contigs[view.contig].length;
            let reads = match stacked.get(&dataset.id) {
                Some(&reads) => reads,
                None => ui::bam::bam_layout(body, view).reads,
            };
            let (width, height) = (reads.width as usize, reads.height as usize);
            if (view.width, view.height) != (width, height) {
                view.width = width;
                view.height = height;
                let whole = *zoom_levels(length, width).last().expect("a level");
                view.per_col = view.per_col.min(whole);
                view.reveal(length);
            }
        }

        self.mirror_stacks(&fresh);

        let Some(dataset) = self.session.datasets().get(self.active) else {
            return;
        };
        let id = dataset.id;
        if dataset.bam().is_none() || !self.bams.contains_key(&id) {
            return;
        }

        // The BAM shown first, then the rest of its stack, one at a time:
        // its coverage pass and window, then theirs.
        let pass_done = self.ask_pass(id, true);
        let window_done = self.ask_window(id, true);
        let others: Vec<u64> = self
            .session
            .stack_of(id)
            .map(|members| members.iter().copied().filter(|&m| m != id).collect())
            .unwrap_or_default();
        if pass_done {
            for &member in &others {
                if !self.ask_pass(member, false) {
                    break;
                }
            }
        }
        if window_done {
            for member in others {
                if !self.ask_window(member, false) {
                    return;
                }
            }
        }
    }

    /// Start the coverage pass of BAM `id`'s contig in view, unless it is
    /// done, running or failed; `take_over` cancels another pass for it,
    /// else it waits for no pass to be running. Returns whether the pass
    /// has finished (or cannot be had).
    fn ask_pass(&mut self, id: u64, take_over: bool) -> bool {
        let Some(source) = self
            .session
            .datasets()
            .iter()
            .find(|d| d.id == id)
            .and_then(Dataset::bam)
            .map(|b| b.source().clone())
        else {
            return true;
        };
        let Some(view) = self.bams.get(&id) else {
            return true;
        };
        let contig = view.contig;
        let length = source.contigs()[contig].length;
        let done = self
            .passes
            .get(&(id, contig))
            .is_some_and(|c| c.done_to >= length);
        if done || self.pass_failed.contains(&(id, contig)) {
            return true;
        }
        let running = self
            .passing
            .as_ref()
            .is_some_and(|(d, c, _)| (*d, *c) == (id, contig));
        if running || (!take_over && self.passing.is_some()) {
            return false;
        }
        if let Some((d, c, progress)) = self.passing.take() {
            progress.cancel();
            // Unfinished, so not kept: it starts again when shown.
            self.passes.remove(&(d, c));
        }
        let progress = Arc::new(Progress::new());
        self.outbox.push(Request::Pass {
            dataset: id,
            contig,
            source,
            progress: progress.clone(),
        });
        self.passing = Some((id, contig, progress));
        false
    }

    /// The rows of reads of each BAM in a stack, by dataset id, as the
    /// stack is laid out in `body`.
    fn stacked_reads(&self, body: ratatui::layout::Rect) -> HashMap<u64, ratatui::layout::Rect> {
        let mut reads = HashMap::new();
        for (members, layout) in self.stack_layouts(body) {
            for (member, rows) in members.iter().zip(&layout.members) {
                reads.insert(*member, rows.reads);
            }
        }
        reads
    }

    /// Each stack (its members' dataset ids) and how it is laid out in
    /// `body`: by the reads each member has, at the zoom of the BAM shown
    /// (else its first).
    pub(crate) fn stack_layouts(
        &self,
        body: ratatui::layout::Rect,
    ) -> Vec<(Vec<u64>, ui::bam::StackLayout)> {
        let shown = self.session.datasets().get(self.active).map(|d| d.id);
        self.session
            .stacks()
            .iter()
            .map(|members| {
                let leader = members
                    .iter()
                    .find(|&&m| Some(m) == shown)
                    .unwrap_or(&members[0]);
                let base_level = self.bams.get(leader).is_some_and(|v| v.per_col == 1);
                let needs: Vec<usize> = members
                    .iter()
                    .map(|m| match self.bams.get(m) {
                        Some(view) if view.loaded.is_some() => view.packed.rows.len(),
                        _ => usize::MAX,
                    })
                    .collect();
                let layout = ui::bam::stack_layout(body, base_level, &needs);
                (members.clone(), layout)
            })
            .collect()
    }

    /// Put every other BAM of the shown BAM's stack where it is: the same
    /// contig (by name), place and zoom. A BAM whose view is new this turn
    /// (`fresh`) has just joined, so it takes the stack's place instead.
    pub(super) fn mirror_stacks(&mut self, fresh: &[u64]) {
        let Some(shown) = self.session.datasets().get(self.active).map(|d| d.id) else {
            return;
        };
        let Some(members) = self.session.stack_of(shown) else {
            return;
        };
        let leader = if fresh.contains(&shown) {
            members
                .iter()
                .copied()
                .find(|m| !fresh.contains(m) && self.bams.contains_key(m))
                .unwrap_or(shown)
        } else {
            shown
        };
        let datasets = self.session.datasets();
        let contigs_of = |id: u64| -> &[seqpeeq_core::bam::Contig] {
            let doc = datasets.iter().find(|d| d.id == id).and_then(Dataset::bam);
            doc.map_or(&[], |b| b.source().contigs())
        };
        let Some(lead) = self.bams.get(&leader).map(BamView::place_now) else {
            return;
        };
        let theirs = contigs_of(leader);
        for &member in members.iter().filter(|&&m| m != leader) {
            let mine = contigs_of(member);
            // Stacked BAMs mostly list contigs in the same order.
            let index = |contig: usize| {
                let name = &theirs.get(contig)?.name;
                match mine.get(contig) {
                    Some(c) if &c.name == name => Some(contig),
                    _ => mine.iter().position(|c| &c.name == name),
                }
            };
            let Some(contig) = index(lead.contig) else {
                continue;
            };
            let back = lead
                .back
                .iter()
                .filter_map(|&(c, pos)| Some((index(c)?, pos)))
                .collect();
            let place = crate::bam_view::StackPlace {
                contig,
                back,
                ..lead.clone()
            };
            if let Some(view) = self.bams.get_mut(&member) {
                view.follow(place, mine[contig].length);
            }
        }
    }

    /// Ask for the window the view of BAM `id` needs, unless it has it, has
    /// asked for it, or failed to read it; `take_over` cancels another
    /// BAM's read for it, else it waits for no read to be running. Returns
    /// whether the view has all it needs.
    fn ask_window(&mut self, id: u64, take_over: bool) -> bool {
        let Some(source) = self
            .session
            .datasets()
            .iter()
            .find(|d| d.id == id)
            .and_then(Dataset::bam)
            .map(|b| b.source().clone())
        else {
            return true;
        };
        let Some(view) = self.bams.get_mut(&id) else {
            return true;
        };
        let length = source.contigs()[view.contig].length;
        let Some(want) = view.wanted(length, pass_bin(length)) else {
            if let Some(reading) = self.reading.take_if(|r| r.dataset == id) {
                reading.progress.cancel();
            }
            return true;
        };
        let shown = view.view_region(length);
        let with_reference = source.reference().is_some();
        // Reads read before a reference was attached are read again with it.
        let complete = |reads: bool, had: bool| !reads || !with_reference || had;
        let loaded = view.loaded.as_ref().is_some_and(|l| {
            let have = (l.window.region, l.window.bin, l.asked_reads);
            serves(&have, &shown, &want) && complete(l.asked_reads, l.with_reference)
        });
        let asked = self.reading.as_ref().is_some_and(|r| {
            r.dataset == id
                && serves(&r.want, &shown, &want)
                && complete(r.want.2, r.with_reference)
        });
        let failed = self.window_failed == Some((id, want));
        if loaded || failed {
            return true;
        }
        if asked {
            return false;
        }
        if !take_over && self.reading.is_some() {
            return false;
        }
        if let Some(reading) = self.reading.take() {
            reading.progress.cancel();
        }
        view.generation += 1;
        let progress = Arc::new(Progress::new());
        self.outbox.push(Request::Window {
            dataset: id,
            generation: view.generation,
            source,
            want,
            progress: progress.clone(),
        });
        self.reading = Some(Reading {
            dataset: id,
            generation: view.generation,
            want,
            with_reference,
            progress,
        });
        false
    }

    /// Take in a window read in the background, if its view still wants it.
    pub(super) fn window_done(
        &mut self,
        dataset: u64,
        generation: u64,
        result: seqpeeq_core::Result<Loaded>,
    ) {
        let reading = self
            .reading
            .take_if(|r| (r.dataset, r.generation) == (dataset, generation));
        let Some(view) = self.bams.get_mut(&dataset) else {
            return;
        };
        if view.generation != generation {
            return;
        }
        match result {
            Ok(loaded) => {
                view.loaded = Some(loaded);
                view.repack();
                if let Some(drag) = self.bam_drag.as_mut().filter(|d| d.dataset == dataset) {
                    drag.kept = view.selected.clone();
                    if let Some(row) = drag.anchor_read.as_ref().and_then(|read| view.row_of(read))
                    {
                        drag.anchor_row = row;
                    }
                }
                let contig = view.contig;
                if let Some(length) = self
                    .session
                    .datasets()
                    .iter()
                    .find(|d| d.id == dataset)
                    .and_then(Dataset::bam)
                    .map(|b| b.source().contigs()[contig].length)
                {
                    view.apply_focus(length);
                }
            }
            Err(Error::Cancelled) => {}
            Err(e) => {
                self.status = e.to_string();
                if let Some(reading) = reading {
                    self.window_failed = Some((dataset, reading.want));
                }
            }
        }
    }

    /// The visible BAM's source and view.
    pub(super) fn bam_mut(&mut self) -> Option<(BamSource, &mut BamView)> {
        let dataset = self.session.datasets().get(self.active)?;
        let (id, source) = (dataset.id, dataset.bam()?.source().clone());
        Some((source, self.bams.get_mut(&id)?))
    }

    /// A key on the BAM tab.
    pub(super) fn bam_key(&mut self, key: &KeyEvent) {
        use Direction::{Back, Forward};
        use Stretch::{Covered, Empty};
        match self.bam().and_then(|(_, _, view)| view.over.last()) {
            Some(crate::bam_view::Over::List(_)) => return self.list_key(key),
            Some(crate::bam_view::Over::Axis(_)) => return self.axis_key(key),
            None => {}
        }
        match key.code {
            KeyCode::Char('a') => return self.bam_axis(),
            KeyCode::Char('A') => return self.bam_elsewhere(),
            KeyCode::Char('g') => return self.open_prompt(PromptKind::Goto, ""),
            KeyCode::Char(']') => return self.bam_jump(Forward, Covered),
            KeyCode::Char('[') => return self.bam_jump(Back, Covered),
            KeyCode::Char('}') => return self.bam_jump(Forward, Empty),
            KeyCode::Char('{') => return self.bam_jump(Back, Empty),
            KeyCode::Char('m') => return self.bam_mate(),
            KeyCode::Char('s') => return self.bam_part(),
            KeyCode::Char('\'') => return self.bam_back(),
            KeyCode::Char('e') => return self.bam_export_reads(),
            KeyCode::Char('E') => return self.bam_export_ref(),
            KeyCode::Char('v') => {
                if let Some((_, view)) = self.bam_mut() {
                    view.mark();
                }
                return;
            }
            KeyCode::Char('c') => {
                if let Some((dataset, doc, _)) = self.bam() {
                    let list = ContigList::new(dataset.id, doc.source().contigs());
                    self.mode = Mode::Contigs(Box::new(list));
                }
                return;
            }
            _ => {}
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
        let length = source.contigs()[view.contig].length;
        let across = view.width.max(1) as i64;
        let page = view.height.max(1) as isize;
        match key.code {
            KeyCode::Left if shift => view.step(-across, length),
            KeyCode::Right if shift => view.step(across, length),
            KeyCode::Left | KeyCode::Char('h') => view.step(-1, length),
            KeyCode::Right | KeyCode::Char('l') => view.step(1, length),
            KeyCode::Up if shift => view.move_row(-page),
            KeyCode::Down if shift => view.move_row(page),
            KeyCode::PageUp => view.move_row(-page),
            KeyCode::PageDown => view.move_row(page),
            KeyCode::Up | KeyCode::Char('k') => view.move_row(-1),
            KeyCode::Down | KeyCode::Char('j') => view.move_row(1),
            KeyCode::Home => view.step(i64::MIN / 2, length),
            KeyCode::End => view.step(i64::MAX / 2, length),
            KeyCode::Char('+' | '=') => {
                view.zoom(-1, length);
                view.repack();
            }
            KeyCode::Char('-') => {
                view.zoom(1, length);
                view.repack();
            }
            KeyCode::Char('z') => {
                view.toggle_whole(length);
                view.repack();
            }
            KeyCode::Char('F') => {
                view.show_filtered = !view.show_filtered;
                view.repack();
            }
            KeyCode::Char('S') => {
                view.hide_clips = !view.hide_clips;
                view.repack();
            }
            KeyCode::Char(' ') => {
                if let Some(read) = view.cursor_read().map(ReadId::of) {
                    view.select(read);
                }
            }
            _ => {}
        }
    }

    /// Tab in the `g` line on a BAM tab, with the cursor at the end of a
    /// contig name being typed: finish the name if one contig starts with
    /// it, or as far as all those that do agree; otherwise open the contig
    /// list, filtered by what was typed, to pick one from.
    pub(super) fn goto_tab(&mut self) {
        let Mode::Prompt(prompt) = &self.mode else {
            return;
        };
        let line = prompt.line.text().to_string();
        if prompt.line.cursor() != line.len() {
            return;
        }
        // As the line is read when entered.
        let text = line.trim().to_string();
        let (typed, braced) = match text.strip_prefix('{') {
            Some(rest) if rest.contains('}') => return,
            Some(rest) => (rest, true),
            None if text.contains(':') => return,
            None => (text.as_str(), false),
        };
        let Some((dataset, doc, _)) = self.bam() else {
            return;
        };
        let contigs = doc.source().contigs();
        let matching: Vec<&str> = contigs
            .iter()
            .map(|c| c.name.as_str())
            .filter(|name| starts_with_any_case(name, typed))
            .collect();
        let done = match matching.as_slice() {
            [one] => Some(quoted(one)),
            [first, rest @ ..] => {
                let common = common_prefix(first, rest);
                (common.chars().count() > typed.chars().count()).then(|| {
                    match braced || common.contains(':') {
                        true => format!("{{{common}"),
                        false => common.to_string(),
                    }
                })
            }
            [] => None,
        };
        if let Some(done) = done {
            self.mode = goto_line(&done);
            return;
        }
        let mut list = ContigList::new(dataset.id, contigs);
        list.filter = LineEdit::new(typed);
        if list.rows(contigs).is_empty() {
            list.all = true;
        }
        list.back_to = Some(line);
        self.mode = Mode::Contigs(Box::new(list));
    }

    /// Go where `text` says: a position on this contig, `ctg:pos`,
    /// `ctg:a-b` (zoomed to fit) or a contig's name.
    pub(super) fn bam_go(&mut self, text: &str) {
        let text = text.trim();
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let contigs = source.contigs();
        let bare = text
            .strip_prefix('{')
            .and_then(|t| t.strip_suffix('}'))
            .unwrap_or(text);
        let named = contigs.iter().any(|c| c.name == text || c.name == bare);
        let position = !named && text.replace(',', "").parse::<u64>().is_ok();
        let query = match position {
            true => format!("{{{}}}:{text}", contigs[view.contig].name),
            false => text.to_string(),
        };
        let region = match parse_region(&query, contigs, &source.file_name()) {
            Ok((region, _)) => region,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        };
        let length = contigs[region.contig].length;
        view.go(region.contig, region.start, length);
        if !named && region.end > region.start {
            view.fit(region.start, region.end, length);
        }
    }

    /// `]` `[` `}` `{`: the next stretch of a kind, from the coverage pass
    /// where it can answer, else read from the BAM in the background.
    fn bam_jump(&mut self, dir: Direction, want: Stretch) {
        let Some(dataset) = self.session.datasets().get(self.active) else {
            return;
        };
        let id = dataset.id;
        let Some(source) = dataset.bam().map(|b| b.source().clone()) else {
            return;
        };
        let Some(view) = self.bams.get_mut(&id) else {
            return;
        };
        let (contig, from) = (view.contig, view.cursor);
        let length = source.contigs()[contig].length;
        if let Some(pass) = self.passes.get(&(id, contig)) {
            match pass.next_start(length, from, dir, want) {
                Some(to) => return view.go(contig, to, length),
                None if pass.done_to >= length => {
                    self.status = no_more(want, dir, &source.contigs()[contig].name);
                    return;
                }
                None => {}
            }
        }
        if let Some(old) = self.jumping.take() {
            old.progress.cancel();
        }
        let progress = Arc::new(Progress::new());
        self.outbox.push(Request::Jump {
            dataset: id,
            contig,
            from,
            dir,
            want,
            source,
            progress: progress.clone(),
        });
        self.jumping = Some(Jumping {
            dataset: id,
            asked: (contig, from, dir, want),
            progress,
        });
    }

    /// Take in where a jump read from the BAM landed, unless the view has
    /// moved since it was asked for.
    pub(super) fn jump_done(
        &mut self,
        dataset: u64,
        asked: (usize, u64, Direction, Stretch),
        result: seqpeeq_core::Result<Option<u64>>,
        progress: &Arc<Progress>,
    ) {
        if self
            .jumping
            .take_if(|j| {
                (j.dataset, j.asked) == (dataset, asked) && Arc::ptr_eq(&j.progress, progress)
            })
            .is_none()
        {
            return;
        }
        let (contig, from, dir, want) = asked;
        let Some(source) = self
            .session
            .datasets()
            .iter()
            .find(|d| d.id == dataset)
            .and_then(Dataset::bam)
            .map(|b| b.source().clone())
        else {
            return;
        };
        let Some(view) = self.bams.get_mut(&dataset) else {
            return;
        };
        if (view.contig, view.cursor) != (contig, from) {
            return;
        }
        let name = &source.contigs()[contig].name;
        match result {
            Ok(Some(to)) => view.go(contig, to, source.contigs()[contig].length),
            Ok(None) => self.status = no_more(want, dir, name),
            Err(Error::Cancelled) => self.status = "jump cancelled".to_string(),
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// What `e` and `E` export: the stretch the selected reads cover on
    /// one contig (the view's, when any are on it), or the view's region
    /// with nothing selected. Also the names of the reads, in the order
    /// picked, and how many selected reads were on other contigs.
    fn export_target(&self) -> Option<(String, Vec<String>, String)> {
        let (_, doc, view) = self.bam()?;
        let contigs = doc.source().contigs();
        let word =
            |w: &str| seqpeeq_core::command::quote(w).map_or(w.to_string(), |q| q.into_owned());
        if let Some((a, b)) = view.stretch() {
            let region = format!("{}:{a}-{b}", contigs[view.contig].name);
            return Some((word(&region), Vec::new(), String::new()));
        }
        let Some(first) = view.selected.first() else {
            let r = view.view_region(contigs[view.contig].length);
            let region = format!("{}:{}-{}", contigs[r.contig].name, r.start, r.end);
            return Some((word(&region), Vec::new(), String::new()));
        };
        let contig = if view.selected.iter().any(|r| r.contig == view.contig) {
            view.contig
        } else {
            first.contig
        };
        let (here, elsewhere): (Vec<&ReadId>, Vec<&ReadId>) =
            view.selected.iter().partition(|r| r.contig == contig);
        let start = here
            .iter()
            .map(|r| r.start)
            .min()
            .expect("one read at least");
        let end = here.iter().map(|r| r.end).max().expect("one read at least");
        let region = format!("{}:{start}-{end}", contigs[contig].name);
        let mut names: Vec<String> = Vec::new();
        for read in &here {
            if !names.contains(&read.name) {
                names.push(read.name.clone());
            }
        }
        let mut note = String::new();
        if !elsewhere.is_empty() {
            let mut on: Vec<&str> = Vec::new();
            for read in &elsewhere {
                let name = contigs[read.contig].name.as_str();
                if !on.contains(&name) {
                    on.push(name);
                }
            }
            let n = elsewhere.len();
            note = format!(
                "{n} selected read{} on {} left out: an export takes one region",
                if n == 1 { "" } else { "s" },
                on.join(", ")
            );
        }
        Some((word(&region), names, note))
    }

    /// `e`: open the command line holding the export of the selected
    /// reads, or of every read in view, ready for the file name.
    pub(super) fn bam_export_reads(&mut self) {
        let Some((region, names, note)) = self.export_target() else {
            return;
        };
        let word =
            |w: &str| seqpeeq_core::command::quote(w).map_or(w.to_string(), |q| q.into_owned());
        let mut line = format!("bam export-reads --region {region}");
        let file = if names.len() > NAMES_ON_THE_LINE {
            match self.session.names_file(&names) {
                Ok(file) => file,
                Err(e) => {
                    self.status = format!("error: {e}");
                    return;
                }
            }
        } else {
            None
        };
        match file {
            Some(file) => {
                line.push_str(&format!(" --names-file {}", word(&file.to_string_lossy())))
            }
            None => {
                for name in &names {
                    line.push_str(&format!(" --name {}", word(name)));
                }
            }
        }
        line.push_str(" --out ");
        self.open_prompt(PromptKind::Command, &line);
        self.status = note;
    }

    /// `E`: open the command line holding the export of the reference
    /// under the selected reads, or in view, ready for the file name.
    pub(super) fn bam_export_ref(&mut self) {
        let Some((region, _, note)) = self.export_target() else {
            return;
        };
        self.open_prompt(
            PromptKind::Command,
            &format!("bam export-ref {region} --out "),
        );
        self.status = note;
    }

    /// A key while the contig list is open.
    pub(super) fn contigs_key(&mut self, key: &KeyEvent) {
        self.close_stale_contig_list();
        let Some(source) = self.bam().map(|(_, doc, _)| doc.source().clone()) else {
            self.mode = Mode::Normal;
            return;
        };
        if !matches!(self.mode, Mode::Contigs(_)) {
            return;
        }
        let contigs = source.contigs();
        let visible = ui::bam::contig_list_rows(self).max(1);
        let Mode::Contigs(list) = &mut self.mode else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let start_over = |list: &mut ContigList| {
            list.cursor = 0;
            list.top = 0;
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = match list.back_to.take() {
                    Some(text) => goto_line(&text),
                    None => Mode::Normal,
                };
                return;
            }
            KeyCode::Enter if list.back_to.is_some() => {
                let picked = list.rows(contigs).get(list.cursor).copied();
                let text = match picked {
                    Some(i) => quoted(&contigs[i].name),
                    None => list.back_to.take().unwrap_or_default(),
                };
                self.mode = goto_line(&text);
                return;
            }
            KeyCode::Enter => {
                let picked = list.rows(contigs).get(list.cursor).copied();
                self.mode = Mode::Normal;
                if let Some((source, view)) = picked.and(self.bam_mut()) {
                    let contig = picked.expect("just checked");
                    view.whole_contig(contig, source.contigs()[contig].length);
                }
                return;
            }
            KeyCode::Up => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Down => list.cursor += 1,
            KeyCode::PageUp => list.cursor = list.cursor.saturating_sub(visible),
            KeyCode::PageDown => list.cursor += visible,
            KeyCode::Tab => {
                list.sort = match list.sort {
                    ContigSort::Name => ContigSort::Reads,
                    ContigSort::Reads => ContigSort::Name,
                };
                start_over(list);
            }
            KeyCode::Char('a') if ctrl => {
                list.all = !list.all;
                start_over(list);
            }
            _ if ctrl => {}
            _ => {
                if list.filter.key(key) {
                    start_over(list);
                }
            }
        }
        let shown = list.rows(contigs).len();
        list.cursor = list.cursor.min(shown.saturating_sub(1));
        if list.cursor < list.top {
            list.top = list.cursor;
        } else if list.cursor >= list.top + visible {
            list.top = list.cursor + 1 - visible;
        }
    }

    /// The mouse while the contig list is open: the wheel moves through it;
    /// a click on a contig goes there, on the heading switches the sort,
    /// and outside the list closes it.
    pub(super) fn contigs_mouse(&mut self, mouse: &MouseEvent) {
        let Mode::Contigs(list) = &self.mode else {
            return;
        };
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            MouseEventKind::Down(MouseButton::Left) => {
                match ui::bam::contig_hit(self, list, mouse.column, mouse.row) {
                    ui::bam::ContigHit::Row(i) => {
                        if let Mode::Contigs(list) = &mut self.mode {
                            list.cursor = i;
                        }
                        KeyCode::Enter
                    }
                    ui::bam::ContigHit::Heading => KeyCode::Tab,
                    ui::bam::ContigHit::Outside => KeyCode::Esc,
                    ui::bam::ContigHit::Inside => return,
                }
            }
            _ => return,
        };
        self.contigs_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// The mouse on the BAM tab: the wheel scrolls rows, or sideways over
    /// the ruler or strip; a click on the strip goes there; clicks and
    /// drags on the reads select them.
    pub(super) fn bam_mouse(&mut self, mouse: &MouseEvent) {
        // A view open over the reference view takes the wheel, as Up and
        // Down; nothing else reaches the reference view under it.
        if self.bam().is_some_and(|(_, _, view)| !view.over.is_empty()) {
            let code = match mouse.kind {
                MouseEventKind::ScrollUp => KeyCode::Up,
                MouseEventKind::ScrollDown => KeyCode::Down,
                _ => return,
            };
            if !self.typing_filter() {
                self.bam_key(&KeyEvent::new(code, KeyModifiers::NONE));
            }
            return;
        }
        let body = self.bam_body();
        let at = ratatui::layout::Position::new(mouse.column, mouse.row);
        let shown = self.session.datasets().get(self.active).map(|d| d.id);
        let stack = self
            .stack_layouts(body)
            .into_iter()
            .find(|(members, _)| shown.is_some_and(|id| members.contains(&id)));
        if let Some((members, layout)) = &stack {
            let under = layout.members.iter().position(|rows| {
                [rows.name, rows.coverage, rows.reads]
                    .iter()
                    .any(|r| r.contains(at))
            });
            if let Some(i) = under {
                let member = members[i];
                match mouse.kind {
                    // The wheel scrolls the BAM under it, which keeps
                    // whatever has the focus.
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                        let by = if mouse.kind == MouseEventKind::ScrollDown {
                            3
                        } else {
                            -3
                        };
                        if let Some(view) = self.bams.get_mut(&member) {
                            view.scroll_rows(by);
                        }
                        return;
                    }
                    // A press focuses the BAM under it, then acts on it.
                    MouseEventKind::Down(_) => {
                        let datasets = self.session.datasets();
                        if let Some(index) = datasets.iter().position(|d| d.id == member) {
                            self.focus(index);
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some(dataset) = self.session.datasets().get(self.active) else {
            return;
        };
        let id = dataset.id;
        let Some(source) = dataset.bam().map(|b| b.source().clone()) else {
            return;
        };
        // Fields, not bam_mut(), so that bam_drag can change alongside.
        let Some(view) = self.bams.get_mut(&id) else {
            return;
        };
        let contig = &source.contigs()[view.contig];
        let length = contig.length;
        // In a stack, the focused BAM's rows; a drag stays with the BAM
        // it started in, since only the focused BAM's rows are hit.
        let layout = match &stack {
            Some((members, layout)) => {
                let i = members.iter().position(|&m| m == id).unwrap_or(0);
                layout.member(i)
            }
            None => ui::bam::bam_layout(body, view),
        };
        let hit = ui::bam::bam_hit(&layout, view, contig, mouse.column, mouse.row);
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let sideways = layout.strip.contains(at) || layout.ruler.contains(at);
        match mouse.kind {
            MouseEventKind::ScrollDown if sideways => view.scroll(3, length),
            MouseEventKind::ScrollUp if sideways => view.scroll(-3, length),
            MouseEventKind::ScrollDown => view.scroll_rows(3),
            MouseEventKind::ScrollUp => view.scroll_rows(-3),
            MouseEventKind::Down(button) => {
                self.bam_drag = None;
                self.ruler_drag = None;
                match hit {
                    BamHit::Strip(pos) if button == MouseButton::Left => {
                        view.go(view.contig, pos, length);
                    }
                    BamHit::Ruler(pos) if button == MouseButton::Left => {
                        view.cursor = pos;
                        self.ruler_drag = view.col_of(pos).map(|c| view.span_of(c));
                    }
                    BamHit::Read { col, row } => {
                        view.clear_stretch();
                        let span = view.span_of(col);
                        if span.0 > length {
                            return;
                        }
                        let read = view.read_at(col, row).map(|i| ReadId::of(&view.reads()[i]));
                        let adding = shift || button == MouseButton::Right;
                        if button == MouseButton::Left {
                            view.cursor = span.0;
                            view.row = view.top + row;
                        }
                        match read.clone() {
                            Some(read) if adding => view.select(read),
                            _ if adding => {}
                            Some(read) => view.selected = vec![read],
                            None => view.selected.clear(),
                        }
                        if button == MouseButton::Left && !adding {
                            self.bam_drag = Some(BamDrag {
                                dataset: id,
                                anchor: span,
                                anchor_row: view.top + row,
                                anchor_read: read,
                                at: (col, row),
                                edge: (0, 0),
                                kept: Vec::new(),
                                scrolled: Instant::now(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.ruler_drag.is_some() => {
                let (Some((a1, b1)), BamHit::Ruler(pos)) = (self.ruler_drag, hit) else {
                    return;
                };
                let Some((a2, b2)) = view.col_of(pos).map(|c| view.span_of(c)) else {
                    return;
                };
                view.set_stretch(a1.min(a2), b1.max(b2), length);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let reads = layout.reads;
                let Some(drag) = self.bam_drag.as_mut().filter(|d| d.dataset == id) else {
                    return;
                };
                if reads.width == 0 || reads.height == 0 {
                    return;
                }
                // Sideways, the reads reach the screen's edges, past which
                // a terminal reports no pointer: their outer columns count
                // as past them. Above and below there are other rows.
                let side = |at: u16, low: u16, size: u16, rim: u16| -> (i8, usize) {
                    let high = low + size - 1;
                    match at {
                        _ if at < low + rim => (-1, usize::from(at.max(low) - low)),
                        _ if at + rim > high => (1, usize::from(at.min(high) - low)),
                        _ => (0, usize::from(at - low)),
                    }
                };
                let (dx, col) = side(mouse.column, reads.x, reads.width, 1);
                let (dy, row) = side(mouse.row, reads.y, reads.height, 0);
                drag.at = (col, row);
                drag.edge = (dx, dy);
                drag_select(view, drag);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.bam_drag = None;
                self.ruler_drag = None;
            }
            _ => {}
        }
    }

    /// Scroll the reads while a drag is held past their edge, at most once
    /// every [`DRAG_EVERY`], and select what it then covers. A drag whose
    /// BAM is no longer on screen ends.
    pub fn tick(&mut self, now: Instant) {
        let Some(drag) = &self.bam_drag else {
            return;
        };
        let shown = self.session.datasets().get(self.active).map(|d| d.id);
        if shown != Some(drag.dataset) {
            self.bam_drag = None;
            return;
        }
        if drag.edge == (0, 0) || now.duration_since(drag.scrolled) < DRAG_EVERY {
            return;
        }
        let id = drag.dataset;
        let Some(source) = self
            .session
            .datasets()
            .get(self.active)
            .and_then(|d| d.bam())
            .map(|b| b.source().clone())
        else {
            return;
        };
        // Fields, not bam_mut(), so that the drag can change alongside.
        let (Some(view), Some(drag)) = (self.bams.get_mut(&id), self.bam_drag.as_mut()) else {
            return;
        };
        let length = source.contigs()[view.contig].length;
        let (dx, dy) = drag.edge;
        if dx != 0 {
            view.scroll(i64::from(dx) * DRAG_COLS, length);
        }
        if dy != 0 {
            view.scroll_rows(isize::from(dy));
        }
        drag.scrolled = now;
        drag_select(view, drag);
    }

    /// `m`: to the mate of the read under the cursor.
    fn bam_mate(&mut self) {
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
        let Some(read) = view.cursor_read() else {
            self.status = "no read under the cursor".to_string();
            return;
        };
        let Some(mate) = read.mate else {
            self.status =
                "the read under the cursor has no mate on a contig of this BAM".to_string();
            return;
        };
        let name = read.name.clone();
        let length = source.contigs()[mate.contig].length;
        view.go(mate.contig, mate.pos, length);
        view.focus = Some((name, mate.pos));
        view.apply_focus(length);
    }

    /// `s`: to the next part of the read under the cursor, in order along
    /// the contigs, coming round to the first after the last.
    fn bam_part(&mut self) {
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
        let Some(read) = view.cursor_read() else {
            self.status = "no read under the cursor".to_string();
            return;
        };
        if read.parts.is_empty() {
            self.status = "the read under the cursor has no split parts".to_string();
            return;
        }
        let here = (read.contig, read.start);
        let mut places: Vec<(usize, u64)> = read
            .parts
            .iter()
            .map(|p| (p.place.contig, p.place.pos))
            .chain([here])
            .collect();
        places.sort_unstable();
        places.dedup();
        let at = places.iter().position(|&p| p == here).unwrap_or(0);
        let (contig, pos) = places[(at + 1) % places.len()];
        let name = read.name.clone();
        let length = source.contigs()[contig].length;
        view.go(contig, pos, length);
        view.focus = Some((name, pos));
        view.apply_focus(length);
    }

    /// `'`: back to where the last jump started.
    fn bam_back(&mut self) {
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
        if let Some(was) = view.parked_from.take() {
            view.over = std::mem::take(&mut view.parked);
            view.back.truncate(was.back);
            view.place(was.contig, was.cursor, source.contigs()[was.contig].length);
            view.focus = None;
            view.per_col = was.per_col;
            view.left = was.left;
            view.before_whole = was.before_whole;
            view.row = was.row;
            view.top = was.top;
            view.repack();
            return;
        }
        match view.back.pop() {
            Some((contig, pos)) => view.place(contig, pos, source.contigs()[contig].length),
            None => self.status = "nothing to go back to".to_string(),
        }
    }

    /// Take in a coverage pass's progress or its end, if it is the pass
    /// running.
    pub(super) fn pass_done(
        &mut self,
        dataset: u64,
        contig: usize,
        coverage: seqpeeq_core::Result<Coverage>,
        finished: bool,
    ) {
        let ours = self
            .passing
            .as_ref()
            .is_some_and(|(d, c, _)| (*d, *c) == (dataset, contig));
        if !ours {
            return;
        }
        if finished {
            self.passing = None;
        }
        match coverage {
            Ok(coverage) => {
                self.passes.insert((dataset, contig), coverage);
            }
            Err(Error::Cancelled) => {
                self.passes.remove(&(dataset, contig));
            }
            Err(e) => {
                self.passes.remove(&(dataset, contig));
                self.pass_failed.insert((dataset, contig));
                self.status = e.to_string();
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::tests::settle;
    use crate::bam_view::zoom_levels;
    use crate::worker::perform;
    use seqpeeq_core::bam::testing::{bam_from_sam, fixture, ref_bases};
    use seqpeeq_core::{BamCmd, Session};
    use tempfile::TempDir;

    fn open(app: &mut App, path: PathBuf) {
        app.enqueue(Command::Open { path, kind: None });
    }

    /// An app showing the fixture's `reads.bam`, everything read.
    pub(crate) fn with_bam() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let mut app = App::new(Session::new(None));
        open(&mut app, f.bam);
        settle(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        (app, dir)
    }

    pub(crate) fn id(app: &App) -> u64 {
        app.session.datasets()[app.active].id
    }

    /// Run the queued open, and hold back every other request.
    fn open_only(app: &mut App) -> Vec<Request> {
        let mut held = Vec::new();
        loop {
            app.pump();
            match app.outbox.pop() {
                None => break,
                Some(Request::Run(job)) => app.handle_done(Done::Command(job.run())),
                Some(other) => held.push(other),
            }
        }
        held
    }

    /// As `settle`, counting the windows asked for.
    fn settle_counting_windows(app: &mut App) -> usize {
        let mut windows = 0;
        loop {
            app.pump();
            let Some(request) = app.outbox.pop() else {
                break;
            };
            windows += usize::from(matches!(request, Request::Window { .. }));
            perform(request, &mut |done| app.handle_done(done));
        }
        windows
    }

    #[test]
    fn opening_a_bam_shows_its_first_contig_with_reads_whole() {
        let (app, _dir) = with_bam();
        let (_, _, view) = app.bam().unwrap();
        assert_eq!((view.contig, view.left, view.cursor), (0, 1, 1));
        assert!(view.width > 0 && view.height > 0);
        assert_eq!(view.per_col, *zoom_levels(1000, view.width).last().unwrap());
        assert_eq!(
            app.passes[&(id(&app), 0)].done_to,
            1000,
            "the pass ran for c1"
        );
        assert!(app.passing.is_none() && app.reading.is_none());
        let loaded = view.loaded.as_ref().unwrap();
        assert_eq!(loaded.window.count, 7);
        assert!(!view.packed.rows.is_empty());
        assert!(!app.wants_tick());
    }

    #[test]
    fn a_bam_opens_on_the_first_contig_with_reads() {
        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:e\tLN:100\n@SQ\tSN:c\tLN:100\n\
                   r\t0\tc\t5\t60\t10M\t*\t0\t0\t*\t*\n";
        let bam = bam_from_sam(dir.path(), "two.bam", sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        assert_eq!(app.bam().unwrap().2.contig, 1);
    }

    #[test]
    fn the_window_loads_around_the_view_at_base_level() {
        let (mut app, dir) = with_bam();
        let id = id(&app);
        let view = app.bams.get_mut(&id).unwrap();
        view.per_col = 1;
        view.place(0, 101, 1000);
        settle(&mut app);
        let view = &app.bams[&id];
        let loaded = view.loaded.as_ref().unwrap();
        assert_eq!(loaded.window.bin, 1);
        assert_eq!(loaded.window.region, view.wanted(1000, 1).unwrap().0);
        let names: Vec<&str> = view.reads().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["r1", "r2"]);
        assert_eq!(loaded.reference, None, "no reference attached");

        // Attaching a reference reads the window again, with it.
        app.enqueue(Command::Bam(BamCmd::Ref {
            file: dir.path().join("ref.fa"),
        }));
        settle(&mut app);
        let loaded = app.bams[&id].loaded.as_ref().unwrap();
        let region = loaded.window.region;
        assert_eq!(
            loaded.reference.as_deref(),
            Some(ref_bases("c1", region.start, region.end).as_bytes())
        );
    }

    #[test]
    fn a_window_older_than_the_view_is_dropped() {
        let (mut app, _dir) = with_bam();
        let id = id(&app);
        let generation = app.bams[&id].generation;
        let mut stale = app.bams[&id].loaded.clone().unwrap();
        stale.window.count = 99;
        app.handle_done(Done::Window {
            dataset: id,
            generation: generation - 1,
            result: Ok(stale),
        });
        assert_eq!(app.bams[&id].loaded.as_ref().unwrap().window.count, 7);
    }

    #[test]
    fn showing_another_contig_cancels_the_unfinished_pass() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let mut app = App::new(Session::new(None));
        open(&mut app, f.bam);
        let held = open_only(&mut app);
        let id = id(&app);
        let (_, contig, first) = app.passing.clone().unwrap();
        assert_eq!(contig, 0);
        assert!(app.wants_tick());

        app.bams.get_mut(&id).unwrap().place(1, 1, 500);
        app.pump();
        assert!(first.is_cancelled());
        assert_eq!(app.passing.as_ref().map(|p| p.1), Some(1));
        for request in held {
            perform(request, &mut |done| app.handle_done(done));
        }
        settle(&mut app);
        assert_eq!(app.passes[&(id, 1)].done_to, 500);
        assert!(
            !app.passes.contains_key(&(id, 0)),
            "the cancelled pass is not kept"
        );

        app.bams.get_mut(&id).unwrap().place(0, 1, 1000);
        app.pump();
        assert_eq!(app.passing.as_ref().map(|p| p.1), Some(0), "started again");
    }

    #[test]
    fn a_wide_view_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:big\tLN:1000000\n\
                   a\t0\tbig\t1\t60\t10M\t*\t0\t0\t*\t*\n\
                   b\t0\tbig\t500000\t60\t10M\t*\t0\t0\t*\t*\n\
                   c\t0\tbig\t999000\t60\t10M\t*\t0\t0\t*\t*\n";
        let bam = bam_from_sam(dir.path(), "big.bam", sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        assert_eq!(settle_counting_windows(&mut app), 0);
        let id = id(&app);
        assert!(app.bams[&id].loaded.is_none());
        assert_eq!(app.passes[&(id, 0)].done_to, 1_000_000);
    }

    #[test]
    fn a_window_that_fails_is_not_asked_for_again() {
        let (mut app, dir) = with_bam();
        let id = id(&app);
        std::fs::remove_file(dir.path().join("reads.bam")).unwrap();
        let view = app.bams.get_mut(&id).unwrap();
        view.per_col = 1;
        view.place(0, 101, 1000);
        assert_eq!(settle_counting_windows(&mut app), 1);
        assert!(app.status.contains("reads.bam"), "{}", app.status);
        assert_eq!(settle_counting_windows(&mut app), 0);
        assert!(app.reading.is_none());
    }

    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    pub(crate) fn press(app: &mut App, code: KeyCode) {
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn shift(app: &mut App, code: KeyCode) {
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT)));
    }

    /// Press each character of `keys`, then let the work finish.
    pub(crate) fn keys(app: &mut App, keys: &str) {
        for c in keys.chars() {
            press(app, KeyCode::Char(c));
        }
        settle(app);
    }

    /// `g`, `text`, Enter.
    fn go(app: &mut App, text: &str) {
        keys(app, &format!("g{text}"));
        press(app, KeyCode::Enter);
        settle(app);
    }

    fn view(app: &App) -> &BamView {
        app.bam().unwrap().2
    }

    /// The fixture at a base a column, cursor at `pos` on its contig.
    pub(crate) fn at_base_level(app: &mut App, contig: usize, pos: u64, row: usize) {
        let id = id(app);
        let length = app.bam().unwrap().1.source().contigs()[contig].length;
        let view = app.bams.get_mut(&id).unwrap();
        view.per_col = 1;
        view.place(contig, pos, length);
        settle(app);
        app.bams.get_mut(&id).unwrap().row = row;
    }

    #[test]
    fn keys_move_zoom_and_go() {
        let (mut app, _dir) = with_bam();
        go(&mut app, "c1:301");
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 301));
        let per_col = view(&app).per_col;
        keys(&mut app, "l");
        assert_eq!(view(&app).cursor, 301 + per_col);
        keys(&mut app, "+");
        assert!(view(&app).per_col < per_col, "zoomed in");
        let closer = view(&app).per_col;
        keys(&mut app, "-");
        assert_eq!(view(&app).per_col, per_col);
        keys(&mut app, "++");
        keys(&mut app, "z");
        assert_eq!(
            (view(&app).per_col, view(&app).left),
            (per_col, 1),
            "whole contig"
        );
        keys(&mut app, "z");
        assert!(view(&app).per_col < closer, "and back");

        at_base_level(&mut app, 0, 301, 0);
        let width = view(&app).width as u64;
        shift(&mut app, KeyCode::Right);
        assert_eq!(view(&app).cursor, 301 + width);
        shift(&mut app, KeyCode::Left);
        assert_eq!(view(&app).cursor, 301);

        go(&mut app, "c1:101");
        keys(&mut app, "j");
        assert_eq!(view(&app).row, 1);
        keys(&mut app, "jjj");
        assert_eq!(view(&app).row, 1, "no rows past the last");
        keys(&mut app, "k");
        assert_eq!(view(&app).row, 0);

        go(&mut app, "2,000");
        assert_eq!(app.status, "error: c1 is 1,000 bp; 2,000 is past its end");
        go(&mut app, "450");
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 450));
        go(&mut app, "c2");
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 1));
        go(&mut app, "c1:101-200");
        let v = view(&app);
        assert_eq!((v.contig, v.cursor), (0, 101));
        assert!(
            v.col_of(101).is_some() && v.col_of(200).is_some(),
            "the range fits"
        );
        assert!(v.per_col * (v.width as u64) < 1000, "zoomed in to it");
        go(&mut app, "nope");
        assert_eq!(app.status, "error: no contig named 'nope' in reads.bam");
    }

    #[test]
    fn jumps_walk_the_stretches_and_stop_at_the_end() {
        let (mut app, _dir) = with_bam();
        let mut walk = Vec::new();
        for _ in 0..4 {
            keys(&mut app, "]");
            walk.push(view(&app).cursor);
        }
        assert_eq!(walk, [101, 301, 701, 901]);
        keys(&mut app, "]");
        assert_eq!(app.status, "no more covered stretches on c1");
        assert_eq!(view(&app).cursor, 901);
        assert!(
            app.outbox.is_empty() && app.jumping.is_none(),
            "the pass answered"
        );

        go(&mut app, "101");
        keys(&mut app, "}");
        assert_eq!(view(&app).cursor, 166, "a deletion does not end a stretch");
        go(&mut app, "750");
        keys(&mut app, "[");
        assert_eq!(view(&app).cursor, 701);
        keys(&mut app, "'");
        assert_eq!(view(&app).cursor, 750);
        keys(&mut app, "{");
        assert_eq!(view(&app).cursor, 741);
        go(&mut app, "50");
        keys(&mut app, "[");
        assert_eq!(app.status, "no covered stretch before here on c1");
        go(&mut app, "1");
        keys(&mut app, "{");
        assert_eq!(app.status, "no stretch without coverage before here on c1");
        go(&mut app, "960");
        keys(&mut app, "}");
        assert_eq!(view(&app).cursor, 960, "already in the last empty stretch");
        assert_eq!(app.status, "no more stretches without coverage on c1");
    }

    #[test]
    fn a_jump_the_pass_cannot_answer_runs_in_the_background_and_esc_cancels_it() {
        let (mut app, _dir) = with_bam();
        let id = id(&app);
        app.passes.clear();
        app.pass_failed.insert((id, 0));
        press(&mut app, KeyCode::Char(']'));
        assert!(matches!(app.outbox.last(), Some(Request::Jump { .. })));
        let progress = app.jumping.as_ref().unwrap().progress.clone();
        assert!(app.wants_tick());
        press(&mut app, KeyCode::Esc);
        assert!(progress.is_cancelled());
        settle(&mut app);
        assert_eq!(app.status, "jump cancelled");
        assert_eq!(view(&app).cursor, 1);
        assert!(app.jumping.is_none());

        keys(&mut app, "]");
        assert_eq!(view(&app).cursor, 101, "read from the BAM");

        // A jump ending after the view moved to another contig is ignored.
        press(&mut app, KeyCode::Char(']'));
        assert!(app.jumping.is_some());
        app.bams.get_mut(&id).unwrap().place(1, 1, 500);
        settle(&mut app);
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 1));
        assert!(app.jumping.is_none());
    }

    #[test]
    fn m_and_s_follow_mates_and_split_parts() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 101, 0);
        keys(&mut app, "m");
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 301));
        assert_eq!(
            view(&app).cursor_read().map(|r| r.flags),
            Some(147),
            "the cursor is on the mate"
        );
        at_base_level(&mut app, 0, 901, 0);
        keys(&mut app, "m");
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 201));

        at_base_level(&mut app, 0, 121, 1);
        keys(&mut app, "s");
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 51));
        assert_eq!(view(&app).cursor_read().map(|r| r.flags), Some(2048));
        keys(&mut app, "s");
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 121), "and back");
        keys(&mut app, "''");
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 121));
        keys(&mut app, "'");
        assert_eq!(
            (view(&app).contig, view(&app).cursor),
            (0, 901),
            "where the second m started"
        );

        at_base_level(&mut app, 0, 701, 0);
        keys(&mut app, "m");
        assert_eq!(
            app.status,
            "the read under the cursor has no mate on a contig of this BAM"
        );
        keys(&mut app, "s");
        assert_eq!(app.status, "the read under the cursor has no split parts");
        at_base_level(&mut app, 0, 500, 0);
        keys(&mut app, "m");
        assert_eq!(app.status, "no read under the cursor");
        app.bams.get_mut(&id(&app)).unwrap().back.clear();
        keys(&mut app, "'");
        assert_eq!(app.status, "nothing to go back to");
    }

    #[test]
    fn f_and_s_toggle_filtered_reads_and_clips() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 701, 0);
        assert_eq!(view(&app).packed.rows.len(), 1);
        keys(&mut app, "F");
        assert_eq!(view(&app).packed.rows.len(), 3);
        keys(&mut app, "F");
        assert_eq!(view(&app).packed.rows.len(), 1);

        at_base_level(&mut app, 0, 101, 1);
        let v = view(&app);
        let col = v.col_of(111).unwrap();
        assert!(v.read_at(col, 1).is_some(), "r2's clip is drawn");
        keys(&mut app, "S");
        assert!(view(&app).read_at(col, 1).is_none());
    }

    fn ctrl(app: &mut App, c: char) {
        app.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )));
    }

    fn listed(app: &App) -> Vec<usize> {
        let Mode::Contigs(list) = &app.mode else {
            panic!("the contig list is not open")
        };
        list.rows(app.bam().unwrap().1.source().contigs())
    }

    #[test]
    fn c_opens_the_list_and_enter_goes_there() {
        let (mut app, _dir) = with_bam();
        keys(&mut app, "c");
        assert_eq!(listed(&app), [0, 1], "c3 has no reads");
        ctrl(&mut app, 'a');
        assert_eq!(listed(&app), [0, 1, 2]);
        ctrl(&mut app, 'a');
        press(&mut app, KeyCode::Tab);
        assert_eq!(listed(&app), [0, 1], "c1 has 7 reads, c2 3");
        keys(&mut app, "2");
        assert_eq!(listed(&app), [1]);
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        let v = view(&app);
        assert_eq!((v.contig, v.cursor, v.left), (1, 1, 1));
        assert_eq!(v.end(500), 500, "the whole contig");

        keys(&mut app, "c");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(view(&app).contig, 1, "Esc goes nowhere");
        keys(&mut app, "c");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(view(&app).contig, 1, "the cursor stops on the last row");
        keys(&mut app, "c");
        keys(&mut app, "zzz");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal), "nothing to go to");
        assert_eq!(view(&app).contig, 1);
    }

    use crate::bam_view::ReadId;
    use crate::ui::bam::{BamLayout, bam_layout};
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::{Rect, Size};

    fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16), shift: bool) {
        let modifiers = if shift {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        app.handle_event(&Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }));
    }

    fn click(app: &mut App, at: (u16, u16)) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), at, false);
        mouse(app, MouseEventKind::Up(MouseButton::Left), at, false);
    }

    /// The tab's layout on an 80 x 14 screen.
    fn layout_of(app: &mut App) -> BamLayout {
        app.resize(Size::new(80, 14));
        settle(app);
        app.status.clear();
        let body = crate::ui::regions(app, Rect::new(0, 0, 80, 14)).body;
        bam_layout(body, view(app))
    }

    fn id_of(name: &str, flags: u16, start: u64) -> ReadId {
        let end = match name {
            "r1" => 150,
            _ => 165,
        };
        ReadId {
            name: name.to_string(),
            flags,
            contig: 0,
            start,
            end,
        }
    }

    #[test]
    fn the_mouse_scrolls_and_clicks_on_the_strip_go_there() {
        let dir = tempfile::tempdir().unwrap();
        let mut sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:1000\n".to_string();
        for i in 0..30 {
            sam.push_str(&format!("r{i}\t0\tc\t1\t60\t5M\t*\t0\t0\t*\t*\n"));
        }
        let bam = bam_from_sam(dir.path(), "t.bam", &sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        layout_of(&mut app);
        at_base_level(&mut app, 0, 1, 0);
        let layout = layout_of(&mut app);

        let down = MouseEventKind::ScrollDown;
        mouse(&mut app, down, (5, layout.reads.y + 1), false);
        assert_eq!(view(&app).top, 3);
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            (5, layout.reads.y),
            false,
        );
        assert_eq!(view(&app).top, 0);
        for _ in 0..20 {
            mouse(&mut app, down, (5, layout.reads.y), false);
        }
        assert_eq!(
            view(&app).top,
            30 - view(&app).height,
            "no further than the last row"
        );

        mouse(&mut app, down, (5, layout.ruler.y), false);
        assert_eq!(view(&app).left, 4, "sideways over the ruler");
        mouse(&mut app, down, (5, layout.strip.y), false);
        assert_eq!(view(&app).left, 7, "and over the strip");
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            (5, layout.strip.y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            (5, layout.strip.y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            (5, layout.strip.y),
            false,
        );
        assert_eq!(view(&app).left, 1);

        let (_, doc, _) = app.bam().unwrap();
        let bar = crate::ui::bam::strip_bar(layout.strip, &doc.source().contigs()[0]);
        click(&mut app, (bar.x + bar.width / 2, bar.y));
        let cursor = view(&app).cursor;
        assert!((480..=520).contains(&cursor), "{cursor}");
        assert!(view(&app).col_of(cursor).is_some());
        assert_eq!(view(&app).back.last(), Some(&(0, 1)));
    }

    // A drag held past the edge of the reads.

    /// An app showing a BAM made from `reads` on a 5,000 bp contig `c`,
    /// at a base a column from position 1, on an 80 x 14 screen.
    fn with_reads(reads: &str) -> (App, TempDir, BamLayout) {
        let dir = tempfile::tempdir().unwrap();
        let sam = format!("@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:5000\n{reads}");
        let bam = seqpeeq_core::bam::testing::bam_from_sam(dir.path(), "d.bam", &sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        layout_of(&mut app);
        at_base_level(&mut app, 0, 1, 0);
        let layout = layout_of(&mut app);
        (app, dir, layout)
    }

    fn names_selected(app: &App) -> Vec<String> {
        let mut names: Vec<String> = view(app).selected.iter().map(|r| r.name.clone()).collect();
        names.sort();
        names
    }

    fn drag_edge(app: &App) -> Option<(i8, i8)> {
        app.bam_drag.as_ref().map(|d| d.edge)
    }

    #[test]
    fn a_drag_held_below_the_reads_scrolls_down_and_selects_on() {
        // Thirty overlapping reads, a row each.
        let reads: String = (0..30)
            .map(|i| format!("t{i:02}\t0\tc\t{}\t60\t100M\t*\t0\t0\t*\t*\n", i + 1))
            .collect();
        let (mut app, _dir, layout) = with_reads(&reads);
        let (x, y, height) = (layout.reads.x, layout.reads.y, layout.reads.height as usize);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (x + 40, y),
            false,
        );
        let below = (x + 40, layout.reads.bottom() + 1);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            below,
            false,
        );
        assert_eq!(drag_edge(&app), Some((0, 1)));
        assert_eq!(
            names_selected(&app).len(),
            height,
            "held to the last row shown"
        );
        assert!(app.wants_tick(), "so the loop wakes to scroll");

        let start = app.bam_drag.as_ref().unwrap().scrolled;
        app.tick(start + Duration::from_millis(10));
        assert_eq!(view(&app).top, 0, "not yet");
        for k in 1..=5 {
            app.tick(start + DRAG_EVERY * k);
        }
        assert_eq!(view(&app).top, 5);
        assert_eq!(names_selected(&app).len(), height + 5);
        assert!(names_selected(&app).contains(&format!("t{:02}", height + 4)));

        // Back inside: no more scrolling, and the rows above the pointer
        // that it left go.
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (x + 40, y + 1),
            false,
        );
        assert_eq!(drag_edge(&app), Some((0, 0)));
        app.tick(start + DRAG_EVERY * 10);
        assert_eq!(view(&app).top, 5);
        assert_eq!(names_selected(&app).len(), 7, "rows 0 to 6");

        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            (x + 40, y + 1),
            false,
        );
        assert!(app.bam_drag.is_none());
        assert!(!app.wants_tick());
    }

    #[test]
    fn a_drag_held_past_the_right_keeps_what_it_selected_as_windows_load() {
        // One row of short reads, one every 50 bp along the contig.
        let reads: String = (0..80)
            .map(|k| format!("h{k:02}\t0\tc\t{}\t60\t20M\t*\t0\t0\t*\t*\n", 50 * k + 1))
            .collect();
        let (mut app, _dir, layout) = with_reads(&reads);
        let (x, y) = (layout.reads.x, layout.reads.y);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (x, y),
            false,
        );
        let drag = MouseEventKind::Drag(MouseButton::Left);
        mouse(&mut app, drag, (layout.reads.right() - 2, y), false);
        assert_eq!(drag_edge(&app), Some((0, 0)));
        // A terminal reports no pointer past its window, so the reads'
        // outer column is their edge.
        mouse(&mut app, drag, (layout.reads.right() - 1, y), false);
        assert_eq!(drag_edge(&app), Some((1, 0)));
        mouse(&mut app, drag, (layout.reads.right() + 3, y), false);
        assert_eq!(drag_edge(&app), Some((1, 0)));
        assert_eq!(names_selected(&app), ["h00", "h01"]);

        let start = app.bam_drag.as_ref().unwrap().scrolled;
        for k in 1..=150 {
            app.tick(start + DRAG_EVERY * k);
            settle(&mut app);
        }
        assert_eq!(view(&app).left, 1 + 150 * DRAG_COLS as u64);
        let names = names_selected(&app);
        assert!(
            !view(&app).reads().iter().any(|r| r.name == "h00"),
            "h00 is no longer loaded"
        );
        assert!(names.contains(&"h00".to_string()), "but stays selected");
        // The pointer is on the last column, at 600 + 80.
        let want: Vec<String> = (0..14).map(|k| format!("h{k:02}")).collect();
        assert_eq!(names, want);

        press(&mut app, KeyCode::Esc);
        assert!(app.bam_drag.is_none(), "Esc lets go of the drag too");
    }

    #[test]
    fn clicks_and_drags_select_reads() {
        let (mut app, _dir) = with_bam();
        layout_of(&mut app);
        at_base_level(&mut app, 0, 101, 0);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().left = 101;
        let layout = layout_of(&mut app);
        let (y, x) = (layout.reads.y, layout.reads.x);
        let (r1, r2) = (id_of("r1", 99, 101), id_of("r2", 0, 121));

        click(&mut app, (x, y));
        assert_eq!(view(&app).selected, vec![r1.clone()]);
        assert_eq!((view(&app).cursor, view(&app).row), (101, 0));
        assert!(crate::ui::info_text(&app).0.contains("1 read selected"));
        click(&mut app, (x + 20, y + 1));
        assert_eq!(view(&app).selected, vec![r2.clone()]);
        assert_eq!((view(&app).cursor, view(&app).row), (121, 1));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (x, y),
            true,
        );
        assert_eq!(view(&app).selected, vec![r2.clone(), r1.clone()]);
        assert!(crate::ui::info_text(&app).0.contains("2 reads selected"));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            (x, y),
            false,
        );
        assert_eq!(view(&app).selected.len(), 2, "already selected");

        press(&mut app, KeyCode::Esc);
        assert!(view(&app).selected.is_empty());
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            (x + 20, y + 1),
            false,
        );
        assert_eq!(view(&app).selected, vec![r2.clone()]);

        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (x + 70, y),
            false,
        );
        assert!(view(&app).selected.is_empty(), "a click on nothing lets go");
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (x + 20, y + 1),
            false,
        );
        assert_eq!(
            view(&app).selected,
            [r1.clone(), r2.clone()],
            "both rows dragged over"
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            (x + 20, y + 1),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (x, y),
            false,
        );
        assert_eq!(view(&app).selected.len(), 2, "a drag ends with the button");

        press(&mut app, KeyCode::Esc);
        app.bams.get_mut(&id).unwrap().cursor = 101;
        app.bams.get_mut(&id).unwrap().row = 0;
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(view(&app).selected, vec![r1.clone()]);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(view(&app).selected, vec![r1], "added once");

        app.passes.clear();
        app.pass_failed.insert((id, 0));
        press(&mut app, KeyCode::Char(']'));
        let progress = app.jumping.as_ref().unwrap().progress.clone();
        press(&mut app, KeyCode::Esc);
        assert!(view(&app).selected.is_empty() && !progress.is_cancelled());
        press(&mut app, KeyCode::Esc);
        assert!(progress.is_cancelled(), "the second Esc cancels the jump");
    }

    fn line_of(app: &App) -> &str {
        match &app.mode {
            Mode::Prompt(prompt) => prompt.line.text(),
            _ => panic!("the command line is not open"),
        }
    }

    #[test]
    fn e_and_shift_e_open_the_command_line_filled_in() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 101, 0);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().left = 101;
        let reads = view(&app).reads();
        let r1 = ReadId::of(reads.iter().find(|r| r.name == "r1").unwrap());
        let r2 = ReadId::of(reads.iter().find(|r| r.name == "r2").unwrap());
        app.bams.get_mut(&id).unwrap().selected = vec![r1, r2];
        keys(&mut app, "e");
        assert_eq!(
            line_of(&app),
            "bam export-reads --region c1:101-165 --name r1 --name r2 --out "
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(view(&app).selected.is_empty());

        keys(&mut app, "e");
        let region = view(&app).view_region(1000);
        assert_eq!(region.start, 101);
        let r = format!("c1:{}-{}", region.start, region.end);
        assert_eq!(
            line_of(&app),
            format!("bam export-reads --region {r} --out ")
        );
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "E");
        assert_eq!(line_of(&app), format!("bam export-ref {r} --out "));
    }

    #[test]
    fn more_than_twenty_names_go_to_a_names_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let log = seqpeeq_core::SessionLog::new(dir.path().join("s.log"));
        let mut app = App::new(Session::new(Some(log)));
        open(&mut app, f.bam);
        settle(&mut app);
        let id = id(&app);
        let names: Vec<String> = (0..21).map(|i| format!("q{i}")).collect();
        app.bams.get_mut(&id).unwrap().selected = names
            .iter()
            .map(|name| ReadId {
                name: name.clone(),
                flags: 0,
                contig: 0,
                start: 11,
                end: 20,
            })
            .collect();
        keys(&mut app, "e");
        let file = dir.path().join("s.names-1.txt");
        assert_eq!(
            line_of(&app),
            format!(
                "bam export-reads --region c1:11-20 --names-file {} --out ",
                file.display()
            )
        );
        let listed = std::fs::read_to_string(&file).unwrap();
        assert_eq!(listed.lines().collect::<Vec<_>>(), names);
    }

    #[test]
    fn selected_reads_on_another_contig_are_left_out_and_said_so() {
        let (mut app, _dir) = with_bam();
        let id = id(&app);
        let read = |name: &str, contig, start, end| ReadId {
            name: name.to_string(),
            flags: 0,
            contig,
            start,
            end,
        };
        app.bams.get_mut(&id).unwrap().selected = vec![
            read("a", 0, 11, 20),
            read("b", 1, 51, 60),
            read("a", 0, 31, 40),
        ];
        keys(&mut app, "e");
        assert_eq!(
            line_of(&app),
            "bam export-reads --region c1:11-40 --name a --out "
        );
        assert!(
            app.status.contains("1 selected read on c2 left out"),
            "{}",
            app.status
        );
    }

    /// Pump and perform up to `turns` times; how many windows were asked.
    fn windows_in(app: &mut App, turns: usize) -> usize {
        let mut windows = 0;
        for _ in 0..turns {
            app.pump();
            while let Some(request) = app.outbox.pop() {
                windows += usize::from(matches!(request, Request::Window { .. }));
                perform(request, &mut |done| app.handle_done(done));
            }
        }
        windows
    }

    #[test]
    fn a_contig_the_reference_lacks_is_read_once() {
        let (mut app, dir) = with_bam();
        let only_c1 = dir.path().join("only_c1.fa");
        let c1 = seqpeeq_core::bam::testing::ref_bases("c1", 1, 1000);
        std::fs::write(&only_c1, format!(">c1\n{c1}\n")).unwrap();
        app.run_line(&format!("bam ref {}", only_c1.display()));
        settle(&mut app);
        let id = id(&app);
        let v = app.bams.get_mut(&id).unwrap();
        v.per_col = 1;
        v.place(1, 51, 500);
        let windows = windows_in(&mut app, 20);
        assert_eq!(windows, 1, "read again and again");
        assert!(app.reading.is_none());
        assert!(!view(&app).packed.rows.is_empty(), "its reads are drawn");
    }

    #[test]
    fn the_contig_list_closes_when_another_file_takes_the_tab() {
        let (mut app, dir) = with_bam();
        let one = bam_from_sam(
            dir.path(),
            "one.bam",
            "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:x\tLN:100\nq\t0\tx\t1\t60\t5M\t*\t0\t0\t*\t*\n",
        );
        keys(&mut app, "c");
        press(&mut app, KeyCode::Down);
        app.run_line(&format!("open {}", one.display()));
        settle(&mut app);
        assert_eq!(
            app.bam().unwrap().1.source().contigs().len(),
            1,
            "one.bam shows"
        );
        let backend = ratatui::backend::TestBackend::new(80, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(view(&app).contig, 0);
    }

    #[test]
    fn the_same_jump_pressed_twice_lands() {
        let (mut app, _dir) = with_bam();
        let id = id(&app);
        app.passes.clear();
        app.pass_failed.insert((id, 0));
        go(&mut app, "c1:50");
        press(&mut app, KeyCode::Char(']'));
        press(&mut app, KeyCode::Char(']'));
        let jumps: Vec<Request> = std::mem::take(&mut app.outbox)
            .into_iter()
            .filter(|r| matches!(r, Request::Jump { .. }))
            .collect();
        assert_eq!(jumps.len(), 2);
        for request in jumps {
            perform(request, &mut |done| app.handle_done(done));
        }
        assert_eq!(view(&app).cursor, 101, "{}", app.status);
        assert!(app.jumping.is_none());
        assert!(!app.status.contains("cancelled"), "{}", app.status);
    }

    #[test]
    fn m_puts_the_row_on_the_mate_after_the_window_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:5000\n\
                   a\t0\tc\t920\t60\t50M\t*\t0\t0\t*\t*\n\
                   b\t0\tc\t950\t60\t450M\t*\t0\t0\t*\t*\n\
                   p\t99\tc\t1000\t60\t50M\t=\t1100\t150\t*\t*\n\
                   p\t147\tc\t1100\t60\t50M\t=\t1000\t-150\t*\t*\n";
        let bam = bam_from_sam(dir.path(), "t.bam", sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        layout_of(&mut app);
        at_base_level(&mut app, 0, 1000, 0);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().left = 1000;
        settle(&mut app);
        let v = app.bams.get_mut(&id).unwrap();
        let reads = v.reads();
        let row = v
            .packed
            .rows
            .iter()
            .position(|row| {
                row.iter()
                    .any(|&i| reads[i].name == "p" && reads[i].start == 1000)
            })
            .unwrap();
        v.row = row;
        v.cursor = 1000;
        keys(&mut app, "m");
        settle(&mut app);
        let read = view(&app).cursor_read().map(|r| (r.name.clone(), r.start));
        assert_eq!(read, Some(("p".to_string(), 1100)));
    }

    #[test]
    #[ignore = "timing on testdata/; run with: cargo test --release -p seqpeeq -- --ignored timings_on"]
    fn timings_on_the_test_bams() {
        use std::time::Instant;
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(200, 50));
        let started = Instant::now();
        open(&mut app, data.join("deep.bam"));
        settle(&mut app);
        eprintln!(
            "deep: open and the strip (the coverage pass) {:?}",
            started.elapsed()
        );
        app.run_line(&format!("bam ref {}", data.join("deep.fa").display()));
        settle(&mut app);
        let started = Instant::now();
        at_base_level(&mut app, 0, 2_000_000, 0);
        eprintln!(
            "deep: a window at base level {:?}, {} rows",
            started.elapsed(),
            view(&app).packed.rows.len()
        );
        assert!(!view(&app).packed.rows.is_empty());
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().cursor = 1;
        app.passes.clear();
        app.pass_failed.insert((id, 0));
        let started = Instant::now();
        press(&mut app, KeyCode::Char('}'));
        settle(&mut app);
        eprintln!(
            "deep: }} from 1, read from the BAM, {:?}",
            started.elapsed()
        );
        let read_from_the_bam = view(&app).cursor;
        app.pass_failed.clear();
        settle(&mut app);
        app.bams.get_mut(&id).unwrap().cursor = 1;
        let started = Instant::now();
        press(&mut app, KeyCode::Char('}'));
        settle(&mut app);
        eprintln!("deep: }} from 1, from the pass, {:?}", started.elapsed());
        assert_eq!(view(&app).cursor, read_from_the_bam);
        // Reads were left out of 4,000,001-4,000,500, so coverage runs
        // out a little before it.
        assert!((3_999_850..=4_000_001).contains(&read_from_the_bam));

        let mut app = App::new(Session::new(None));
        app.resize(Size::new(200, 50));
        let started = Instant::now();
        open(&mut app, data.join("assembly.bam"));
        settle(&mut app);
        eprintln!("assembly: open and the strip {:?}", started.elapsed());
        let started = Instant::now();
        keys(&mut app, "c");
        keys(&mut app, "12");
        eprintln!(
            "assembly: c and a filter {:?}, {} shown",
            started.elapsed(),
            listed(&app).len()
        );
    }

    #[test]
    #[ignore = "timing on testdata/; run with: cargo test --release -p seqpeeq -- --ignored timings_on"]
    fn timings_on_a_search() {
        use std::time::Instant;
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(200, 50));
        open(&mut app, data.join("deep.bam"));
        settle(&mut app);
        at_base_level(&mut app, 0, 2_000_000, 0);
        let name = view(&app).reads()[0].name.clone();
        let started = Instant::now();
        app.bam_search(vec![name.clone()]);
        settle(&mut app);
        let s = &app.searches[&id(&app)];
        assert!(s.searched.contains(&name), "{}", app.status);
        eprintln!(
            "deep: a search for one name {:?}, {} records found",
            started.elapsed(),
            s.found.get(&name).map_or(0, Vec::len)
        );
        let started = Instant::now();
        press(&mut app, KeyCode::Char('A'));
        settle(&mut app);
        eprintln!(
            "deep: A at base level {:?}, {}",
            started.elapsed(),
            match view(&app).over.last() {
                Some(Over::List(l)) => format!("{} groups", l.groups.len()),
                _ => app.status.clone(),
            }
        );
    }

    // The read-as-axis view and the reads aligning elsewhere (phase 5).

    use crate::bam_view::Over;
    use crate::elsewhere::ElsewhereList;
    use crate::read_axis::{AxisView, Found, Kind};

    // Tab in the `g` line.

    /// An app showing a BAM whose contigs are named for completing.
    fn with_names() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut sam = "@HD\tVN:1.6\tSO:coordinate\n".to_string();
        for name in ["chr1", "chr2", "chr10", "HLA:01", "HLA:02", "scaffold_9"] {
            sam.push_str(&format!("@SQ\tSN:{name}\tLN:100\n"));
        }
        for (i, name) in ["chr1", "chr10", "HLA:02"].iter().enumerate() {
            sam.push_str(&format!("r{i}\t0\t{name}\t1\t60\t10M\t*\t0\t0\t*\t*\n"));
        }
        let bam = seqpeeq_core::bam::testing::bam_from_sam(dir.path(), "n.bam", &sam);
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        (app, dir)
    }

    fn line(app: &App) -> &str {
        match &app.mode {
            Mode::Prompt(p) if matches!(p.kind, PromptKind::Goto) => p.line.text(),
            _ => panic!("the g line is not open"),
        }
    }

    fn names(app: &App) -> Vec<String> {
        let contigs = app.bam().unwrap().1.source().contigs();
        listed(app)
            .iter()
            .map(|&i| contigs[i].name.clone())
            .collect()
    }

    #[test]
    fn tab_in_g_finishes_a_contig_name() {
        let (mut app, _dir) = with_names();
        keys(&mut app, "g sc ");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "scaffold_9", "the only one, spaces aside");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(view(&app).contig, 5);

        keys(&mut app, "gCH");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "chr", "as far as they agree, in their case");
        keys(&mut app, "1");
        press(&mut app, KeyCode::Tab);
        assert_eq!(names(&app), ["chr1", "chr10"], "both still start so");
        press(&mut app, KeyCode::Esc);
        assert_eq!(line(&app), "chr1");
        keys(&mut app, ":5");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "chr1:5", "nothing to finish after the :");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!((view(&app).contig, view(&app).cursor), (0, 5));
    }

    #[test]
    fn tab_again_picks_from_the_contig_list() {
        let (mut app, _dir) = with_names();
        keys(&mut app, "gchr");
        press(&mut app, KeyCode::Tab);
        assert_eq!(names(&app), ["chr1", "chr10"], "chr2 has no reads");
        press(&mut app, KeyCode::Esc);
        assert_eq!(line(&app), "chr", "Esc goes back to the line as it was");

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(line(&app), "chr10", "the pick goes on the line");
        keys(&mut app, ":7");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!((view(&app).contig, view(&app).cursor), (2, 7));
    }

    #[test]
    fn names_with_a_colon_are_braced() {
        let (mut app, _dir) = with_names();
        keys(&mut app, "gH");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "{HLA:0");
        keys(&mut app, "2");
        press(&mut app, KeyCode::Tab);
        assert_eq!(line(&app), "{HLA:02}");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(view(&app).contig, 4);
        assert_eq!(view(&app).left, 1, "the whole contig");

        keys(&mut app, "gzz");
        press(&mut app, KeyCode::Tab);
        assert!(names(&app).is_empty(), "nothing holds zz");
        press(&mut app, KeyCode::Esc);
        assert_eq!(line(&app), "zz");
    }

    #[test]
    fn prefixes_are_compared_in_any_case() {
        assert!(starts_with_any_case("chrX", "CHRx"));
        assert!(!starts_with_any_case("chr", "chr1"));
        assert_eq!(common_prefix("chr1", &["CHR10"]), "chr1");
        assert_eq!(common_prefix("chr10", &["chr1"]), "chr1");
        assert_eq!(common_prefix("ab", &["xy"]), "");
        assert_eq!(common_prefix("é1", &["É2"]), "é");
        // The Kelvin sign is a K of three bytes; ẞ is ß's capital.
        assert_eq!(common_prefix("kx", &["\u{212A}"]), "k");
        assert_eq!(common_prefix("\u{212A}x", &["k"]), "\u{212A}");
        assert_eq!(common_prefix("ßx", &["ẞ"]), "ß");
        assert_eq!(common_prefix("chr", &["chr"]), "chr");
        assert_eq!(quoted("HLA:01"), "{HLA:01}");
    }

    /// An app showing `multi.bam`, everything read.
    pub(crate) fn with_multi() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let bam = seqpeeq_core::bam::testing::multi(dir.path());
        let mut app = App::new(Session::new(None));
        open(&mut app, bam);
        settle(&mut app);
        assert!(app.status.starts_with("opened "), "{}", app.status);
        (app, dir)
    }

    fn top(app: &App) -> Option<&Over> {
        view(app).over.last()
    }

    fn axis(app: &App) -> &AxisView {
        match top(app) {
            Some(Over::Axis(a)) => a,
            other => panic!("not the axis view: {other:?}"),
        }
    }

    fn list(app: &App) -> &ElsewhereList {
        match top(app) {
            Some(Over::List(l)) => l,
            other => panic!("not the list: {other:?}"),
        }
    }

    fn requests(app: &mut App) -> Vec<Request> {
        std::mem::take(&mut app.outbox)
    }

    fn deliver(app: &mut App, held: Vec<Request>) {
        for request in held {
            perform(request, &mut |done| app.handle_done(done));
        }
        settle(app);
    }

    #[test]
    fn a_opens_the_axis_view_and_the_mate_fills_in() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 101, 0);
        press(&mut app, KeyCode::Char('a'));
        let held = requests(&mut app);
        assert_eq!(held.len(), 1);
        assert!(matches!(&held[0], Request::Gather { read, .. } if read.name == "x1"));
        assert_eq!(axis(&app).name, "x1");
        assert_eq!(axis(&app).segments.len(), 1, "read 2 waits for the gather");
        assert!(app.wants_tick());
        deliver(&mut app, held);
        let segs = &axis(&app).segments;
        assert_eq!(segs.len(), 2);
        let p = &segs[1].placements[0];
        assert_eq!(
            (segs[1].number, p.kind, p.contig, p.pos),
            (2, Kind::Primary, 0, 401)
        );
        assert!(app.gathering.is_empty() && !app.wants_tick());

        press(&mut app, KeyCode::Esc);
        assert!(view(&app).over.is_empty());
        at_base_level(&mut app, 0, 500, 0);
        keys(&mut app, "a");
        assert_eq!(app.status, "no read under the cursor");
        assert!(view(&app).over.is_empty());
    }

    #[test]
    fn capital_a_lists_and_enter_opens_a_read_and_esc_steps_back() {
        let (mut app, _dir) = with_multi();
        let before = (view(&app).contig, view(&app).left, view(&app).per_col);
        keys(&mut app, "A");
        assert_eq!(list(&app).groups.len(), 3);
        assert_eq!(list(&app).rows(), [0, 1, 2]);
        keys(&mut app, "/p");
        assert_eq!(list(&app).rows(), [1], "typing filters");
        press(&mut app, KeyCode::Esc);
        assert_eq!(list(&app).rows(), [0, 1, 2], "Esc clears the filter");
        assert!(matches!(top(&app), Some(Over::List(_))));
        press(&mut app, KeyCode::Enter);
        assert_eq!(list(&app).open, Some(0));
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(axis(&app).name, "x1");
        assert_eq!(view(&app).over.len(), 2);
        press(&mut app, KeyCode::Esc);
        assert_eq!(list(&app).open, Some(0));
        press(&mut app, KeyCode::Esc);
        assert_eq!(list(&app).open, None);
        press(&mut app, KeyCode::Esc);
        assert!(view(&app).over.is_empty());
        assert_eq!(
            (view(&app).contig, view(&app).left, view(&app).per_col),
            before
        );

        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:big\tLN:1000000\n\
                   a\t0\tbig\t1\t60\t10M\t*\t0\t0\t*\t*\n";
        let mut app = App::new(Session::new(None));
        open(&mut app, bam_from_sam(dir.path(), "big.bam", sam));
        settle(&mut app);
        keys(&mut app, "A");
        assert_eq!(app.status, "zoom in to list the reads in view");
        assert!(view(&app).over.is_empty());
    }

    #[test]
    fn s_searches_once_for_every_name_in_question() {
        let (mut app, _dir) = with_multi();
        keys(&mut app, "A");
        press(&mut app, KeyCode::Char('S'));
        let held = requests(&mut app);
        assert_eq!(held.len(), 1);
        assert!(matches!(&held[0], Request::Search { names, .. } if names == &["y1"]));
        assert!(app.wants_tick());
        deliver(&mut app, held);
        let groups = &list(&app).groups;
        assert!(groups.iter().all(|g| g.contig.is_some()), "no ? group");
        assert_eq!(groups[0].reads.len(), 3);
        assert_eq!(groups[0].contig, Some(1));
        assert!(!app.wants_tick());

        press(&mut app, KeyCode::Char('S'));
        assert!(requests(&mut app).is_empty());
        assert_eq!(app.status, "nothing left to search");

        press(&mut app, KeyCode::Esc);
        at_base_level(&mut app, 0, 201, 0);
        keys(&mut app, "a");
        assert_eq!(axis(&app).name, "y1");
        assert!(axis(&app).searched);
        let found: Vec<Found> = axis(&app).segments[0]
            .placements
            .iter()
            .map(|p| p.found)
            .collect();
        assert_eq!(found, [Found::Record, Found::Search, Found::Search]);
        press(&mut app, KeyCode::Char('S'));
        assert!(requests(&mut app).is_empty());
    }

    #[test]
    fn asking_to_search_while_one_runs_waits_for_it() {
        let (mut app, _dir) = with_multi();
        app.bam_search(vec!["y1".to_string()]);
        let first = requests(&mut app);
        app.bam_search(vec!["y1".to_string(), "p1".to_string()]);
        assert!(requests(&mut app).is_empty());
        assert_eq!(app.searches[&id(&app)].waiting, ["p1"]);
        deliver(&mut app, first);
        let s = &app.searches[&id(&app)];
        assert!(s.searched.contains("y1") && s.searched.contains("p1"));
        assert_eq!(s.found["p1"].len(), 2, "the queued search ran after");
    }

    #[test]
    fn esc_cancels_a_search_before_stepping_back() {
        let (mut app, _dir) = with_multi();
        keys(&mut app, "A");
        press(&mut app, KeyCode::Char('S'));
        let held = requests(&mut app);
        let Some(Request::Search { progress, .. }) = held.first() else {
            panic!("no search");
        };
        let progress = progress.clone();
        press(&mut app, KeyCode::Esc);
        assert!(progress.is_cancelled());
        assert_eq!(app.status, "search cancelled");
        assert!(matches!(top(&app), Some(Over::List(_))), "the list stays");
        deliver(&mut app, held);
        assert_eq!(
            list(&app).pending_names(),
            ["y1"],
            "the late end changes nothing"
        );
        assert!(app.searches[&id(&app)].searched.is_empty());
        press(&mut app, KeyCode::Char('S'));
        assert_eq!(requests(&mut app).len(), 1, "a new search");
    }

    #[test]
    fn g_from_the_axis_goes_there_and_quote_comes_back() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 101, 0);
        keys(&mut app, "a");
        press(&mut app, KeyCode::Down);
        let picked = axis(&app).current().map(|(_, p)| (p.contig, p.pos));
        assert_eq!(picked, Some((1, 301)));
        keys(&mut app, "g");
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 301));
        assert_eq!(
            view(&app).focus,
            None,
            "an XA entry has no record to follow"
        );
        assert!(view(&app).over.is_empty());
        assert!(!view(&app).parked.is_empty());
        keys(&mut app, "'");
        assert_eq!(axis(&app).picked, 1, "the axis view, as it was");
        assert_eq!(
            (view(&app).contig, view(&app).cursor),
            (0, 101),
            "and the reference view where it was opened"
        );
        assert!(view(&app).parked.is_empty());
    }

    #[test]
    fn e_fills_the_everywhere_export() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 101, 0);
        keys(&mut app, "ae");
        assert_eq!(
            line_of(&app),
            "bam export-reads --name x1 --everywhere --out "
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(view(&app).over.is_empty());
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.status, "the reads in view are still being read");
        assert!(view(&app).over.is_empty());
        settle(&mut app);
        keys(&mut app, "Ae");
        assert_eq!(
            line_of(&app),
            "bam export-reads --name x1 --name z1 --everywhere --out "
        );
    }

    #[test]
    fn a_search_or_gather_for_a_closed_bam_is_ignored() {
        let (mut app, _dir) = with_multi();
        let multi = id(&app);
        app.bam_search(vec!["y1".to_string()]);
        let held = requests(&mut app);
        app.run_line("close");
        settle(&mut app);
        assert!(app.session.datasets().is_empty());
        deliver(&mut app, held);
        assert!(!app.searches.contains_key(&multi));

        let (mut app, dir) = with_multi();
        let multi = id(&app);
        open(&mut app, fixture(dir.path()).bam);
        settle(&mut app);
        assert_ne!(id(&app), multi);
        press(&mut app, KeyCode::Tab);
        assert_eq!(id(&app), multi);
        at_base_level(&mut app, 0, 101, 0);
        press(&mut app, KeyCode::Char('a'));
        let held = requests(&mut app);
        press(&mut app, KeyCode::Tab);
        assert_ne!(id(&app), multi);
        deliver(&mut app, held);
        let Some(Over::Axis(a)) = app.bams[&multi].over.last() else {
            panic!("no axis view");
        };
        assert_eq!(a.segments.len(), 2, "the mate arrived");
    }

    // Fixes from the phase 5 review.

    #[test]
    fn the_mouse_leaves_the_reference_view_alone_under_a_view() {
        let (mut app, _dir) = with_multi();
        keys(&mut app, "A");
        let before = {
            let v = view(&app);
            (
                v.contig,
                v.left,
                v.cursor,
                v.row,
                v.top,
                v.selected.clone(),
                v.back.len(),
            )
        };
        let layout = layout_of(&mut app);
        let (x, y) = (layout.reads.x + 3, layout.reads.y);
        mouse(&mut app, MouseEventKind::ScrollDown, (x, y), false);
        assert_eq!(list(&app).cursor, 1, "the wheel moves through the list");
        click(&mut app, (x, y));
        click(&mut app, (layout.strip.x + 40, layout.strip.y));
        settle(&mut app);
        press(&mut app, KeyCode::Esc);
        let v = view(&app);
        assert_eq!(
            (
                v.contig,
                v.left,
                v.cursor,
                v.row,
                v.top,
                v.selected.clone(),
                v.back.len()
            ),
            before
        );
    }

    #[test]
    fn g_from_the_axis_puts_the_row_on_the_read() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 1001, 0);
        keys(&mut app, "a");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(
            view(&app).focus,
            Some(("z1".to_string(), 1501)),
            "the row follows the read once it is read, as m does"
        );
        settle(&mut app);
        assert_eq!((view(&app).contig, view(&app).cursor), (1, 1501));
        assert_eq!(view(&app).cursor_read().map(|r| r.flags), Some(2048));
    }

    #[test]
    fn a_says_only_what_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:1000\n\
                   r\t0\tc\t5\t60\t10M\t*\t0\t0\t*\t*\n";
        let mut app = App::new(Session::new(None));
        open(&mut app, bam_from_sam(dir.path(), "plain.bam", sam));
        settle(&mut app);
        keys(&mut app, "A");
        assert_eq!(app.status, "no read in view is known to align elsewhere");
    }

    #[test]
    fn searching_for_a_crowd_of_names_is_quick() {
        let (mut app, _dir) = with_multi();
        let names: Vec<String> = (0..20_000).map(|i| format!("read{i}")).collect();
        let started = std::time::Instant::now();
        app.bam_search(names.clone());
        app.bam_search(
            names
                .iter()
                .rev()
                .cloned()
                .chain(["more".to_string()])
                .collect(),
        );
        assert!(
            started.elapsed().as_millis() < 500,
            "{:?}",
            started.elapsed()
        );
        assert_eq!(app.searches[&id(&app)].waiting, ["more"]);
    }

    #[test]
    fn quote_after_g_puts_the_reference_view_back_as_it_was() {
        let (mut app, _dir) = with_multi();
        at_base_level(&mut app, 0, 101, 0);
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().left = 81;
        settle(&mut app);
        let before = {
            let v = view(&app);
            (v.contig, v.left, v.per_col, v.cursor, v.row, v.top)
        };
        keys(&mut app, "a");
        press(&mut app, KeyCode::Down);
        keys(&mut app, "g");
        assert_eq!(view(&app).contig, 1);
        keys(&mut app, "'");
        let v = view(&app);
        assert_eq!(
            (v.contig, v.left, v.per_col, v.cursor, v.row, v.top),
            before
        );
    }

    // A stretch of reference.

    fn right(app: &mut App, n: usize) {
        for _ in 0..n {
            press(app, KeyCode::Right);
        }
    }

    #[test]
    fn v_twice_marks_a_stretch_and_e_and_capital_e_export_it() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 113, 0);
        keys(&mut app, "v");
        assert_eq!(view(&app).stretch(), Some((113, 113)));
        right(&mut app, 18);
        assert_eq!(view(&app).stretch(), Some((113, 131)), "follows the cursor");
        keys(&mut app, "v");
        assert_eq!(view(&app).marked, Some((113, 131)));
        assert_eq!(view(&app).marking, None);
        keys(&mut app, "E");
        assert_eq!(line_of(&app), "bam export-ref c1:113-131 --out ");
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "e");
        assert_eq!(line_of(&app), "bam export-reads --region c1:113-131 --out ");
    }

    #[test]
    fn v_backwards_orders_the_ends_and_v_v_in_place_is_one_base() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 131, 0);
        keys(&mut app, "v");
        for _ in 0..18 {
            press(&mut app, KeyCode::Left);
        }
        keys(&mut app, "v");
        assert_eq!(view(&app).marked, Some((113, 131)));
        at_base_level(&mut app, 0, 140, 0);
        keys(&mut app, "vv");
        assert_eq!(view(&app).marked, Some((140, 140)));
    }

    #[test]
    fn dragging_the_column_numbers_marks_a_stretch_and_a_click_does_not() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 101, 0);
        let layout = layout_of(&mut app);
        let y = layout.ruler.y;
        let at = |pos: u64| layout.ruler.x + view(&app).col_of(pos).unwrap() as u16;
        let (from, to) = (at(113), at(131));
        click(&mut app, (from, y));
        assert_eq!(view(&app).stretch(), None);
        assert_eq!(view(&app).cursor, 113);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (from, y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (to, y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            (to, y),
            false,
        );
        assert_eq!(view(&app).marked, Some((113, 131)));
    }

    #[test]
    fn a_stretch_and_selected_reads_replace_each_other() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 101, 0);
        let layout = layout_of(&mut app);
        keys(&mut app, " ");
        assert_eq!(view(&app).selected.len(), 1);
        keys(&mut app, "vv");
        assert!(view(&app).selected.is_empty());
        assert!(view(&app).marked.is_some());
        let x = layout.reads.x + view(&app).col_of(105).unwrap() as u16;
        click(&mut app, (x, layout.reads.y));
        assert_eq!(view(&app).stretch(), None);
        assert_eq!(view(&app).selected.len(), 1);
        keys(&mut app, "v");
        assert!(view(&app).marking.is_some());
        assert!(view(&app).selected.is_empty());
        keys(&mut app, " ");
        assert_eq!(view(&app).stretch(), None);
        assert_eq!(view(&app).selected.len(), 1);
    }

    #[test]
    fn esc_clears_the_stretch() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 113, 0);
        keys(&mut app, "vv");
        press(&mut app, KeyCode::Esc);
        assert_eq!(view(&app).stretch(), None);
        keys(&mut app, "v");
        press(&mut app, KeyCode::Esc);
        assert_eq!(view(&app).stretch(), None);
    }

    #[test]
    fn going_to_another_contig_clears_the_stretch() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 113, 0);
        keys(&mut app, "vv");
        let id = id(&app);
        app.bams.get_mut(&id).unwrap().place(1, 1, 500);
        assert_eq!(view(&app).stretch(), None);
        at_base_level(&mut app, 0, 113, 0);
        keys(&mut app, "v");
        app.bams.get_mut(&id).unwrap().place(1, 1, 500);
        assert_eq!(view(&app).stretch(), None);
    }

    #[test]
    fn a_stretch_dragged_past_the_end_stops_at_the_contig_end() {
        let dir = tempfile::tempdir().unwrap();
        let sam = "@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:c\tLN:997\n\
                   r\t0\tc\t5\t60\t10M\t*\t0\t0\t*\t*\n";
        let mut app = App::new(Session::new(None));
        open(&mut app, bam_from_sam(dir.path(), "ragged.bam", sam));
        settle(&mut app);
        let layout = layout_of(&mut app);
        let id = id(&app);
        let v = app.bams.get_mut(&id).unwrap();
        v.per_col = 10;
        v.place(0, 997, 997);
        v.left = 701;
        settle(&mut app);
        let last = view(&app).col_of(997).unwrap();
        assert_eq!(
            view(&app).span_of(last).1,
            1000,
            "the last column runs past the end"
        );
        let last = last as u16;
        let (x, y) = (layout.ruler.x, layout.ruler.y);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (x + last - 2, y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (x + last, y),
            false,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (x + last + 3, y),
            false,
        );
        let (a, b) = view(&app).marked.unwrap();
        assert_eq!(b, 997);
        assert!(a >= 1 && a < b);
        let buf = crate::ui::tests::render(&app, 80, 14);
        let cell = &buf.backend().buffer()[(x + last + 1, y)];
        assert_ne!(cell.bg, crate::palette::STRETCH.bg.unwrap());
    }

    #[test]
    fn the_stretch_stays_put_when_zooming() {
        let (mut app, _dir) = with_bam();
        at_base_level(&mut app, 0, 113, 0);
        keys(&mut app, "v");
        right(&mut app, 18);
        keys(&mut app, "v");
        keys(&mut app, "-");
        settle(&mut app);
        keys(&mut app, "+");
        assert_eq!(view(&app).marked, Some((113, 131)));
    }

    // The mouse in the contig list.

    fn contig_list(app: &mut App) -> Rect {
        app.resize(Size::new(80, 14));
        settle(app);
        keys(app, "c");
        assert!(matches!(app.mode, Mode::Contigs(_)));
        crate::ui::regions(app, Rect::new(0, 0, 80, 14)).body
    }

    fn the_list(app: &App) -> &ContigList {
        match &app.mode {
            Mode::Contigs(list) => list,
            _ => panic!("the contig list is closed"),
        }
    }

    #[test]
    fn the_wheel_moves_through_the_contig_list() {
        let (mut app, _dir) = with_multi();
        let body = contig_list(&mut app);
        let at = (body.x + 5, body.y + 3);
        mouse(&mut app, MouseEventKind::ScrollDown, at, false);
        mouse(&mut app, MouseEventKind::ScrollDown, at, false);
        assert_eq!(the_list(&app).cursor, 2);
        mouse(&mut app, MouseEventKind::ScrollUp, at, false);
        assert_eq!(the_list(&app).cursor, 1);
    }

    #[test]
    fn a_click_on_a_contig_goes_there() {
        let (mut app, _dir) = with_multi();
        let body = contig_list(&mut app);
        click(&mut app, (body.x + 5, body.y + 3 + 1));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(view(&app).contig, 1);
    }

    #[test]
    fn a_click_on_the_heading_switches_the_sort() {
        let (mut app, _dir) = with_multi();
        let body = contig_list(&mut app);
        click(&mut app, (body.x + 5, body.y + 2));
        assert_eq!(the_list(&app).sort, ContigSort::Reads);
    }

    #[test]
    fn a_click_outside_the_list_closes_it_and_inside_does_nothing() {
        let (mut app, _dir) = with_multi();
        let body = contig_list(&mut app);
        click(&mut app, (body.x + 5, body.y + 3 + 5));
        assert!(matches!(app.mode, Mode::Contigs(_)), "an empty row");
        click(&mut app, (5, 13));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(view(&app).contig, 0);
    }

    #[test]
    fn the_mouse_in_an_empty_contig_list_does_nothing() {
        let (mut app, _dir) = with_multi();
        let body = contig_list(&mut app);
        keys(&mut app, "zzz");
        let at = (body.x + 5, body.y + 3);
        mouse(&mut app, MouseEventKind::ScrollDown, at, false);
        click(&mut app, at);
        assert_eq!(the_list(&app).cursor, 0);
        assert_eq!(view(&app).contig, 0);
    }

    #[test]
    fn under_the_read_axis_views_the_bottom_row_says_esc_back() {
        let (mut app, _dir) = with_multi();
        app.resize(Size::new(80, 14));
        at_base_level(&mut app, 0, 1001, 0);
        keys(&mut app, "vv");
        keys(&mut app, "a");
        app.status.clear();
        let screen = crate::ui::tests::rows(&crate::ui::tests::render(&app, 80, 14));
        assert_eq!(screen[12], "Esc back", "{screen:#?}");
        press(&mut app, KeyCode::Esc);
        keys(&mut app, " ");
        keys(&mut app, "A");
        app.status.clear();
        let screen = crate::ui::tests::rows(&crate::ui::tests::render(&app, 80, 14));
        assert_eq!(screen[12], "Esc back", "{screen:#?}");
    }

    // A stack of BAMs.

    /// An app showing `first` and then `second` (opened in that order, so
    /// the second is focused), everything read.
    fn stack_of(dir: &TempDir, first: PathBuf, second: PathBuf) -> App {
        let _ = dir;
        let mut app = App::new(Session::new(None));
        open(&mut app, first);
        open(&mut app, second);
        settle(&mut app);
        assert!(app.status.contains("stacked with"), "{}", app.status);
        app
    }

    /// The fixture's reads.bam and a copy of it, b.bam, stacked.
    fn two_stacked() -> (App, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let copy = dir.path().join("b.bam");
        std::fs::copy(&f.bam, &copy).unwrap();
        let app = stack_of(&dir, f.bam, copy);
        (app, dir)
    }

    /// The views of the open BAMs, in file order.
    fn views(app: &App) -> Vec<&BamView> {
        let datasets = app.session.datasets();
        datasets.iter().map(|d| &app.bams[&d.id]).collect()
    }

    /// Where a view is: its contig's name, left edge, zoom and cursor.
    fn place_of(app: &App, index: usize) -> (String, u64, u64, u64) {
        let dataset = &app.session.datasets()[index];
        let view = &app.bams[&dataset.id];
        let contigs = dataset.bam().unwrap().source().contigs();
        let name = contigs[view.contig].name.clone();
        (name, view.left, view.per_col, view.cursor)
    }

    /// The datasets the windows in `requests` are for, in order.
    fn window_ids(requests: &[Request]) -> Vec<u64> {
        requests
            .iter()
            .filter_map(|r| match r {
                Request::Window { dataset, .. } => Some(*dataset),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn moving_the_focused_bam_moves_the_stack() {
        let (mut app, _dir) = two_stacked();
        assert_eq!(app.active, 1);
        go(&mut app, "c2:301");
        right(&mut app, 5);
        keys(&mut app, "-");
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
        assert_eq!(place_of(&app, 1).0, "c2");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.active, 0);
        keys(&mut app, "+");
        assert_eq!(place_of(&app, 1), place_of(&app, 0));
    }

    #[test]
    fn members_that_number_contigs_differently_land_on_the_same_contig() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture(dir.path());
        let reordered = seqpeeq_core::bam::testing::header_bam(
            dir.path(),
            "b.bam",
            &[("c3", 300), ("c1", 1000), ("c2", 500)],
        );
        let mut app = stack_of(&dir, f.bam, reordered);
        go(&mut app, "c2:301");
        assert_eq!((views(&app)[0].contig, views(&app)[1].contig), (1, 2));
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
        go(&mut app, "c1:10");
        assert_eq!(place_of(&app, 0).0, "c1");
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
        keys(&mut app, "'");
        assert_eq!(place_of(&app, 0).0, "c2");
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
    }

    #[test]
    fn every_member_reads_its_window_the_focused_first() {
        let (mut app, _dir) = two_stacked();
        let (a, b) = (app.session.datasets()[0].id, id(&app));
        // Somewhere new at a base a column, nothing read there yet.
        let length = app.bam().unwrap().1.source().contigs()[1].length;
        let focused = app.bams.get_mut(&b).unwrap();
        focused.per_col = 1;
        focused.place(1, 301, length);
        app.pump();
        let held = requests(&mut app);
        assert_eq!(window_ids(&held), [b]);
        for request in held {
            perform(request, &mut |done| app.handle_done(done));
        }
        app.pump();
        let held = requests(&mut app);
        assert_eq!(window_ids(&held), [a]);
        deliver(&mut app, held);
        assert!(views(&app).iter().all(|v| v.loaded.is_some()));
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
    }

    #[test]
    fn closing_the_focused_member_while_it_reads_leaves_the_other_alone() {
        let (mut app, _dir) = two_stacked();
        let a = app.session.datasets()[0].id;
        let b = id(&app);
        app.bams.get_mut(&b).unwrap().place(2, 101, 300);
        app.pump();
        assert_eq!(window_ids(&requests(&mut app)), [b]);
        app.enqueue(Command::Close);
        app.pump();
        assert_eq!(app.active, 0);
        assert_eq!(crate::ui::tab_labels(&app), ["reads.bam"]);
        assert_eq!(window_ids(&requests(&mut app)), [a]);
    }

    #[test]
    fn tab_away_from_a_members_axis_view_and_back_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = seqpeeq_core::bam::testing::multi(dir.path());
        let copy = dir.path().join("multi2.bam");
        std::fs::copy(&first, &copy).unwrap();
        let mut app = stack_of(&dir, first, copy);
        at_base_level(&mut app, 0, 101, 0);
        press(&mut app, KeyCode::Char('a'));
        let held = requests(&mut app);
        deliver(&mut app, held);
        assert_eq!(view(&app).over.len(), 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.active, 0);
        assert!(view(&app).over.is_empty());
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.active, 1);
        assert_eq!(view(&app).over.len(), 1);
    }

    #[test]
    fn clicks_focus_a_member_and_the_wheel_scrolls_the_one_under_it() {
        let (mut app, _dir) = two_stacked();
        let (a, b) = (app.session.datasets()[0].id, app.session.datasets()[1].id);
        app.active = 0;
        // Short enough that b.bam has one row for its two rows of reads.
        app.resize(Size::new(80, 13));
        settle(&mut app);
        at_base_level(&mut app, 0, 101, 0);
        app.status.clear();
        let body = app.bam_body();
        let (members, layout) = app.stack_layouts(body).remove(0);
        assert_eq!(members, [a, b]);
        let second = layout.members[1].reads;
        assert_eq!(second.height, 1);

        let row = app.bams[&b].row;
        mouse(&mut app, MouseEventKind::ScrollDown, (5, second.y), false);
        assert_eq!((app.bams[&b].top, app.bams[&b].row), (1, row));
        assert_eq!(app.bams[&a].top, 0);
        assert_eq!(app.active, 0);

        click(&mut app, (5, second.y));
        assert_eq!(app.active, 1);
        let column_5 = app.bams[&b].left + 5;
        assert_eq!((app.bams[&b].cursor, app.bams[&b].row), (column_5, 1));

        let left = app.bams[&a].left;
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            (5, layout.ruler.y),
            false,
        );
        settle(&mut app);
        assert!(app.bams[&b].left > left);
        assert_eq!(app.bams[&a].left, app.bams[&b].left);

        let left = app.bams[&a].left;
        click(&mut app, (79, layout.strip.y));
        settle(&mut app);
        assert_ne!(app.bams[&b].left, left);
        assert_eq!(app.bams[&a].left, app.bams[&b].left);
    }

    #[test]
    fn a_bam_joining_a_stack_takes_its_place_and_keeps_the_way_back() {
        let (mut app, dir) = with_bam();
        go(&mut app, "c2:301");
        go(&mut app, "c3:101");
        let before = place_of(&app, 0);
        let back = view(&app).back.clone();
        assert_eq!(before.0, "c3");
        assert_eq!(back.len(), 2);
        let copy = dir.path().join("b.bam");
        std::fs::copy(app.session.datasets()[0].path.clone(), &copy).unwrap();
        open(&mut app, copy);
        settle(&mut app);
        assert!(app.status.contains("stacked with"), "{}", app.status);
        assert_eq!(place_of(&app, 0), before);
        assert_eq!(place_of(&app, 1), before);
        assert_eq!(views(&app)[0].back, back);
        assert_eq!(views(&app)[1].back, back);
    }

    #[test]
    #[ignore = "timing on testdata/; run with: cargo test --release -p seqpeeq -- --ignored timings_on"]
    fn timings_on_a_stack_turn() {
        use std::time::Instant;
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let dir = tempfile::tempdir().unwrap();
        for ext in ["bam", "bam.bai"] {
            std::fs::copy(
                data.join(format!("deep.{ext}")),
                dir.path().join(format!("b.{ext}")),
            )
            .unwrap();
        }
        let mut app = App::new(Session::new(None));
        app.resize(Size::new(200, 50));
        open(&mut app, data.join("deep.bam"));
        open(&mut app, dir.path().join("b.bam"));
        settle(&mut app);
        assert!(app.status.contains("stacked with"), "{}", app.status);
        // As wide a window as still shows reads: 200 columns of 100 bases.
        let id = id(&app);
        let length = app.bam().unwrap().1.source().contigs()[0].length;
        let v = app.bams.get_mut(&id).unwrap();
        v.per_col = 100;
        v.place(0, 2_000_000, length);
        settle(&mut app);
        let reads = view(&app).reads().len();
        assert!(reads > 10_000, "{reads}");
        let turns = 200;
        let started = Instant::now();
        for _ in 0..turns {
            app.pump();
        }
        let each = started.elapsed() / turns;
        eprintln!("deep stacked: a turn with {reads} reads in the window {each:?}");
        assert!(each < std::time::Duration::from_micros(500), "{each:?}");
    }

    #[test]
    fn moves_then_a_change_of_focus_before_the_next_turn_are_kept() {
        let (mut app, _dir) = two_stacked();
        at_base_level(&mut app, 0, 101, 0);
        let cursor = view(&app).cursor;
        // Keys arriving together are handled before the next turn.
        for _ in 0..30 {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::NONE,
            )));
        }
        app.handle_event(&Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        settle(&mut app);
        assert_eq!(place_of(&app, 0).3, cursor + 30);
        assert_eq!(place_of(&app, 0), place_of(&app, 1));

        // And the same with a press on the other BAM's rows.
        let body = app.bam_body();
        let (_, layout) = app.stack_layouts(body).remove(0);
        let other = layout.members[1].name;
        assert_eq!(app.active, 0);
        for _ in 0..10 {
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::NONE,
            )));
        }
        click(&mut app, (0, other.y));
        settle(&mut app);
        assert_eq!(app.active, 1);
        assert_eq!(place_of(&app, 1).3, cursor + 40);
        assert_eq!(place_of(&app, 0), place_of(&app, 1));
    }

    #[test]
    fn every_member_gets_its_coverage_pass_the_shown_first() {
        let (mut app, _dir) = two_stacked();
        let (a, b) = (app.session.datasets()[0].id, id(&app));
        let contig = view(&app).contig;
        let done = |app: &App, id: u64| {
            let length = app.bam().unwrap().1.source().contigs()[contig].length;
            app.passes
                .get(&(id, contig))
                .is_some_and(|c| c.done_to >= length)
        };
        assert!(done(&app, b) && done(&app, a));

        // Another contig: the shown BAM's pass is asked first, then the other.
        go(&mut app, "c2:1");
        app.passes.clear();
        app.pump();
        let passes = |held: &[Request]| -> Vec<u64> {
            held.iter()
                .filter_map(|r| match r {
                    Request::Pass { dataset, .. } => Some(*dataset),
                    _ => None,
                })
                .collect()
        };
        let held = requests(&mut app);
        assert_eq!(passes(&held), [b]);
        for request in held {
            perform(request, &mut |done| app.handle_done(done));
        }
        app.pump();
        let held = requests(&mut app);
        assert_eq!(passes(&held), [a]);
        deliver(&mut app, held);
    }
}
