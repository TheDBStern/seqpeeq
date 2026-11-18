//! The BAM tab's side of the app: one view per open BAM, and the reading
//! it hands to worker threads.

use super::*;
use crate::bam_view::{pass_bin, serves, zoom_levels};

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
    #[allow(dead_code)] // drawn from in the next task
    pub fn bam(&self) -> Option<(&Dataset, &BamDoc, &BamView)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset, dataset.bam()?, self.bams.get(&dataset.id)?))
    }

    /// Columns across and rows of reads a BAM view has on screen.
    pub(super) fn bam_area(&self) -> (usize, usize) {
        let screen = ratatui::layout::Rect::new(0, 0, self.screen.width, self.screen.height);
        let body = ui::regions(self, screen).body;
        // The strip, ruler, reference, coverage and details rows.
        (body.width as usize, body.height.saturating_sub(5) as usize)
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
        let (width, height) = self.bam_area();
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
                BamView::new(first, contigs[first].length, width)
            });
            let length = contigs[view.contig].length;
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
}
