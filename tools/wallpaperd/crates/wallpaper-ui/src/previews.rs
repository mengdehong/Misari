//! Decode previews off the UI thread and retain a bounded cache of recent images.
use crate::{Message, blocking, services};
use iced::{
    Task,
    futures::{SinkExt, StreamExt},
    widget::image,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// All generations share the network limit; cancelling a stream releases its requests.
static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Default)]
pub struct Previews {
    images: BTreeMap<String, Option<image::Handle>>,
    sources: Vec<String>,
    recent: VecDeque<String>,
    cached_bytes: usize,
    generation: u64,
    task: Option<iced::task::Handle>,
    pub pending: usize,
}

fn source(item: &Value) -> Option<&str> {
    item["preview_url"]
        .as_str()
        .filter(|url| !url.is_empty())
        .or_else(|| item["thumbnail"].as_str())
}

impl Previews {
    pub fn get(&self, item: &Value) -> Option<&Option<image::Handle>> {
        self.images.get(source(item)?)
    }

    pub fn cancel_pending(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.pending = 0;
        self.sources.clear();
    }

    pub fn clear(&mut self) {
        self.cancel_pending();
        self.images.clear();
        self.recent.clear();
        self.cached_bytes = 0;
    }

    pub fn retry_failed(&mut self) {
        self.cancel_pending();
        self.images.retain(|_, image| image.is_some());
        self.recent.retain(|key| self.images.contains_key(key));
    }

    pub fn finished(&self, item: &Value) -> bool {
        source(item).is_none() || self.get(item).is_some()
    }

