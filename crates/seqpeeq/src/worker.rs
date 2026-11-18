//! Threads for work that would otherwise freeze the screen.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use crate::app::{Done, Request};
use crate::bam_view::{Loaded, PASS_BINS, READS_LIMIT};

/// Runs each request on a thread of its own and collects the results.
/// Requests are rare (one per slow command), so there is no pool.
pub struct Worker {
    results: Receiver<Done>,
    sender: Sender<Done>,
    threads: Vec<JoinHandle<()>>,
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
            threads: Vec::new(),
        }
    }

    /// Start `request` on a new thread.
    pub fn submit(&mut self, request: Request) {
        self.threads.retain(|thread| !thread.is_finished());
        let sender = self.sender.clone();
        self.threads.push(std::thread::spawn(move || {
            perform(request, &mut |done| {
                let _ = sender.send(done);
            });
        }));
    }

    pub fn poll(&self) -> Option<Done> {
        self.results.try_recv().ok()
    }

    /// Wait for every thread to end.
    pub fn join(self) {
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seqpeeq_core::{Command, Session, Started};
    use std::time::{Duration, Instant};

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
