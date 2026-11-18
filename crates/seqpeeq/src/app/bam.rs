//! The BAM tab's side of the app: one view per open BAM, and the reading
//! it hands to worker threads.

use super::*;
use crate::bam_view::{ContigList, ContigSort, pass_bin, serves, zoom_levels};
use seqpeeq_core::bam::parse_region;

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
    pub(super) fn sync_bams(&mut self) {
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
        let body = self.bam_body();
        for dataset in self.session.datasets() {
            let Some(doc) = dataset.bam().filter(|_| open.contains(&dataset.id)) else {
                continue;
            };
            let contigs = doc.source().contigs();
            let view = self.bams.entry(dataset.id).or_insert_with(|| {
                let first = contigs
                    .iter()
                    .position(|c| c.mapped.is_some_and(|m| m > 0))
                    .unwrap_or(0);
                BamView::new(first, contigs[first].length, body.width as usize)
            });
            let length = contigs[view.contig].length;
            let reads = ui::bam::bam_layout(body, view).reads;
            let (width, height) = (reads.width as usize, reads.height as usize);
            if (view.width, view.height) != (width, height) {
                view.width = width;
                view.height = height;
                let whole = *zoom_levels(length, width).last().expect("a level");
                view.per_col = view.per_col.min(whole);
                view.reveal(length);
            }
        }

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
        let contig = view.contig;
        let length = source.contigs()[contig].length;

        let done = self
            .passes
            .get(&(id, contig))
            .is_some_and(|c| c.done_to >= length);
        let running = self
            .passing
            .as_ref()
            .is_some_and(|(d, c, _)| (*d, *c) == (id, contig));
        if !done && !running && !self.pass_failed.contains(&(id, contig)) {
            if let Some((d, c, progress)) = self.passing.take() {
                progress.cancel();
                // Unfinished, so not kept: it starts again when shown.
                self.passes.remove(&(d, c));
            }
            let progress = Arc::new(Progress::new());
            self.outbox.push(Request::Pass {
                dataset: id,
                contig,
                source: source.clone(),
                progress: progress.clone(),
            });
            self.passing = Some((id, contig, progress));
        }

        let Some(want) = view.wanted(length, pass_bin(length)) else {
            if let Some(reading) = self.reading.take_if(|r| r.dataset == id) {
                reading.progress.cancel();
            }
            return;
        };
        let shown = view.view_region(length);
        let with_reference = source.reference().is_some();
        // Reads read before a reference was attached are read again with it.
        let complete = |reads: bool, had: bool| !reads || !with_reference || had;
        let loaded = view.loaded.as_ref().is_some_and(|l| {
            let have = (l.window.region, l.window.bin, l.asked_reads);
            serves(&have, &shown, &want) && complete(l.asked_reads, l.reference.is_some())
        });
        let asked = self.reading.as_ref().is_some_and(|r| {
            r.dataset == id
                && serves(&r.want, &shown, &want)
                && complete(r.want.2, r.with_reference)
        });
        let failed = self.window_failed == Some((id, want));
        if loaded || asked || failed {
            return;
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
                view.apply_focus();
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
    fn bam_mut(&mut self) -> Option<(BamSource, &mut BamView)> {
        let dataset = self.session.datasets().get(self.active)?;
        let (id, source) = (dataset.id, dataset.bam()?.source().clone());
        Some((source, self.bams.get_mut(&id)?))
    }

    /// A key on the BAM tab.
    pub(super) fn bam_key(&mut self, key: &KeyEvent) {
        use Direction::{Back, Forward};
        use Stretch::{Covered, Empty};
        match key.code {
            KeyCode::Char('g') => return self.open_prompt(PromptKind::Goto, ""),
            KeyCode::Char(']') => return self.bam_jump(Forward, Covered),
            KeyCode::Char('[') => return self.bam_jump(Back, Covered),
            KeyCode::Char('}') => return self.bam_jump(Forward, Empty),
            KeyCode::Char('{') => return self.bam_jump(Back, Empty),
            KeyCode::Char('m') => return self.bam_mate(),
            KeyCode::Char('s') => return self.bam_part(),
            KeyCode::Char('\'') => return self.bam_back(),
            KeyCode::Char('c') => {
                if let Some((_, doc, _)) = self.bam() {
                    let list = ContigList::new(doc.source().contigs());
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
            _ => {}
        }
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
        let named = contigs.iter().any(|c| c.name == text);
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
    ) {
        if self
            .jumping
            .take_if(|j| (j.dataset, j.asked) == (dataset, asked))
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

    /// A key while the contig list is open.
    pub(super) fn contigs_key(&mut self, key: &KeyEvent) {
        let Some(source) = self.bam().map(|(_, doc, _)| doc.source().clone()) else {
            self.mode = Mode::Normal;
            return;
        };
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
                self.mode = Mode::Normal;
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
        view.go(mate.contig, mate.pos, source.contigs()[mate.contig].length);
        view.focus = Some((name, mate.pos));
        view.apply_focus();
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
        view.go(contig, pos, source.contigs()[contig].length);
        view.focus = Some((name, pos));
        view.apply_focus();
    }

    /// `'`: back to where the last jump started.
    fn bam_back(&mut self) {
        let Some((source, view)) = self.bam_mut() else {
            return;
        };
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
        stale.generation = generation - 1;
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

    fn press(app: &mut App, code: KeyCode) {
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn shift(app: &mut App, code: KeyCode) {
        app.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT)));
    }

    /// Press each character of `keys`, then let the work finish.
    fn keys(app: &mut App, keys: &str) {
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
    fn at_base_level(app: &mut App, contig: usize, pos: u64, row: usize) {
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
}
