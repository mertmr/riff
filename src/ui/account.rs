use super::{
    theme::*,
    widgets::{self, Row},
};
use crate::{
    account::Tab,
    app::{Action, MusicApp, Playback},
    model::Track,
};
use egui::{self, RichText, Ui};

pub fn show(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    widgets::heading(
        ui,
        "Your YouTube account",
        "Your likes, subscribed channels, and playlists in Riff.",
    );
    ui.add_space(16.);
    if app.account.library.is_some() || app.account.loading || app.account.error.is_some() {
        ui.horizontal_wrapped(|ui| {
            if let Some(library) = &app.account.library {
                ui.label(RichText::new(&library.name).font(bold(18.)));
            }
            if ui
                .add_enabled(!app.account.loading, egui::Button::new("Sync account"))
                .clicked()
            {
                actions.push(Action::RefreshAccount);
            }
            if ui
                .button(if app.account.loading {
                    "Cancel / disconnect"
                } else {
                    "Disconnect"
                })
                .clicked()
            {
                actions.push(Action::DisconnectAccount);
            }
        });
    }
    if app.account.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Finish sign-in in your browser, or wait for account data to load…");
        });
    }
    if let Some(error) = &app.account.error {
        ui.add_space(8.);
        ui.label(RichText::new(error).color(ERROR));
    }
    ui.add_space(12.);
    playback_settings(app, ui, actions);
    ui.add_space(12.);
    if app.account.library.is_none() {
        ui.label("Sign in with Google to fetch your YouTube likes, subscriptions, and private playlists.");
        ui.label(RichText::new("Riff uses these likes for recommendations. Your Google password stays in your browser.").color(MUTED));
        ui.add_space(12.);
        if ui
            .add_enabled_ui(!app.account.loading, |ui| {
                widgets::button(ui, "Sign in with Google", true).clicked()
            })
            .inner
        {
            actions.push(Action::ConnectAccount);
        }
        ui.add_space(12.);
        egui::ScrollArea::vertical().id_salt("account_setup").auto_shrink([false, false]).show(ui, |ui| {
            egui::CollapsingHeader::new("Google sign-in setup")
                .default_open(app.account.config.client_id.is_empty())
                .show(ui, |ui| {
                    ui.label("Create a Desktop app OAuth client, enable YouTube Data API v3, and add your Google account as a test user if the consent screen is in testing.");
                    ui.hyperlink_to("Open Google OAuth clients", "https://console.cloud.google.com/auth/clients");
                    ui.hyperlink_to("Enable YouTube Data API v3", "https://console.cloud.google.com/apis/library/youtube.googleapis.com");
                    ui.label("Paste the downloaded Desktop app credentials JSON:");
                    ui.add(egui::TextEdit::multiline(&mut app.account.credentials_json)
                        .password(true).desired_rows(3).desired_width(ui.available_width())
                        .hint_text("Desktop app credentials JSON"));
                    if !app.account.config.client_id.is_empty() { ui.label(RichText::new("Google client configured.").color(MUTED)); }
                });
            ui.add_space(16.);
            ui.label(RichText::new("YouTube Music's listening history, uploads, and personalized home feed are not available through this account connection. Favorites and playlist edits in Riff are saved locally.").color(MUTED));
        });
        return;
    }
    if let Some((collection, page)) = &app.account.playlist {
        ui.horizontal_wrapped(|ui| {
            if ui.button("← Account playlists").clicked() {
                actions.push(Action::AccountBack);
            }
            ui.label(RichText::new(&collection.title).font(bold(20.)));
            if ui
                .add_enabled(
                    !page.tracks.is_empty(),
                    egui::Button::new("Play loaded songs"),
                )
                .clicked()
            {
                actions.push(Action::PlayTracks(page.tracks.clone()));
            }
            if ui
                .add_enabled(
                    !app.account.loading,
                    egui::Button::new(if page.continuation.is_some() {
                        "Load more"
                    } else {
                        "Reload"
                    }),
                )
                .clicked()
            {
                if page.continuation.is_some() {
                    actions.push(Action::AccountMore);
                } else {
                    actions.push(Action::AccountPlaylist(collection.clone()));
                }
            }
        });
        ui.label(RichText::new(format!("{} videos loaded", page.tracks.len())).color(MUTED));
        let tracks = &page.tracks;
        rows(
            ui,
            tracks,
            &mut app.artwork,
            &app.current,
            app.playback,
            &app.favorites,
            &app.saved_tracks,
            app.queue_revision,
            app.selected,
            actions,
        );
        return;
    }
    let library = app.account.library.as_ref().unwrap();
    ui.horizontal_wrapped(|ui| {
        for (tab, label, count) in [
            (Tab::Likes, "Liked videos", library.likes.len()),
            (
                Tab::Subscriptions,
                "Subscriptions",
                library.subscriptions.len(),
            ),
            (Tab::Playlists, "YouTube playlists", library.playlists.len()),
        ] {
            if ui
                .selectable_label(app.account.tab == tab, format!("{label} ({count})"))
                .clicked()
            {
                actions.push(Action::AccountTab(tab));
            }
        }
    });
    ui.add_space(12.);
    match app.account.tab {
        Tab::Likes => {
            ui.label(
                RichText::new("Likes from YouTube. Hearts in Riff save local favorites.")
                    .color(MUTED),
            );
            if ui
                .add_enabled(
                    !library.likes.is_empty(),
                    egui::Button::new("Play liked videos"),
                )
                .clicked()
            {
                actions.push(Action::PlayTracks(library.likes.clone()));
            }
            rows(
                ui,
                &library.likes,
                &mut app.artwork,
                &app.current,
                app.playback,
                &app.favorites,
                &app.saved_tracks,
                app.queue_revision,
                app.selected,
                actions,
            );
        }
        Tab::Subscriptions | Tab::Playlists => {
            let items = if app.account.tab == Tab::Subscriptions {
                &library.subscriptions
            } else {
                &library.playlists
            };
            if items.is_empty() {
                ui.label("No account items found.");
            }
            egui::ScrollArea::vertical()
                .id_salt(("account_collections", app.account.tab == Tab::Playlists))
                .auto_shrink([false, false])
                .show_rows(ui, 64., items.len(), |ui, range| {
                    for index in range {
                        let item = &items[index];
                        ui.push_id(&item.id, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button(&item.title).clicked() {
                                    actions.push(if app.account.tab == Tab::Playlists {
                                        Action::AccountPlaylist(item.clone())
                                    } else {
                                        Action::AccountSearch(item.title.clone())
                                    });
                                }
                                ui.label(RichText::new(&item.subtitle).color(MUTED));
                            });
                        });
                    }
                });
        }
    }
    ui.add_space(12.);
    playback_settings(app, ui, actions);
}

