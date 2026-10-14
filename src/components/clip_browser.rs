use crate::{
    clip_library::{Catalog, Clip, Event, LibraryWorker, Request, Stamp},
    clip_store::{Config, Discovery, History},
};
use gpui_kit::{
    base::{Selectable, VirtualListScrollHandle, v_virtual_list},
    component::{
        ActiveTheme as _, Icon, IconName, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputState},
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

pub enum BrowserEvent {
    Play(PathBuf, Vec<PathBuf>),
    Delete(Vec<PathBuf>),
    Open,
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
    worker: LibraryWorker,
    config: Config,
    catalog: Catalog,
    history: History,
    thumbnails: HashMap<PathBuf, (Stamp, Option<Arc<RenderImage>>)>,
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
    error: Option<String>,
}
impl EventEmitter<BrowserEvent> for ClipBrowser {}

impl ClipBrowser {
    pub fn new(prompt_on_start: bool, cx: &mut Context<Self>) -> Self {
        Self::with_worker(prompt_on_start, LibraryWorker::new(), cx)
    }
    #[cfg(test)]
    pub fn for_test(directory: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_worker(false, LibraryWorker::for_test(directory), cx)
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
    fn with_worker(prompt_on_start: bool, worker: LibraryWorker, cx: &mut Context<Self>) -> Self {
        Self {
            worker,
            config: Config::default(),
            catalog: Catalog::default(),
            history: History::new(),
            thumbnails: HashMap::new(),
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
            error: None,
        }
    }

    pub fn poll(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        let events = self.worker.events().collect::<Vec<_>>();
        for event in events {
            match event {
                Event::Config(config) => {
                    self.config = config;
                    self.config_ready = true;
                    self.rebuild();
                }
                Event::Catalog(catalog) => {
                    self.catalog = catalog;
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
                    self.audio_overrides.retain(|key, gains| {
                        !history
                            .get(key)
                            .is_some_and(|state| &state.track_gains == gains)
                    });
                    self.history = history;
                    self.history_ready = true;
                }
                Event::Renamed(old, new) => {
                    let mut view = self.view();
                    view.rename(&old, &new);
                    self.selected = view.selected;
                    self.anchor = view.anchor;
                    self.thumbnails.remove(&old);
                    self.requested.retain(|(path, _)| path != &old);
                    if let (Ok(old_hash), Ok(new_hash)) = (
                        crate::clip_store::hash_filename(&old),
                        crate::clip_store::hash_filename(&new),
                    ) {
                        if let Some(gains) = self.audio_overrides.remove(&old_hash) {
                            self.audio_overrides.insert(new_hash, gains);
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
                Event::Thumbnail(path, stamp, image) => {
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
                        self.thumbnails
                            .insert(path, (stamp, image.map(crate::render_image)));
                    }
                }
                Event::Error(error) => {
                    crate::logging::Print(crate::logging::Level::Warn, &error);
                    cx.emit(BrowserEvent::Error(error.clone()));
                    self.error = Some(error);
                }
            }
            cx.notify();
        }
        if visible && self.config_ready && self.prompt_on_start {
            self.prompt_on_start = false;
            if self.config.clip_discovery == Discovery::NotAsked {
                self.prompt(window, cx);
            }
        }
    }

    fn prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let yes = cx.weak_entity();
        let no = yes.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let yes = yes.clone(); let no = no.clone();
            alert.title("Detect local game clips?")
                .description("Spacer can read NVIDIA Overlay's gallery location and watch that folder for clips. Your choice is saved locally.")
                .confirm().ok_text("Detect clips").cancel_text("Not now").close_button(false)
                .on_ok(move |_, _, cx| { let _ = yes.update(cx, |this, _| this.worker.send(Request::Consent(true))); true })
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
    pub fn media_state(&self, path: &Path) -> crate::clip_store::MediaState {
        let mut state = crate::clip_store::media_state(&self.history, path);
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
    pub fn prompt_rename(&self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(stem));
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
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
                                            window.push_notification(error, cx);
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
        if self.selected.is_empty() {
            return;
        }
        let paths = self.selected.iter().cloned().collect::<Vec<_>>();
        let owner = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let paths = paths.clone();
            let owner = owner.clone();
            alert
                .title("Move clips to Recycle Bin?")
                .description(format!(
                    "{} selected clips will be moved to the Windows Recycle Bin.",
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
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = selection.await {
                let _ = this.update_in(cx, |this, _, _| {
                    for path in paths {
                        this.worker.send(Request::AddFolder(path));
                    }
                });
            }
        })
        .detach();
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.rows[index].clone() {
            Row::Folder(path, count, latest) => {
                let label = name(&path);
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
                    .child(Icon::new(IconName::FolderOpen).size(px(28.)))
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
                let rename_path = path.clone();
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
                    .when(!watched, |s| {
                        s.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().primary)
                                .child("Unwatched"),
                        )
                    })
                    .child(
                        div()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                Button::new(("rename-clip", index))
                                    .ghost()
                                    .label("Rename")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.prompt_rename(rename_path.clone(), window, cx)
                                    })),
                            ),
                    )
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
                    .into_any_element()
            }
        }
    }
}

impl Render for ClipBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.control && event.keystroke.key == "a" {
                    this.selected = this
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
                    cx.notify();
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape"
                    && event.keystroke.modifiers == Modifiers::default()
                {
                    this.deselect(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "delete" {
                    this.confirm_delete(window, cx);
                    cx.stop_propagation();
                }
            }))
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
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "Game clips".into()),
                        ),
                    )
                    .child(
                        Button::new("detect-clips")
                            .ghost()
                            .label("Detect clips")
                            .on_click(cx.listener(|this, _, window, cx| this.prompt(window, cx))),
                    )
                    .child(
                        Button::new("add-clips")
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .label("Add folder")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_folder(window, cx)),
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
                    .py_2()
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
                    ),
            )
            .when(count > 0, |s| {
                s.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().muted)
                        .child(format!("{count} selected"))
                        .child(
                            Button::new("mark-watched")
                                .ghost()
                                .label("Mark watched")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.mark_selection(true, cx)),
                                ),
                        )
                        .child(
                            Button::new("mark-unwatched")
                                .ghost()
                                .label("Mark unwatched")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.mark_selection(false, cx)),
                                ),
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
            .when_some(self.error.clone(), |s, error| {
                s.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error)
                        .child(
                            Button::new("dismiss-library-error")
                                .ghost()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(self.rows.is_empty(), |s| {
                s.child(div().p_8().text_color(cx.theme().muted_foreground).child(
                    if self.config_ready {
                        "No clips here. Detect clips or add a folder to get started."
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
    }
}

fn opens_clip(modifiers: &Modifiers) -> bool {
    *modifiers == Modifiers::default()
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
    use super::{Row, folder_row, opens_clip};
    use crate::clip_library::{Catalog, scan};
    use gpui_kit::Modifiers;
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
    chrono::DateTime::<chrono::Local>::from(time)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}
