//! Threads for work that would otherwise freeze the screen.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use crate::app::{Done, Request};

/// Runs each request on a thread of its own and collects the results.
/// Requests are rare (one per slow command), so there is no pool.
pub struct Worker {
    results: Receiver<Done>,
    sender: Sender<Done>,
    threads: Vec<JoinHandle<()>>,
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
            let done = match request {
                Request::Run(job) => Done::Command(job.run()),
            };
            // The receiver is gone only when the interface is closing.
            let _ = sender.send(done);
        }));
    }

    /// A finished piece of work, if one is waiting. Never blocks.
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
        let Done::Command(finished) = done;
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
}
