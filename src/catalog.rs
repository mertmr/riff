use crate::model::{Collection, CollectionKind, Track, is_valid_video_id};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }
    fn params(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Songs => Some("EgWKAQIIAWoSEAUQChADEAkQBBAQEBUQDhAR"),
            Self::Albums => Some("EgWKAQIYAWoSEAUQChADEAkQBBAQEBUQDhAR"),
            Self::Artists => Some("EgWKAQIgAWoSEAUQChADEAkQBBAQEBUQDhAR"),
            Self::Playlists => Some("EgeKAQQoAEABahIQBRAKEAMQCRAEEBAQFRAOEBE%3D"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub tracks: Vec<Track>,
    pub collections: Vec<Collection>,
    pub continuation: Option<String>,
}
impl From<Vec<Track>> for Page {
    fn from(tracks: Vec<Track>) -> Self {
        Self {
            tracks,
            ..Default::default()
        }
    }
}
impl Page {
    pub fn append(&mut self, page: Page) {
        let mut ids: HashSet<String> = self.tracks.iter().map(|t| t.id.clone()).collect();
        self.tracks
            .extend(page.tracks.into_iter().filter(|t| ids.insert(t.id.clone())));
        let mut ids: HashSet<String> = self.collections.iter().map(|c| c.id.clone()).collect();
        self.collections.extend(
            page.collections
                .into_iter()
                .filter(|c| ids.insert(c.id.clone())),
        );
        self.continuation = page.continuation;
    }
    pub fn append_collection(&mut self, page: Page) {
        self.tracks.extend(page.tracks);
        self.collections.extend(page.collections);
        self.continuation = page.continuation;
    }
}

pub struct Client {
    agent: ureq::Agent,
}
impl Client {
    pub fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(20)))
                .timeout_connect(Some(Duration::from_secs(5)))
                .max_idle_connections(1)
                .build()
                .into(),
        }
    }
    fn request(&self, endpoint: &str, body: Value) -> Result<Value, String> {
        self.request_with_timeout(endpoint, body, Duration::from_secs(20))
    }
    fn request_with_timeout(
        &self,
        endpoint: &str,
        mut body: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        body["context"] = json!({"client": {"clientName": "WEB_REMIX",
            "clientVersion": "1.20240918.01.00", "hl": "en", "gl": "US"}});
        let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let mut response = self
            .agent
            .post(format!("https://music.youtube.com/youtubei/v1/{endpoint}"))
            .header("Content-Type", "application/json")
            .header("Origin", "https://music.youtube.com")
            .header("User-Agent", "Mozilla/5.0")
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(bytes.as_slice())
            .map_err(|e| format!("Couldn't load YouTube Music: {e}"))?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| format!("Couldn't read the catalog: {e}"))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid catalog response: {e}"))?;
        if let Some(error) = value.get("error") {
            return Err(error["message"]
                .as_str()
                .unwrap_or("Catalog request failed")
                .into());
        }
        if value.get("contents").is_none()
            && value.get("continuationContents").is_none()
            && value.get("onResponseReceivedActions").is_none()
            && value.get("onResponseReceivedEndpoints").is_none()
            && value.get("onResponseReceivedCommands").is_none()
        {
            return Err("YouTube Music returned no catalog data. Try again.".into());
        }
        Ok(value)
    }
    pub fn recommendations(&self, id: &str) -> Result<Vec<Track>, String> {
        if !is_valid_video_id(id) {
            return Err("Cannot find recommendations for an invalid track.".into());
        }
        // One watch-list page supplies enough candidates; never follow mix continuations.
        let body = json!({
            "videoId": id,
            "playlistId": format!("RDAMVM{id}"),
            "enablePersistentPlaylistPanel": true,
            "isAudioOnly": true,
            "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
            "params": "wAEB",
        });
        self.request_with_timeout("next", body, Duration::from_secs(8))
            .and_then(|value| parse_recommendations(&value))
    }
    pub fn search(
        &self,
        query: &str,
        filter: SearchFilter,
        continuation: Option<&str>,
    ) -> Result<Page, String> {
        let mut body = json!({"query": query});
        if let Some(params) = filter.params() {
            body["params"] = json!(params);
        }
        if let Some(token) = continuation {
            body["continuation"] = json!(token);
        }
        self.request("search", body).map(|v| parse_page(&v))
    }
    pub fn browse(
        &self,
        collection: &Collection,
        continuation: Option<&str>,
    ) -> Result<Page, String> {
        if collection.id.is_empty() || collection.id.len() > 256 {
            return Err("Invalid collection ID".into());
        }
        let mut body = json!({"browseId": collection.id});
        if let Some(params) = &collection.params {
            body["params"] = json!(params);
        }
        if let Some(token) = continuation {
            body["continuation"] = json!(token);
        }
        let value = self.request("browse", body)?;
        let content = if matches!(
            collection.kind,
            CollectionKind::Album | CollectionKind::Playlist
        ) {
            find_renderer(&value, "musicPlaylistShelfRenderer")
                .or_else(|| find_renderer(&value, "musicPlaylistShelfContinuation"))
                .or_else(|| find_renderer(&value, "musicShelfRenderer"))
                .or_else(|| find_renderer(&value, "musicShelfContinuation"))
                .unwrap_or(&value)
        } else {
            &value
        };
        let mut page = parse_contents(content, false);
        let artist = header_artist(&value);
        for track in &mut page.tracks {
            if collection.kind == CollectionKind::Album {
                track.album = Some(collection.title.clone());
                track.album_id = Some(collection.id.clone());
                if track.artist == "Unknown artist"
                    && let Some((name, id)) = artist
                {
                    track.artist = name.into();
                    track.artist_id = Some(id.into());
                }
            }
            if collection.kind == CollectionKind::Artist && track.artist == "Unknown artist" {
                track.artist = collection.title.clone();
                track.artist_id = Some(collection.id.clone());
            }
        }
        Ok(page)
    }
}

