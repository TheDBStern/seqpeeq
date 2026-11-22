//! Threads for work that would otherwise freeze the screen.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use seqpeeq_core::Progress;

use crate::app::{Done, Request};
use crate::bam_view::{Loaded, PASS_BINS, READS_LIMIT};

/// Runs each request on a thread of its own and collects the results.
/// Requests are rare (one per slow command), so there is no pool.
pub struct Worker {
    results: Receiver<Done>,
    sender: Sender<Done>,
    jobs: Jobs,
}

/// The threads running requests, each with the handle that cancels it.
/// Shared, so that a thread other than the interface's can stop them.
#[derive(Clone, Default)]
pub struct Jobs(Arc<Mutex<Vec<Running>>>);

/// A thread and the handle that cancels its work.
type Running = (Arc<Progress>, JoinHandle<()>);

impl Jobs {
    /// Keep `thread`, which `progress` cancels; let go of finished ones.
    pub fn add(&self, progress: Arc<Progress>, thread: JoinHandle<()>) {
        let mut jobs = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        jobs.retain(|(_, thread)| !thread.is_finished());
        jobs.push((progress, thread));
    }

    /// Cancel every job, then wait for their threads, but no longer than
    /// `limit`. Returns whether they all ended. A cancelled write removes
    /// what it had written, so waiting is what keeps partial files away.
    pub fn cancel_and_wait(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let all_ended = || {
            let jobs = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (progress, _) in jobs.iter() {
                progress.cancel();
            }
            jobs.iter().all(|(_, thread)| thread.is_finished())
        };
        loop {
            if all_ended() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Take every thread out, to be joined.
    fn take(&self) -> Vec<JoinHandle<()>> {
        let mut jobs = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        jobs.drain(..).map(|(_, thread)| thread).collect()
    }
}

/// Do the work `request` asks for on this thread, handing each result to
/// `send`. Most requests have one result; a coverage pass hands out
/// partial ones as it goes.
pub fn perform(request: Request, send: &mut dyn FnMut(Done)) {
    match request {
        Request::Run(job) => send(Done::Command(job.run())),
        Request::Conserve {
            dataset,
            view,
            progress,
        } => send(Done::Conservation {
            dataset,
            result: view.conservation(&progress),
        }),
        Request::Summarise {
            dataset,
            view,
            conserved,
            progress,
        } => send(Done::Summary {
            dataset,
            result: view.summary(&conserved, &progress),
        }),
        Request::Window {
            dataset,
            generation,
            source,
            want,
            progress,
        } => {
            let (region, bin, reads) = want;
            let limit = reads.then_some(READS_LIMIT);
            let result = source.window(&region, bin, limit, &progress).map(|window| {
                let name = &source.contigs()[region.contig].name;
                let reference = source
                    .reference()
                    .filter(|_| reads)
                    .and_then(|r| r.fetch(name, region.start, region.end).ok());
                Loaded {
                    window,
                    asked_reads: reads,
                    reference,
                    with_reference: source.reference().is_some(),
                }
            });
            send(Done::Window {
                dataset,
                generation,
                result,
            });
        }
        Request::Pass {
            dataset,
            contig,
            source,
            progress,
        } => {
            let length = source.contigs()[contig].length;
            let bins = PASS_BINS.min(length).max(1) as usize;
            let result = source.coverage(contig, bins, &progress, |so_far| {
                if so_far.done_to < length {
                    send(Done::Pass {
                        dataset,
                        contig,
                        coverage: Ok(so_far.clone()),
                        finished: false,
                    });
                }
            });
            send(Done::Pass {
                dataset,
                contig,
                coverage: result,
                finished: true,
            });
        }
        Request::Gather {
            dataset,
            read,
            source,
            progress,
        } => send(Done::Gather {
            dataset,
            name: read.name.clone(),
            result: source.gather(&read, &progress),
        }),
        Request::Search {
            dataset,
            names,
            source,
            progress,
        } => {
            let wanted = names.iter().cloned().collect();
            send(Done::Search {
                dataset,
                result: source.search_names(&wanted, &progress),
                names,
                progress,
            })
        }
        Request::Jump {
            dataset,
            contig,
            from,
            dir,
            want,
            source,
            progress,
        } => send(Done::Jump {
            dataset,
            contig,
            from,
            dir,
            want,
            result: source.jump(contig, from, dir, want, &progress),
            progress,
        }),
    }
}

impl Worker {
    pub fn new() -> Worker {
        let (sender, results) = mpsc::channel();
        Worker {
            results,
            sender,
            jobs: Jobs::default(),
        }
    }

    /// Start `request` on a new thread.
    pub fn submit(&mut self, request: Request) {
        let sender = self.sender.clone();
        let progress = request.progress();
        let thread = std::thread::spawn(move || {
            perform(request, &mut |done| {
                let _ = sender.send(done);
            });
        });
        self.jobs.add(progress, thread);
    }

    pub fn poll(&self) -> Option<Done> {
        self.results.try_recv().ok()
    }

    /// The running threads, to be stopped from elsewhere.
    pub fn jobs(&self) -> Jobs {
        self.jobs.clone()
    }

    /// Wait for every thread to end.
    pub fn join(self) {
        for thread in self.jobs.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqpeeq_core::{Command, Progress, Session, Started};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// A thread that runs until `progress` is cancelled, then sets `ended`.
    fn until_cancelled(progress: &Arc<Progress>, ended: &Arc<AtomicBool>) -> JoinHandle<()> {
        let (progress, ended) = (Arc::clone(progress), Arc::clone(ended));
        std::thread::spawn(move || {
            while !progress.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            ended.store(true, Ordering::SeqCst);
        })
    }

    #[test]
    fn cancel_and_wait_cancels_and_waits_for_every_job() {
        let jobs = Jobs::default();
        let ended: Vec<Arc<AtomicBool>> = (0..2).map(|_| Arc::default()).collect();
        for ended in &ended {
            let progress = Arc::new(Progress::new());
            jobs.add(Arc::clone(&progress), until_cancelled(&progress, ended));
        }
        assert!(jobs.cancel_and_wait(Duration::from_secs(5)));
        assert!(ended.iter().all(|ended| ended.load(Ordering::SeqCst)));
    }

    #[test]
    fn cancel_and_wait_gives_up_after_its_limit() {
        let jobs = Jobs::default();
        let deaf = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(2)));
        jobs.add(Arc::new(Progress::new()), deaf);
        let started = Instant::now();
        assert!(!jobs.cancel_and_wait(Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_job_runs_on_another_thread_and_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.fa");
        std::fs::write(&file, ">a\nACGT\n").unwrap();
        let mut session = Session::new(None);
        let open = Command::Open {
            path: file,
            kind: None,
        };
        let Started::Running(job) = session.start(open).unwrap() else {
            panic!("an alignment opens in the background")
        };
        let mut worker = Worker::new();
        worker.submit(Request::Run(job));
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = loop {
            if let Some(done) = worker.poll() {
                break done;
            }
            assert!(Instant::now() < deadline, "the job never came back");
            std::thread::sleep(Duration::from_millis(5));
        };
        let Done::Command(finished) = done else {
            panic!("a command was submitted")
        };
        assert!(
            session
                .finish(finished)
                .unwrap()
                .message
                .starts_with("opened ")
        );
        assert!(worker.poll().is_none());
        worker.join();
    }

    #[test]
    fn a_count_runs_on_another_thread_and_comes_back() {
        use seqpeeq_core::{Alignment, Progress};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.fa");
        std::fs::write(&file, ">a\nACGT\n>b\nACGA\n").unwrap();
        let alignment = Alignment::load(&file).unwrap();
        let mut worker = Worker::new();
        worker.submit(Request::Conserve {
            dataset: 7,
            view: alignment.view().clone(),
            progress: Arc::new(Progress::new()),
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = loop {
            if let Some(done) = worker.poll() {
                break done;
            }
            assert!(Instant::now() < deadline, "the count never came back");
            std::thread::sleep(Duration::from_millis(5));
        };
        let Done::Conservation { dataset, result } = done else {
            panic!("a count was submitted")
        };
        assert_eq!((dataset, result.map(|found| found.cols())), (7, Some(4)));
        worker.join();
    }

    #[test]
    fn a_summary_runs_on_another_thread_and_comes_back() {
        use seqpeeq_core::{Alignment, Progress};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.fa");
        std::fs::write(&file, ">a\nACGT\n>b\nACGA\n").unwrap();
        let alignment = Alignment::load(&file).unwrap();
        let conserved = alignment.view().conservation(&Progress::new()).unwrap();
        let mut worker = Worker::new();
        worker.submit(Request::Summarise {
            dataset: 7,
            view: alignment.view().clone(),
            conserved,
            progress: Arc::new(Progress::new()),
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = loop {
            if let Some(done) = worker.poll() {
                break done;
            }
            assert!(Instant::now() < deadline, "the summary never came back");
            std::thread::sleep(Duration::from_millis(5));
        };
        let Done::Summary { dataset, result } = done else {
            panic!("a summary was submitted")
        };
        let result = result.unwrap();
        assert_eq!((dataset, result.rows(), result.cols()), (7, 2, 4));
        assert_eq!(result.row(0).score(), Some(75), "a tie goes to A");
        worker.join();
    }
}
