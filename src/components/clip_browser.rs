use crate::{
    clip_library::{Catalog, Clip, Event, LibraryWorker, Request, Stamp},
    clip_store::{Config, Discovery, History},
    game_icons::{self, Request as IconRequest},
};
use gpui_kit::{
    base::{Disableable, Selectable, VirtualListScrollHandle, v_virtual_list},
    component::{
        ActiveTheme as _, Icon, IconName, IndexPath, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState},
        list::{List, ListDelegate, ListEvent, ListItem, ListState},
        menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    },
    prelude::FluentBuilder as _,
    *,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};
mod settings;

pub enum BrowserEvent {
    Play(PathBuf, Vec<PathBuf>),
    Edit(PathBuf, Vec<PathBuf>),
    Delete(Vec<PathBuf>),
    Open,
    CloseSettings,
    RestartUpdate,
    Navigate(BrowserView),
    Rename(PathBuf, String),
    Renamed(PathBuf, PathBuf),
    RenameFailed(PathBuf, String),
    Error(String),
}
#[derive(Clone)]
pub struct BrowserView {
    folder: Option<PathBuf>,
    selected: HashSet<PathBuf>,
    anchor: Option<PathBuf>,
    offset: Point<Pixels>,
}
impl BrowserView {
    pub fn rename(&mut self, old: &Path, new: &Path) {
        if self.selected.remove(old) {
            self.selected.insert(new.to_owned());
        }
        if self.anchor.as_deref() == Some(old) {
            self.anchor = Some(new.to_owned());
        }
    }
}
#[derive(Clone)]
enum Row {
    Folder(PathBuf, usize, Option<SystemTime>),
    Clip(Clip),
}

pub struct ClipBrowser {
    pub updater: crate::updater::Updater,
    worker: LibraryWorker,
    icon_worker: game_icons::Worker,
    icons: game_icons::State,
    icon_images: HashMap<String, Option<Arc<RenderImage>>>,
    icon_requested: HashSet<String>,
    detecting_icons: bool,
    steam_prompt_pending: bool,
    steam_scan_pending: Option<Option<PathBuf>>,
    steam_prompt_open: bool,
    icon_picker_subscription: Option<Subscription>,
    icon_picker_observer: Option<Subscription>,
    config: Config,
    catalog: Catalog,
    history: History,
    thumbnails: HashMap<PathBuf, (Stamp, Option<Arc<RenderImage>>)>,
    durations: HashMap<PathBuf, (Stamp, Option<f64>)>,
    requested: HashSet<(PathBuf, Stamp)>,
    pending_watched: HashMap<PathBuf, bool>,
    folder: Option<PathBuf>,
    rows: Vec<Row>,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    selected: HashSet<PathBuf>,
    anchor: Option<PathBuf>,
    scroll: VirtualListScrollHandle,
    offsets: HashMap<Option<PathBuf>, Point<Pixels>>,
    focus: FocusHandle,
    prompt_on_start: bool,
    config_ready: bool,
    history_ready: bool,
    audio_overrides: HashMap<String, std::collections::BTreeMap<i64, f64>>,
    resume_overrides: HashMap<String, Option<f64>>,
    file_overrides: HashMap<String, crate::chapters::FileSettings>,
    pub settings: bool,
    rename_subscription: Option<Subscription>,
    settings_state: Option<settings::SettingsState>,
}
impl EventEmitter<BrowserEvent> for ClipBrowser {}