fn text(value: &Value) -> String {
    value["runs"]
        .as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect())
        .or_else(|| value["simpleText"].as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn browse(value: &Value) -> Option<(&str, &str)> {
    let endpoint = &value["browseEndpoint"];
    Some((
        endpoint["browseId"].as_str()?,
        endpoint
            .pointer(
                "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
            )
            .and_then(Value::as_str)
            .unwrap_or(""),
    ))
}
fn header_artist(value: &Value) -> Option<(&str, &str)> {
    let header = find_renderer(value, "musicResponsiveHeaderRenderer")
        .or_else(|| find_renderer(value, "musicDetailHeaderRenderer"))?;
    let runs = ["straplineTextOne", "straplineText", "subtitle"]
        .iter()
        .find_map(|key| header[key]["runs"].as_array())?;
    runs.iter().find_map(|r| {
        let (id, kind) = browse(&r["navigationEndpoint"])?;
        (kind == "MUSIC_PAGE_TYPE_ARTIST").then(|| (r["text"].as_str().unwrap_or_default(), id))
    })
}

fn duration(label: &str) -> Option<u64> {
    let parts = label.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    parts.iter().try_fold(0u64, |total, part| {
        total.checked_mul(60)?.checked_add(part.parse().ok()?)
    })
}

fn parse_row(row: &Value, page: &mut Page) {
    let columns: Vec<&Value> = row["flexColumns"]
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|c| &c["musicResponsiveListItemFlexColumnRenderer"]["text"])
                .collect()
        })
        .unwrap_or_default();
    let title_value = columns.first().copied().unwrap_or(&row["title"]);
    let title = text(title_value);
    if title.trim().is_empty() {
        return;
    }
    let endpoint = row
        .get("navigationEndpoint")
        .or_else(|| title_value.pointer("/runs/0/navigationEndpoint"))
        .unwrap_or(&Value::Null);
    if let Some((id, kind)) = browse(endpoint) {
        let kind = match kind {
            "MUSIC_PAGE_TYPE_ALBUM" => CollectionKind::Album,
            "MUSIC_PAGE_TYPE_ARTIST" => CollectionKind::Artist,
            "MUSIC_PAGE_TYPE_ARTIST_DISCOGRAPHY" => CollectionKind::Discography,
            "MUSIC_PAGE_TYPE_PLAYLIST" => CollectionKind::Playlist,
            _ => return,
        };
        let thumbnail = row
            .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
            .or_else(|| {
                row.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails")
            })
            .and_then(Value::as_array)
            .and_then(|t| {
                t.iter()
                    .find(|v| v["width"].as_u64().unwrap_or(0) >= 120)
                    .or_else(|| t.last())
            })
            .and_then(|v| v["url"].as_str())
            .map(str::to_owned);
        page.collections.push(Collection {
            id: id.into(),
            title,
            subtitle: text(columns.get(1).copied().unwrap_or(&row["subtitle"])),
            kind,
            thumbnail,
            params: endpoint["browseEndpoint"]["params"]
                .as_str()
                .map(str::to_owned),
        });
        return;
    }
    let id = row
        .pointer("/playlistItemData/videoId")
        .and_then(Value::as_str)
        .or_else(|| {
            endpoint
                .pointer("/watchEndpoint/videoId")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            title_value
                .pointer("/runs/0/navigationEndpoint/watchEndpoint/videoId")
                .and_then(Value::as_str)
        });
    let Some(id) = id.filter(|id| is_valid_video_id(id)) else {
        return;
    };
    if row["musicItemRendererDisplayPolicy"].as_str()
        == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
    {
        return;
    }
    let mut track = Track {
        id: id.into(),
        title,
        artist: "Unknown artist".into(),
        ..Default::default()
    };
    track_metadata(
        &mut track,
        columns
            .iter()
            .skip(1)
            .copied()
            .chain(std::iter::once(&row["subtitle"])),
    );
    if let Some(columns) = row["fixedColumns"].as_array() {
        track.duration_seconds = columns
            .iter()
            .find_map(|c| {
                duration(&text(
                    &c["musicResponsiveListItemFixedColumnRenderer"]["text"],
                ))
            })
            .or(track.duration_seconds);
    }
    page.tracks.push(track);
}

