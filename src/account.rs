use crate::{
    catalog::Page,
    model::{Collection, CollectionKind, Track, is_valid_video_id},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

const YOUTUBE_SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";
const SCOPES: &str = "openid profile https://www.googleapis.com/auth/youtube.readonly";

#[derive(Clone, Default)]
pub struct Library {
    pub name: String,
    pub picture: Option<String>,
    pub likes: Vec<Track>,
    pub subscriptions: Vec<Collection>,
    pub playlists: Vec<Collection>,
}
impl Library {
    pub fn seed(&self) -> Option<&Track> {
        let first = self.likes.first()?;
        let channels: HashSet<&str> = self.subscriptions.iter().map(|c| c.id.as_str()).collect();
        self.likes
            .iter()
            .find(|track| {
                track
                    .artist_id
                    .as_deref()
                    .is_some_and(|id| channels.contains(id))
            })
            .or(Some(first))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Likes,
    Subscriptions,
    Playlists,
}
pub struct State {
    pub library: Option<Library>,
    pub loading: bool,
    pub error: Option<String>,
    pub config: Config,
    pub credentials_json: String,
    pub tab: Tab,
    pub playlist: Option<(Collection, Page)>,
    pub worker: Worker,
}
impl State {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        Self {
            library: None,
            loading: false,
            error: None,
            config: Config::configured(),
            credentials_json: String::new(),
            tab: Tab::Likes,
            playlist: None,
            worker: Worker::new(ctx),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}
impl Config {
    pub fn configured() -> Self {
        Self {
            client_id: std::env::var("RIFF_GOOGLE_CLIENT_ID").unwrap_or_else(|_| {
                option_env!("RIFF_GOOGLE_CLIENT_ID")
                    .unwrap_or_default()
                    .into()
            }),
            client_secret: std::env::var("RIFF_GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| {
                option_env!("RIFF_GOOGLE_CLIENT_SECRET")
                    .unwrap_or_default()
                    .into()
            }),
        }
    }
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text)
            .map_err(|_| "Paste the downloaded Google Desktop app credentials JSON.".to_owned())?;
        let installed = value
            .get("installed")
            .ok_or("Credentials must be for a Desktop app OAuth client.")?;
        let config: Self = serde_json::from_value(installed.clone())
            .map_err(|_| "The credentials JSON has no client ID.".to_owned())?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<(), String> {
        if self.client_id.trim().is_empty()
            || !self.client_id.ends_with(".apps.googleusercontent.com")
        {
            return Err("Configure a Google Desktop app OAuth client first.".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
struct Credentials {
    config: Config,
    refresh_token: String,
}
fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new("dev.riff.google", "account")
        .map_err(|_| "Couldn't open the system credential store.".into())
}
fn load_credentials() -> Result<Option<Credentials>, String> {
    match entry()?.get_password() {
        Ok(json) => serde_json::from_str(&json).map(Some).map_err(|_| {
            "Saved Google credentials are unreadable. Disconnect and sign in again.".into()
        }),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("Couldn't read Google sign-in from the system credential store.".into()),
    }
}
fn save_credentials(credentials: &Credentials) -> Result<(), String> {
    entry()?
        .set_password(
            &serde_json::to_string(credentials)
                .map_err(|_| "Couldn't encode Google credentials.")?,
        )
        .map_err(|_| "Couldn't save Google sign-in in the system credential store.".into())
}
fn delete_credentials() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Couldn't remove Google sign-in from the system credential store. Try disconnecting again.".into()),
    }
}

pub enum Request {
    Restore,
    Connect(Config),
    Refresh,
    Playlist(String, Option<String>),
    Disconnect,
}
pub enum Response {
    Library(Option<Library>),
    Playlist {
        id: String,
        append: bool,
        page: Page,
    },
}
struct Job {
    token: u64,
    request: Request,
}
pub struct Message {
    pub token: u64,
    pub result: Result<Response, String>,
}
pub struct Worker {
    tx: Option<Sender<Job>>,
    pub rx: Receiver<Message>,
    epoch: Arc<AtomicU64>,
    pub token: u64,
    worker: Option<thread::JoinHandle<()>>,
    logout_pending: bool,
}
impl Worker {
    #[cfg(test)]
    pub fn fixture() -> (Self, Sender<Message>) {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (results, rx) = mpsc::channel();
        let worker = thread::spawn(move || while jobs.recv().is_ok() {});
        (
            Self {
                tx: Some(tx),
                rx,
                epoch: Arc::new(AtomicU64::new(0)),
                token: 0,
                worker: Some(worker),
                logout_pending: false,
            },
            results,
        )
    }
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (results, rx) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let cancellation = epoch.clone();
        let worker = thread::spawn(move || {
            let mut client: Option<Client> = None;
            while let Ok(job) = jobs.recv() {
                let canceled = || cancellation.load(Ordering::Relaxed) != job.token;
                // A queued disconnect must remove credentials even if the app
                // closes or a newer request supersedes its UI response.
                if canceled() && !matches!(&job.request, Request::Disconnect) {
                    continue;
                }
                let result = (|| match job.request {
                    Request::Restore => {
                        client = load_credentials()?.map(Client::new);
                        client
                            .as_mut()
                            .map(|c| c.library(&canceled))
                            .transpose()
                            .map(Response::Library)
                    }
                    Request::Connect(config) => {
                        let authorized = authorize(config, &canceled)?;
                        if canceled() {
                            return Err("Sign-in canceled.".into());
                        }
                        save_credentials(&authorized.credentials)?;
                        client = Some(authorized);
                        client
                            .as_mut()
                            .unwrap()
                            .library(&canceled)
                            .map(|l| Response::Library(Some(l)))
                    }
                    Request::Refresh => client
                        .as_mut()
                        .ok_or_else(|| "Sign in with Google first.".to_owned())?
                        .library(&canceled)
                        .map(|l| Response::Library(Some(l))),
                    Request::Playlist(id, continuation) => {
                        let append = continuation.is_some();
                        let page = client
                            .as_mut()
                            .ok_or("Sign in with Google first.")?
                            .playlist(&id, continuation.as_deref())?;
                        Ok(Response::Playlist { id, append, page })
                    }
                    Request::Disconnect => {
                        client = None;
                        delete_credentials()?;
                        Ok(Response::Library(None))
                    }
                })();
                if !canceled() {
                    let _ = results.send(Message {
                        token: job.token,
                        result,
                    });
                    ctx.request_repaint();
                }
            }
        });
        Self {
            tx: Some(tx),
            rx,
            epoch,
            token: 0,
            worker: Some(worker),
            logout_pending: false,
        }
    }
    pub fn request(&mut self, request: Request) -> Result<(), String> {
        self.token = self.token.wrapping_add(1);
        self.logout_pending |= matches!(&request, Request::Disconnect);
        self.epoch.store(self.token, Ordering::Relaxed);
        self.tx
            .as_ref()
            .ok_or("The account worker stopped. Restart Riff.")?
            .send(Job {
                token: self.token,
                request,
            })
            .map_err(|_| "The account worker stopped. Restart Riff.".into())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.tx.take();
        // Flush explicit logout before process exit can discard its disk effect.
        if self.logout_pending
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| "Couldn't generate secure sign-in randomness.".to_owned())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn authorization_url(config: &Config, redirect: &str, state: &str, verifier: &str) -> Url {
    let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("state", state),
        (
            "code_challenge",
            URL_SAFE_NO_PAD
                .encode(Sha256::digest(verifier.as_bytes()))
                .as_str(),
        ),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
    ]);
    url
}
fn callback(target: &str, state: &str) -> Result<Option<String>, String> {
    if !target.starts_with("/?") {
        return Ok(None);
    }
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| "Invalid Google sign-in response.".to_owned())?;
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.iter().filter(|(k, _)| k == "state").count() != 1
        || !pairs.iter().any(|(k, v)| k == "state" && v == state)
    {
        return Ok(None);
    }
    if pairs.iter().any(|(k, _)| k == "error") {
        return Err("Google sign-in was declined. You can try again.".into());
    }
    let codes: Vec<_> = pairs.iter().filter(|(k, _)| k == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() {
        return Err("Google returned no authorization code.".into());
    }
    Ok(Some(codes[0].1.to_string()))
}
fn receive_callback(stream: &mut TcpStream, state: &str) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Couldn't read the sign-in response.")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Couldn't respond to the browser.")?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < 8192 && !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| "Couldn't read the sign-in response.")?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let text = std::str::from_utf8(&bytes).unwrap_or_default();
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.strip_suffix(" HTTP/1.1"));
    let result = target.map_or(Ok(None), |t| callback(t, state));
    let (status, body) = match &result {
        Ok(Some(_)) => (
            "200 OK",
            "Google sign-in received. Return to Riff to finish loading your account.",
        ),
        Err(_) => (
            "400 Bad Request",
            "Sign-in was declined or incomplete. Return to Riff and try again.",
        ),
        _ => (
            "404 Not Found",
            "This is not a valid Riff sign-in response.",
        ),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    result
}
fn authorize(config: Config, canceled: &impl Fn() -> bool) -> Result<Client, String> {
    config.validate()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "Couldn't open the local Google sign-in callback.")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Couldn't start Google sign-in.")?;
    let redirect = format!(
        "http://127.0.0.1:{}/",
        listener
            .local_addr()
            .map_err(|_| "Couldn't read the sign-in callback address.")?
            .port()
    );
    let state = random()?;
    let verifier = random()?;
    let url = authorization_url(&config, &redirect, &state, &verifier);
    webbrowser::open(url.as_str()).map_err(|_| "Couldn't open your browser for Google sign-in.")?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let code = loop {
        if canceled() {
            return Err("Sign-in canceled.".into());
        }
        if Instant::now() >= deadline {
            return Err("Google sign-in timed out. Try again.".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(code) = receive_callback(&mut stream, &state)? {
                    break code;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(_) => return Err("Couldn't receive Google sign-in.".into()),
        }
    };
    let agent = agent();
    let value = token_request(
        &agent,
        &config,
        &[
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
        ],
    )?;
    check_scope(&value)?;
    let refresh_token = value["refresh_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Google returned no refresh token. Sign in again.")?
        .to_owned();
    let mut client = Client::new(Credentials {
        config,
        refresh_token,
    });
    client.apply_token(&value)?;
    Ok(client)
}
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .max_idle_connections(1)
        .http_status_as_error(false)
        .build()
        .into()
}
fn response(mut response: ureq::http::Response<ureq::Body>) -> Result<Value, String> {
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|_| "Couldn't read the Google response.")?;
    let value: Value =
        serde_json::from_str(&text).map_err(|_| "Google returned an unreadable response.")?;
    match status {
        200..=299 => Ok(value),
        401 => Err("Google sign-in expired. Disconnect and sign in again.".into()),
        403 => Err("Google denied account access. Enable YouTube Data API v3 for the OAuth project and grant YouTube access when signing in; the API quota may also be exhausted.".into()),
        _ if value["error"] == "invalid_grant" => Err("Google sign-in was revoked or expired. Disconnect and sign in again.".into()),
        _ => Err(format!("Google account request failed (HTTP {status}). Try again or check the OAuth Desktop app credentials.")),
    }
}
fn token_request(
    agent: &ureq::Agent,
    config: &Config,
    fields: &[(&str, &str)],
) -> Result<Value, String> {
    let mut form = fields.to_vec();
    form.push(("client_id", &config.client_id));
    if !config.client_secret.is_empty() {
        form.push(("client_secret", &config.client_secret));
    }
    response(
        agent
            .post("https://oauth2.googleapis.com/token")
            .send_form(form)
            .map_err(|_| "Couldn't reach Google sign-in. Check your connection.")?,
    )
}
fn check_scope(value: &Value) -> Result<(), String> {
    if value
        .get("scope")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.split_whitespace().any(|scope| scope == YOUTUBE_SCOPE))
    {
        return Err(
            "YouTube account access wasn't granted. Sign in again and allow YouTube access.".into(),
        );
    }
    Ok(())
}
struct Client {
    credentials: Credentials,
    agent: ureq::Agent,
    access_token: String,
    expires: Instant,
}
impl Client {
    fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            agent: agent(),
            access_token: String::new(),
            expires: Instant::now(),
        }
    }
    fn apply_token(&mut self, value: &Value) -> Result<(), String> {
        check_scope(value)?;
        self.access_token = value["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or("Google returned no access token.")?
            .into();
        self.expires = Instant::now()
            + Duration::from_secs(
                value["expires_in"]
                    .as_u64()
                    .unwrap_or(3600)
                    .saturating_sub(60)
                    .min(86400),
            );
        Ok(())
    }
    fn get(&mut self, endpoint: &str, parameters: &[(&str, &str)]) -> Result<Value, String> {
        if Instant::now() >= self.expires {
            let value = token_request(
                &self.agent,
                &self.credentials.config,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &self.credentials.refresh_token),
                ],
            )?;
            self.apply_token(&value)?;
        }
        let mut url = Url::parse(&format!("https://www.googleapis.com/{endpoint}")).unwrap();
        url.query_pairs_mut()
            .extend_pairs(parameters.iter().copied());
        response(
            self.agent
                .get(url.as_str())
                .header("Authorization", &format!("Bearer {}", self.access_token))
                .call()
                .map_err(|_| "Couldn't reach your YouTube account. Check your connection.")?,
        )
    }
    fn all<T>(
        &mut self,
        endpoint: &str,
        parameters: &[(&str, &str)],
        parse: impl Fn(&Value) -> Option<T>,
        canceled: &impl Fn() -> bool,
    ) -> Result<Vec<T>, String> {
        collect_pages(
            |next| {
                let mut params = parameters.to_vec();
                params.push(("maxResults", "50"));
                if let Some(token) = next {
                    params.push(("pageToken", token));
                }
                self.get(endpoint, &params)
            },
            parse,
            canceled,
        )
    }
    fn library(&mut self, canceled: &impl Fn() -> bool) -> Result<Library, String> {
        let profile = self.get("oauth2/v3/userinfo", &[])?;
        let likes = self.all(
            "youtube/v3/videos",
            &[("part", "snippet,contentDetails"), ("myRating", "like"),
                ("fields", "nextPageToken,items(id,snippet(title,channelTitle,channelId),contentDetails(duration))")],
            video, canceled,
        )?;
        let subscriptions = self.all(
            "youtube/v3/subscriptions",
            &[("part", "snippet"), ("mine", "true"),
                ("fields", "nextPageToken,items(snippet(title,resourceId(channelId),thumbnails(medium(url))))")],
            |item| collection(item, true), canceled,
        )?;
        let playlists = self.all(
            "youtube/v3/playlists",
            &[("part", "snippet,contentDetails"), ("mine", "true"),
                ("fields", "nextPageToken,items(id,snippet(title,thumbnails(medium(url))),contentDetails(itemCount))")],
            |item| collection(item, false), canceled,
        )?;
        Ok(Library {
            name: profile["name"]
                .as_str()
                .unwrap_or("Google account")
                .to_owned(),
            picture: profile["picture"].as_str().map(str::to_owned),
            likes: unique_tracks(likes),
            subscriptions,
            playlists,
        })
    }
    fn playlist(&mut self, id: &str, token: Option<&str>) -> Result<Page, String> {
        let mut params = vec![
            ("part", "snippet,contentDetails"),
            ("playlistId", id),
            ("maxResults", "50"),
            (
                "fields",
                "nextPageToken,items(snippet(title,videoOwnerChannelTitle,videoOwnerChannelId),contentDetails(videoId))",
            ),
        ];
        if let Some(token) = token {
            params.push(("pageToken", token));
        }
        let value = self.get("youtube/v3/playlistItems", &params)?;
        let items = account_items(&value)?;
        let tracks = items
            .iter()
            .filter_map(|item| {
                let snippet = &item["snippet"];
                let id = item["contentDetails"]["videoId"].as_str()?;
                let title = snippet["title"].as_str()?;
                if !is_valid_video_id(id) || matches!(title, "Private video" | "Deleted video") {
                    return None;
                }
                Some(Track {
                    id: id.into(),
                    title: title.into(),
                    artist: snippet["videoOwnerChannelTitle"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    artist_id: snippet["videoOwnerChannelId"].as_str().map(str::to_owned),
                    ..Default::default()
                })
            })
            .collect();
        Ok(Page {
            tracks,
            continuation: continuation(&value),
            ..Default::default()
        })
    }
}
fn collect_pages<T>(
    mut fetch: impl FnMut(Option<&str>) -> Result<Value, String>,
    parse: impl Fn(&Value) -> Option<T>,
    canceled: &impl Fn() -> bool,
) -> Result<Vec<T>, String> {
    let mut items = Vec::new();
    let mut next = None::<String>;
    let mut seen = HashSet::new();
    loop {
        if canceled() {
            return Err("Account loading canceled.".into());
        }
        let value = fetch(next.as_deref())?;
        items.extend(account_items(&value)?.iter().filter_map(&parse));
        next = continuation(&value);
        let Some(token) = &next else {
            return Ok(items);
        };
        if !seen.insert(token.clone()) || seen.len() > 1000 {
            return Err("YouTube account pagination didn't finish. Try syncing again.".into());
        }
    }
}
fn account_items(value: &Value) -> Result<&[Value], String> {
    match value.get("items") {
        Some(Value::Array(items)) => Ok(items),
        None if value.is_object() && value.get("error").is_none() => Ok(&[]),
        _ => Err("YouTube returned unreadable account items.".into()),
    }
}
fn continuation(value: &Value) -> Option<String> {
    value["nextPageToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn unique_tracks(tracks: Vec<Track>) -> Vec<Track> {
    let mut ids = HashSet::new();
    tracks
        .into_iter()
        .filter(|t| ids.insert(t.id.clone()))
        .collect()
}
fn video(item: &Value) -> Option<Track> {
    let id = item["id"].as_str()?;
    let snippet = &item["snippet"];
    let title = snippet["title"].as_str()?;
    if !is_valid_video_id(id) || title.trim().is_empty() {
        return None;
    }
    Some(Track {
        id: id.into(),
        title: title.into(),
        artist: snippet["channelTitle"].as_str().unwrap_or_default().into(),
        artist_id: snippet["channelId"].as_str().map(str::to_owned),
        duration_seconds: item["contentDetails"]["duration"]
            .as_str()
            .and_then(duration),
        ..Default::default()
    })
}
fn collection(item: &Value, subscription: bool) -> Option<Collection> {
    let snippet = &item["snippet"];
    let id = if subscription {
        snippet["resourceId"]["channelId"].as_str()?
    } else {
        item["id"].as_str()?
    };
    Some(Collection {
        id: id.into(),
        title: snippet["title"].as_str()?.into(),
        subtitle: if subscription {
            "Subscribed channel".into()
        } else {
            format!(
                "{} videos",
                item["contentDetails"]["itemCount"]
                    .as_u64()
                    .unwrap_or_default()
            )
        },
        kind: if subscription {
            CollectionKind::Artist
        } else {
            CollectionKind::Playlist
        },
        thumbnail: snippet["thumbnails"]["medium"]["url"]
            .as_str()
            .map(str::to_owned),
        params: None,
    })
}
fn duration(value: &str) -> Option<u64> {
    let body = value.strip_prefix("PT")?;
    let mut number = 0u64;
    let mut total = 0u64;
    let mut digits = false;
    for c in body.chars() {
        if let Some(digit) = c.to_digit(10) {
            number = number.checked_mul(10)?.checked_add(digit.into())?;
            digits = true;
        } else {
            if !digits {
                return None;
            }
            let multiplier = match c {
                'H' => 3600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
            total = total.checked_add(number.checked_mul(multiplier)?)?;
            number = 0;
            digits = false;
        }
    }
    (!body.is_empty() && !digits).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn pages_follow_tokens_preserve_order_and_fail_without_partial_results() {
        let mut calls = 0;
        let items = collect_pages(
            |next| {
                calls += 1;
                match calls {
                    1 => {
                        assert!(next.is_none());
                        Ok(json!({"items":[1,2],"nextPageToken":"next + /"}))
                    }
                    2 => {
                        assert_eq!(next, Some("next + /"));
                        Ok(json!({"items":[3]}))
                    }
                    _ => panic!("Unexpected page"),
                }
            },
            Value::as_u64,
            &|| false,
        )
        .unwrap();
        assert_eq!(items, [1, 2, 3]);
        let error = collect_pages(
            |_| Ok(json!({"items":[1],"nextPageToken":"repeated"})),
            Value::as_u64,
            &|| false,
        );
        assert!(error.is_err());
        let error = collect_pages(
            |_| panic!("Canceled work must not fetch"),
            Value::as_u64,
            &|| true,
        );
        assert!(error.is_err());
        let error = collect_pages(
            |_| Ok(json!({"error":"unexpected"})),
            Value::as_u64,
            &|| false,
        );
        assert!(error.is_err());
    }
    #[test]
    fn browser_callback_handles_fragmented_http_and_responds_without_echoing_code() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let browser = thread::spawn(move || {
            let mut socket = TcpStream::connect(address).unwrap();
            socket.write_all(b"GET /?state=expected&code=top").unwrap();
            socket
                .write_all(b"secret HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(!response.contains("topsecret"));
            assert!(response.contains("Cache-Control: no-store"));
        });
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(
            receive_callback(&mut socket, "expected").unwrap(),
            Some("topsecret".into())
        );
        drop(socket);
        browser.join().unwrap();
    }
    #[test]
    fn pkce_uses_s256_and_encodes_scopes_and_redirect() {
        let config = Config {
            client_id: "riff.apps.googleusercontent.com".into(),
            ..Default::default()
        };
        let url = authorization_url(
            &config,
            "http://127.0.0.1:1234/",
            "random-state",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        );
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query["code_challenge"],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:1234/");
        assert_eq!(query["scope"], SCOPES);
        assert_eq!(query["state"], "random-state");
    }
    #[test]
    fn callback_rejects_wrong_state_paths_duplicates_and_denials() {
        assert_eq!(
            callback("/?state=expected&code=a%2Bb", "expected").unwrap(),
            Some("a+b".into())
        );
        for target in [
            "/?state=wrong&code=secret",
            "/?code=secret",
            "/favicon.ico",
            "//evil/?state=expected&code=secret",
            "/?state=expected&state=other&code=a",
        ] {
            assert!(callback(target, "expected").unwrap().is_none());
        }
        assert!(callback("/?state=expected&error=access_denied", "expected").is_err());
        assert!(callback("/?state=expected&code=a&code=b", "expected").is_err());
    }
    #[test]
    fn parses_account_items_and_keeps_account_ids() {
        let track = video(&json!({"id":"dQw4w9WgXcQ", "snippet":{"title":"Song","channelTitle":"Artist","channelId":"UCartist"},"contentDetails":{"duration":"PT1H2M3S"}})).unwrap();
        assert_eq!(track.duration_seconds, Some(3723));
        assert_eq!(track.artist_id.as_deref(), Some("UCartist"));
        assert!(video(&json!({"id":"invalid","snippet":{"title":"Song"}})).is_none());
        assert_eq!(unique_tracks(vec![track.clone(), track]).len(), 1);
        let subscribed = collection(&json!({"id":"subscription-id","snippet":{"title":"Artist","resourceId":{"channelId":"UCartist"}}}), true).unwrap();
        assert_eq!(subscribed.id, "UCartist");
        assert_eq!(duration("PT213S"), Some(213));
        assert_eq!(duration("PT0S"), Some(0));
        for value in ["", "PT", "PT1", "PT1D", "PT18446744073709551615H"] {
            assert_eq!(duration(value), None);
        }
    }
    #[test]
    fn only_accepts_desktop_credentials_and_required_scope() {
        assert!(
            Config::from_json(r#"{"web":{"client_id":"x.apps.googleusercontent.com"}}"#).is_err()
        );
        assert!(Config::from_json(r#"{"installed":{"client_id":"x.apps.googleusercontent.com","client_secret":"test"}}"#).is_ok());
        assert!(check_scope(&json!({"scope":"openid profile"})).is_err());
        assert!(check_scope(&json!({"scope":SCOPES})).is_ok());
    }
    #[test]
    fn account_preferences_prioritize_likes_from_subscribed_channels() {
        let first = Track {
            id: "dQw4w9WgXcQ".into(),
            title: "First like".into(),
            ..Default::default()
        };
        let followed = Track {
            artist_id: Some("UCfollowed".into()),
            ..first.clone()
        };
        let mut library = Library {
            likes: vec![first.clone(), followed.clone()],
            subscriptions: vec![Collection {
                id: "UCfollowed".into(),
                title: "Followed channel".into(),
                subtitle: String::new(),
                kind: CollectionKind::Artist,
                thumbnail: None,
                params: None,
            }],
            ..Default::default()
        };
        assert_eq!(library.seed(), Some(&followed));
        library.subscriptions.clear();
        assert_eq!(library.seed(), Some(&first));
        library.likes.clear();
        assert!(library.seed().is_none());
    }
    #[test]
    fn empty_accounts_are_valid_and_access_tokens_remain_out_of_saved_credentials() {
        assert!(account_items(&json!({})).unwrap().is_empty());
        assert!(account_items(&json!({"items":[]})).unwrap().is_empty());
        assert!(account_items(&json!({"items":"invalid"})).is_err());
        let mut client = Client::new(Credentials {
            config: Config::default(),
            refresh_token: "refresh".into(),
        });
        assert!(client.apply_token(&json!({"expires_in":3600})).is_err());
        client
            .apply_token(
                &json!({"access_token":"memory-only-access", "expires_in":3600, "scope":SCOPES}),
            )
            .unwrap();
        assert!(client.expires > Instant::now() + Duration::from_secs(3500));
        assert_eq!(client.access_token, "memory-only-access");
        let stored = serde_json::to_string(&client.credentials).unwrap();
        assert!(!stored.contains("memory-only-access"));
        assert!(stored.contains("refresh"));
    }
}
