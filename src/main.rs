// Ohm Player - main entry point.
// Wires the Slint UI to the rodio audio engine and the SQLite database.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod db;

use id3::TagLike;
use rand::seq::SliceRandom;
use rodio::source::ChannelVolume;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

slint::include_modules!();

/// A track in the in-memory playback queue / library index.
#[derive(Clone)]
struct Track {
    path: String,
    title: String,
    artist: String,
    album: String,
    /// Heuristic flag: speech content (podcasts, audiobooks, voice notes).
    is_voice: bool,
}

/// Global thread-safe audio state. The rodio `Sink` lives here, guarded by a
/// Mutex and shared through an Arc as required.
struct AudioState {
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    /// Sink of the previous track while it fades out (crossfade).
    fading: Option<Sink>,
    /// Current play order (may be a shuffled copy of `original`).
    queue: Vec<Track>,
    /// Original order as loaded (never mutated by shuffle).
    original: Vec<Track>,
    index: usize,
    shuffle: bool,
    /// 0 = off, 1 = repeat all, 2 = repeat one.
    repeat: i32,
    /// Duration of the current track in seconds (0.0 = unknown).
    duration: f32,
    /// User volume 0..1.
    volume: f32,
    /// Normalization preset: 0 off, 1 quiet, 2 normal, 3 loud.
    normalization: i32,
    mono: bool,
    /// Crossfade length in seconds (0 = off).
    crossfade: i32,
    /// Set while a crossfade advance is in flight to avoid re-triggering.
    crossfading: bool,
    /// Sleep timer deadline.
    sleep_deadline: Option<Instant>,
}

impl AudioState {
    /// Effective sink volume = user volume x normalization gain.
    fn effective_volume(&self) -> f32 {
        let gain = match self.normalization {
            1 => 0.55, // quiet
            2 => 0.85, // normal
            3 => 1.20, // loud
            _ => 1.0,
        };
        (self.volume * gain).clamp(0.0, 1.5)
    }
}

type SharedAudio = Arc<Mutex<AudioState>>;

/// Library cache shared between search / library callbacks (UI thread only).
#[derive(Default)]
struct Library {
    /// All indexed tracks, in the currently selected sort order.
    tracks: Vec<Track>,
    /// Tracks currently shown on the search page.
    search: Vec<Track>,
    /// Album name -> representative path, artist, count (kept sorted).
    albums: Vec<(String, String, i64, String)>,
    artists: Vec<(String, i64)>,
}

type SharedLibrary = Rc<RefCell<Library>>;

// ---------- Metadata helpers ----------

/// Reads ID3 metadata for a file, falling back to the file name.
fn read_track(path: &str) -> Track {
    let tag = id3::Tag::read_from_path(path).ok();
    let file_stem = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let genre = tag
        .as_ref()
        .and_then(|t| t.genre().map(|g| g.to_lowercase()))
        .unwrap_or_default();
    let lower_path = path.to_lowercase();
    let is_voice = ["podcast", "audiobook", "speech", "voice", "voz", "spoken"]
        .iter()
        .any(|k| genre.contains(k) || lower_path.contains(k));
    Track {
        path: path.to_string(),
        title: tag
            .as_ref()
            .and_then(|t| t.title().map(String::from))
            .unwrap_or(file_stem),
        artist: tag
            .as_ref()
            .and_then(|t| t.artist().map(String::from))
            .unwrap_or_else(|| "Artista desconhecido".to_string()),
        album: tag
            .as_ref()
            .and_then(|t| t.album().map(String::from))
            .unwrap_or_default(),
        is_voice,
    }
}

/// Extracts the embedded album art (APIC frame) and decodes it for Slint.
fn load_cover(path: &str) -> slint::Image {
    if let Ok(tag) = id3::Tag::read_from_path(path) {
        if let Some(pic) = tag.pictures().next() {
            if let Ok(img) = image::load_from_memory(&pic.data) {
                let rgba = img.into_rgba8();
                let (w, h) = rgba.dimensions();
                let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    rgba.as_raw(),
                    w,
                    h,
                );
                return slint::Image::from_rgba8(buf);
            }
        }
    }
    slint::Image::default()
}

fn load_cover_from_path(path: Option<&str>) -> slint::Image {
    path.and_then(|p| slint::Image::load_from_path(Path::new(p)).ok())
        .unwrap_or_default()
}

fn format_time(secs: f32) -> SharedString {
    let s = secs.max(0.0) as u64;
    SharedString::from(format!("{}:{:02}", s / 60, s % 60))
}