impl ClipBrowser {
    pub fn new(prompt_on_start: bool, cx: &mut Context<Self>) -> Self {
        Self::with_worker(prompt_on_start, LibraryWorker::new(), None, cx)
    }
    #[cfg(test)]
    pub fn for_test(directory: PathBuf, cx: &mut Context<Self>) -> Self {
        cx.set_global(crate::keybinds::Keymap::open(
            directory.join("keybinds.json"),
        ));
        Self::with_worker(
            false,
            LibraryWorker::for_test(directory.clone()),
            Some(directory),
            cx,
        )
    }
    #[cfg(test)]
    pub fn test_navigate(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        self.navigate(folder, cx);
    }
    #[cfg(test)]
    pub fn test_select(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.select(path, &Modifiers::default(), true, cx);
    }
    #[cfg(test)]
    pub fn test_location(&self) -> Option<PathBuf> {
        self.folder.clone()
    }
    #[cfg(test)]
    pub fn test_selection_empty(&self) -> bool {
        self.selected.is_empty() && self.anchor.is_none()
    }
    fn with_worker(
        prompt_on_start: bool,
        worker: LibraryWorker,
        directory: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            worker,
            icon_worker: game_icons::Worker::new(directory),
            icons: game_icons::State::default(),
            icon_images: HashMap::new(),
            icon_requested: HashSet::new(),
            detecting_icons: false,
            steam_prompt_pending: false,
            steam_scan_pending: None,
            steam_prompt_open: false,
            icon_picker_subscription: None,
            icon_picker_observer: None,
            config: Config::default(),
            updater: crate::updater::Updater::default(),
            catalog: Catalog::default(),
            history: History::new(),
            thumbnails: HashMap::new(),
            durations: HashMap::new(),
            requested: HashSet::new(),
            pending_watched: HashMap::new(),
            folder: None,
            rows: vec![],
            row_sizes: Rc::new(vec![]),
            selected: HashSet::new(),
            anchor: None,
            scroll: VirtualListScrollHandle::new(),
            offsets: HashMap::new(),
            focus: cx.focus_handle(),
            prompt_on_start,
            config_ready: false,
            history_ready: false,
            audio_overrides: HashMap::new(),
            resume_overrides: HashMap::new(),
            file_overrides: HashMap::new(),
            settings: false,
            rename_subscription: None,
            settings_state: None,
        }
    }

    pub fn poll(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.config_ready && self.updater.poll(&self.config.updates) {
            cx.notify();
        }
        for event in self.icon_worker.events().collect::<Vec<_>>() {
            match event {
                game_icons::Event::State(state) => {
                    self.icons = state;
                    // A rescan may repair missing images with the same content hash.
                    self.icon_images.retain(|_, image| image.is_some());
                    self.icon_requested
                        .retain(|hash| self.icon_images.contains_key(hash));
                }
                game_icons::Event::Image(hash, pixels) => {
                    if self.icon_images.len() >= 256
                        && let Some(key) = self.icon_images.keys().next().cloned()
                    {
                        self.icon_images.remove(&key);
                        self.icon_requested.remove(&key);
                    }
                    self.icon_images
                        .insert(hash, pixels.map(crate::render_image));
                }
                game_icons::Event::Detected(message) => {
                    self.detecting_icons = false;
                    crate::notifications::success(window, cx, "Steam search complete", &message);
                }
                game_icons::Event::Warning(message) => {
                    crate::notifications::warning(window, cx, "Steam search warnings", &message);
                }
                game_icons::Event::Error(error) => {
                    self.detecting_icons = false;
                    cx.emit(BrowserEvent::Error(error));
                }
            }
            cx.notify();
        }
        let events = self.worker.events().collect::<Vec<_>>();
        for event in events {
            match event {
                Event::Config(config) => {
                    self.config = config;
                    if self.folder.as_ref().is_some_and(|folder| {
                        !self
                            .config
                            .clip_directories
                            .iter()
                            .any(|root| folder.starts_with(root))
                    }) {
                        self.folder = None;
                        self.selected.clear();
                        self.anchor = None;
                        self.scroll.set_offset(Point::default());
                    }
                    self.config_ready = true;
                    self.rebuild();
                }
                Event::SteamConsent(result) => match result {
                    Ok(true) => {
                        if let Some(root) = self.steam_scan_pending.take() {
                            self.start_steam_scan(root, cx);
                        }
                    }
                    Ok(false) => self.steam_scan_pending = None,
                    Err(error) => {
                        self.steam_scan_pending = None;
                        cx.emit(BrowserEvent::Error(error));
                    }
                },
                Event::Catalog(catalog) => {
                    self.catalog = catalog;
                    self.durations.retain(|p, (s, _)| {
                        self.catalog
                            .clips
                            .iter()
                            .any(|c| c.path == *p && c.stamp == *s)
                    });
                    self.thumbnails.retain(|p, (s, _)| {
                        self.catalog
                            .clips
                            .iter()
                            .any(|c| c.path == *p && c.stamp == *s)
                    });
                    self.requested.retain(|(p, s)| {
                        self.catalog
                            .clips
                            .iter()
                            .any(|c| c.path == *p && c.stamp == *s)
                            && self.thumbnails.contains_key(p)
                    });
                    self.selected
                        .retain(|p| self.catalog.clips.iter().any(|c| c.path == *p));
                    self.rebuild();
                }
                Event::History(history) => {
                    self.file_overrides.retain(|key, settings| {
                        history
                            .get(key)
                            .is_none_or(|state| &state.file_settings != settings)
                    });
                    self.resume_overrides.retain(|key, position| {
                        !history
                            .get(key)
                            .is_some_and(|state| state.resume_position == *position)
                    });
                    self.audio_overrides.retain(|key, gains| {
                        !history
                            .get(key)
                            .is_some_and(|state| &state.track_gains == gains)
                    });
                    self.history = history;
                    self.history_ready = true;
                }
                Event::FileSettingsFailed(path, attempted, error) => {
                    if let Ok(key) = crate::clip_store::hash_filename(&path)
                        && self.file_overrides.get(&key) == Some(&attempted)
                    {
                        self.file_overrides.remove(&key);
                    }
                    cx.emit(BrowserEvent::Error(error));
                }
                Event::Renamed(old, new) => {
                    let mut view = self.view();
                    view.rename(&old, &new);
                    self.selected = view.selected;
                    self.anchor = view.anchor;
                    self.thumbnails.remove(&old);
                    self.durations.remove(&old);
                    self.requested.retain(|(path, _)| path != &old);
                    if let (Ok(old_hash), Ok(new_hash)) = (
                        crate::clip_store::hash_filename(&old),
                        crate::clip_store::hash_filename(&new),
                    ) {
                        if let Some(gains) = self.audio_overrides.remove(&old_hash) {
                            self.audio_overrides.insert(new_hash.clone(), gains);
                        }
                        if let Some(position) = self.resume_overrides.remove(&old_hash) {
                            self.resume_overrides.insert(new_hash.clone(), position);
                        }
                        if let Some(settings) = self.file_overrides.remove(&old_hash) {
                            self.file_overrides.insert(new_hash, settings);
                        }
                    }
                    for clip in &mut self.catalog.clips {
                        if clip.path == old {
                            clip.path = new.clone();
                        }
                    }
                    self.rebuild();
                    if let Some(value) = self.pending_watched.remove(&old) {
                        self.pending_watched.insert(new.clone(), value);
                    }
                    cx.emit(BrowserEvent::Renamed(old, new));
                }
                Event::RenameFailed(path, error) => {
                    cx.emit(BrowserEvent::RenameFailed(path, error));
                }
                Event::WatchedComplete(paths) => {
                    for path in paths {
                        self.pending_watched.remove(&path);
                    }
                }
                Event::Thumbnail(path, stamp, image, duration) => {
                    if self
                        .catalog
                        .clips
                        .iter()
                        .any(|c| c.path == path && c.stamp == stamp)
                    {
                        // Bound decoded UI images; disk thumbnails remain reusable.
                        if self.thumbnails.len() >= 256
                            && let Some(key) = self.thumbnails.keys().next().cloned()
                        {
                            self.thumbnails.remove(&key);
                            self.requested.retain(|(p, _)| p != &key);
                        }
                        self.thumbnails.insert(
                            path.clone(),
                            (stamp.clone(), image.map(crate::render_image)),
                        );
                        self.durations.insert(path, (stamp, duration));
                    }
                }
                Event::Error(error) => {
                    cx.emit(BrowserEvent::Error(error));
                }
            }
            cx.notify();
        }
        if visible && self.config_ready && self.prompt_on_start {
            self.prompt_on_start = false;
            if self.config.clip_discovery == Discovery::NotAsked {
                self.prompt(window, cx);
            } else if should_prompt_steam(&self.config) {
                self.approve_steam_scan(None, window, cx);
            }
        }
        if visible && self.steam_prompt_pending && should_prompt_steam(&self.config) {
            self.steam_prompt_pending = false;
            self.approve_steam_scan(None, window, cx);
        }
    }

    fn prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let yes = cx.weak_entity();
        let no = yes.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let yes = yes.clone(); let no = no.clone();
            alert.title("Allow NVIDIA Overlay folder discovery?")
                .description("StarPlayer can read NVIDIA Overlay's gallery location and watch that folder for clips. Your choice is saved locally.")
                .confirm().ok_text("Detect clips").cancel_text("Not now").close_button(false)
                .on_ok(move |_, _, cx| { let _ = yes.update(cx, |this, _| {
                    this.steam_prompt_pending = true;
                    this.worker.send(Request::Consent(true));
                }); true })
                .on_cancel(move |_, _, cx| { let _ = no.update(cx, |this, _| this.worker.send(Request::Consent(false))); true })
        });
    }

    fn rebuild(&mut self) {
        let mut folders = self
            .catalog
            .folders
            .iter()
            .filter(|path| {
                if let Some(folder) = &self.folder {
                    path.parent() == Some(folder.as_path())
                } else {
                    self.config
                        .clip_directories
                        .iter()
                        .any(|root| path.parent() == Some(root.as_path()))
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        // Roots containing loose clips are navigable, too.
        if self.folder.is_none() {
            for root in &self.config.clip_directories {
                if self
                    .catalog
                    .clips
                    .iter()
                    .any(|c| c.path.parent() == Some(root.as_path()))
                    && !folders.contains(root)
                {
                    folders.push(root.clone());
                }
            }
        }
        let mut rows = folders
            .into_iter()
            .filter_map(|path| folder_row(path, &self.catalog))
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| match (a, b) {
            (Row::Folder(a, ac, at), Row::Folder(b, bc, bt)) => {
                let order = match self.config.game_folder_sort.as_str() {
                    "name" => name(a).to_lowercase().cmp(&name(b).to_lowercase()),
                    "clip_count" => bc.cmp(ac),
                    _ => bt.cmp(at),
                };
                order.then_with(|| a.cmp(b))
            }
            _ => std::cmp::Ordering::Equal,
        });
        if let Some(folder) = &self.folder {
            rows.extend(
                self.catalog
                    .clips
                    .iter()
                    .filter(|c| c.path.parent() == Some(folder.as_path()))
                    .cloned()
                    .map(Row::Clip),
            );
        }
        self.rows = rows;
        self.row_sizes = Rc::new(vec![size(px(300.), px(88.)); self.rows.len()]);
        self.icon_worker.send(IconRequest::Folders(
            self.catalog
                .folders
                .iter()
                .filter(|path| {
                    self.config
                        .clip_directories
                        .iter()
                        .any(|root| path.parent() == Some(root.as_path()))
                })
                .cloned()
                .collect(),
        ));
    }

    fn navigate(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.folder == folder {
            return;
        }
        cx.emit(BrowserEvent::Navigate(self.view()));
        self.offsets
            .insert(self.folder.clone(), self.scroll.offset());
        self.folder = folder;
        self.selected.clear();
        self.anchor = None;
        self.rebuild();
        self.scroll
            .set_offset(self.offsets.get(&self.folder).copied().unwrap_or_default());
        cx.notify();
    }
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }
    pub fn deselect(&mut self, cx: &mut Context<Self>) {
        self.selected.clear();
        self.anchor = None;
        cx.notify();
    }
    pub fn view(&self) -> BrowserView {
        BrowserView {
            folder: self.folder.clone(),
            selected: self.selected.clone(),
            anchor: self.anchor.clone(),
            offset: self.scroll.offset(),
        }
    }
    pub fn restore(&mut self, view: BrowserView, cx: &mut Context<Self>) {
        self.offsets
            .insert(self.folder.clone(), self.scroll.offset());
        self.folder = view
            .folder
            .filter(|folder| self.catalog.folders.contains(folder));
        self.selected = view
            .selected
            .into_iter()
            .filter(|path| self.exists(path))
            .collect();
        self.anchor = view.anchor.filter(|path| self.exists(path));
        self.rebuild();
        self.scroll.set_offset(view.offset);
        cx.notify();
    }
    pub fn audio_ready(&self) -> bool {
        self.config_ready && self.history_ready
    }
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn update_action(&mut self, cx: &mut Context<Self>) {
        match &self.updater.state {
            crate::updater::State::Ready(..) => cx.emit(BrowserEvent::RestartUpdate),
            crate::updater::State::Available(_) => self.updater.download(),
            _ => self.updater.check(),
        }
        cx.notify();
    }
    pub fn media_state(&self, path: &Path) -> crate::clip_store::MediaState {
        let mut state = crate::clip_store::media_state(&self.history, path);
        if let Some(settings) = crate::clip_store::hash_filename(path)
            .ok()
            .and_then(|key| self.file_overrides.get(&key))
        {
            state.file_settings = settings.clone();
        }
        if let Some(position) = crate::clip_store::hash_filename(path)
            .ok()
            .and_then(|key| self.resume_overrides.get(&key))
        {
            state.resume_position = *position;
        }
        if let Some(gains) = crate::clip_store::hash_filename(path)
            .ok()
            .and_then(|key| self.audio_overrides.get(&key))
        {
            state.track_gains = gains.clone();
        }
        state
    }
    pub fn save_audio(&mut self, path: PathBuf, gains: std::collections::BTreeMap<i64, f64>) {
        if let Ok(hash) = crate::clip_store::hash_filename(&path) {
            let mut state = self.media_state(&path);
            state.track_gains = gains.clone();
            self.history.insert(hash, state);
            if let Ok(hash) = crate::clip_store::hash_filename(&path) {
                self.audio_overrides.insert(hash, gains.clone());
            }
        }
        self.worker.send(Request::Audio(path, gains));
    }
    pub fn save_file_settings(&mut self, path: PathBuf, settings: crate::chapters::FileSettings) {
        if let Ok(hash) = crate::clip_store::hash_filename(&path) {
            self.file_overrides.insert(hash, settings.clone());
        }
        self.worker.send(Request::FileSettings(path, settings));
    }
    pub fn save_resume(&mut self, path: PathBuf, position: Option<f64>) {
        if let Ok(hash) = crate::clip_store::hash_filename(&path) {
            let mut state = self.media_state(&path);
            state.resume_position = position;
            self.history.insert(hash.clone(), state);
            self.resume_overrides.insert(hash, position);
        }
        self.worker.send(Request::Resume(path, position));
    }

    pub fn save_master(&mut self, volume: f64, muted: bool) {
        self.config.volume = volume;
        self.config.muted = muted;
        self.worker.send(Request::Master(volume, muted));
    }
    pub fn rename(
        &self,
        path: PathBuf,
        stem: String,
        released: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.worker.send(Request::Rename(path, stem, released));
    }
    pub fn prompt_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(stem));
        let enter_path = path.clone();
        self.rename_subscription = Some(cx.subscribe_in(
            &input,
            window,
            move |_, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let stem = input.read(cx).value().to_string();
                    match crate::clip_library::rename_destination(&enter_path, &stem) {
                        Ok(_) => {
                            cx.emit(BrowserEvent::Rename(enter_path.clone(), stem));
                            window.close_dialog(cx);
                        }
                        Err(error) => {
                            crate::notifications::error(window, cx, "Cannot rename file", &error)
                        }
                    }
                }
            },
        ));
        let owner = cx.weak_entity();
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            let input = input.clone();
            let owner = owner.clone();
            let path = path.clone();
            let field = input.clone();
            dialog
                .title("Rename file")
                .child(Input::new(&field))
                .footer(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("rename-confirm")
                                .primary()
                                .label("Rename")
                                .on_click(move |_, window, cx| {
                                    let stem = input.read(cx).value().to_string();
                                    match crate::clip_library::rename_destination(&path, &stem) {
                                        Ok(_) => {
                                            let _ = owner.update(cx, |_, cx| {
                                                cx.emit(BrowserEvent::Rename(
                                                    path.clone(),
                                                    stem.clone(),
                                                ))
                                            });
                                            window.close_dialog(cx);
                                        }
                                        Err(error) => {
                                            crate::notifications::error(
                                                window,
                                                cx,
                                                "Cannot rename file",
                                                &error,
                                            );
                                        }
                                    }
                                }),
                        )
                        .child(Button::new("rename-cancel").label("Cancel").on_click(
                            |_, window, cx| {
                                window.close_dialog(cx);
                            },
                        )),
                )
        });
    }
    pub fn mark_played(&mut self, path: PathBuf) {
        self.pending_watched.insert(path.clone(), true);
        self.worker.mark_watched(vec![path], true);
    }
    fn mark_selection(&mut self, watched: bool, cx: &mut Context<Self>) {
        let paths = self.selected.iter().cloned().collect::<Vec<_>>();
        self.mark_paths(paths, watched, cx);
    }
    fn mark_paths(&mut self, paths: Vec<PathBuf>, watched: bool, cx: &mut Context<Self>) {
        for path in &paths {
            self.pending_watched.insert(path.clone(), watched);
        }
        self.worker.mark_watched(paths, watched);
        cx.notify();
    }
    pub fn delete(&self, paths: Vec<PathBuf>, released: Arc<std::sync::atomic::AtomicBool>) {
        self.worker.send(Request::Delete(paths, released));
    }
    pub fn exists(&self, path: &Path) -> bool {
        self.catalog.clips.iter().any(|c| c.path == path)
    }

    pub fn thumbnail(&mut self, path: &Path) -> Option<Arc<RenderImage>> {
        let clip = self.catalog.clips.iter().find(|c| c.path == path)?.clone();
        self.request_thumbnail(&clip);
        self.thumbnails
            .get(path)
            .and_then(|(_, image)| image.clone())
    }
    fn request_thumbnail(&mut self, clip: &Clip) {
        if self
            .requested
            .insert((clip.path.clone(), clip.stamp.clone()))
        {
            self.worker
                .send(Request::Thumbnail(clip.path.clone(), clip.stamp.clone()));
        }
    }
    fn select(
        &mut self,
        path: PathBuf,
        modifiers: &Modifiers,
        toggle: bool,
        cx: &mut Context<Self>,
    ) {
        if modifiers.shift {
            let clips = self
                .rows
                .iter()
                .filter_map(|r| {
                    if let Row::Clip(c) = r {
                        Some(&c.path)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if let (Some(a), Some(b)) = (
                self.anchor
                    .as_ref()
                    .and_then(|p| clips.iter().position(|q| *q == p)),
                clips.iter().position(|p| **p == path),
            ) {
                if !modifiers.control {
                    self.selected.clear();
                }
                self.selected
                    .extend(clips[a.min(b)..=a.max(b)].iter().map(|p| (*p).clone()));
            }
        } else {
            if !toggle && !modifiers.control {
                self.selected.clear();
            }
            if !self.selected.insert(path.clone()) && (toggle || modifiers.control) {
                self.selected.remove(&path);
            }
            self.anchor = Some(path);
        }
        cx.notify();
    }
    fn play(&self, path: PathBuf, cx: &mut Context<Self>) {
        let list = self
            .rows
            .iter()
            .filter_map(|r| {
                if let Row::Clip(c) = r {
                    Some(c.path.clone())
                } else {
                    None
                }
            })
            .collect();
        cx.emit(BrowserEvent::Play(path, list));
    }
    fn confirm_delete(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_delete_paths(self.selected.iter().cloned().collect(), window, cx);
    }
    fn confirm_delete_paths(
        &self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let owner = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let paths = paths.clone();
            let owner = owner.clone();
            alert
                .title("Move clips to Recycle Bin?")
                .description(format!(
                    "{} clips will be moved to the Windows Recycle Bin.",
                    paths.len()
                ))
                .confirm()
                .ok_text("Move to Recycle Bin")
                .cancel_text("Keep clips")
                .on_ok(move |_, _, cx| {
                    let _ = owner.update(cx, |_, cx| cx.emit(BrowserEvent::Delete(paths.clone())));
                    true
                })
        });
    }
    fn add_folder(&self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add clip directory".into()),
        });
        cx.spawn_in(window, async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update_in(cx, |this, _, _| {
                    for path in paths {
                        this.worker.send(Request::AddFolder(path));
                    }
                });
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |_, window, cx| {
                    crate::notifications::error(
                        window,
                        cx,
                        "Cannot select folder",
                        &error.to_string(),
                    );
                });
            }
            _ => {}
        })
        .detach();
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.rows[index].clone() {
            Row::Folder(path, count, latest) => {
                let label = name(&path);
                let image = self
                    .icons
                    .image(&path)
                    .map(str::to_owned)
                    .and_then(|hash| self.icon_image(&hash));
                let menu_path = path.clone();
                let owner = cx.entity();
                let subtitle = format!(
                    "Latest {} · {} clips",
                    latest.map(date).unwrap_or_else(|| "none".into()),
                    count
                );
                div()
                    .id(SharedString::from(format!("folder:{}", path.display())))
                    .w_full()
                    .h(px(88.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .hover(|s| s.bg(cx.theme().muted))
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(40.))
                            .h(px(40.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when_some(image.clone(), |s, image| {
                                s.child(img(image).size_full().object_fit(ObjectFit::Contain))
                            })
                            .when(image.is_none(), |s| {
                                s.child(Icon::new(IconName::FolderOpen).size(px(28.)))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(label))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .child(Icon::new(IconName::ChevronRight))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.navigate(Some(path.clone()), cx)),
                    )
                    .context_menu(move |menu, _, cx| {
                        let restore = owner.read(cx).icons.has_override(&menu_path);
                        let custom_owner = owner.clone();
                        let choose_owner = owner.clone();
                        let restore_owner = owner.clone();
                        let custom_path = menu_path.clone();
                        let choose_path = menu_path.clone();
                        let restore_path = menu_path.clone();
                        let reveal_path = menu_path.clone();
                        menu.item(PopupMenuItem::new("Reveal in File Explorer").on_click(
                            move |_, window, cx| {
                                crate::file_explorer::reveal(reveal_path.clone(), window, cx)
                            },
                        ))
                        .separator()
                        .item(PopupMenuItem::new("Set custom icon…").on_click(
                            move |_, window, cx| {
                                custom_owner.update(cx, |this, cx| {
                                    this.custom_icon(custom_path.clone(), window, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new("Choose game icon…").on_click(
                            move |_, window, cx| {
                                choose_owner.update(cx, |this, cx| {
                                    this.choose_game_icon(choose_path.clone(), window, cx)
                                });
                            },
                        ))
                        .item(
                            PopupMenuItem::new("Restore assigned icon")
                                .disabled(!restore)
                                .on_click(move |_, _, cx| {
                                    restore_owner.update(cx, |this, _| {
                                        this.icon_worker
                                            .send(IconRequest::Restore(restore_path.clone()))
                                    });
                                }),
                        )
                    })
                    .into_any_element()
            }
            Row::Clip(clip) => {
                self.request_thumbnail(&clip);
                let path = clip.path.clone();
                let selected = self.selected.contains(&path);
                let watched = self
                    .pending_watched
                    .get(&path)
                    .copied()
                    .unwrap_or_else(|| !self.media_state(&path).watched_at.is_empty());
                let thumbnail = self
                    .thumbnails
                    .get(&path)
                    .and_then(|(_, image)| image.clone());
                let check_path = path.clone();
                let play_path = path.clone();
                let menu_path = path.clone();
                let owner = cx.entity();
                div()
                    .id(SharedString::from(format!("clip:{}", path.display())))
                    .w_full()
                    .h(px(88.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(selected, |s| s.bg(cx.theme().accent))
                    .hover(|s| s.bg(cx.theme().muted))
                    .child(
                        div()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    window.focus(&this.focus, cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                Checkbox::new(("select-clip", index))
                                    .checked(selected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select(
                                            check_path.clone(),
                                            &Modifiers::default(),
                                            true,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .w(px(112.))
                            .h(px(63.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .when_some(thumbnail, |s, image| {
                                s.child(img(image).size_full().object_fit(ObjectFit::Contain))
                            })
                            .when(
                                !self.thumbnails.get(&path).is_some_and(|(_, i)| i.is_some()),
                                |s| s.child(Icon::new(IconName::Play)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(name(&path)))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(date(clip.created)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::clip_details::duration(
                                self.durations
                                    .get(&path)
                                    .filter(|(stamp, _)| stamp == &clip.stamp)
                                    .and_then(|(_, duration)| *duration),
                            )),
                    )
                    .when(!watched, |s| {
                        s.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().primary)
                                .child("Unwatched"),
                        )
                    })
                    .child(
                        Button::new(("play-clip", index))
                            .ghost()
                            .icon(IconName::Play)
                            .tooltip("Play clip")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.play(play_path.clone(), cx)),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if opens_clip(&event.modifiers) {
                                this.play(path.clone(), cx);
                            } else {
                                this.select(path.clone(), &event.modifiers, false, cx);
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .context_menu(move |menu, window, cx| {
                        let mut targets = vec![menu_path.clone()];
                        let mut has_watched = false;
                        let mut has_unwatched = false;
                        let _ = owner.update(cx, |this, cx| {
                            targets = context_targets(&this.selected, &menu_path);
                            for path in &targets {
                                let watched =
                                    this.pending_watched.get(path).copied().unwrap_or_else(|| {
                                        !this.media_state(path).watched_at.is_empty()
                                    });
                                has_watched |= watched;
                                has_unwatched |= !watched;
                            }
                            window.focus(&this.focus, cx);
                        });
                        let single = targets.len() == 1;
                        let watched_paths = targets.clone();
                        let unwatched_paths = targets.clone();
                        let watched_owner = owner.clone();
                        let unwatched_owner = owner.clone();
                        let rename_owner = owner.clone();
                        let delete_owner = owner.clone();
                        let rename_path = menu_path.clone();
                        let reveal_path = menu_path.clone();
                        let edit_path = menu_path.clone();
                        let edit_owner = owner.clone();
                        menu.item(PopupMenuItem::new("Reveal in File Explorer").on_click(
                            move |_, window, cx| {
                                crate::file_explorer::reveal(reveal_path.clone(), window, cx)
                            },
                        ))
                        .item(PopupMenuItem::new("Edit clip…").disabled(!single).on_click(
                            move |_, _, cx| {
                                edit_owner.update(cx, |this, cx| {
                                    let list = this
                                        .rows
                                        .iter()
                                        .filter_map(|row| match row {
                                            Row::Clip(clip) => Some(clip.path.clone()),
                                            _ => None,
                                        })
                                        .collect();
                                    cx.emit(BrowserEvent::Edit(edit_path.clone(), list));
                                });
                            },
                        ))
                        .separator()
                        .when(has_unwatched, |menu| {
                            menu.item(
                                PopupMenuItem::new("Mark watched")
                                    .icon(IconName::Eye)
                                    .on_click(move |_, _, cx| {
                                        watched_owner.update(cx, |this, cx| {
                                            this.mark_paths(watched_paths.clone(), true, cx)
                                        });
                                    }),
                            )
                        })
                        .when(has_watched, |menu| {
                            menu.item(
                                PopupMenuItem::new("Mark unwatched")
                                    .icon(IconName::EyeOff)
                                    .on_click(move |_, _, cx| {
                                        unwatched_owner.update(cx, |this, cx| {
                                            this.mark_paths(unwatched_paths.clone(), false, cx)
                                        });
                                    }),
                            )
                        })
                        .item(
                            PopupMenuItem::new("Rename")
                                .icon(Icon::new(gpui_kit::assets::IconName::Pencil))
                                .disabled(!single)
                                .on_click(move |_, window, cx| {
                                    rename_owner.update(cx, |this, cx| {
                                        this.prompt_rename(rename_path.clone(), window, cx)
                                    });
                                }),
                        )
                        .separator()
                        .item(delete_menu_item(cx).on_click(move |_, window, cx| {
                            delete_owner.update(cx, |this, cx| {
                                this.confirm_delete_paths(targets.clone(), window, cx)
                            });
                        }))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Render for ClipBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.settings {
            return self.render_settings(window, cx).into_any_element();
        }
        let compact = window.viewport_size().width < px(1000.);
        let menu_owner = cx.entity();
        let folder = self.folder.clone();
        let parent = folder.as_ref().and_then(|p| {
            if self.config.clip_directories.contains(p)
                || p.parent()
                    .is_some_and(|p| self.config.clip_directories.iter().any(|r| r == p))
            {
                None
            } else {
                p.parent().map(Path::to_owned)
            }
        });
        let count = self.selected.len();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .track_focus(&self.focus)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(folder.is_some(), |s| {
                        s.child(
                            Button::new("folder-back")
                                .ghost()
                                .icon(IconName::ChevronLeft)
                                .label("Back")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.navigate(parent.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        div().flex_1().min_w_0().truncate().child(
                            folder
                                .as_ref()
                                .map(|p| crate::clip_store::display_path(p))
                                .unwrap_or_else(|| "Game clips".into()),
                        ),
                    )
                    .child(
                        Button::new("open-file")
                            .ghost()
                            .label("Open file")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::Open))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(48.))
                    .flex_shrink_0()
                    .child(div().text_xs().child("Sort folders"))
                    .children(
                        [
                            ("newest_clip", "Newest clip"),
                            ("name", "Name"),
                            ("clip_count", "Clip count"),
                        ]
                        .into_iter()
                        .map(|(value, label)| {
                            Button::new(value)
                                .ghost()
                                .label(label)
                                .selected(self.config.game_folder_sort == value)
                                .on_click(cx.listener(move |this, _, _, _| {
                                    this.worker.send(Request::Sort(value.into()))
                                }))
                        }),
                    )
                    .child(
                        Button::new("refresh-clips")
                            .ghost()
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, _, _| {
                                this.requested.clear();
                                this.worker.send(Request::Refresh);
                            })),
                    )
                    .child(div().flex_1())
                    .when(count > 0 && !compact, |s| {
                        s.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(format!("{count} selected"))
                                .child(
                                    Button::new("mark-watched")
                                        .ghost()
                                        .label("Mark watched")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.mark_selection(true, cx)
                                        })),
                                )
                                .child(
                                    Button::new("mark-unwatched")
                                        .ghost()
                                        .label("Mark unwatched")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.mark_selection(false, cx)
                                        })),
                                )
                                .child(
                                    Button::new("delete-clips")
                                        .danger()
                                        .label("Delete")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_delete(window, cx)
                                        })),
                                ),
                        )
                    })
                    .when(count > 0 && compact, |s| {
                        s.child(
                            Button::new("bulk-actions")
                                .ghost()
                                .label(format!("{count} selected"))
                                .icon(IconName::ChevronDown)
                                .dropdown_menu(move |menu, _, cx| {
                                    let watched = menu_owner.clone();
                                    let unwatched = menu_owner.clone();
                                    let delete = menu_owner.clone();
                                    menu.item(
                                        PopupMenuItem::new("Mark watched")
                                            .icon(IconName::Eye)
                                            .on_click(move |_, _, cx| {
                                                watched.update(cx, |this, cx| {
                                                    this.mark_selection(true, cx)
                                                });
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new("Mark unwatched")
                                            .icon(IconName::EyeOff)
                                            .on_click(move |_, _, cx| {
                                                unwatched.update(cx, |this, cx| {
                                                    this.mark_selection(false, cx)
                                                });
                                            }),
                                    )
                                    .separator()
                                    .item(
                                        delete_menu_item(cx).on_click(move |_, window, cx| {
                                            delete.update(cx, |this, cx| {
                                                this.confirm_delete(window, cx)
                                            });
                                        }),
                                    )
                                }),
                        )
                    }),
            )
            .when(self.rows.is_empty(), |s| {
                s.child(div().p_8().text_color(cx.theme().muted_foreground).child(
                    if self.config_ready {
                        "No clips here. Open Settings to detect clips or add a folder."
                    } else {
                        "Loading library…"
                    },
                ))
            })
            .child(
                v_virtual_list(
                    cx.entity(),
                    "clip-rows",
                    self.row_sizes.clone(),
                    |this, range, _, cx| {
                        range
                            .map(|index| this.render_row(index, cx))
                            .collect::<Vec<_>>()
                    },
                )
                .track_scroll(&self.scroll)
                .w_full()
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }
}

impl ClipBrowser {
    fn icon_image(&mut self, hash: &str) -> Option<Arc<RenderImage>> {
        if self.icon_requested.insert(hash.to_owned()) {
            self.icon_worker.send(IconRequest::Image(hash.to_owned()));
        }
        self.icon_images.get(hash).and_then(Clone::clone)
    }

    fn approve_steam_scan(
        &mut self,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.detecting_icons || self.steam_scan_pending.is_some() || self.steam_prompt_open {
            return;
        }
        if self.config.steam_discovery == Discovery::Enabled {
            self.start_steam_scan(root, cx);
            return;
        }
        self.steam_prompt_open = true;
        let owner = cx.weak_entity();
        let decline = owner.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let owner = owner.clone();
            let decline = decline.clone();
            let root = root.clone();
            alert.title("Search for local Steam games?")
                .description("StarPlayer can find installed Steam games and use their local icons for your clip folders. Your choice is saved locally.")
                .confirm().ok_text("Search games").cancel_text("Not now").close_button(false)
                .on_ok(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.steam_prompt_open = false;
                        this.steam_scan_pending = Some(root.clone());
                        this.worker.send(Request::SteamConsent(true));
                        cx.notify();
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    let _ = decline.update(cx, |this, cx| {
                        this.steam_prompt_open = false;
                        this.steam_scan_pending = None;
                        this.worker.send(Request::SteamConsent(false));
                        cx.notify();
                    });
                    true
                })
        });
    }

    fn start_steam_scan(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.config.steam_discovery != Discovery::Enabled || self.detecting_icons {
            return;
        }
        self.detecting_icons = true;
        self.icon_worker.send(IconRequest::Detect(root));
        cx.notify();
    }

    fn select_steam_folder(&self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose Steam installation folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.approve_steam_scan(Some(path), window, cx)
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |_, _, cx| {
                    cx.emit(BrowserEvent::Error(error.to_string()))
                });
            }
            _ => {}
        })
        .detach();
    }

    #[cfg(windows)]
    fn custom_icon(&self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // GPUI's path prompt has no extension filters. Use a parented native
        // dialog here, and validate again in the worker for manually typed paths.
        let selection = rfd::AsyncFileDialog::new()
            .set_title("Choose a PNG, JPEG, ICO, or EXE file")
            .add_filter("Icons and executables", game_icons::CUSTOM_ICON_EXTENSIONS)
            .set_parent(window)
            .pick_file();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(file) = selection.await {
                let path = file.path().to_owned();
                let _ = this.update_in(cx, |this, _, _| {
                    this.icon_worker.send(IconRequest::Custom(folder, path));
                });
            }
        })
        .detach();
    }

    #[cfg(not(windows))]
    fn custom_icon(&self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a PNG, JPEG, ICO, or EXE file".into()),
        });
        cx.spawn_in(window, async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, _, _| {
                        this.icon_worker.send(IconRequest::Custom(folder, path))
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |_, _, cx| {
                    cx.emit(BrowserEvent::Error(error.to_string()))
                });
            }
            _ => {}
        })
        .detach();
    }

    fn choose_game_icon(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let mut games = self
            .icons
            .games
            .iter()
            .filter_map(|(&id, game)| Some((id, game.name.clone(), game.image.clone()?)))
            .collect::<Vec<_>>();
        games.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then(a.0.cmp(&b.0))
        });
        let owner = cx.entity();
        let weak = cx.weak_entity();
        let list = cx.new(|cx| {
            ListState::new(
                GameIconPicker {
                    filtered: games.clone(),
                    games,
                    selected: None,
                    owner: weak,
                },
                window,
                cx,
            )
            .searchable(true)
        });
        self.icon_picker_observer =
            Some(list.update(cx, |_, cx| cx.observe(&owner, |_, _, cx| cx.notify())));
        self.icon_picker_subscription = Some(cx.subscribe_in(
            &list,
            window,
            move |this, list, event, window, cx| {
                match event {
                    ListEvent::Confirm(index) => {
                        if let Some((id, _, hash)) =
                            list.read(cx).delegate().filtered.get(index.row)
                            && this
                                .icon_images
                                .get(hash)
                                .is_some_and(|image| image.is_some())
                        {
                            this.icon_worker
                                .send(IconRequest::Choose(folder.clone(), *id));
                            window.close_dialog(cx);
                        }
                    }
                    ListEvent::Cancel => window.close_dialog(cx),
                    _ => {}
                }
            },
        ));
        let initial_focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if initial_focus.replace(false) {
                list.focus_handle(cx).focus(window, cx);
            }
            dialog.title("Choose game icon").w(px(520.)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().text_sm().child(
                            "Cached Steam icons remain available after uninstalling a game.",
                        ),
                    )
                    .child(List::new(&list).h(px(360.))),
            )
        });
    }

    fn render_library_settings(&self, query: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.config.clip_discovery == Discovery::Enabled;
        let steam_enabled = self.config.steam_discovery == Discovery::Enabled;
        let steam_busy = self.detecting_icons || self.steam_scan_pending.is_some();
        let folders = "clip library folders monitored paths add remove".contains(query)
            || self
                .config
                .clip_directories
                .iter()
                .any(|p| p.to_string_lossy().to_lowercase().contains(query));
        let overlay =
            "clip library nvidia overlay automatic folder discovery detect".contains(query);
        let steam = "clip library steam game icons local search permission cache".contains(query);
        div().flex().flex_col().gap_3()
            .when(folders, |s| s
            .child(div().text_lg().child("Folders"))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Removing a folder keeps its files and watched history."))
            .children(self.config.clip_directories.iter().enumerate().map(|(index, path)| {
                let remove = path.clone();
                div().flex().items_center().gap_3().p_3().border_1().border_color(cx.theme().border).rounded_lg()
                    .child(Icon::new(IconName::FolderOpen))
                    .child(div().flex_1().min_w_0()
                        .child(div().truncate().child(crate::clip_store::display_path(path)))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child(if path.is_dir() { "Monitored" } else { "Unavailable. Waiting for the drive." })))
                    .child(Button::new(("remove-folder", index)).ghost().label("Remove")
                        .on_click(cx.listener(move |this, _, _, _| this.worker.send(Request::RemoveFolder(remove.clone())))))
            }))
            .when(self.config.clip_directories.is_empty(), |s| s.child("No folders added yet."))
            .child(Button::new("settings-add-folder").ghost().icon(IconName::FolderOpen).label("Add folder")
                .on_click(cx.listener(|this, _, window, cx| this.add_folder(window, cx)))))
            .when(overlay, |s| s
            .child(div().mt_4().text_lg().child("NVIDIA Overlay"))
            .child(settings::setting_row("Automatic folder discovery", "Find NVIDIA's recording folder. Listed folders stay monitored.", cx)
                .child(gpui_kit::component::switch::Switch::new("automatic-discovery").checked(enabled)
                    .accessibility_label("Automatic folder discovery")
                    .on_click(cx.listener(move |this, next: &bool, _, _| {
                        this.steam_prompt_pending = *next;
                        this.worker.send(Request::Consent(*next));
                    }))))
            .child(div().flex().items_center().gap_3()
                .child(Button::new("settings-detect").ghost().label("Detect now")
                    .on_click(cx.listener(|this, _, _, _| this.worker.send(Request::Detect)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                .child(if enabled { "Enabled. NVIDIA's current folder may be added again if removed." } else { "Disabled. Manual detection is still available." })))
            .when(steam, |s| s
            .child(div().mt_4().text_lg().child("Steam"))
            .child(settings::setting_row("Allow local game searches", "Find Steam game icons locally. No network requests or startup searches.", cx)
                .child(gpui_kit::component::switch::Switch::new("steam-search-permission").checked(steam_enabled)
                .accessibility_label("Allow local game searches")
                .disabled(steam_busy).on_click(cx.listener(move |this, next: &bool, window, cx| {
                    if !*next { this.worker.send(Request::SteamConsent(false)); }
                    else { this.approve_steam_scan(None, window, cx); }
                }))))
            .child(div().flex().items_center().gap_3()
                .child(Button::new("search-steam-games").ghost().label(if steam_busy { "Searching…" } else { "Search now" })
                    .disabled(steam_busy).on_click(cx.listener(|this, _, window, cx| this.approve_steam_scan(None, window, cx))))
                .child(Button::new("choose-steam-folder").ghost().label("Choose Steam folder…")
                    .disabled(steam_busy).on_click(cx.listener(|this, _, window, cx| this.select_steam_folder(window, cx)))))
            .child(format!("{} detected games, {} with cached icons", self.icons.games.len(), self.icons.games.values().filter(|g| g.image.is_some()).count()))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Cached icons stay available after uninstalling games. Right-click a game folder to change its icon.")))
            .when(!folders && !overlay && !steam, |s| s.child("No matching settings on this page."))
    }
}

struct GameIconPicker {
    games: Vec<(u32, String, String)>,
    filtered: Vec<(u32, String, String)>,
    selected: Option<IndexPath>,
    owner: WeakEntity<ClipBrowser>,
}

fn should_prompt_steam(config: &Config) -> bool {
    config.clip_discovery == Discovery::Enabled && config.steam_discovery == Discovery::NotAsked
}
impl ListDelegate for GameIconPicker {
    type Item = ListItem;
    fn items_count(&self, _: usize, _: &App) -> usize {
        self.filtered.len()
    }
    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let (_, name, hash) = self.filtered.get(index.row)?;
        let image = self
            .owner
            .update(cx, |owner, _| owner.icon_image(hash))
            .ok()
            .flatten();
        Some(
            ListItem::new(index)
                .selected(self.selected == Some(index))
                .disabled(image.is_none())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(32.)).h(px(32.)).flex_shrink_0().when_some(
                            image.clone(),
                            |s, image| {
                                s.child(img(image).size_full().object_fit(ObjectFit::Contain))
                            },
                        ))
                        .child(name.clone())
                        .when(image.is_none(), |s| {
                            s.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Loading or unavailable"),
                            )
                        }),
                ),
        )
    }
    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
        cx.notify();
    }
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.filtered = self
            .games
            .iter()
            .filter(|(_, name, _)| name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        self.selected = None;
        cx.notify();
        Task::ready(())
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div().p_4().child("No cached game icons found. Run Detect Steam game icons in Settings, or choose a custom image.")
    }
}