fn playback_settings(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.label(RichText::new("Playback").font(bold(15.)));
    let mut cache = app.cache_audio;
    if ui
        .checkbox(&mut cache, "Cache audio on disk")
        .on_hover_text(
            "When off, audio is streamed only and never saved to disk. Uses more data on replays.",
        )
        .changed()
    {
        actions.push(Action::SetCacheAudio(cache));
    }
    ui.label(
        RichText::new(if app.cache_audio {
            "Fully played songs are cached for up to 512 MB to replay offline."
        } else {
            "Stream-only mode: nothing is saved, every replay re-downloads."
        })
        .color(MUTED),
    );
}

#[allow(clippy::too_many_arguments)]
fn rows(
    ui: &mut Ui,
    tracks: &[Track],
    artwork: &mut widgets::Artwork,
    current: &Option<Track>,
    playback: Playback,
    favorites: &[Track],
    saved: &[Track],
    revision: u64,
    selected: Option<usize>,
    actions: &mut Vec<Action>,
) {
    if tracks.is_empty() {
        ui.label("No videos loaded.");
    }
    egui::ScrollArea::vertical()
        .id_salt("account_tracks")
        .auto_shrink([false, false])
        .show_rows(ui, 64., tracks.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.;
            for index in range {
                let track = &tracks[index];
                widgets::track_row(
                    ui,
                    artwork,
                    Row {
                        track,
                        index,
                        current: current.as_ref().is_some_and(|t| t.id == track.id),
                        playing: playback == Playback::Playing,
                        liked: favorites.iter().any(|t| t.id == track.id),
                        selected: selected == Some(index),
                        queue: false,
                        collection: false,
                        radio: false,
                        queue_range: 0..0,
                        saved: saved.iter().any(|t| t.id == track.id),
                        queue_revision: revision,
                        playlist: None,
                    },
                    actions,
                );
            }
        });
}
