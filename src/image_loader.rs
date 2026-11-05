use crate::media::{self, ImagePreview};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Debug, Default)]
struct State {
    generation: u64,
    job: Option<(u64, PathBuf)>,
    result: Option<(u64, Result<ImagePreview, String>)>,
    pending: bool,
    stopped: bool,
}

/// One worker decodes the newest requested image without writing to the terminal.
#[derive(Debug)]
pub struct ImageLoader {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl ImageLoader {
    pub fn new() -> Self {
        Self::with_decoder(media::load_image)
    }

    fn with_decoder(
        decoder: impl Fn(&Path) -> Result<ImagePreview, String> + Send + 'static,
    ) -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = Arc::clone(&shared);
        thread::spawn(move || {
            loop {
                let (lock, wake) = &*worker;
                let mut state = lock.lock().unwrap();
                while state.job.is_none() && !state.stopped {
                    state = wake.wait(state).unwrap();
                }
                if state.stopped {
                    return;
                }
                let (generation, path) = state.job.take().unwrap();
                drop(state);
                let result = decoder(&path);
                let mut state = lock.lock().unwrap();
                if !state.stopped && state.generation == generation {
                    state.result = Some((generation, result));
                    wake.notify_all();
                    state.pending = false;
                }
            }
        });
        Self { shared }
    }

    pub fn request(&mut self, path: PathBuf) -> u64 {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.job = Some((generation, path));
        state.result = None;
        state.pending = true;
        wake.notify_one();
        generation
    }

    pub fn cancel(&mut self) {
        let mut state = self.shared.0.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.job = None;
        state.result = None;
        state.pending = false;
    }

    pub fn poll(&mut self) -> Option<(u64, Result<ImagePreview, String>)> {
        self.shared.0.lock().unwrap().result.take()
    }

    pub fn is_pending(&self) -> bool {
        let state = self.shared.0.lock().unwrap();
        state.pending || state.result.is_some()
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.stopped = true;
        state.job = None;
        wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn replaces_queued_jobs_and_discards_stale_results() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut loader = ImageLoader::with_decoder(move |path| {
            started_tx.send(path.to_path_buf()).unwrap();
            release_rx.recv().unwrap();
            Err(path.display().to_string())
        });
        loader.request("first".into());
        assert_eq!(started_rx.recv().unwrap(), PathBuf::from("first"));
        loader.request("skipped".into());
        let newest = loader.request("newest".into());
        release_tx.send(()).unwrap();
        assert_eq!(started_rx.recv().unwrap(), PathBuf::from("newest"));
        assert!(loader.poll().is_none());
        assert!(loader.is_pending());
        release_tx.send(()).unwrap();
        // The lock and condition variable provide a deterministic completion barrier.
        let (lock, wake) = &*loader.shared;
        let mut state = lock.lock().unwrap();
        while state.pending {
            state = wake.wait(state).unwrap();
        }
        drop(state);
        assert!(
            loader.is_pending(),
            "a ready result must keep the event loop awake"
        );
        let (generation, result) = loader.poll().unwrap();
        assert_eq!(generation, newest);
        assert_eq!(result.unwrap_err(), "newest");
        assert!(!loader.is_pending());
    }

    #[test]
    fn cancel_discards_a_running_result_before_the_next_request() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut loader = ImageLoader::with_decoder(move |path| {
            started_tx.send(path.to_path_buf()).unwrap();
            release_rx.recv().unwrap();
            Err(path.display().to_string())
        });
        loader.request("old".into());
        started_rx.recv().unwrap();
        loader.cancel();
        let next = loader.request("next".into());
        release_tx.send(()).unwrap();
        assert_eq!(started_rx.recv().unwrap(), PathBuf::from("next"));
        assert!(loader.poll().is_none());
        release_tx.send(()).unwrap();
        let (lock, wake) = &*loader.shared;
        let mut state = lock.lock().unwrap();
        while state.pending {
            state = wake.wait(state).unwrap();
        }
        drop(state);
        let (generation, result) = loader.poll().unwrap();
        assert_eq!(generation, next);
        assert_eq!(result.unwrap_err(), "next");
    }

    #[test]
    fn cancel_and_drop_do_not_wait_for_running_decode() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut loader = ImageLoader::with_decoder(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Err("finished".into())
        });
        loader.request("first".into());
        started_rx.recv().unwrap();
        loader.cancel();
        assert!(!loader.is_pending());
        assert!(loader.poll().is_none());
        drop(loader);
        release_tx.send(()).unwrap();
    }
}
