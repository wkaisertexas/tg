use crate::references::BackgroundExecutor;
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct ManualExecutor {
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl BackgroundExecutor for ManualExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        self.jobs.lock().unwrap().push(job);
    }
}

impl ManualExecutor {
    pub(crate) fn len(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    pub(crate) fn run(&self, index: usize) {
        let job = self.jobs.lock().unwrap().remove(index);
        job();
    }

    pub(crate) fn run_on_background_thread(&self, index: usize) {
        let job = self.jobs.lock().unwrap().remove(index);
        std::thread::spawn(job).join().unwrap();
    }
}
