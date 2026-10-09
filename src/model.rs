use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollectionKind {
    Album,
    Artist,
    Playlist,
    Discography,
}

impl CollectionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Album => "Album",
            Self::Artist => "Artist",
            Self::Playlist => "Playlist",
            Self::Discography => "Discography",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub kind: CollectionKind,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedAlbum {
    pub collection: Collection,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: u64,
    pub title: String,
    pub tracks: Vec<Track>,
}

pub fn move_item<T>(items: &mut std::collections::VecDeque<T>, from: usize, slot: usize) -> bool {
    if from >= items.len() || slot > items.len() {
        return false;
    }
    let target = slot.saturating_sub(usize::from(from < slot));
    if target == from {
        return false;
    }
    let item = items.remove(from).unwrap();
    items.insert(target, item);
    true
}

impl Track {
    pub fn youtube_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }

    pub fn artwork_url_for(id: &str) -> String {
        format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg")
    }

    pub fn duration_label(&self) -> String {
        match self.duration_seconds {
            Some(seconds) => format!("{}:{:02}", seconds / 60, seconds % 60),
            None => "--:--".to_owned(),
        }
    }
}

pub fn is_valid_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_video_ids() {
        assert!(is_valid_video_id("dQw4w9WgXcQ"));
        assert!(!is_valid_video_id("invalid &id"));
        assert!(!is_valid_video_id("too_short"));
    }

    #[test]
    fn displays_duration() {
        let track = Track {
            id: "dQw4w9WgXcQ".to_owned(),
            title: "Test".to_owned(),
            artist: "Example".to_owned(),
            duration_seconds: Some(235),
            ..Default::default()
        };
        assert_eq!(track.duration_label(), "3:55");
        assert_eq!(
            track.youtube_url(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }
}