fn song_item(t: &Track) -> SongItem {
    SongItem {
        title: SharedString::from(t.title.clone()),
        artist: SharedString::from(t.artist.clone()),
        album: SharedString::from(t.album.clone()),
        path: SharedString::from(t.path.clone()),
        is_voice: t.is_voice,
    }
}

// ---------- Playback core ----------

/// Rebuilds the queue side-panel model from the audio state.
fn refresh_queue_model(app: &AppWindow, audio: &SharedAudio) {
    let st = audio.lock().unwrap();
    let items: Vec<QueueItem> = st
        .queue
        .iter()
        .enumerate()
        .map(|(i, t)| QueueItem {
            title: SharedString::from(t.title.clone()),
            artist: SharedString::from(t.artist.clone()),
            playing: i == st.index && st.sink.is_some(),
        })
        .collect();
    drop(st);
    app.set_queue(ModelRc::new(VecModel::from(items)));
}

/// Starts playback of queue[index]: replaces the sink, updates UI metadata
/// and records a scrobble in the database. `fade_in_secs > 0` makes the new
/// track ramp up (used by the crossfade engine).
fn play_at(app: &AppWindow, audio: &SharedAudio, database: &Rc<db::Db>, index: usize, fade_in_secs: f32) {
    let track = {
        let mut st = audio.lock().unwrap();
        if st.queue.is_empty() {
            return;
        }
        let index = index.min(st.queue.len() - 1);
        st.index = index;
        st.crossfading = false;
        let track = st.queue[index].clone();

        // Stop whatever is playing (unless it was moved to the fade-out slot).
        if let Some(old) = st.sink.take() {
            old.stop();
        }

        let file = match File::open(&track.path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Cannot open {}: {e}", track.path);
                return;
            }
        };
        let source = match Decoder::new(BufReader::new(file)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Cannot decode {}: {e}", track.path);
                return;
            }
        };

        // rodio often can't report MP3 duration, so fall back to frame scanning.
        st.duration = source
            .total_duration()
            .or_else(|| mp3_duration::from_path(&track.path).ok())
            .map(|d| d.as_secs_f32())
            .unwrap_or(0.0);

        match Sink::try_new(&st.handle) {
            Ok(sink) => {
                sink.set_volume(st.effective_volume());
                let fade = Duration::from_secs_f32(fade_in_secs.max(0.001));
                let src = source.convert_samples::<f32>().fade_in(fade);
                if st.mono {
                    // Mixes every input channel down to one and plays it on
                    // both stereo channels at equal level.
                    sink.append(ChannelVolume::new(src, vec![0.5, 0.5]));
                } else {
                    sink.append(src);
                }
                sink.play();
                st.sink = Some(sink);
            }
            Err(e) => {
                eprintln!("Audio output error: {e}");
                return;
            }
        }

        app.set_queue_info(SharedString::from(format!(
            "Faixa {} de {}{}",
            index + 1,
            st.queue.len(),
            if st.shuffle { "  ·  aleatório" } else { "" }
        )));
        app.set_duration_text(format_time(st.duration));
        track
    };

    app.set_current_title(SharedString::from(track.title.clone()));
    app.set_current_artist(SharedString::from(track.artist.clone()));
    app.set_current_album(SharedString::from(track.album.clone()));
    app.set_cover_art(load_cover(&track.path));
    app.set_playing(true);
    app.set_progress(0.0);
    app.set_position_text(SharedString::from("0:00"));

    // Scrobble: every playback start goes to the history table (with path,
    // which feeds the Home "recently played" rail and the History page).
    if let Err(e) =
        database.add_scrobble_with_path(&track.title, &track.artist, &track.album, &track.path)
    {
        eprintln!("Failed to record scrobble: {e}");
    }
    refresh_queue_model(app, audio);
}

/// Replaces the queue with `tracks` and starts playing from `start`.
/// Honors the shuffle flag without touching the database order.
fn start_queue(
    app: &AppWindow,
    audio: &SharedAudio,
    database: &Rc<db::Db>,
    tracks: Vec<Track>,
    start: usize,
) {
    if tracks.is_empty() {
        return;
    }
    let begin = {
        let mut st = audio.lock().unwrap();
        st.original = tracks.clone();
        if st.shuffle {
            let mut queue = tracks;
            let clicked = queue.remove(start.min(queue.len() - 1));
            queue.shuffle(&mut rand::thread_rng());
            queue.insert(0, clicked);
            st.queue = queue;
            0
        } else {
            st.queue = tracks;
            start.min(st.queue.len() - 1)
        }
    };
    play_at(app, audio, database, begin, 0.0);
}

// ---------- UI model refresh helpers ----------