fn opens_clip(modifiers: &Modifiers) -> bool {
    *modifiers == Modifiers::default()
}

fn context_targets(selected: &HashSet<PathBuf>, path: &Path) -> Vec<PathBuf> {
    if selected.contains(path) {
        let mut paths = selected.iter().cloned().collect::<Vec<_>>();
        paths.sort();
        paths
    } else {
        vec![path.to_owned()]
    }
}

fn delete_menu_item(cx: &App) -> PopupMenuItem {
    PopupMenuItem::element(|_, cx| div().text_color(cx.theme().danger).child("Delete"))
        .icon(Icon::new(gpui_kit::assets::IconName::Trash).text_color(cx.theme().danger))
}

fn folder_row(path: PathBuf, catalog: &Catalog) -> Option<Row> {
    let mut count = 0;
    let mut latest = None;
    for clip in catalog
        .clips
        .iter()
        .filter(|clip| clip.path.starts_with(&path))
    {
        count += 1;
        latest = Some(latest.map_or(clip.created, |time: SystemTime| time.max(clip.created)));
    }
    (count > 0).then_some(Row::Folder(path, count, latest))
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::{Row, context_targets, folder_row, opens_clip};
    use crate::clip_library::{Catalog, scan};
    use gpui_kit::Modifiers;
    use std::{collections::HashSet, path::PathBuf};
    #[test]
    fn steam_onboarding_follows_overlay_permission_and_does_not_repeat() {
        let mut config = crate::clip_store::Config::default();
        assert!(!super::should_prompt_steam(&config));
        config.clip_discovery = crate::clip_store::Discovery::Declined;
        assert!(!super::should_prompt_steam(&config));
        config.clip_discovery = crate::clip_store::Discovery::Enabled;
        assert!(super::should_prompt_steam(&config));
        for choice in [
            crate::clip_store::Discovery::Enabled,
            crate::clip_store::Discovery::Declined,
        ] {
            config.steam_discovery = choice;
            assert!(!super::should_prompt_steam(&config));
        }
        let old: crate::clip_store::Config =
            serde_json::from_str(r#"{"clip_discovery":"enabled"}"#).unwrap();
        assert!(super::should_prompt_steam(&old));
    }
    #[test]
    fn right_click_targets_rows_without_changing_selection() {
        let first = PathBuf::from("first.mp4");
        let second = PathBuf::from("second.mp4");
        let third = PathBuf::from("third.mp4");
        let selected = HashSet::from([first.clone(), second.clone()]);
        assert_eq!(
            context_targets(&selected, &second),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(context_targets(&selected, &third), vec![third.clone()]);
        assert_eq!(selected, HashSet::from([first, second]));
        let empty = HashSet::new();
        assert_eq!(context_targets(&empty, &third), vec![third]);
        assert!(empty.is_empty());
    }
    #[test]
    fn renaming_a_history_view_updates_selection_and_anchor() {
        let old = std::path::PathBuf::from("Game/old.mp4");
        let new = std::path::PathBuf::from("Game/new.mp4");
        let mut view = super::BrowserView {
            folder: Some("Game".into()),
            selected: std::collections::HashSet::from([old.clone()]),
            anchor: Some(old.clone()),
            offset: gpui_kit::point(gpui_kit::px(0.0), gpui_kit::px(-88.0)),
        };
        view.rename(&old, &new);
        assert!(view.selected.contains(&new));
        assert!(!view.selected.contains(&old));
        assert_eq!(view.anchor, Some(new));
        assert_eq!(view.offset.y, gpui_kit::px(-88.0));
    }

    #[test]
    fn plain_click_opens_clip_but_modified_click_does_not() {
        assert!(opens_clip(&Modifiers::default()));
        for modifiers in [
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
            Modifiers {
                platform: true,
                ..Modifiers::default()
            },
        ] {
            assert!(!opens_clip(&modifiers));
        }
    }

    #[test]
    fn hides_empty_folders_but_keeps_parents_with_nested_clips() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("Game");
        let empty = game.join("Empty");
        let nested = game.join("Highlights");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(empty.join("screenshot.png"), b"image").unwrap();
        std::fs::write(nested.join("clip.mp4"), b"video").unwrap();
        let (catalog, errors) = scan(&[root.path().to_owned()]);
        assert!(errors.is_empty());
        assert!(folder_row(empty, &catalog).is_none());
        assert!(matches!(
            folder_row(game.clone(), &catalog),
            Some(Row::Folder(_, 1, Some(_)))
        ));
        assert!(matches!(
            folder_row(nested, &catalog),
            Some(Row::Folder(_, 1, Some(_)))
        ));
        assert!(folder_row(game, &Catalog::default()).is_none());
    }
}
fn date(time: SystemTime) -> String {
    crate::clip_details::date(time, SystemTime::now())
}
