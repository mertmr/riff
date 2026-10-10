use super::{resolver::Media, stream::Stream};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

struct Sample {
    generation: u64,
    selected: Instant,
    received: Duration,
    resolved: Option<Duration>,
    loaded: Option<Duration>,
    kind: &'static str,
    stream: Option<Arc<Stream>>,
}
pub(super) struct Trace {
    sample: Option<Sample>,
    history: VecDeque<(&'static str, Duration)>,
}
impl Trace {
    pub fn enabled() -> Option<Self> {
        std::env::var_os("RIFF_PLAYBACK_TRACE")
            .filter(|v| v != "0")
            .map(|_| Self {
                sample: None,
                history: VecDeque::new(),
            })
    }
    pub fn begin(&mut self, generation: u64, selected: Instant) {
        self.sample = Some(Sample {
            generation,
            selected,
            received: selected.elapsed(),
            resolved: None,
            loaded: None,
            kind: "cold",
            stream: None,
        });
    }
    pub fn resolved(&mut self, media: &Media, cached: bool) {
        if let Some(sample) = &mut self.sample {
            sample
                .resolved
                .get_or_insert_with(|| sample.selected.elapsed());
            sample.kind = if cached {
                "disk"
            } else if media.stream.as_ref().is_some_and(|s| s.prefix_ready()) {
                "prepared"
            } else {
                "cold"
            };
            sample.stream = media.stream.clone();
        }
    }
    pub fn loaded(&mut self) {
        if let Some(sample) = &mut self.sample {
            sample
                .loaded
                .get_or_insert_with(|| sample.selected.elapsed());
        }
    }
    pub fn clear(&mut self) {
        self.sample = None;
    }
    pub fn output_clock(&mut self) {
        let Some(sample) = self.sample.take() else {
            return;
        };
        if sample.loaded.is_none() {
            self.sample = Some(sample);
            return;
        }
        let output = sample.selected.elapsed();
        let first_byte = sample
            .stream
            .as_ref()
            .and_then(|s| s.first_byte())
            .map(|time| time.saturating_duration_since(sample.selected));
        self.history.push_back((sample.kind, output));
        if self.history.len() > 64 {
            self.history.pop_front();
        }
        let mut times: Vec<_> = self
            .history
            .iter()
            .filter(|(kind, _)| *kind == sample.kind)
            .map(|(_, time)| *time)
            .collect();
        times.sort_unstable();
        let percentile = |percent: usize| {
            times[(times.len() * percent).div_ceil(100).saturating_sub(1)].as_millis()
        };
        eprintln!(
            "playback generation={} kind={} received_ms={} resolved_ms={:?} first_byte_ms={:?} file_loaded_ms={:?} output_clock_ms={} p50_ms={} p95_ms={} samples={}",
            sample.generation,
            sample.kind,
            sample.received.as_millis(),
            sample.resolved.map(|d| d.as_millis()),
            first_byte.map(|d| d.as_millis()),
            sample.loaded.map(|d| d.as_millis()),
            output.as_millis(),
            percentile(50),
            percentile(95),
            times.len()
        );
    }
}