fn track_metadata<'a>(track: &mut Track, columns: impl IntoIterator<Item = &'a Value>) {
    let mut channel = None;
    for column in columns {
        if let Some(runs) = column["runs"].as_array() {
            let mut artists = Vec::new();
            for run in runs {
                if let Some((id, kind)) = browse(&run["navigationEndpoint"]) {
                    let name = run["text"].as_str().unwrap_or_default().trim();
                    if kind == "MUSIC_PAGE_TYPE_ARTIST" && !name.is_empty() {
                        artists.push(name);
                        if track.artist_id.is_none() {
                            track.artist_id = Some(id.into());
                        }
                    } else if !name.is_empty()
                        && (kind == "MUSIC_PAGE_TYPE_USER_CHANNEL"
                            || (kind.is_empty() && id.starts_with("UC")))
                    {
                        channel.get_or_insert((name, id));
                    } else if kind == "MUSIC_PAGE_TYPE_ALBUM" {
                        track.album = run["text"].as_str().map(str::to_owned);
                        track.album_id = Some(id.into());
                    }
                }
                if let Some(seconds) = run["text"].as_str().and_then(duration) {
                    track.duration_seconds = Some(seconds);
                }
            }
            if !artists.is_empty() {
                track.artist = artists.join(", ");
            }
        }
    }
    if track.artist == "Unknown artist"
        && let Some((name, id)) = channel
    {
        track.artist = name.into();
        track.artist_id = Some(id.into());
    }
}

fn parse_recommendations(value: &Value) -> Result<Vec<Track>, String> {
    let contents = find_renderer(value, "playlistPanelRenderer")
        .and_then(|panel| panel["contents"].as_array())
        .ok_or("YouTube Music returned no recommendations. Try again.")?;
    let mut seen = HashSet::new();
    let tracks: Vec<_> = contents
        .iter()
        .filter_map(|entry| {
            let entry = entry
                .pointer("/playlistPanelVideoWrapperRenderer/primaryRenderer")
                .unwrap_or(entry);
            let row = entry.get("playlistPanelVideoRenderer")?;
            if row.get("unplayableText").is_some()
                || row["musicItemRendererDisplayPolicy"].as_str()
                    == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
            {
                return None;
            }
            let id = row["videoId"].as_str().filter(|id| is_valid_video_id(id))?;
            let title = text(&row["title"]);
            if title.trim().is_empty() || !seen.insert(id) {
                return None;
            }
            let mut track = Track {
                id: id.into(),
                title,
                artist: "Unknown artist".into(),
                duration_seconds: duration(&text(&row["lengthText"])),
                ..Default::default()
            };
            track_metadata(&mut track, std::iter::once(&row["longBylineText"]));
            if track.artist == "Unknown artist" {
                let byline = text(&row["shortBylineText"]);
                if !byline.trim().is_empty() {
                    track.artist = byline;
                }
            }
            Some(track)
        })
        .take(50)
        .collect();
    if tracks.is_empty() {
        return Err("YouTube Music returned no playable recommendations. Try again.".into());
    }
    Ok(tracks)
}

