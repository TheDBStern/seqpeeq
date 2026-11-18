//! The read-as-axis view and the list of reads aligning elsewhere, opened
//! over a BAM tab's reference view, and the gathers and searches that fill
//! them.

use super::*;
use crate::bam_view::{Over, READS_WIDTH};
use crate::elsewhere::ElsewhereList;
use crate::read_axis::{AxisView, Found, Kind, assemble};

/// More names than this go to a names file beside the log.
const NAMES_ON_THE_LINE: usize = 20;

/// What one BAM has been searched for this session.
#[derive(Debug, Default)]
pub struct Searches {
    /// Every record found, by name.
    pub found: HashMap<String, Vec<Read>>,
    /// Every name searched for, found or not.
    pub searched: HashSet<String>,
    /// Names asked for while a search ran, for the next one.
    pub waiting: Vec<String>,
    /// The search running: its names and progress.
    pub running: Option<(Vec<String>, Arc<Progress>)>,
}

impl Searches {
    fn found_for(&self, name: &str) -> Vec<&Read> {
        self.found
            .get(name)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

/// A word for the command line, quoted if it needs to be.
fn word(w: &str) -> String {
    seqpeeq_core::command::quote(w).map_or(w.to_string(), |q| q.into_owned())
}

impl App {
    /// The visible BAM's id and source.
    fn visible_bam(&self) -> Option<(u64, BamSource)> {
        let dataset = self.session.datasets().get(self.active)?;
        Some((dataset.id, dataset.bam()?.source().clone()))
    }

    /// Whether keys go to the filter of the list on the visible tab.
    pub(super) fn typing_filter(&self) -> bool {
        matches!(
            self.bam().and_then(|(_, _, v)| v.over.last()),
            Some(Over::List(list)) if list.typing
        )
    }

    /// `a`: the read under the cursor along its own bases.
    pub(super) fn bam_axis(&mut self) {
        let Some(read) = self.bam().and_then(|(_, _, v)| v.cursor_read().cloned()) else {
            self.status = "no read under the cursor".to_string();
            return;
        };
        self.open_axis(read);
    }

    /// Open the axis view of `read` on top, and fetch its mate and parts.
    fn open_axis(&mut self, read: Read) {
        let Some((id, source)) = self.visible_bam() else {
            return;
        };
        let none = Searches::default();
        let searches = self.searches.get(&id).unwrap_or(&none);
        let Some(view) = self.bams.get_mut(&id) else {
            return;
        };
        if view.over.is_empty() {
            view.parked.clear();
            view.parked_from = None;
        }
        let segments = assemble(&[&read], &searches.found_for(&read.name));
        let mut axis = AxisView::new(&read.name, segments, view.width);
        axis.records = vec![read.clone()];
        axis.searched = searches.searched.contains(&read.name);
        view.over.push(Over::Axis(axis));
        if read.mate.is_some() || !read.parts.is_empty() {
            let progress = Arc::new(Progress::new());
            self.gathering
                .push((id, read.name.clone(), progress.clone()));
            self.outbox.push(Request::Gather {
                dataset: id,
                read,
                source,
                progress,
            });
        }
    }

    /// `A`: the reads in view that align elsewhere.
    pub(super) fn bam_elsewhere(&mut self) {
        let Some((id, doc, view)) = self.bam().map(|(d, doc, v)| (d.id, doc, v)) else {
            return;
        };
        let length = doc.source().contigs()[view.contig].length;
        let shown = view.view_region(length);
        let wide = (view.width as u64).saturating_mul(view.per_col) >= READS_WIDTH;
        let loaded = view
            .loaded
            .as_ref()
            .filter(|l| !wide && l.window.reads.is_some());
        let Some(loaded) = loaded else {
            self.status = "zoom in to list the reads in view".to_string();
            return;
        };
        let window = loaded.window.region;
        let covers =
            window.contig == shown.contig && window.start <= shown.start && window.end >= shown.end;
        if !covers {
            self.status = "the reads in view are still being read".to_string();
            return;
        }
        let reads: Vec<Read> = view
            .reads()
            .iter()
            .filter(|r| r.contig == shown.contig && r.start <= shown.end && r.end >= shown.start)
            .cloned()
            .collect();
        let none = Searches::default();
        let s = self.searches.get(&id).unwrap_or(&none);
        let list = ElsewhereList::build(reads, shown, window, &s.found, &s.searched);
        if list.groups.is_empty() {
            self.status = "no read in view is known to align elsewhere".to_string();
            return;
        }
        let view = self.bams.get_mut(&id).expect("the view just read");
        if view.over.is_empty() {
            view.parked.clear();
            view.parked_from = None;
        }
        view.over.push(Over::List(list));
    }

    /// Esc on a BAM tab with views open over it: cancel a search running
    /// for this BAM, else close the open group or the top view. Whether it
    /// did anything.
    pub(super) fn bam_esc(&mut self) -> bool {
        let Some((id, _)) = self.visible_bam() else {
            return false;
        };
        if self.bams.get(&id).is_none_or(|v| v.over.is_empty()) {
            return false;
        }
        if self.cancel_search() {
            return true;
        }
        let view = self.bams.get_mut(&id).expect("just found");
        match view.over.last_mut() {
            Some(Over::List(list)) if list.open.is_some() => {
                let open = list.open.take();
                list.cursor = list
                    .rows()
                    .iter()
                    .position(|&i| Some(i) == open)
                    .unwrap_or(0);
                list.top = 0;
            }
            _ => {
                view.over.pop();
            }
        }
        true
    }

    /// Stop the search running for the visible BAM, if there is one.
    pub(super) fn cancel_search(&mut self) -> bool {
        let Some((id, _)) = self.visible_bam() else {
            return false;
        };
        let Some(s) = self.searches.get_mut(&id) else {
            return false;
        };
        let Some((_, progress)) = s.running.take() else {
            return false;
        };
        progress.cancel();
        s.waiting.clear();
        self.status = "search cancelled".to_string();
        true
    }

    /// A key while the list is on top.
    pub(super) fn list_key(&mut self, key: &KeyEvent) {
        let Some((id, _)) = self.visible_bam() else {
            return;
        };
        let Some(view) = self.bams.get_mut(&id) else {
            return;
        };
        let page = view.height.max(1);
        let Some(Over::List(list)) = view.over.last_mut() else {
            return;
        };
        if list.typing {
            match key.code {
                KeyCode::Enter => list.typing = false,
                KeyCode::Esc => {
                    list.filter.set("");
                    list.typing = false;
                }
                _ => {
                    if list.filter.key(key) {
                        list.cursor = 0;
                        list.top = 0;
                    }
                }
            }
            return;
        }
        let rows = list.rows();
        match key.code {
            KeyCode::Char('/') => list.typing = true,
            KeyCode::Up | KeyCode::Char('k') => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => list.cursor += 1,
            KeyCode::PageUp => list.cursor = list.cursor.saturating_sub(page),
            KeyCode::PageDown => list.cursor += page,
            KeyCode::Enter => match (list.open, rows.get(list.cursor)) {
                (None, Some(&group)) => {
                    list.open = Some(group);
                    list.cursor = 0;
                    list.top = 0;
                }
                (Some(group), Some(&i)) => {
                    let read = list.read(&list.groups[group].reads[i]).cloned();
                    match read {
                        Some(read) => self.open_axis(read),
                        None => self.status = "that read is no longer loaded".to_string(),
                    }
                    return;
                }
                _ => {}
            },
            KeyCode::Char('S') => {
                let names = list.pending_names();
                return self.bam_search(names);
            }
            KeyCode::Char('e') => {
                let names = list.names();
                return self.export_everywhere(&names);
            }
            _ => {}
        }
        let shown = list.rows().len();
        list.cursor = list.cursor.min(shown.saturating_sub(1));
        if list.cursor < list.top {
            list.top = list.cursor;
        } else if list.cursor >= list.top + page {
            list.top = list.cursor + 1 - page;
        }
    }

    /// A key while the axis view is on top.
    pub(super) fn axis_key(&mut self, key: &KeyEvent) {
        let Some((id, source)) = self.visible_bam() else {
            return;
        };
        let Some(view) = self.bams.get_mut(&id) else {
            return;
        };
        let Some(Over::Axis(axis)) = view.over.last_mut() else {
            return;
        };
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = axis.height.max(1) as isize;
        let across = axis.width.max(1) as i64;
        match key.code {
            KeyCode::Up if shift => axis.pick(-page),
            KeyCode::Down if shift => axis.pick(page),
            KeyCode::PageUp => axis.pick(-page),
            KeyCode::PageDown => axis.pick(page),
            KeyCode::Up | KeyCode::Char('k') => axis.pick(-1),
            KeyCode::Down | KeyCode::Char('j') => axis.pick(1),
            KeyCode::Left if shift => axis.scroll(-across),
            KeyCode::Right if shift => axis.scroll(across),
            KeyCode::Left | KeyCode::Char('h') => axis.scroll(-1),
            KeyCode::Right | KeyCode::Char('l') => axis.scroll(1),
            KeyCode::Home => axis.scroll(i64::MIN / 2),
            KeyCode::End => axis.scroll(i64::MAX / 2),
            KeyCode::Char('+' | '=') => axis.zoom('+'),
            KeyCode::Char('-') => axis.zoom('-'),
            KeyCode::Char('z') => axis.zoom('z'),
            KeyCode::Char('g') => {
                let Some((_, p)) = axis.current() else {
                    return;
                };
                let (contig, pos) = (p.contig, p.pos);
                // An XA entry or an unmapped record has no read there to
                // put the row on.
                let follow = !matches!(p.found, Found::Xa) && p.kind != Kind::Unmapped;
                let name = axis.name.clone();
                let Some(length) = source.contigs().get(contig).map(|c| c.length) else {
                    return;
                };
                view.parked_from = Some(crate::bam_view::Parked {
                    contig: view.contig,
                    left: view.left,
                    per_col: view.per_col,
                    cursor: view.cursor,
                    row: view.row,
                    top: view.top,
                    before_whole: view.before_whole,
                    back: view.back.len(),
                });
                view.parked = std::mem::take(&mut view.over);
                view.go(contig, pos, length);
                if follow {
                    view.focus = Some((name, pos));
                    view.apply_focus(length);
                }
            }
            KeyCode::Char('e') => {
                let names = vec![axis.name.clone()];
                self.export_everywhere(&names);
            }
            KeyCode::Char('S') => {
                let names = vec![axis.name.clone()];
                self.bam_search(names);
            }
            _ => {}
        }
    }

    /// Open the command line holding the export of every record of
    /// `names`, ready for the file name.
    fn export_everywhere(&mut self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let mut line = "bam export-reads".to_string();
        if names.len() > NAMES_ON_THE_LINE {
            match self.session.names_file(names) {
                Ok(Some(file)) => {
                    line.push_str(&format!(" --names-file {}", word(&file.to_string_lossy())))
                }
                Ok(None) => {
                    for name in names {
                        line.push_str(&format!(" --name {}", word(name)));
                    }
                }
                Err(e) => {
                    self.status = format!("error: {e}");
                    return;
                }
            }
        } else {
            for name in names {
                line.push_str(&format!(" --name {}", word(name)));
            }
        }
        line.push_str(" --everywhere --out ");
        self.open_prompt(PromptKind::Command, &line);
    }

    /// `S`: search the visible BAM for `names`, leaving out those already
    /// searched for or asked; while a search runs they wait for the next.
    pub(super) fn bam_search(&mut self, names: Vec<String>) {
        let Some((id, source)) = self.visible_bam() else {
            return;
        };
        let s = self.searches.entry(id).or_default();
        let mut asked: HashSet<String> = s.waiting.iter().cloned().collect();
        if let Some((running, _)) = &s.running {
            asked.extend(running.iter().cloned());
        }
        let mut new: Vec<String> = Vec::new();
        for name in names {
            if !s.searched.contains(&name) && asked.insert(name.clone()) {
                new.push(name);
            }
        }
        if new.is_empty() {
            self.status = "nothing left to search".to_string();
            return;
        }
        if s.running.is_some() {
            let n = new.len();
            s.waiting.extend(new);
            self.status = format!(
                "{n} more name{} to search when this search ends",
                if n == 1 { "" } else { "s" }
            );
            return;
        }
        self.start_search(id, source, new);
    }

    fn start_search(&mut self, id: u64, source: BamSource, names: Vec<String>) {
        let progress = Arc::new(Progress::new());
        let n = names.len();
        self.status = format!(
            "searching {} for {n} name{}…",
            source.file_name(),
            if n == 1 { "" } else { "s" }
        );
        self.searches.entry(id).or_default().running = Some((names.clone(), progress.clone()));
        self.outbox.push(Request::Search {
            dataset: id,
            names,
            source,
            progress,
        });
    }

    /// Take in a read's mate and parts, for every axis view of its name on
    /// that BAM.
    pub(super) fn gather_done(
        &mut self,
        dataset: u64,
        name: &str,
        result: seqpeeq_core::Result<Vec<Read>>,
    ) {
        if let Some(i) = self
            .gathering
            .iter()
            .position(|(d, n, _)| *d == dataset && n == name)
        {
            self.gathering.remove(i);
        }
        let records = match result {
            Ok(records) => records,
            Err(Error::Cancelled) => return,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        };
        let Some(view) = self.bams.get_mut(&dataset) else {
            return;
        };
        for over in view.over.iter_mut().chain(view.parked.iter_mut()) {
            if let Over::Axis(axis) = over
                && axis.name == name
            {
                for r in &records {
                    let key = (r.flags, r.contig, r.start);
                    if !axis
                        .records
                        .iter()
                        .any(|h| (h.flags, h.contig, h.start) == key)
                    {
                        axis.records.push(r.clone());
                    }
                }
            }
        }
        self.refresh_overs(dataset);
    }

    /// Take in a search's records, if it is the search running for that
    /// BAM, and start the next if names are waiting.
    pub(super) fn search_done(
        &mut self,
        dataset: u64,
        names: Vec<String>,
        result: seqpeeq_core::Result<Vec<Read>>,
        progress: &Arc<Progress>,
    ) {
        let Some(s) = self.searches.get_mut(&dataset) else {
            return;
        };
        if !s
            .running
            .as_ref()
            .is_some_and(|(_, p)| Arc::ptr_eq(p, progress))
        {
            return;
        }
        s.running = None;
        match result {
            Ok(records) => {
                let count = records.len();
                for name in &names {
                    s.searched.insert(name.clone());
                }
                for r in records {
                    s.found.entry(r.name.clone()).or_default().push(r);
                }
                let n = names.len();
                self.status = format!(
                    "searched for {n} name{}: {count} record{} found",
                    if n == 1 { "" } else { "s" },
                    if count == 1 { "" } else { "s" }
                );
            }
            Err(Error::Cancelled) => {}
            Err(e) => self.status = format!("error: {e}"),
        }
        let waiting =
            std::mem::take(&mut self.searches.get_mut(&dataset).expect("just found").waiting);
        self.refresh_overs(dataset);
        let source = self
            .session
            .datasets()
            .iter()
            .find(|d| d.id == dataset)
            .and_then(Dataset::bam)
            .map(|b| b.source().clone());
        if let (false, Some(source)) = (waiting.is_empty(), source) {
            self.start_search(dataset, source, waiting);
        }
    }

    /// Build every view open over a BAM again from what is known now.
    fn refresh_overs(&mut self, dataset: u64) {
        let none = Searches::default();
        let s = self.searches.get(&dataset).unwrap_or(&none);
        let Some(view) = self.bams.get_mut(&dataset) else {
            return;
        };
        for over in view.over.iter_mut().chain(view.parked.iter_mut()) {
            match over {
                Over::Axis(axis) => {
                    axis.searched = s.searched.contains(&axis.name);
                    axis.rebuild(&s.found_for(&axis.name));
                }
                Over::List(list) => list.regroup(&s.found, &s.searched),
            }
        }
    }
}
