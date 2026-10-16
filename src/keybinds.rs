//! Defaults live in code; keybinds.json contains only additions and tombstones.
use gpui_kit::{Global, Keystroke};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Action {
    pub id: String,
    pub reference: String,
    pub label: String,
    pub context: String,
    pub keys: Vec<String>,
}

fn reference_for(id: &str, context: &str) -> String {
    let namespace = if id.contains("theme") {
        "theme"
    } else if id.contains("keybinding") {
        "keybinds"
    } else if context.starts_with("Player") {
        "player"
    } else if context.starts_with("ClipLibrary") {
        "library"
    } else if context.starts_with("Settings") {
        "settings"
    } else {
        "app"
    };
    format!("{namespace}.{id}")
}

fn resolve_action(id: &str) -> Option<String> {
    actions()
        .into_iter()
        .find(|action| action.id == id || action.reference == id)
        .map(|action| action.id)
}

pub fn actions() -> Vec<Action> {
    let definitions = [
        ("open", "Open media", "Application", "ctrl-o"),
        ("settings", "Open settings", "Application", "ctrl-,"),
        (
            "reload_theme",
            "Reload theme",
            "Application",
            "ctrl-shift-t",
        ),
        (
            "toggle",
            "Play / pause",
            "Player",
            "space|k|media-play-pause",
        ),
        ("play", "Play", "Player", "p"),
        ("pause", "Pause", "Player", "pause"),
        ("stop", "Stop and rewind", "Player", "s|media-stop"),
        (
            "seek_back",
            "Seek backward 10 seconds",
            "Player",
            "left|j|media-previous",
        ),
        (
            "seek_forward",
            "Seek forward 10 seconds",
            "Player",
            "right",
        ),
        (
            "seek_back_long",
            "Seek backward 60 seconds",
            "Player",
            "shift-left",
        ),
        (
            "seek_forward_long",
            "Seek forward 60 seconds",
            "Player",
            "shift-right",
        ),
        ("start", "Seek to start", "Player", "home"),
        ("end", "Seek to end", "Player", "end"),
        ("frame_back", "Previous frame", "Player && paused", ","),
        ("frame_forward", "Next frame", "Player && paused", "."),
        ("volume_up", "Increase volume", "Player", "up"),
        ("volume_down", "Decrease volume", "Player", "down"),
        ("mute", "Toggle mute", "Player", "m"),
        ("speed_up", "Increase playback speed", "Player", "shift-."),
        ("speed_down", "Decrease playback speed", "Player", "shift-,"),
        ("speed_reset", "Reset playback speed", "Player", "r"),
        (
            "track_previous",
            "Select previous audio track",
            "Player",
            "[",
        ),
        ("track_next", "Select next audio track", "Player", "]"),
        ("track_down", "Decrease track volume", "Player", ""),
        ("track_up", "Increase track volume", "Player", ""),
        ("track_toggle", "Toggle audio track", "Player", ""),
        ("fullscreen", "Toggle fullscreen", "Player", "f"),
        ("fullscreen", "Exit fullscreen", "Player && fullscreen", "escape"),
        ("help", "Toggle keyboard shortcuts", "Player", ""),
        ("dismiss", "Dismiss panels", "Player", "escape"),
        ("close_settings", "Close settings", "Settings", "escape"),
        ("select_all", "Select all clips", "ClipLibrary", "ctrl-a"),
        ("deselect", "Deselect clips", "ClipLibrary", "escape"),
        ("delete", "Delete selected clips", "ClipLibrary", "delete"),
        ("library", "Return to library", "Player && !fullscreen", "escape"),
        ("previous_clip", "Previous clip", "Player", ""),
        ("next_clip", "Next clip", "Player", ""),
        ("info", "Toggle file information", "Player", ""),
        ("tracks", "Toggle audio mixer", "Player", ""),
        ("add_folder", "Add library folder", "ClipLibrary", ""),
        ("detect", "Detect NVIDIA folder", "ClipLibrary", ""),
        ("steam_search", "Search Steam games", "ClipLibrary", ""),
        ("steam_folder", "Choose Steam folder", "ClipLibrary", ""),
        ("new_theme", "Create theme", "Settings", ""),
        ("restore_theme", "Restore default theme", "Settings", ""),
        (
            "rename",
            "Rename clip",
            "Player || (ClipLibrary && selected)",
            "",
        ),
        ("history_back", "Navigate back", "Application", ""),
        ("history_forward", "Navigate forward", "Application", ""),
        (
            "play_selected",
            "Play selected clip",
            "ClipLibrary && selected",
            "",
        ),
        (
            "mark_watched",
            "Mark selected clips watched",
            "ClipLibrary && selected",
            "",
        ),
        (
            "mark_unwatched",
            "Mark selected clips unwatched",
            "ClipLibrary && selected",
            "",
        ),
        ("refresh_library", "Refresh library", "ClipLibrary", ""),
        (
            "folder_up",
            "Open parent folder",
            "ClipLibrary && folder",
            "",
        ),
        ("sort_name", "Sort folders by name", "ClipLibrary", ""),
        (
            "sort_newest",
            "Sort folders by newest clip",
            "ClipLibrary",
            "",
        ),
        (
            "sort_count",
            "Sort folders by clip count",
            "ClipLibrary",
            "",
        ),
        (
            "custom_icon",
            "Set custom folder icon",
            "ClipLibrary && folder",
            "",
        ),
        (
            "choose_icon",
            "Choose game folder icon",
            "ClipLibrary && folder",
            "",
        ),
        (
            "restore_icon",
            "Restore assigned folder icon",
            "ClipLibrary && folder",
            "",
        ),
        (
            "toggle_discovery",
            "Toggle NVIDIA discovery",
            "Settings",
            "",
        ),
        (
            "toggle_steam",
            "Toggle local Steam search permission",
            "Settings",
            "",
        ),
        (
            "settings_library",
            "Show Clip Library settings",
            "Settings",
            "",
        ),
        ("settings_theme", "Show Theme settings", "Settings", ""),
        ("settings_keys", "Show Keybinds settings", "Settings", ""),
        ("settings_about", "Show About", "Settings", ""),
        ("rename_theme", "Rename active theme", "Settings", ""),
        ("theme_folder", "Open themes folder", "Settings", ""),
        (
            "create_keybinding",
            "Create keybinding",
            "Settings && KeybindEditor",
            "",
        ),
        (
            "edit_keybinding",
            "Edit selected keybinding",
            "Settings && KeybindEditor",
            "",
        ),
        (
            "remove_keybinding",
            "Remove selected keybinding",
            "Settings && KeybindEditor",
            "",
        ),
        (
            "reset_keybinding",
            "Reset selected action bindings",
            "Settings && KeybindEditor",
            "",
        ),
    ];
    let mut actions: Vec<_> = definitions
        .into_iter()
        .map(|(id, label, context, keys)| Action {
            id: id.into(),
            reference: reference_for(id, context),
            label: label.into(),
            context: context.into(),
            keys: keys
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        })
        .collect();
    for n in 0..10 {
        actions.push(Action {
            id: format!("seek_{n}"),
            reference: format!("player.seek_{n}"),
            label: format!("Seek to {}%", n * 10),
            context: "Player".into(),
            keys: vec![n.to_string()],
        });
    }
    actions
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub action: String,
    pub keys: String,
    pub context: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

pub const CONTEXTS: &[&str] = &[
    "Application",
    "Player",
    "ClipLibrary",
    "Settings",
    "TextInput",
    "Dialog",
    "KeybindEditor",
    "paused",
    "playing",
    "fullscreen",
    "folder",
    "selected",
];
pub const PRESETS: &[&str] = &[
    "Application",
    "Player",
    "ClipLibrary",
    "Settings",
    "TextInput",
    "Dialog",
    "KeybindEditor",
    "Player && paused",
    "Player && playing",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub values: Vec<&'static str>,
}
impl Scope {
    pub fn new(values: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }
    fn has(&self, name: &str) -> bool {
        self.values.contains(&name)
    }
}

#[derive(Debug, Clone)]
enum Expr {
    Atom(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}
impl Expr {
    fn eval(&self, scope: &Scope) -> bool {
        match self {
            Self::Atom(name) => scope.has(name),
            Self::Not(e) => !e.eval(scope),
            Self::And(a, b) => a.eval(scope) && b.eval(scope),
            Self::Or(a, b) => a.eval(scope) || b.eval(scope),
        }
    }
    fn weight(&self) -> usize {
        match self {
            Self::Atom(a) => {
                if a == "Application" {
                    0
                } else {
                    1
                }
            }
            Self::Not(e) => e.weight(),
            Self::And(a, b) | Self::Or(a, b) => a.weight() + b.weight(),
        }
    }
}
fn expression(text: &str) -> Result<Expr, String> {
    let tokens = text
        .replace("&&", " & ")
        .replace("||", " | ")
        .replace('!', " ! ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    let tokens: Vec<_> = tokens.split_whitespace().collect();
    fn atom(t: &[&str], i: &mut usize) -> Result<Expr, String> {
        let token = *t.get(*i).ok_or("Expected a context")?;
        *i += 1;
        match token {
            "!" => Ok(Expr::Not(Box::new(atom(t, i)?))),
            "(" => {
                let result = or(t, i)?;
                if t.get(*i) != Some(&")") {
                    return Err("Missing closing parenthesis".into());
                }
                *i += 1;
                Ok(result)
            }
            name if CONTEXTS.contains(&name) => Ok(Expr::Atom(name.into())),
            _ => Err(format!("Unknown context '{token}'")),
        }
    }
    fn and(t: &[&str], i: &mut usize) -> Result<Expr, String> {
        let mut e = atom(t, i)?;
        while t.get(*i) == Some(&"&") {
            *i += 1;
            e = Expr::And(Box::new(e), Box::new(atom(t, i)?));
        }
        Ok(e)
    }
    fn or(t: &[&str], i: &mut usize) -> Result<Expr, String> {
        let mut e = and(t, i)?;
        while t.get(*i) == Some(&"|") {
            *i += 1;
            e = Expr::Or(Box::new(e), Box::new(and(t, i)?));
        }
        Ok(e)
    }
    let mut i = 0;
    let e = or(&tokens, &mut i)?;
    if i != tokens.len() {
        return Err("Expected && or || between contexts".into());
    }
    Ok(e)
}
pub fn validate_context(text: &str) -> Result<(), String> {
    expression(text).map(|_| ())
}

pub fn normalize_keys(text: &str) -> Result<String, String> {
    let mut keys = vec![];
    for text in text.split_whitespace() {
        let lower = text.to_lowercase();
        if lower.starts_with("media-") {
            if ![
                "media-play-pause",
                "media-stop",
                "media-previous",
                "media-next",
            ]
            .contains(&lower.as_str())
            {
                return Err("Unknown media key".into());
            }
            keys.push(lower);
            continue;
        }
        let key = Keystroke::parse(&lower).map_err(|e| e.to_string())?;
        keys.push(stroke(&key));
    }
    if keys.is_empty() || keys.len() > 4 {
        return Err("Record between one and four keystrokes".into());
    }
    Ok(keys.join(" "))
}
pub fn stroke(key: &Keystroke) -> String {
    let mut parts = vec![];
    if key.modifiers.control {
        parts.push("ctrl");
    }
    if key.modifiers.alt {
        parts.push("alt");
    }
    if key.modifiers.platform {
        parts.push("cmd");
    }
    if key.modifiers.shift {
        parts.push("shift");
    }
    let name = match key.key.as_str() {
        ">" => ".",
        "<" => ",",
        "?" => "/",
        "+" => "=",
        other => other,
    };
    parts.push(name);
    parts.join("-")
}

pub struct Keymap {
    pub changes: Vec<Binding>,
    pub error: Option<String>,
    path: PathBuf,
    pending: Vec<String>,
    pending_scope: Scope,
    since: Instant,
    delayed: Option<String>,
}
impl Global for Keymap {}
impl Keymap {
    pub fn open(path: PathBuf) -> Self {
        let mut this = Self {
            changes: vec![],
            error: None,
            path,
            pending: vec![],
            pending_scope: Scope::default(),
            since: Instant::now(),
            delayed: None,
        };
        match fs::read(&this.path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<Binding>>(&bytes) {
                Ok(mut bindings) => {
                    let result: Result<(), String> = bindings.iter_mut().try_for_each(|b| {
                        b.action = resolve_action(&b.action)
                            .ok_or_else(|| format!("Unknown action: {}", b.action))?;
                        Self::validate(b)?;
                        b.keys = normalize_keys(&b.keys)?;
                        Ok(())
                    });
                    match result {
                        Ok(()) => this.changes = bindings,
                        Err(e) => this.error = Some(e),
                    }
                }
                Err(e) => this.error = Some(e.to_string()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => this.error = Some(e.to_string()),
        }
        this
    }
    fn validate(b: &Binding) -> Result<(), String> {
        if !actions().iter().any(|a| a.id == b.action) {
            return Err(format!("Unknown action: {}", b.action));
        }
        validate_context(&b.context)?;
        normalize_keys(&b.keys)?;
        Ok(())
    }
    pub fn defaults() -> Vec<Binding> {
        actions()
            .into_iter()
            .flat_map(|a| {
                a.keys.into_iter().map(move |keys| Binding {
                    action: a.id.clone(),
                    keys,
                    context: a.context.clone(),
                    disabled: false,
                })
            })
            .collect()
    }
    pub fn effective(&self) -> Vec<(Binding, bool)> {
        let mut all: Vec<_> = Self::defaults()
            .into_iter()
            .filter(|b| {
                !self
                    .changes
                    .iter()
                    .any(|u| u.keys == b.keys && u.context == b.context)
            })
            .map(|b| (b, false))
            .collect();
        all.extend(
            self.changes
                .iter()
                .filter(|b| !b.disabled)
                .cloned()
                .map(|b| (b, true)),
        );
        all
    }
    fn commit(&mut self, changes: Vec<Binding>) -> Result<(), String> {
        if self.error.is_some() {
            return Err("Repair keybinds.json and restart StarPlayer before saving. The invalid file was left unchanged.".into());
        }
        fs::create_dir_all(self.path.parent().ok_or("Missing keymap directory")?)
            .map_err(|e| e.to_string())?;
        crate::theme::atomic_json(&self.path, &changes, false)?;
        self.changes = changes;
        self.clear();
        Ok(())
    }
    pub fn save(&mut self, old: Option<&Binding>, mut binding: Binding) -> Result<(), String> {
        binding.action = resolve_action(&binding.action)
            .ok_or_else(|| format!("Unknown action: {}", binding.action))?;
        Self::validate(&binding)?;
        binding.keys = normalize_keys(&binding.keys)?;
        let mut changes = self.changes.clone();
        if let Some(old) = old {
            Self::remove_from(&mut changes, old);
        }
        changes.retain(|b| !(b.keys == binding.keys && b.context == binding.context));
        if !Self::defaults().contains(&binding) {
            changes.push(binding);
        }
        self.commit(changes)
    }
    fn remove_from(changes: &mut Vec<Binding>, binding: &Binding) {
        changes.retain(|b| !(b.keys == binding.keys && b.context == binding.context));
        for default in Self::defaults()
            .into_iter()
            .filter(|b| b.keys == binding.keys && b.context == binding.context)
        {
            changes.push(Binding {
                disabled: true,
                ..default
            });
        }
    }
    pub fn remove(&mut self, binding: &Binding) -> Result<(), String> {
        let mut changes = self.changes.clone();
        Self::remove_from(&mut changes, binding);
        self.commit(changes)
    }
    pub fn reset(&mut self, binding: Option<&Binding>) -> Result<(), String> {
        let defaults = Self::defaults();
        let changes = self
            .changes
            .iter()
            .filter(|b| {
                binding.is_some_and(|target| {
                    b.action != target.action
                        && !defaults.iter().any(|default| {
                            default.action == target.action
                                && default.keys == b.keys
                                && default.context == b.context
                        })
                })
            })
            .cloned()
            .collect();
        self.commit(changes)
    }
    pub fn conflicts(&self, binding: &Binding, old: Option<&Binding>) -> Vec<String> {
        self.effective()
            .into_iter()
            .filter(|(b, _)| {
                Some(b) != old
                    && b.action != binding.action
                    && (b.keys == binding.keys
                        || b.keys.starts_with(&(binding.keys.clone() + " "))
                        || binding.keys.starts_with(&(b.keys.clone() + " ")))
                    && overlaps(&b.context, &binding.context)
            })
            .map(|(b, _)| format!("{} · {} · {}", b.action, b.keys, b.context))
            .collect()
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.delayed = None;
    }
    pub fn expire(&mut self, scope: &Scope) -> Option<String> {
        if !self.pending.is_empty()
            && (self.since.elapsed() >= Duration::from_secs(1) || &self.pending_scope != scope)
        {
            let action = if &self.pending_scope == scope {
                self.delayed.take()
            } else {
                None
            };
            self.clear();
            action
        } else {
            None
        }
    }
    pub fn feed(&mut self, key: &str, scope: Scope) -> (Option<String>, bool) {
        if self.pending_scope != scope || self.since.elapsed() >= Duration::from_secs(1) {
            self.clear();
        }
        self.pending_scope = scope.clone();
        self.since = Instant::now();
        self.pending.push(key.into());
        let sequence = self.pending.join(" ");
        let mut matches: Vec<_> = self
            .effective()
            .into_iter()
            .filter_map(|(b, user)| {
                let expr = expression(&b.context).ok()?;
                // Text editing and modal dialogs opt in explicitly, never through Application/Player.
                if scope.has("TextInput") && !b.context.contains("TextInput") {
                    return None;
                }
                if scope.has("Dialog") && !b.context.contains("Dialog") {
                    return None;
                }
                (expr.eval(&scope)
                    && (b.keys == sequence || b.keys.starts_with(&(sequence.clone() + " "))))
                .then_some((b, user, expr.weight()))
            })
            .collect();
        matches.sort_by_key(|(_, user, weight)| (*weight, *user));
        let exact = matches
            .iter()
            .rev()
            .find(|(b, _, _)| b.keys == sequence)
            .map(|(b, _, _)| b.action.clone());
        if matches.iter().any(|(b, _, _)| b.keys != sequence) {
            self.delayed = exact;
            return (None, true);
        }
        let had_prefix = self.pending.len() > 1;
        self.clear();
        if exact.is_some() {
            return (exact, true);
        }
        if had_prefix {
            return self.feed(key, scope);
        }
        (None, false)
    }
    pub fn hints(&self) -> Vec<(String, String)> {
        let all = self.effective();
        actions()
            .into_iter()
            .filter_map(|a| {
                let keys = all
                    .iter()
                    .filter(|(b, _)| b.action == a.id)
                    .map(|(b, _)| b.keys.clone())
                    .collect::<Vec<_>>();
                (!keys.is_empty()).then(|| (a.label, keys.join(" / ")))
            })
            .collect()
    }
}

fn overlaps(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (expression(a), expression(b)) else {
        return false;
    };
    // Enumerate realizable app states rather than treating unrelated pages as conflicts.
    for page in ["Player", "ClipLibrary", "Settings"] {
        for bits in 0..128 {
            let mut scope = Scope::new(["Application", page]);
            for (i, name) in [
                "paused",
                "fullscreen",
                "folder",
                "selected",
                "TextInput",
                "Dialog",
                "KeybindEditor",
            ]
            .iter()
            .enumerate()
            {
                if bits & (1 << i) != 0 {
                    scope.values.push(name);
                }
            }
            if page == "Player" && !scope.has("paused") {
                scope.values.push("playing");
            }
            if (scope.has("paused") && page != "Player")
                || (scope.has("KeybindEditor") && page != "Settings")
            {
                continue;
            }
            if a.eval(&scope) && b.eval(&scope) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespaced_references_resolve_without_breaking_existing_keymaps() {
        let actions = actions();
        assert_eq!(
            actions.iter().find(|a| a.id == "pause").unwrap().reference,
            "player.pause"
        );
        let references: std::collections::HashSet<_> =
            actions.iter().map(|a| &a.reference).collect();
        assert_eq!(references.len(), actions.len());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybinds.json");
        fs::write(
            &path,
            r#"[{"action":"player.pause","keys":"ctrl-p","context":"Player"}]"#,
        )
        .unwrap();
        let mut map = Keymap::open(path);
        assert!(map.error.is_none());
        assert_eq!(
            map.feed("ctrl-p", Scope::new(["Application", "Player"])).0,
            Some("pause".into())
        );
    }
    #[test]
    fn user_overrides_and_scope_specificity() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = Keymap::open(dir.path().join("keybinds.json"));
        map.save(
            None,
            Binding {
                action: "pause".into(),
                keys: "space".into(),
                context: "Player".into(),
                disabled: false,
            },
        )
        .unwrap();
        assert_eq!(
            map.feed("space", Scope::new(["Application", "Player"])).0,
            Some("pause".into())
        );
        map.save(
            None,
            Binding {
                action: "play".into(),
                keys: "space".into(),
                context: "Player && paused".into(),
                disabled: false,
            },
        )
        .unwrap();
        assert_eq!(
            map.feed("space", Scope::new(["Application", "Player", "paused"]))
                .0,
            Some("play".into())
        );
        assert_eq!(
            map.feed("space", Scope::new(["Application", "Settings"])),
            (None, false)
        );
    }
    #[test]
    fn typing_and_dialogs_require_explicit_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = Keymap::open(dir.path().join("keybinds.json"));
        let scope = Scope::new(["Application", "Player", "TextInput"]);
        assert_eq!(map.feed("k", scope.clone()), (None, false));
        assert_eq!(map.feed("ctrl-o", scope.clone()), (None, false));
        map.save(
            None,
            Binding {
                action: "settings".into(),
                keys: "ctrl-o".into(),
                context: "TextInput".into(),
                disabled: false,
            },
        )
        .unwrap();
        assert_eq!(map.feed("ctrl-o", scope).0, Some("settings".into()));
        assert_eq!(
            map.feed("k", Scope::new(["Application", "Player", "Dialog"])),
            (None, false)
        );
    }
    #[test]
    fn exact_prefix_waits_then_runs_and_context_change_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = Keymap::open(dir.path().join("keybinds.json"));
        map.save(
            None,
            Binding {
                action: "settings".into(),
                keys: "k s".into(),
                context: "Player".into(),
                disabled: false,
            },
        )
        .unwrap();
        let scope = Scope::new(["Application", "Player"]);
        assert_eq!(map.feed("k", scope.clone()), (None, true));
        map.since = Instant::now() - Duration::from_secs(2);
        assert_eq!(map.expire(&scope), Some("toggle".into()));
        map.feed("k", scope);
        assert_eq!(map.expire(&Scope::new(["Application", "Settings"])), None);
        assert!(map.pending.is_empty());
    }
    #[test]
    fn invalid_existing_keymap_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybinds.json");
        fs::write(&path, "bad").unwrap();
        let mut map = Keymap::open(path.clone());
        assert!(map.error.is_some());
        assert!(map.reset(None).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "bad");
    }
    #[test]
    fn editing_a_default_keeps_its_removal_and_reset_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybinds.json");
        let mut map = Keymap::open(path.clone());
        let original = Keymap::defaults()
            .into_iter()
            .find(|b| b.action == "open")
            .unwrap();
        let replacement = Binding {
            keys: "ctrl-k ctrl-o".into(),
            ..original.clone()
        };
        map.save(Some(&original), replacement.clone()).unwrap();
        assert_eq!(map.changes.len(), 2);
        let mut map = Keymap::open(path);
        assert!(!map.effective().iter().any(|(b, _)| b == &original));
        assert!(map.effective().iter().any(|(b, _)| b == &replacement));
        map.reset(Some(&replacement)).unwrap();
        assert!(map.changes.is_empty());
    }
    #[test]
    fn resetting_a_shadowed_default_removes_the_conflicting_user_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = Keymap::open(dir.path().join("keybinds.json"));
        let original = Keymap::defaults()
            .into_iter()
            .find(|b| b.action == "open")
            .unwrap();
        map.save(
            Some(&original),
            Binding {
                action: "toggle".into(),
                ..original.clone()
            },
        )
        .unwrap();
        assert!(!map.effective().iter().any(|(b, _)| b == &original));
        map.reset(Some(&original)).unwrap();
        assert!(map.effective().iter().any(|(b, _)| b == &original));
        assert!(map.changes.is_empty());
    }
    #[test]
    fn contexts_and_conflicts() {
        assert!(validate_context("Player && (paused || !fullscreen)").is_ok());
        for invalid in ["", "Player &&", "Unknown", "(Player", "Player Settings"] {
            assert!(validate_context(invalid).is_err());
        }
        assert!(!overlaps("Player", "Settings"));
        assert!(overlaps("Application", "Player && paused"));
        assert!(!overlaps("Player && paused", "Player && !paused"));
        assert!(!overlaps("Player", "KeybindEditor"));
        assert!(!overlaps("paused", "Settings"));
    }
    #[test]
    fn overrides_only_and_removal_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybinds.json");
        let mut map = Keymap::open(path.clone());
        let original = Keymap::defaults()
            .into_iter()
            .find(|b| b.action == "open")
            .unwrap();
        map.remove(&original).unwrap();
        assert_eq!(map.changes.len(), 1);
        let mut map = Keymap::open(path);
        assert!(!map.effective().iter().any(|(b, _)| b == &original));
        map.reset(Some(&original)).unwrap();
        assert!(map.changes.is_empty());
    }
    #[test]
    fn sequences_scopes_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = Keymap::open(dir.path().join("keybinds.json"));
        map.save(
            None,
            Binding {
                action: "settings".into(),
                keys: "ctrl-k ctrl-s".into(),
                context: "Application".into(),
                disabled: false,
            },
        )
        .unwrap();
        let scope = Scope::new(["Application", "Player"]);
        assert_eq!(map.feed("ctrl-k", scope.clone()), (None, true));
        assert_eq!(
            map.feed("ctrl-s", scope.clone()),
            (Some("settings".into()), true)
        );
        map.feed("ctrl-k", scope.clone());
        map.since = Instant::now() - Duration::from_secs(2);
        assert_eq!(map.feed("ctrl-s", scope), (None, false));
    }
    #[test]
    fn all_default_keystrokes_are_valid() {
        for b in Keymap::defaults() {
            assert_eq!(normalize_keys(&b.keys).unwrap(), b.keys);
        }
    }
}