fn find_renderer<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(object) => object
            .get(key)
            .or_else(|| object.values().find_map(|v| find_renderer(v, key))),
        Value::Array(values) => values.iter().find_map(|v| find_renderer(v, key)),
        _ => None,
    }
}

pub fn parse_page(value: &Value) -> Page {
    parse_contents(value, true)
}

fn parse_contents(value: &Value, unique_tracks: bool) -> Page {
    fn walk(value: &Value, page: &mut Page) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    match key.as_str() {
                        "musicResponsiveListItemRenderer" | "musicTwoRowItemRenderer" => {
                            parse_row(child, page)
                        }
                        "nextContinuationData"
                        | "reloadContinuationData"
                        | "continuationCommand" => {
                            if let Some(token) = child["continuation"]
                                .as_str()
                                .or_else(|| child["token"].as_str())
                            {
                                page.continuation = Some(token.into());
                            }
                        }
                        // Menus and headers contain duplicate tracks and unrelated destinations.
                        "menu" | "background" => {}
                        "musicCardShelfRenderer" => {
                            // Top songs under an artist card omit the artist because it is
                            // implied by the card header (e.g. "Song • 4:10" with no
                            // artist run). Remember the card's artist for those tracks.
                            let fallback = child
                                .get("title")
                                .and_then(|t| t.get("runs"))
                                .and_then(Value::as_array)
                                .and_then(|runs| {
                                    runs.iter().find_map(|r| {
                                        let (id, kind) = browse(&r["navigationEndpoint"])?;
                                        (kind == "MUSIC_PAGE_TYPE_ARTIST").then(|| {
                                            (
                                                r["text"].as_str().unwrap_or_default().to_owned(),
                                                id.to_owned(),
                                            )
                                        })
                                    })
                                });
                            let before = page.tracks.len();
                            walk(child, page);
                            if let Some((name, id)) = fallback
                                && !name.trim().is_empty()
                            {
                                for track in &mut page.tracks[before..] {
                                    if track.artist == "Unknown artist" {
                                        track.artist = name.clone();
                                        track.artist_id = Some(id.clone());
                                    }
                                }
                            }
                        }
                        "header" | "title" => {
                            let header = child
                                .get("musicCarouselShelfBasicHeaderRenderer")
                                .unwrap_or(child);
                            let title = header.get("title").unwrap_or(header);
                            if let Some(endpoint) = title.pointer("/runs/0/navigationEndpoint")
                                && let Some((id, kind)) = browse(endpoint)
                                && matches!(
                                    kind,
                                    "MUSIC_PAGE_TYPE_ARTIST_DISCOGRAPHY"
                                        | "MUSIC_PAGE_TYPE_PLAYLIST"
                                )
                            {
                                page.collections.push(Collection {
                                    id: id.into(),
                                    title: format!("All {}", text(title).to_lowercase()),
                                    subtitle: "Browse the full collection".into(),
                                    kind: if kind == "MUSIC_PAGE_TYPE_PLAYLIST" {
                                        CollectionKind::Playlist
                                    } else {
                                        CollectionKind::Discography
                                    },
                                    thumbnail: None,
                                    params: endpoint["browseEndpoint"]["params"]
                                        .as_str()
                                        .map(str::to_owned),
                                });
                            }
                        }
                        _ => walk(child, page),
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, page);
                }
            }
            _ => {}
        }
    }
    let mut page = Page::default();
    walk(value, &mut page);
    let mut ids = HashSet::new();
    if unique_tracks {
        page.tracks.retain(|t| ids.insert(t.id.clone()));
    }
    ids.clear();
    page.collections.retain(|c| ids.insert(c.id.clone()));
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watch_recommendations_preserve_metadata_skip_unplayable_and_ignore_counterparts() {
        let primary = json!({
            "videoId": "dQw4w9WgXcQ",
            "title": {"runs": [{"text": "Song"}]},
            "lengthText": {"runs": [{"text": "3:33"}]},
            "longBylineText": {"runs": [
                {"text": "Artist", "navigationEndpoint": {"browseEndpoint": {
                    "browseId": "UCartist", "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}
                    }
                }}},
                {"text": "Album", "navigationEndpoint": {"browseEndpoint": {
                    "browseId": "MPREalbum", "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}
                    }
                }}}
            ]}
        });
        let counterpart = json!({"videoId": "hTWKbfoikeg", "title": {"simpleText": "Other video"}});
        let response = json!({"contents": {"playlistPanelRenderer": {"contents": [
            {"playlistPanelVideoWrapperRenderer": {
                "primaryRenderer": {"playlistPanelVideoRenderer": primary},
                "counterpart": [{"counterpartRenderer": {"playlistPanelVideoRenderer": counterpart}}]
            }},
            {"playlistPanelVideoRenderer": primary},
            {"playlistPanelVideoRenderer": {"videoId": "4NRXx6U8ABQ", "title": {"simpleText": "Video"}, "shortBylineText": {"runs": [{"text": "Uploader"}]}}},
            {"playlistPanelVideoRenderer": {"videoId": "hTWKbfoikeg", "title": {"simpleText": "Unavailable"}, "unplayableText": {"simpleText": "Unavailable"}}},
            {"playlistPanelVideoRenderer": {"videoId": "JGwWNGJdvx8", "title": {"simpleText": "Blocked"}, "musicItemRendererDisplayPolicy": "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT"}},
            {"playlistPanelVideoRenderer": {"videoId": "invalid", "title": {"simpleText": "Invalid"}}},
            {"playlistPanelVideoRenderer": {"videoId": "RgKAFK5djSk", "title": {"simpleText": " "}}},
            {"automixPreviewVideoRenderer": {"playlistPanelVideoRenderer": counterpart}}
        ]}}});
        let tracks = parse_recommendations(&response).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, "dQw4w9WgXcQ");
        assert_eq!(tracks[0].duration_seconds, Some(213));
        assert_eq!(tracks[0].artist, "Artist");
        assert_eq!(tracks[0].artist_id.as_deref(), Some("UCartist"));
        assert_eq!(tracks[0].album.as_deref(), Some("Album"));
        assert_eq!(tracks[0].album_id.as_deref(), Some("MPREalbum"));
        assert_eq!(tracks[1].artist, "Uploader");
        assert!(parse_recommendations(&json!({})).is_err());
        assert!(
            parse_recommendations(&json!({"playlistPanelRenderer": {"contents": []}})).is_err()
        );
        assert!(
            Client::new()
                .recommendations("invalid")
                .unwrap_err()
                .contains("invalid track")
        );
    }
    #[test]
    fn watch_recommendations_are_bounded_to_one_page() {
        let contents: Vec<_> = (0..60)
            .map(|id| {
                json!({"playlistPanelVideoRenderer": {
                    "videoId": format!("{id:011}"), "title": {"simpleText": "Song"}
                }})
            })
            .collect();
        let tracks =
            parse_recommendations(&json!({"playlistPanelRenderer": {"contents": contents}}))
                .unwrap();
        assert_eq!(tracks.len(), 50);
    }
    #[test]
    #[ignore = "requires live YouTube Music access"]
    fn live_recommendations_single_page() {
        let start = std::time::Instant::now();
        let tracks = Client::new().recommendations("4NRXx6U8ABQ").unwrap();
        eprintln!(
            "Loaded {} recommendation candidates in {:?}",
            tracks.len(),
            start.elapsed()
        );
        assert!(tracks.len() > 1 && tracks.len() <= 50);
        assert!(
            tracks
                .iter()
                .any(|track| track.id != "4NRXx6U8ABQ" && track.artist != "Unknown artist")
        );
    }
    #[test]
    fn parses_typed_results_metadata_and_continuations() {
        let response = json!({"contents": [
            {"musicResponsiveListItemRenderer": {"playlistItemData":{"videoId":"dQw4w9WgXcQ"}, "flexColumns":[
                {"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Song"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer":{"text":{"runs":[{"text":"Artist", "navigationEndpoint":{"browseEndpoint":{"browseId":"UCartist","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}},{"text":"3:33"}]}}}]}},
            {"musicTwoRowItemRenderer":{"title":{"runs":[{"text":"Album"}]},"navigationEndpoint":{"browseEndpoint":{"browseId":"MPREalbum","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}}},
            {"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":"more"}}}}
        ]});
        let page = parse_page(&response);
        assert_eq!(page.tracks[0].artist_id.as_deref(), Some("UCartist"));
        assert_eq!(page.tracks[0].duration_seconds, Some(213));
        assert_eq!(page.collections[0].kind, CollectionKind::Album);
        assert_eq!(page.continuation.as_deref(), Some("more"));
    }
    #[test]
    fn video_results_use_the_uploader_channel_for_the_artist() {
        // Video uploads use USER_CHANNEL rather than an artist page, and some
        // catalog rows omit the page type entirely.
        for kind in [Some("MUSIC_PAGE_TYPE_USER_CHANNEL"), None] {
            let mut channel = json!({"text": " Timeless Pop Music ", "navigationEndpoint": {
                "browseEndpoint": {"browseId": "UC_hkrook3LFBIA0kfpDwrgg"}
            }});
            if let Some(kind) = kind {
                channel["navigationEndpoint"]["browseEndpoint"]["browseEndpointContextSupportedConfigs"] = json!({
                    "browseEndpointContextMusicConfig": {"pageType": kind}
                });
            }
            let title = json!({"runs": [{"text": "Top Hits 2026", "navigationEndpoint": {
                "watchEndpoint": {"videoId": "Wjeb9ybZfN8"}
            }}]});
            let subtitle = json!({"runs": [
                {"text": "Video"}, {"text": " • "}, channel,
                {"text": " • "}, {"text": "30K views"}
            ]});
            let response = json!({"contents": [
                {"musicResponsiveListItemRenderer": {
                    "playlistItemData": {"videoId": "Wjeb9ybZfN8"},
                    "flexColumns": [
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": title}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": subtitle}}
                    ]
                }},
                {"musicTwoRowItemRenderer": {"title": title, "subtitle": subtitle}}
            ]});
            let page = parse_contents(&response, false);
            assert_eq!(page.tracks.len(), 2);
            for track in page.tracks {
                assert_eq!(track.artist, "Timeless Pop Music");
                assert_eq!(track.artist_id.as_deref(), Some("UC_hkrook3LFBIA0kfpDwrgg"));
                assert!(track.duration_seconds.is_none());
            }
        }
    }

    #[test]
    fn uploader_fallback_keeps_artist_metadata_and_ignores_other_links() {
        let run = |name, id, kind| {
            json!({"text": name, "navigationEndpoint": {
                "browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {
                    "browseEndpointContextMusicConfig": {"pageType": kind}
                }}
            }})
        };
        let artist = run("Artist", "UCartist", "MUSIC_PAGE_TYPE_ARTIST");
        let channel = run("Uploader", "UCchannel", "MUSIC_PAGE_TYPE_USER_CHANNEL");
        let album = run("Album", "MPREalbum", "MUSIC_PAGE_TYPE_ALBUM");
        let playlist = run("Playlist", "VLplaylist", "MUSIC_PAGE_TYPE_PLAYLIST");
        let blank = run("  ", "UCblank", "MUSIC_PAGE_TYPE_USER_CHANNEL");
        for runs in [
            json!([channel, artist, album]),
            json!([album, playlist, blank]),
        ] {
            let response = json!({"musicTwoRowItemRenderer": {
                "title": {"runs": [{"text": "Song", "navigationEndpoint": {
                    "watchEndpoint": {"videoId": "dQw4w9WgXcQ"}
                }}]},
                "subtitle": {"runs": runs}
            }});
            let page = parse_page(&response);
            let track = &page.tracks[0];
            assert_eq!(track.album.as_deref(), Some("Album"));
            if runs.as_array().unwrap().contains(&artist) {
                assert_eq!(track.artist, "Artist");
                assert_eq!(track.artist_id.as_deref(), Some("UCartist"));
            } else {
                assert_eq!(track.artist, "Unknown artist");
                assert!(track.artist_id.is_none());
            }
        }
    }

    #[test]
    fn card_shelf_songs_inherit_the_card_artist() {
        let response = json!({"contents": [
            {"musicCardShelfRenderer": {
                "title": {"runs": [{"text": "Barış Demirel", "navigationEndpoint": {"browseEndpoint": {"browseId": "UChYM-XrMbGeSjpPzIHE9u4Q", "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}]},
                "contents": [
                    {"musicResponsiveListItemRenderer": {"playlistItemData": {"videoId": "UkdRteW30g0"}, "flexColumns": [
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Uyku Yok"}]}}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Song"}, {"text": " • "}, {"text": "4:10"}]}}}]}},
                    {"musicResponsiveListItemRenderer": {"playlistItemData": {"videoId": "-sfRUF916jA"}, "flexColumns": [
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Ağlamak İstiyorum"}]}}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Song"}, {"text": " • "}, {"text": "Barış Demirel", "navigationEndpoint": {"browseEndpoint": {"browseId": "UChYM-XrMbGeSjpPzIHE9u4Q", "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}]}}}]}},
                ],
            }},
        ]});
        let page = parse_page(&response);
        assert_eq!(page.tracks.len(), 2);
        assert_eq!(page.tracks[0].title, "Uyku Yok");
        assert_eq!(page.tracks[0].artist, "Barış Demirel");
        assert_eq!(
            page.tracks[0].artist_id.as_deref(),
            Some("UChYM-XrMbGeSjpPzIHE9u4Q")
        );
        assert_eq!(page.tracks[0].duration_seconds, Some(250));
        assert_eq!(page.tracks[1].artist, "Barış Demirel");
        assert_eq!(
            page.tracks[1].artist_id.as_deref(),
            Some("UChYM-XrMbGeSjpPzIHE9u4Q")
        );
    }
    #[test]
    fn album_header_artist_comes_from_strapline_text_one() {
        let response = json!({"contents": [{
            "musicResponsiveHeaderRenderer": {
                "title": {"runs": [{"text": "The Dark Side of the Moon"}]},
                "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, {"text": "1973"}]},
                "straplineTextOne": {"runs": [{"text": "Pink Floyd", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCO6LS_5W7vqG9mALDNzSFug", "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}}]},
            },
        }]});
        assert_eq!(
            header_artist(&response),
            Some(("Pink Floyd", "UCO6LS_5W7vqG9mALDNzSFug"))
        );
    }
    #[test]
    #[ignore = "requires live YouTube Music access"]
    fn live_catalog_search_and_browse() {
        let client = Client::new();
        for (filter, kind) in [
            (SearchFilter::Albums, CollectionKind::Album),
            (SearchFilter::Artists, CollectionKind::Artist),
            (SearchFilter::Playlists, CollectionKind::Playlist),
        ] {
            let page = client.search("Daft Punk", filter, None).unwrap();
            let collection = page
                .collections
                .iter()
                .find(|c| c.kind == kind)
                .expect("typed results");
            let detail = client.browse(collection, None).unwrap();
            assert!(!detail.tracks.is_empty(), "{collection:?}");
            if kind == CollectionKind::Artist {
                let discography = detail
                    .collections
                    .iter()
                    .find(|c| c.kind == CollectionKind::Discography)
                    .expect("full discography link");
                assert!(
                    !client
                        .browse(discography, None)
                        .unwrap()
                        .collections
                        .is_empty()
                );
            }
        }
        let page = client
            .search("Daft Punk", SearchFilter::Songs, None)
            .unwrap();
        assert!(!page.tracks.is_empty());
        let next = client
            .search(
                "Daft Punk",
                SearchFilter::Songs,
                Some(page.continuation.as_deref().expect("search pagination")),
            )
            .unwrap();
        assert!(!next.tracks.is_empty());
        assert!(
            next.tracks
                .iter()
                .any(|track| !page.tracks.iter().any(|old| old.id == track.id))
        );
    }
}
