//! Ephemeral radio recommendations; explicit queue items remain separate and take priority.
use crate::model::Track;
use std::collections::VecDeque;

pub struct Radio {
    pub seed: Track,
    pub continuation: Track,
    pub upcoming: VecDeque<Track>,
    pub searching: bool,
    pub error: Option<String>,
    pub waiting: bool,
    seen: VecDeque<String>,
}

impl Radio {
    pub fn new(seed: Track) -> Self {
        let seen = VecDeque::from([seed.id.clone()]);
        Self {
            continuation: seed.clone(),
            seed,
            upcoming: VecDeque::new(),
            searching: false,
            error: None,
            waiting: false,
            seen,
        }
    }

    pub fn remember(&mut self, id: &str) {
        if !self.seen.iter().any(|seen| seen == id) {
            self.seen.push_back(id.to_owned());
            // Prevent an hours-long session from accumulating an unbounded history.
            if self.seen.len() > 500 {
                self.seen.pop_front();
            }
        }
    }

    pub fn extend(&mut self, tracks: Vec<Track>, manual: &VecDeque<Track>) {
        for track in tracks {
            if !self.seen.contains(&track.id)
                && !self.upcoming.iter().any(|t| t.id == track.id)
                && !manual.iter().any(|t| t.id == track.id)
                && self.upcoming.len() < 25
            {
                self.remember(&track.id);
                self.upcoming.push_back(track);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: usize) -> Track {
        Track {
            id: format!("{id:011}"),
            title: "Song".into(),
            artist: "Artist".into(),
            duration_seconds: Some(180),
            ..Default::default()
        }
    }
    #[test]
    fn radio_is_bounded_and_does_not_repeat_or_include_manual_tracks() {
        let mut radio = Radio::new(track(0));
        radio.extend((0..100).map(track).collect(), &VecDeque::from([track(1)]));
        assert_eq!(radio.upcoming.len(), 25);
        assert_eq!(radio.upcoming.front().unwrap().id, track(2).id);
        radio.upcoming.clear();
        radio.extend((0..27).map(track).collect(), &VecDeque::from([track(1)]));
        assert!(radio.upcoming.is_empty());
        for id in 100..1000 {
            radio.remember(&track(id).id);
        }
        assert_eq!(radio.seen.len(), 500);
    }
}