fn refresh_playlists(app: &AppWindow, database: &db::Db) {
    let items: Vec<PlaylistItem> = database
        .list_playlists()
        .unwrap_or_default()
        .into_iter()
        .map(|p| PlaylistItem {
            id: p.id as i32,
            name: SharedString::from(p.name),
            cover: load_cover_from_path(p.cover.as_deref()),
            song_count: p.song_count as i32,
        })
        .collect();
    app.set_playlists(ModelRc::new(VecModel::from(items)));
}

fn refresh_songs(app: &AppWindow, database: &db::Db, playlist_id: i64) {
    let items: Vec<SongItem> = database
        .get_songs(playlist_id)
        .unwrap_or_default()
        .iter()
        .map(|path| song_item(&read_track(path)))
        .collect();
    app.set_songs(ModelRc::new(VecModel::from(items)));
}

fn select_playlist(app: &AppWindow, database: &db::Db, playlist_id: i64) {
    let info = database
        .list_playlists()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.id == playlist_id);
    match info {
        Some(p) => {
            app.set_selected_playlist_id(p.id as i32);
            app.set_selected_playlist_name(SharedString::from(p.name));
            app.set_selected_playlist_cover(load_cover_from_path(p.cover.as_deref()));
            refresh_songs(app, database, playlist_id);
        }
        None => {
            app.set_selected_playlist_id(-1);
            app.set_selected_playlist_name(SharedString::default());
            app.set_selected_playlist_cover(slint::Image::default());
            app.set_songs(ModelRc::new(VecModel::from(Vec::<SongItem>::new())));
        }
    }
}

/// Rebuilds the library index (Songs / Albums / Artists tabs) applying the
/// selected sort mode. Also refreshes the Slint models.
fn refresh_library(app: &AppWindow, database: &db::Db, library: &SharedLibrary, sort: i32) {
    let paths = database.library_paths(sort).unwrap_or_default();
    let mut tracks: Vec<Track> = paths.iter().map(|p| read_track(p)).collect();
    if sort == 0 {
        tracks.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    }

    // Group albums and artists.
    let mut albums: Vec<(String, String, i64, String)> = Vec::new();
    let mut artists: Vec<(String, i64)> = Vec::new();
    for t in &tracks {
        if !t.album.is_empty() {
            match albums.iter_mut().find(|(n, _, _, _)| *n == t.album) {
                Some(entry) => entry.2 += 1,
                None => albums.push((t.album.clone(), t.artist.clone(), 1, t.path.clone())),
            }
        }
        match artists.iter_mut().find(|(n, _)| *n == t.artist) {
            Some(entry) => entry.1 += 1,
            None => artists.push((t.artist.clone(), 1)),
        }
    }
    albums.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    artists.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let song_items: Vec<SongItem> = tracks.iter().map(song_item).collect();
    let album_items: Vec<AlbumItem> = albums
        .iter()
        .map(|(name, artist, count, path)| AlbumItem {
            name: SharedString::from(name.clone()),
            artist: SharedString::from(artist.clone()),
            count: *count as i32,
            cover: load_cover(path),
        })
        .collect();
    let artist_items: Vec<ArtistItem> = artists
        .iter()
        .map(|(name, count)| ArtistItem {
            name: SharedString::from(name.clone()),
            count: *count as i32,
        })
        .collect();

    app.set_library_songs(ModelRc::new(VecModel::from(song_items)));
    app.set_albums(ModelRc::new(VecModel::from(album_items)));
    app.set_artists(ModelRc::new(VecModel::from(artist_items)));

    let mut lib = library.borrow_mut();
    lib.tracks = tracks;
    lib.albums = albums;
    lib.artists = artists;
}

/// Runs the on-device search over the cached library index.
fn run_search(app: &AppWindow, library: &SharedLibrary, query: &str, filter: i32) {
    let q = query.trim().to_lowercase();
    let mut lib = library.borrow_mut();
    let results: Vec<Track> = if q.is_empty() {
        Vec::new()
    } else {
        lib.tracks
            .iter()
            .filter(|t| {
                let matches = t.title.to_lowercase().contains(&q)
                    || t.artist.to_lowercase().contains(&q)
                    || t.album.to_lowercase().contains(&q);
                let category = match filter {
                    1 => !t.is_voice,
                    2 => t.is_voice,
                    _ => true,
                };
                matches && category
            })
            .cloned()
            .collect()
    };
    let items: Vec<SongItem> = results.iter().map(song_item).collect();
    app.set_search_results(ModelRc::new(VecModel::from(items)));
    lib.search = results;
}

