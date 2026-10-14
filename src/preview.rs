//! A latest-request worker. Preview decoding never seeks the playback instance.
use crate::playback::PreviewDecoder;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct PreviewFrame {
    pub generation: u64,
    pub seconds: f64,
    pub image: Result<image::RgbaImage, String>,
}

#[derive(Clone)]
struct Request {
    serial: u64,
    generation: u64,
    path: PathBuf,
    seconds: f64,
}

pub struct PreviewWorker {
    request: Arc<Mutex<Option<Request>>>,
    result: Arc<Mutex<Option<PreviewFrame>>>,
    latest: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl PreviewWorker {
    pub fn new() -> Self {
        let request = Arc::new(Mutex::new(None::<Request>));
        let result = Arc::new(Mutex::new(None));
        let latest = Arc::new(AtomicU64::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let incoming = request.clone();
        let outgoing = result.clone();
        let revision = latest.clone();
        let stop = stopping.clone();
        let worker = thread::spawn(move || {
            let mut decoder: Option<PreviewDecoder> = None;
            while !stop.load(Ordering::Acquire) {
                let next = incoming.lock().unwrap().take();
                let Some(mut next) = next else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                // Collapse pointer movement before opening or seeking media.
                thread::sleep(Duration::from_millis(90));
                if let Some(newer) = incoming.lock().unwrap().take() {
                    next = newer;
                }
                let canceled = || {
                    stop.load(Ordering::Acquire) || revision.load(Ordering::Acquire) != next.serial
                };
                if canceled() {
                    continue;
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                let image = (|| {
                    if decoder.is_none() {
                        decoder = Some(PreviewDecoder::new()?);
                    }
                    decoder
                        .as_mut()
                        .unwrap()
                        .decode(&next.path, next.seconds, deadline, canceled)
                })();
                if image.is_err() || canceled() {
                    // Unfinished seeks must not leave events in a reused decoder.
                    decoder = None;
                }
                // Serialize publication with request replacement. Even requests
                // reusing the UI generation supersede an in-flight decode.
                let _request = incoming.lock().unwrap();
                if !canceled() {
                    *outgoing.lock().unwrap() = Some(PreviewFrame {
                        generation: next.generation,
                        seconds: next.seconds,
                        image,
                    });
                }
            }
        });
        Self {
            request,
            result,
            latest,
            stopping,
            worker: Some(worker),
        }
    }

    pub fn request(&self, generation: u64, path: PathBuf, seconds: f64) {
        let mut request = self.request.lock().unwrap();
        let serial = self.latest.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        *request = Some(Request {
            serial,
            generation,
            path,
            seconds,
        });
        self.result.lock().unwrap().take();
    }

    pub fn take(&self) -> Option<PreviewFrame> {
        self.result.lock().unwrap().take()
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Native initialization/destruction cannot be interrupted safely.
            // Do not block the UI on it. A detached worker still owns its DLL,
            // core, renderer and mailboxes until teardown completes.
            let deadline = Instant::now() + Duration::from_millis(250);
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::playback::test_fixtures::avi;

    fn wait(worker: &PreviewWorker) -> PreviewFrame {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(frame) = worker.take() {
                return frame;
            }
            assert!(
                Instant::now() < deadline,
                "Preview worker did not publish a result"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn bundled_preview_latest_request_and_repeated_seek() {
        let directory = tempfile::tempdir().unwrap();
        let changing = directory.path().join("changing.avi");
        let blue = directory.path().join("blue.avi");
        avi(&changing, 2, false);
        avi(&blue, 0, true);
        let worker = PreviewWorker::new();
        worker.request(1, changing.clone(), 0.0);
        worker.request(2, blue, 1.0);
        let frame = wait(&worker);
        assert_eq!(frame.generation, 2);
        assert_eq!(frame.seconds, 1.0);
        let image = frame.image.unwrap();
        assert!(image.width() <= 240 && image.height() <= 135);
        assert!(
            image
                .pixels()
                .all(|p| p[0] < 10 && p[1] < 10 && p[2] > 240 && p[3] == 255)
        );

        worker.request(3, changing.clone(), 0.0);
        let first = wait(&worker).image.unwrap();
        worker.request(4, changing.clone(), 2.0);
        let later = wait(&worker).image.unwrap();
        assert_ne!(
            first.as_raw(),
            later.as_raw(),
            "Seek returned the previous frame"
        );
        worker.request(5, changing, 0.0);
        let again = wait(&worker).image.unwrap();
        assert_eq!(
            first.as_raw(),
            again.as_raw(),
            "Backward seek returned a stale frame"
        );
    }

    #[test]
    fn drop_does_not_wait_for_native_decoder() {
        let worker = PreviewWorker::new();
        worker.request(1, PathBuf::from("missing-preview.avi"), 0.0);
        let start = Instant::now();
        drop(worker);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