    pub fn load(&mut self, items: Vec<Value>) -> Task<Message> {
        let mut seen = BTreeSet::new();
        let mut work: Vec<_> = items
            .into_iter()
            .filter_map(|item| {
                let source = source(&item)?.to_owned();
                seen.insert(source.clone()).then_some((source, item))
            })
            .collect();
        let sources: Vec<_> = work.iter().map(|(source, _)| source.clone()).collect();
        if self.sources == sources {
            return Task::none();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.sources = sources;
        for source in &self.sources {
            if self.images.contains_key(source) {
                self.recent.retain(|key| key != source);
                self.recent.push_back(source.clone());
            }
        }
        work.retain(|(source, _)| !self.images.contains_key(source));
        self.pending = work.len();
        let generation = self.generation;
        let (task, handle) = Task::run(requests(work, generation), |message| message).abortable();
        self.task = Some(handle.abort_on_drop());
        task
    }

    pub fn accept(&mut self, generation: u64, source: String, handle: Option<image::Handle>) {
        if generation == self.generation && self.sources.contains(&source) {
            self.pending = self.pending.saturating_sub(1);
            let size = |handle: &Option<image::Handle>| match handle {
                Some(image::Handle::Rgba { width, height, .. }) => {
                    *width as usize * *height as usize * 4
                }
                _ => 0,
            };
            self.cached_bytes += size(&handle);
            if let Some(previous) = self.images.insert(source.clone(), handle) {
                self.cached_bytes -= size(&previous);
            }
            self.recent.retain(|key| key != &source);
            self.recent.push_back(source);
            // Keep recent queries warm without growing with the search history.
            while self.cached_bytes > 64 * 1024 * 1024 || self.images.len() > 128 {
                let Some(oldest) = self.recent.pop_front() else {
                    break;
                };
                if let Some(removed) = self.images.remove(&oldest) {
                    self.cached_bytes -= size(&removed);
                }
            }
        }
    }
}

fn requests(
    work: Vec<(String, Value)>,
    generation: u64,
) -> impl iced::futures::Stream<Item = Message> {
    requests_with(work, generation, services::download_thumbnail)
}

fn requests_with<F, Fut>(
    work: Vec<(String, Value)>,
    generation: u64,
    download: F,
) -> impl iced::futures::Stream<Item = Message>
where
    F: Fn(Value) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Option<image::Handle>> + Send,
{
    iced::stream::channel(8, async move |mut output| {
        let cached = iced::futures::stream::iter(work.into_iter().enumerate())
            .map(|(rank, (source, item))| async move {
                let permit = DECODERS.acquire().await.expect("preview semaphore closed");
                blocking(move || {
                    let _permit = permit;
                    let cached = services::cached_thumbnail(&item);
                    (rank, source, item, cached)
                })
                .await
            })
            .buffer_unordered(4);
        iced::futures::pin_mut!(cached);
        let mut downloads = Vec::new();
        // Probe every cache entry before queuing network misses. Even eight slow
        // downloads must not prevent a later cached/local image from appearing.
        while let Some((rank, source, item, handle)) = cached.next().await {
            if handle.is_some() || !item["preview_url"].is_string() {
                if output
                    .send(Message::ThumbnailLoaded(generation, source, handle))
                    .await
                    .is_err()
                {
                    return;
                }
            } else {
                downloads.push((rank, source, item));
            }
        }
        downloads.sort_by_key(|(rank, _, _)| *rank);
        let downloads = iced::futures::stream::iter(downloads)
            .map(move |(_, source, item)| {
                let download = download.clone();
                async move {
                    let _permit = DOWNLOADS.acquire().await.expect("preview semaphore closed");
                    let handle = download(item).await;
                    Message::ThumbnailLoaded(generation, source, handle)
                }
            })
            .buffer_unordered(8);
        iced::futures::pin_mut!(downloads);
        while let Some(message) = downloads.next().await {
            if output.send(message).await.is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    static NETWORK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn new_search_does_not_wait_for_cancelled_downloads() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let _serial = NETWORK_TEST.lock().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        runtime.block_on(async {
            let count = started.clone();
            let old = tokio::spawn(async move {
                let jobs = requests_with((1..=8).map(|id| (format!("old-{id}"),
                    json!({"id":id.to_string(), "preview_url":format!("https://example.invalid/old-{id}")}))).collect(), 1,
                    move |_| {
                        count.fetch_add(1, Ordering::SeqCst);
                        std::future::pending::<Option<image::Handle>>()
                    });
                iced::futures::pin_mut!(jobs);
                jobs.next().await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while started.load(Ordering::SeqCst) < 8 { tokio::task::yield_now().await; }
            }).await.unwrap();
            old.abort();
            let _ = old.await;
            let current = requests_with(vec![("new".into(), json!({"id":"100","preview_url":"https://example.invalid/new"}))], 2,
                |_| std::future::ready(Some(image::Handle::from_rgba(1, 1, vec![255;4]))));
            iced::futures::pin_mut!(current);
            let result = tokio::time::timeout(std::time::Duration::from_millis(200), current.next())
                .await.expect("cancelled requests still occupy all download slots");
            assert!(matches!(result, Some(Message::ThumbnailLoaded(2, _, Some(_)))));
        });
    }

    #[test]
    fn generations_ignore_old_results_and_reuse_recent_images() {
        let mut previews = Previews::default();
        let item = json!({"thumbnail":"first.png"});
        let _ = previews.load(vec![item.clone(), item.clone()]);
        assert_eq!(previews.pending, 1);
        let generation = previews.generation;
        let handle = image::Handle::from_rgba(1, 1, vec![255; 4]);
        previews.accept(generation, "first.png".into(), Some(handle.clone()));
        assert_eq!(previews.pending, 0);
        let _ = previews.load(vec![item.clone()]);
        assert_eq!(
            previews.get(&item).unwrap().as_ref().unwrap().id(),
            handle.id()
        );

        let next = json!({"thumbnail":"next.png"});
        let _ = previews.load(vec![next.clone()]);
        previews.accept(generation, "first.png".into(), None);
        assert_eq!(
            previews.get(&item).unwrap().as_ref().unwrap().id(),
            handle.id()
        );
        assert_eq!(previews.pending, 1);
        previews.accept(previews.generation, "next.png".into(), None);
        assert!(matches!(previews.get(&next), Some(None)));
        previews.clear();
        assert_eq!(previews.pending, 0);
        assert!(previews.get(&next).is_none());
        let _ = previews.load(vec![next]);
        assert_eq!(previews.pending, 1, "refresh retries failed previews");
    }

    #[test]
    fn cached_preview_arrives_while_network_slots_are_blocked() {
        let _serial = NETWORK_TEST.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.png");
        ::image::RgbaImage::from_pixel(4, 4, ::image::Rgba([42, 84, 126, 255]))
            .save(&path)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let _blocked = DOWNLOADS.acquire_many(8).await.unwrap();
            let mut work = (1..=8)
                .map(|id| {
                    (
                        format!("slow-{id}"),
                        json!({"id":id.to_string(),"preview_url":"https://example.invalid/slow"}),
                    )
                })
                .collect::<Vec<_>>();
            work.push(("cached".into(), json!({"id":"local:test","thumbnail":path})));
            let jobs = requests(work, 1);
            iced::futures::pin_mut!(jobs);
            let first = tokio::time::timeout(std::time::Duration::from_secs(2), jobs.next())
                .await
                .expect("cached images queued behind network")
                .unwrap();
            match first {
                Message::ThumbnailLoaded(
                    1,
                    key,
                    Some(image::Handle::Rgba { width, height, .. }),
                ) => {
                    assert_eq!(key, "cached");
                    assert!(width > 0 && width <= 480 && height > 0 && height <= 320);
                }
                other => {
                    panic!("expected a decoded cached image before the blocked download: {other:?}")
                }
            }
        });
        if let Some(cached) = crate::engine::thumbnail(path.to_str().unwrap()) {
            let _ = std::fs::remove_file(cached);
        }
    }
}