fn refresh_home(app: &AppWindow, database: &db::Db) {
    let all = database.list_playlists().unwrap_or_default();
    let recent_pls: Vec<PlaylistItem> = database
        .recent_playlists(8)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, name)| {
            all.iter().find(|p| p.id == id).map(|p| PlaylistItem {
                id: id as i32,
                name: SharedString::from(name),
                cover: load_cover_from_path(p.cover.as_deref()),
                song_count: p.song_count as i32,
            })
        })
        .collect();

    let recent_songs: Vec<SongItem> = database
        .recent_songs(10)
        .unwrap_or_default()
        .into_iter()
        .map(|(title, artist, path)| SongItem {
            title: SharedString::from(title),
            artist: SharedString::from(artist),
            album: SharedString::default(),
            path: SharedString::from(path),
            is_voice: false,
        })
        .collect();

    app.set_recent_playlists(ModelRc::new(VecModel::from(recent_pls)));
    app.set_recent_songs(ModelRc::new(VecModel::from(recent_songs)));
}

fn refresh_history(app: &AppWindow, database: &db::Db) {
    let items: Vec<ScrobbleItem> = database
        .recent_scrobbles(100)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            // "YYYY-MM-DD HH:MM:SS" -> date + short time.
            let (date, time) = match s.date.split_once(' ') {
                Some((d, t)) => (d.to_string(), t.chars().take(5).collect::<String>()),
                None => (s.date.clone(), String::new()),
            };
            ScrobbleItem {
                title: SharedString::from(s.title),
                artist: SharedString::from(s.artist),
                album: SharedString::from(s.album),
                date: SharedString::from(date),
                time: SharedString::from(time),
            }
        })
        .collect();
    app.set_history(ModelRc::new(VecModel::from(items)));

    // Last.fm style statistics.
    let to_stats = |rows: Vec<db::StatRow>| -> Vec<StatItem> {
        let max = rows.iter().map(|r| r.count).max().unwrap_or(1).max(1) as f32;
        rows.into_iter()
            .map(|r| StatItem {
                name: SharedString::from(r.name),
                count: r.count as i32,
                ratio: r.count as f32 / max,
            })
            .collect()
    };
    app.set_top_artists(ModelRc::new(VecModel::from(to_stats(
        database.top_artists(5).unwrap_or_default(),
    ))));
    app.set_top_albums(ModelRc::new(VecModel::from(to_stats(
        database.top_albums(5).unwrap_or_default(),
    ))));
    app.set_top_tracks(ModelRc::new(VecModel::from(to_stats(
        database.top_tracks(5).unwrap_or_default(),
    ))));
    app.set_daily_plays(ModelRc::new(VecModel::from(to_stats(
        database.plays_per_day(7).unwrap_or_default(),
    ))));
    app.set_total_plays(database.total_plays().unwrap_or(0) as i32);
    app.set_plays_today(database.plays_today().unwrap_or(0) as i32);
    app.set_unique_artists(database.unique_artists().unwrap_or(0) as i32);
}

// ---------- Settings persistence ----------

