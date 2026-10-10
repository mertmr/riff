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
    fn request(&self, endpoint: &str, mut body: Value) -> Result<Value, String> {
        body["context"] = json!({"client": {"clientName": "WEB_REMIX",
            "clientVersion": "1.20240918.01.00", "hl": "en", "gl": "US"}});
        let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let mut response = self
            .agent
            .post(format!("https://music.youtube.com/youtubei/v1/{endpoint}"))
            .header("Content-Type", "application/json")
            .header("Origin", "https://music.youtube.com")
            .header("User-Agent", "Mozilla/5.0")
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
    for column in columns
        .iter()
        .skip(1)
        .chain(std::iter::once(&&row["subtitle"]))
    {
        if let Some(runs) = column["runs"].as_array() {
            let mut artists = Vec::new();
            for run in runs {
                if let Some((id, kind)) = browse(&run["navigationEndpoint"]) {
                    if kind == "MUSIC_PAGE_TYPE_ARTIST" {
                        artists.push(run["text"].as_str().unwrap_or_default());
                        if track.artist_id.is_none() {
                            track.artist_id = Some(id.into());
                        }
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