fn load_settings(app: &AppWindow, audio: &SharedAudio, database: &db::Db) {
    let getf = |k: &str, d: f32| -> f32 {
        database
            .get_setting(k)
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let geti = |k: &str, d: i32| -> i32 {
        database
            .get_setting(k)
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let volume = getf("volume", 0.7).clamp(0.0, 1.0);
    let crossfade = geti("crossfade", 0).clamp(0, 12);
    let normalization = geti("normalization", 0).clamp(0, 3);
    let mono = geti("mono", 0) != 0;

    app.set_volume(volume);
    app.set_crossfade(crossfade);
    app.set_normalization(normalization);
    app.set_mono(mono);
    app.set_eq_bass(getf("eq_bass", 0.0));
    app.set_eq_mid(getf("eq_mid", 0.0));
    app.set_eq_treble(getf("eq_treble", 0.0));
    app.set_eq_preset(geti("eq_preset", 0));

    let mut st = audio.lock().unwrap();
    st.volume = volume;
    st.crossfade = crossfade;
    st.normalization = normalization;
    st.mono = mono;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = Rc::new(db::Db::open()?);
    let library: SharedLibrary = Rc::new(RefCell::new(Library::default()));

    // The OutputStream must stay alive for the whole program (it is !Send,
    // so it lives on the main thread while the handle goes into the state).
    let (_stream, handle) = OutputStream::try_default()?;

    let audio: SharedAudio = Arc::new(Mutex::new(AudioState {
        handle,
        sink: None,
        fading: None,
        queue: Vec::new(),
        original: Vec::new(),
        index: 0,
        shuffle: false,
        repeat: 0,
        duration: 0.0,
        volume: 0.7,
        normalization: 0,
        mono: false,
        crossfade: 0,
        crossfading: false,
        sleep_deadline: None,
    }));

    let app = AppWindow::new()?;
    load_settings(&app, &audio, &database);
    refresh_playlists(&app, &database);
    refresh_library(&app, &database, &library, 0);
    refresh_home(&app, &database);
    refresh_history(&app, &database);

    // ---------- Transport callbacks ----------

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_play_pause(move || {
            let app = weak.unwrap();
            let st = audio.lock().unwrap();
            if let Some(sink) = &st.sink {
                if sink.is_paused() {
                    sink.play();
                    app.set_playing(true);
                } else {
                    sink.pause();
                    app.set_playing(false);
                }
            }
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_next_track(move || {
            let app = weak.unwrap();
            let next = {
                let st = audio.lock().unwrap();
                if st.queue.is_empty() {
                    return;
                }
                (st.index + 1) % st.queue.len()
            };
            play_at(&app, &audio, &database, next, 0.0);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_prev_track(move || {
            let app = weak.unwrap();
            let prev = {
                let st = audio.lock().unwrap();
                if st.queue.is_empty() {
                    return;
                }
                if st.index == 0 {
                    st.queue.len() - 1
                } else {
                    st.index - 1
                }
            };
            play_at(&app, &audio, &database, prev, 0.0);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_toggle_shuffle(move || {
            let app = weak.unwrap();
            let mut st = audio.lock().unwrap();
            st.shuffle = !st.shuffle;
            let current_path = st.queue.get(st.index).map(|t| t.path.clone());
            if st.shuffle {
                // Temporary shuffled copy in memory; the database order
                // (st.original) is never touched.
                let mut rest: Vec<Track> = st.original.clone();
                if let Some(cur) = &current_path {
                    rest.retain(|t| &t.path != cur);
                }
                rest.shuffle(&mut rand::thread_rng());
                let mut queue = Vec::with_capacity(st.original.len());
                if let Some(cur) = current_path {
                    if let Some(t) = st.original.iter().find(|t| t.path == cur) {
                        queue.push(t.clone());
                    }
                }
                queue.extend(rest);
                st.queue = queue;
                st.index = 0;
            } else {
                st.queue = st.original.clone();
                st.index = current_path
                    .and_then(|p| st.queue.iter().position(|t| t.path == p))
                    .unwrap_or(0);
            }
            app.set_shuffle_on(st.shuffle);
            if !st.queue.is_empty() {
                app.set_queue_info(SharedString::from(format!(
                    "Faixa {} de {}{}",
                    st.index + 1,
                    st.queue.len(),
                    if st.shuffle { "  ·  aleatório" } else { "" }
                )));
            }
            drop(st);
            refresh_queue_model(&app, &audio);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_cycle_repeat(move || {
            let app = weak.unwrap();
            let mut st = audio.lock().unwrap();
            st.repeat = (st.repeat + 1) % 3;
            app.set_repeat_mode(st.repeat);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_seek(move |ratio| {
            let app = weak.unwrap();
            let st = audio.lock().unwrap();
            if st.duration <= 0.0 {
                return;
            }
            if let Some(sink) = &st.sink {
                let target = Duration::from_secs_f32(st.duration * ratio.clamp(0.0, 1.0));
                if sink.try_seek(target).is_ok() {
                    app.set_progress(ratio.clamp(0.0, 1.0));
                    app.set_position_text(format_time(st.duration * ratio));
                }
            }
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_volume_changed(move |volume| {
            let app = weak.unwrap();
            let volume = volume.clamp(0.0, 1.0);
            app.set_volume(volume);
            let mut st = audio.lock().unwrap();
            st.volume = volume;
            let v = st.effective_volume();
            if let Some(sink) = &st.sink {
                sink.set_volume(v);
            }
            database.set_setting("volume", &volume.to_string());
        });
    }

    // ---------- Queue panel callbacks ----------

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_queue_play(move |i| {
            let app = weak.unwrap();
            play_at(&app, &audio, &database, i as usize, 0.0);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_queue_remove(move |i| {
            let app = weak.unwrap();
            let i = i as usize;
            {
                let mut st = audio.lock().unwrap();
                if i >= st.queue.len() || i == st.index {
                    return; // never remove the playing track
                }
                let path = st.queue[i].path.clone();
                st.queue.remove(i);
                if i < st.index {
                    st.index -= 1;
                }
                st.original.retain(|t| t.path != path);
            }
            refresh_queue_model(&app, &audio);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_queue_move(move |from, to| {
            let app = weak.unwrap();
            let (from, to) = (from as usize, to as usize);
            {
                let mut st = audio.lock().unwrap();
                if from >= st.queue.len() || to >= st.queue.len() || from == to {
                    return;
                }
                let item = st.queue.remove(from);
                st.queue.insert(to, item);
                // Keep the playing index pointing at the same track.
                if st.index == from {
                    st.index = to;
                } else if from < st.index && to >= st.index {
                    st.index -= 1;
                } else if from > st.index && to <= st.index {
                    st.index += 1;
                }
            }
            refresh_queue_model(&app, &audio);
        });
    }

    // ---------- Search callbacks ----------

    {
        let weak = app.as_weak();
        let library = library.clone();
        app.on_search_changed(move |text| {
            let app = weak.unwrap();
            let filter = app.get_search_filter();
            run_search(&app, &library, &text, filter);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_play_search_result(move |i| {
            let app = weak.unwrap();
            let tracks = library.borrow().search.clone();
            start_queue(&app, &audio, &database, tracks, i as usize);
        });
    }

    // ---------- Library callbacks ----------

    {
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_set_sort_mode(move |sort| {
            let app = weak.unwrap();
            refresh_library(&app, &database, &library, sort);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_play_library_song(move |i| {
            let app = weak.unwrap();
            let tracks = library.borrow().tracks.clone();
            start_queue(&app, &audio, &database, tracks, i as usize);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_play_album(move |i| {
            let app = weak.unwrap();
            let (album, tracks) = {
                let lib = library.borrow();
                match lib.albums.get(i as usize) {
                    Some((name, _, _, _)) => (
                        name.clone(),
                        lib.tracks
                            .iter()
                            .filter(|t| t.album == *name)
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                    None => return,
                }
            };
            let _ = album;
            start_queue(&app, &audio, &database, tracks, 0);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_play_artist(move |i| {
            let app = weak.unwrap();
            let tracks = {
                let lib = library.borrow();
                match lib.artists.get(i as usize) {
                    Some((name, _)) => lib
                        .tracks
                        .iter()
                        .filter(|t| t.artist == *name)
                        .cloned()
                        .collect::<Vec<_>>(),
                    None => return,
                }
            };
            start_queue(&app, &audio, &database, tracks, 0);
        });
    }

    // ---------- Playlist CRUD callbacks ----------

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_create_playlist(move |name| {
            let app = weak.unwrap();
            match database.create_playlist(name.trim()) {
                Ok(id) => {
                    refresh_playlists(&app, &database);
                    select_playlist(&app, &database, id);
                }
                Err(e) => eprintln!("Failed to create playlist: {e}"),
            }
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_delete_playlist(move |id| {
            let app = weak.unwrap();
            if let Err(e) = database.delete_playlist(id as i64) {
                eprintln!("Failed to delete playlist: {e}");
            }
            refresh_playlists(&app, &database);
            select_playlist(&app, &database, -1);
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_rename_playlist(move |id, name| {
            let app = weak.unwrap();
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            if let Err(e) = database.rename_playlist(id as i64, &name) {
                eprintln!("Failed to rename playlist: {e}");
            }
            refresh_playlists(&app, &database);
            select_playlist(&app, &database, id as i64);
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_set_playlist_cover(move |id| {
            let app = weak.unwrap();
            if let Some(file) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "webp"])
                .pick_file()
            {
                let path = file.to_string_lossy().into_owned();
                if let Err(e) = database.set_playlist_cover(id as i64, &path) {
                    eprintln!("Failed to set cover: {e}");
                }
                refresh_playlists(&app, &database);
                select_playlist(&app, &database, id as i64);
            }
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_add_songs(move |id| {
            let app = weak.unwrap();
            if let Some(files) = rfd::FileDialog::new()
                .add_filter("Audio", &["mp3", "flac", "wav", "ogg"])
                .pick_files()
            {
                for f in files {
                    let path = f.to_string_lossy().into_owned();
                    if let Err(e) = database.add_song(id as i64, &path) {
                        eprintln!("Failed to add song: {e}");
                    }
                }
                refresh_playlists(&app, &database);
                refresh_songs(&app, &database, id as i64);
                refresh_library(&app, &database, &library, app.get_sort_mode());
            }
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        let library = library.clone();
        app.on_remove_song(move |id, index| {
            let app = weak.unwrap();
            if let Err(e) = database.remove_song(id as i64, index as usize) {
                eprintln!("Failed to remove song: {e}");
            }
            refresh_playlists(&app, &database);
            refresh_songs(&app, &database, id as i64);
            refresh_library(&app, &database, &library, app.get_sort_mode());
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_move_song(move |id, from, to| {
            let app = weak.unwrap();
            if let Err(e) = database.move_song(id as i64, from as usize, to as usize) {
                eprintln!("Failed to reorder song: {e}");
            }
            refresh_songs(&app, &database, id as i64);
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_select_playlist(move |id| {
            let app = weak.unwrap();
            select_playlist(&app, &database, id as i64);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_play_playlist_from(move |id, index| {
            let app = weak.unwrap();
            let paths = database.get_songs(id as i64).unwrap_or_default();
            if paths.is_empty() {
                return;
            }
            if let Err(e) = database.track_playlist_access(id as i64) {
                eprintln!("Failed to track playlist access: {e}");
            }
            let tracks: Vec<Track> = paths.iter().map(|p| read_track(p)).collect();
            start_queue(&app, &audio, &database, tracks, index as usize);
        });
    }

    // ---------- Home callbacks ----------

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_refresh_home(move || {
            let app = weak.unwrap();
            refresh_home(&app, &database);
        });
    }

    {
        let weak = app.as_weak();
        let database = database.clone();
        app.on_refresh_history(move || {
            let app = weak.unwrap();
            refresh_history(&app, &database);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_play_recent_song(move |path| {
            let app = weak.unwrap();
            let track = read_track(path.as_str());
            start_queue(&app, &audio, &database, vec![track], 0);
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        app.on_play_recent_playlist(move |playlist_id| {
            let app = weak.unwrap();
            let paths = database.get_songs(playlist_id as i64).unwrap_or_default();
            if paths.is_empty() {
                return;
            }
            if let Err(e) = database.track_playlist_access(playlist_id as i64) {
                eprintln!("Failed to track playlist access: {e}");
            }
            let tracks: Vec<Track> = paths.iter().map(|p| read_track(p)).collect();
            start_queue(&app, &audio, &database, tracks, 0);
        });
    }

    // ---------- Audio engineering panel callbacks ----------

    {
        let database = database.clone();
        let weak = app.as_weak();
        app.on_eq_changed(move |bass, mid, treble| {
            let app = weak.unwrap();
            app.set_eq_bass(bass);
            app.set_eq_mid(mid);
            app.set_eq_treble(treble);
            database.set_setting("eq_bass", &bass.to_string());
            database.set_setting("eq_mid", &mid.to_string());
            database.set_setting("eq_treble", &treble.to_string());
            database.set_setting("eq_preset", "-1");
        });
    }

    {
        let database = database.clone();
        let weak = app.as_weak();
        app.on_set_eq_preset(move |preset| {
            let app = weak.unwrap();
            // (bass, mid, treble) in dB for each preset.
            let (b, m, t) = match preset {
                1 => (4.0, 1.0, 3.0),   // Rock
                2 => (-1.0, 2.0, 4.0),  // Classical
                3 => (8.0, 1.0, -1.0),  // Bass boost
                4 => (2.0, 3.0, 2.0),   // Pop
                _ => (0.0, 0.0, 0.0),   // Flat
            };
            app.set_eq_preset(preset);
            app.set_eq_bass(b);
            app.set_eq_mid(m);
            app.set_eq_treble(t);
            database.set_setting("eq_preset", &preset.to_string());
            database.set_setting("eq_bass", &b.to_string());
            database.set_setting("eq_mid", &m.to_string());
            database.set_setting("eq_treble", &t.to_string());
        });
    }

    {
        let audio = audio.clone();
        let database = database.clone();
        let weak = app.as_weak();
        app.on_set_crossfade(move |secs| {
            let app = weak.unwrap();
            let secs = secs.clamp(0, 12);
            app.set_crossfade(secs);
            audio.lock().unwrap().crossfade = secs;
            database.set_setting("crossfade", &secs.to_string());
        });
    }

    {
        let audio = audio.clone();
        let database = database.clone();
        let weak = app.as_weak();
        app.on_set_normalization(move |mode| {
            let app = weak.unwrap();
            app.set_normalization(mode);
            let mut st = audio.lock().unwrap();
            st.normalization = mode;
            let v = st.effective_volume();
            if let Some(sink) = &st.sink {
                sink.set_volume(v);
            }
            database.set_setting("normalization", &mode.to_string());
        });
    }

    {
        let audio = audio.clone();
        let database = database.clone();
        let weak = app.as_weak();
        app.on_set_mono(move |mono| {
            let app = weak.unwrap();
            app.set_mono(mono);
            audio.lock().unwrap().mono = mono;
            // Takes effect on the next track (the current source is already
            // wired into the sink).
            database.set_setting("mono", if mono { "1" } else { "0" });
        });
    }

    {
        let audio = audio.clone();
        let weak = app.as_weak();
        app.on_set_sleep(move |minutes| {
            let app = weak.unwrap();
            let mut st = audio.lock().unwrap();
            if minutes <= 0 {
                st.sleep_deadline = None;
                app.set_sleep_minutes(0);
                app.set_sleep_remaining(SharedString::default());
            } else {
                st.sleep_deadline =
                    Some(Instant::now() + Duration::from_secs(minutes as u64 * 60));
                app.set_sleep_minutes(minutes);
            }
        });
    }

    // ---------- Progress timer (runs on the UI event loop) ----------
    // Drives the progress bar, auto-advance, repeat modes, crossfade ramps
    // and the sleep timer.

    let timer = Timer::default();
    {
        let audio = audio.clone();
        let weak = app.as_weak();
        let database = database.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(300), move || {
            let app = match weak.upgrade() {
                Some(a) => a,
                None => return,
            };
            enum Action {
                None,
                Update(f32, f32),
                Advance(usize, f32), // next index, fade-in seconds
                Stop,
            }
            let action = {
                let mut st = audio.lock().unwrap();

                // Sleep timer check.
                if let Some(deadline) = st.sleep_deadline {
                    let now = Instant::now();
                    if now >= deadline {
                        st.sleep_deadline = None;
                        if let Some(sink) = &st.sink {
                            sink.pause();
                        }
                        app.set_sleep_minutes(0);
                        app.set_sleep_remaining(SharedString::default());
                        app.set_playing(false);
                    } else {
                        let rem = (deadline - now).as_secs();
                        app.set_sleep_remaining(SharedString::from(format!(
                            "⏳ {}:{:02}",
                            rem / 60,
                            rem % 60
                        )));
                    }
                }

                // Fade-out ramp for the crossfading previous track.
                if let Some(fading) = &st.fading {
                    let step = if st.crossfade > 0 {
                        st.effective_volume() * 0.3 / st.crossfade as f32
                    } else {
                        1.0
                    };
                    let v = fading.volume() - step;
                    if v <= 0.02 || fading.empty() {
                        fading.stop();
                        st.fading = None;
                    } else {
                        fading.set_volume(v);
                    }
                }

                match &st.sink {
                    Some(sink) if sink.empty() && !sink.is_paused() => {
                        // Track ended.
                        if st.repeat == 2 {
                            Action::Advance(st.index, 0.0)
                        } else if st.index + 1 < st.queue.len() {
                            Action::Advance(st.index + 1, 0.0)
                        } else if st.repeat == 1 && !st.queue.is_empty() {
                            Action::Advance(0, 0.0)
                        } else {
                            Action::Stop
                        }
                    }
                    Some(sink) if !sink.is_paused() => {
                        let pos = sink.get_pos().as_secs_f32();
                        // Early advance with fade when crossfade is enabled.
                        let remaining = st.duration - pos;
                        let can_advance = st.repeat != 2
                            && (st.index + 1 < st.queue.len()
                                || (st.repeat == 1 && st.queue.len() > 1));
                        if st.crossfade > 0
                            && !st.crossfading
                            && st.duration > 0.0
                            && remaining > 0.0
                            && remaining <= st.crossfade as f32
                            && can_advance
                        {
                            st.crossfading = true;
                            // Move the current sink to the fade-out slot so
                            // play_at() won't stop it.
                            st.fading = st.sink.take();
                            let next = if st.index + 1 < st.queue.len() {
                                st.index + 1
                            } else {
                                0
                            };
                            Action::Advance(next, st.crossfade as f32)
                        } else {
                            Action::Update(pos, st.duration)
                        }
                    }
                    _ => Action::None,
                }
            };
            match action {
                Action::Update(pos, dur) => {
                    app.set_position_text(format_time(pos));
                    if dur > 0.0 {
                        app.set_progress((pos / dur).min(1.0));
                    }
                }
                Action::Advance(next, fade) => {
                    play_at(&app, &audio, &database, next, fade);
                }
                Action::Stop => {
                    let mut st = audio.lock().unwrap();
                    st.sink = None;
                    drop(st);
                    app.set_playing(false);
                    app.set_progress(0.0);
                    app.set_position_text(SharedString::from("0:00"));
                    refresh_queue_model(&app, &audio);
                }
                Action::None => {}
            }
        });
    }

    app.run()?;
    Ok(())
}
