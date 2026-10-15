use super::*;
use crate::{
    keybinds::{self, Binding, Keymap},
    theme::{self, ThemeEntry, ThemeLibrary},
};
use gpui_kit::base::FocusableExt as _;
use gpui_kit::component::{
    Sizable as _,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    sidebar::{Sidebar, SidebarFooter, SidebarHeader, SidebarMenu, SidebarMenuItem},
    table::{Column, DataTable, TableDelegate, TableEvent, TableState},
};

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Player,
    Library,
    Theme,
    Keys,
    About,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Player => "Player",
            Self::Library => "Clip Library",
            Self::Theme => "Theme",
            Self::Keys => "Keybinds",
            Self::About => "About",
        }
    }
}

pub(super) struct SettingsState {
    page: Page,
    search: Entity<InputState>,
    key_filter: Entity<InputState>,
    themes: Entity<SelectState<Vec<ThemeChoice>>>,
    remember_playback: Entity<SelectState<Vec<RememberChoice>>>,
    theme_revision: u64,
    theme_table: Entity<TableState<ThemeTable>>,
    table: Entity<TableState<KeyTable>>,
    selected: Option<KeyRow>,
    editor: Option<Entity<BindingEditor>>,
    editor_subscription: Option<Subscription>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    theme_layout: ThemeLayout,
}

#[cfg(test)]
#[derive(Default)]
struct ThemeLayout {
    active_row: Rc<std::cell::Cell<Bounds<Pixels>>>,
    selector: Rc<std::cell::Cell<Bounds<Pixels>>>,
    table: Rc<std::cell::Cell<Bounds<Pixels>>>,
}

#[derive(Clone)]
struct RememberChoice(crate::clip_store::RememberPlayback);

impl SelectItem for RememberChoice {
    type Value = crate::clip_store::RememberPlayback;
    fn title(&self) -> SharedString {
        self.0.label().into()
    }
    fn value(&self) -> &Self::Value {
        &self.0
    }
}

#[derive(Clone)]
struct ThemeChoice {
    name: SharedString,
    file: String,
}
impl SelectItem for ThemeChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.name.clone()
    }
    fn value(&self) -> &String {
        &self.file
    }
}
#[derive(Clone)]
struct ActionChoice {
    name: SharedString,
    id: String,
    reference: String,
}
impl SelectItem for ActionChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.name.clone()
    }
    fn value(&self) -> &String {
        &self.id
    }
    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.name.to_lowercase().contains(&query) || self.reference.to_lowercase().contains(&query)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct KeyRow {
    binding: Binding,
    source: &'static str,
    label: String,
    reference: String,
}
#[derive(Clone, PartialEq, Eq)]
enum KeyTableRow {
    Context(String),
    Binding(KeyRow),
}
impl KeyTableRow {
    fn binding(&self) -> Option<KeyRow> {
        match self {
            Self::Binding(row) => Some(row.clone()),
            _ => None,
        }
    }
}
struct KeyTable {
    rows: Vec<KeyTableRow>,
    width: f32,
}

#[derive(Debug, PartialEq, Eq)]
enum KeyRowBand {
    Context,
    First,
    Second,
}

impl KeyTable {
    fn row_band(&self, index: usize) -> KeyRowBand {
        if matches!(self.rows.get(index), Some(KeyTableRow::Context(_))) {
            return KeyRowBand::Context;
        }
        let group_start = self.rows[..index.min(self.rows.len())]
            .iter()
            .rposition(|row| matches!(row, KeyTableRow::Context(_)))
            .map_or(0, |header| header + 1);
        if (index - group_start).is_multiple_of(2) {
            KeyRowBand::First
        } else {
            KeyRowBand::Second
        }
    }
}
impl TableDelegate for KeyTable {
    fn columns_count(&self, _: &App) -> usize {
        5
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, index: usize, _: &App) -> Column {
        let (id, label, fraction) = [
            ("action", "Action", 0.25),
            ("reference", "Reference", 0.25),
            ("keys", "Keystrokes", 0.17),
            ("context", "Context", 0.23),
            ("source", "Source", 0.10),
        ][index];
        Column::new(id, label).width(self.width.max(1100.) * fraction)
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let row = match &self.rows[row] {
            KeyTableRow::Binding(row) => row,
            KeyTableRow::Context(context) => {
                return div()
                    .size_full()
                    .flex()
                    .items_center()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if col == 0 {
                        context.clone()
                    } else {
                        String::new()
                    })
                    .into_any_element();
            }
        };
        let text = match col {
            0 => row.label.clone(),
            1 => row.reference.clone(),
            2 => {
                if row.binding.keys.is_empty() {
                    "-".into()
                } else {
                    row.binding.keys.clone()
                }
            }
            3 => row.binding.context.clone(),
            _ => row.source.into(),
        };
        div()
            .id(("key-cell", col))
            .size_full()
            .flex()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .text_color(
                if row.binding.disabled || row.binding.keys.is_empty() || col == 1 {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                },
            )
            .child(text)
            .into_any_element()
    }
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let background = match self.row_band(row) {
            KeyRowBand::Context => cx.theme().muted,
            KeyRowBand::First => cx.theme().group_box,
            KeyRowBand::Second => cx.theme().background,
        };
        div().id(("key-row", row)).bg(background).when(
            matches!(self.rows.get(row), Some(KeyTableRow::Context(_))),
            |row| row.border_t_1().border_color(cx.theme().border),
        )
    }
}

struct ThemeTable {
    rows: Vec<ThemeEntry>,
    active: String,
    width: f32,
    owner: WeakEntity<ClipBrowser>,
}
impl TableDelegate for ThemeTable {
    fn columns_count(&self, _: &App) -> usize {
        4
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, index: usize, _: &App) -> Column {
        let (id, label, fraction) = [
            ("name", "Theme", 0.26),
            ("file", "Filename", 0.28),
            ("status", "Status", 0.10),
            ("actions", "Actions", 0.36),
        ][index];
        Column::new(id, label).width(self.width.max(650.) * fraction)
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let entry = self.rows[row].clone();
        let active = entry.file == self.active;
        if col < 3 {
            return div()
                .text_sm()
                .text_color(if col == 1 {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                })
                .child(match col {
                    0 => entry.name,
                    1 => entry.file,
                    _ => {
                        if active {
                            "Active".into()
                        } else {
                            String::new()
                        }
                    }
                })
                .into_any_element();
        }
        let owner = self.owner.clone();
        let file = entry.file.clone();
        let rename_owner = self.owner.clone();
        let rename_file = entry.file.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new(("use-theme", row))
                    .ghost()
                    .label("Use")
                    .disabled(active)
                    .on_click(move |_, window, cx| {
                        let result = cx.global_mut::<ThemeLibrary>().select(&file);
                        let _ = owner.update(cx, |this, cx| this.theme_result(result, window, cx));
                    }),
            )
            .child(
                Button::new(("rename-theme-row", row))
                    .ghost()
                    .label("Rename")
                    .on_click(move |_, window, cx| {
                        let _ = rename_owner.update(cx, |this, cx| {
                            this.prompt_theme_rename(&rename_file, window, cx)
                        });
                    }),
            )
            .child(
                Button::new(("open-theme-file", row))
                    .ghost()
                    .label("Show file")
                    .on_click(move |_, _, cx| {
                        cx.reveal_path(&cx.global::<ThemeLibrary>().directory.join(&entry.file));
                    }),
            )
            .into_any_element()
    }
}

fn settings_input(state: &Entity<InputState>, window: &Window, cx: &App) -> Input {
    Input::new(state).focus_bordered(false).border_color(
        if state.focus_handle(cx).is_focused(window) {
            cx.theme().ring
        } else {
            cx.theme().input
        },
    )
}

fn settings_body_width(window: &Window) -> f32 {
    (window.viewport_size().width.as_f32() - 274.).max(0.)
}

fn key_row(binding: Binding, source: &'static str, actions: &[keybinds::Action]) -> KeyRow {
    let action = actions
        .iter()
        .find(|a| a.id == binding.action)
        .expect("Validated action");
    KeyRow {
        label: action.label.clone(),
        reference: action.reference.clone(),
        binding,
        source,
    }
}

fn group_key_rows(mut rows: Vec<KeyRow>) -> Vec<KeyTableRow> {
    rows.sort_by(|a, b| {
        (&a.binding.context, &a.label, &a.binding.keys).cmp(&(
            &b.binding.context,
            &b.label,
            &b.binding.keys,
        ))
    });
    let mut grouped = Vec::new();
    let mut context = None;
    for row in rows {
        if context.as_ref() != Some(&row.binding.context) {
            context = Some(row.binding.context.clone());
            grouped.push(KeyTableRow::Context(row.binding.context.clone()));
        }
        grouped.push(KeyTableRow::Binding(row));
    }
    grouped
}

pub(super) fn setting_row(title: &'static str, description: &'static str, cx: &App) -> Div {
    div()
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_4()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
}

impl ClipBrowser {
    fn ensure_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_state.is_some() {
            return;
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings…"));
        let key_filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter actions, keystrokes or contexts…")
        });
        let themes = cx.new(|cx| SelectState::new(Vec::<ThemeChoice>::new(), None, window, cx));
        let remember_playback = cx.new(|cx| {
            let mut state = SelectState::new(
                crate::clip_store::RememberPlayback::OPTIONS
                    .into_iter()
                    .map(RememberChoice)
                    .collect::<Vec<_>>(),
                None,
                window,
                cx,
            );
            state.set_selected_value(&self.config.player.remember_playback, window, cx);
            state
        });
        let table = cx.new(|cx| {
            TableState::new(
                KeyTable {
                    rows: vec![],
                    width: 1100.,
                },
                window,
                cx,
            )
        });
        let owner = cx.weak_entity();
        let theme_table = cx.new(|cx| {
            TableState::new(
                ThemeTable {
                    rows: vec![],
                    active: String::new(),
                    width: 950.,
                    owner,
                },
                window,
                cx,
            )
            .row_selectable(false)
        });
        let subscriptions = vec![
            cx.subscribe_in(&remember_playback, window, |this, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.config.player.remember_playback = *value;
                    this.worker.send(Request::RememberPlayback(*value));
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&key_filter, |this, _, _: &InputEvent, cx| {
                this.settings_state.as_mut().unwrap().selected = None;
                cx.notify();
            }),
            cx.subscribe_in(&table, window, |this, table, event, window, cx| {
                match event {
                    TableEvent::SelectRow(index) | TableEvent::DoubleClickedRow(index) => {
                        let selected = table
                            .read(cx)
                            .delegate()
                            .rows
                            .get(*index)
                            .and_then(KeyTableRow::binding);
                        let editable = selected.is_some();
                        this.settings_state.as_mut().unwrap().selected = selected;
                        if editable && matches!(event, TableEvent::DoubleClickedRow(_)) {
                            this.edit_keybinding(false, window, cx);
                        }
                    }
                    TableEvent::ClearSelection => {
                        this.settings_state.as_mut().unwrap().selected = None
                    }
                    _ => {}
                }
                cx.notify();
            }),
            cx.subscribe_in(&themes, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(file)) = event {
                    let result = cx.global_mut::<ThemeLibrary>().select(file);
                    this.theme_result(result, window, cx);
                }
            }),
        ];
        self.settings_state = Some(SettingsState {
            page: Page::Library,
            search,
            key_filter,
            themes,
            remember_playback,
            theme_revision: u64::MAX,
            theme_table,
            table,
            selected: None,
            editor: None,
            editor_subscription: None,
            error: None,
            _subscriptions: subscriptions,
            #[cfg(test)]
            theme_layout: ThemeLayout::default(),
        });
    }

    pub(super) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_settings(window, cx);
        if cx.has_global::<ThemeLibrary>() {
            let library = cx.global::<ThemeLibrary>();
            let state = self.settings_state.as_mut().unwrap();
            if state.theme_revision != library.revision {
                let items: Vec<_> = library
                    .entries
                    .iter()
                    .map(|e| ThemeChoice {
                        name: e.name.clone().into(),
                        file: e.file.clone(),
                    })
                    .collect();
                let active = library.active.clone();
                state.theme_revision = library.revision;
                state.themes.update(cx, |s, cx| {
                    s.set_items(items, window, cx);
                    s.set_selected_value(&active, window, cx);
                });
            }
        }
        let state = self.settings_state.as_ref().unwrap();
        let page = state.page;
        let query = state.search.read(cx).value().to_lowercase();
        let sidebar = Sidebar::new("settings-sidebar")
            .w(px(210.))
            .bg(cx.theme().sidebar)
            .header(SidebarHeader::new().child(settings_input(&state.search, window, cx)))
            .child(
                SidebarMenu::new().children(
                    [
                        Page::Player,
                        Page::Library,
                        Page::Theme,
                        Page::Keys,
                        Page::About,
                    ]
                    .into_iter()
                    .map(|p| {
                        SidebarMenuItem::new(p.title())
                            .active(page == p)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let state = this.settings_state.as_mut().unwrap();
                                state.page = p;
                                state.selected = None;
                                this.focus.focus(window, cx);
                                cx.notify();
                            }))
                    }),
                ),
            )
            .footer(
                SidebarFooter::new().child(
                    Button::new("settings-back")
                        .ghost()
                        .icon(IconName::ChevronLeft)
                        .label("Back")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::CloseSettings))),
                ),
            );
        let content = match page {
            Page::Player => self.render_player_settings(&query, window, cx),
            Page::Library => self.render_library_filtered(&query, cx),
            Page::Theme => self.render_theme(window, cx),
            Page::Keys => self.render_keymap(window, cx),
            Page::About => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_lg()
                        .child(format!("Spacer {}", env!("CARGO_PKG_VERSION"))),
                )
                .child("Video playback powered by embedded libmpv.")
                .child("Built with GPUI and GPUI Kit.")
                .child(
                    "GPUI and GPUI Kit use Apache-2.0. The decoder includes GPL/LGPL components.",
                )
                .child(
                    Button::new("licenses")
                        .ghost()
                        .label("Decoder license notices")
                        .on_click(|_, window, cx| {
                            window.open_dialog(cx, |dialog, _, _| {
                                dialog.title("Decoder notices").w(px(640.)).child(
                                    div()
                                        .id("decoder-notices")
                                        .max_h(px(360.))
                                        .overflow_y_scroll()
                                        .text_sm()
                                        .child(include_str!(
                                            "../../../licenses/THIRD_PARTY_NOTICES.md"
                                        )),
                                )
                            });
                        }),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(cx.theme().background)
            .track_focus(&self.focus)
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .p_6()
                    .gap_4()
                    .child(div().text_xl().child(page.title()))
                    .when_some(
                        self.settings_state.as_ref().unwrap().error.clone(),
                        |s, e| s.child(div().text_sm().text_color(cx.theme().danger).child(e)),
                    )
                    .child(content),
            )
            .into_any_element()
    }

    fn render_player_settings(
        &self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = &self.settings_state.as_ref().unwrap().remember_playback;
        if selector.read(cx).selected_value() != Some(&self.config.player.remember_playback) {
            selector.update(cx, |state, cx| {
                state.set_selected_value(&self.config.player.remember_playback, window, cx);
            });
        }
        let matches =
            "player.remember_playback remember playback resume minimum clip length duration"
                .contains(query);
        div().flex().flex_col().gap_3()
            .when(matches, |body| body.child(
                div().id("player.remember_playback").flex().items_center().justify_between().gap_4()
                    .child(div().flex_1().flex().flex_col().gap_1()
                        .child("Remember playback")
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child("Offer to resume clips longer than this duration. All remembers every clip."))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child("player.remember_playback")))
                    .child(div().w(px(160.)).flex_shrink_0().child(Select::new(selector)))
            ))
            .when(!matches, |body| body.child("No matching player settings."))
            .into_any_element()
    }

    fn render_library_filtered(&self, query: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("library-settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(self.render_library_settings(query, cx))
            .into_any_element()
    }

    fn render_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !cx.has_global::<ThemeLibrary>() {
            return div()
                .child("Theme directory is unavailable.")
                .into_any_element();
        }
        let library = cx.global::<ThemeLibrary>();
        let entries = library.entries.clone();
        let active = library.active.clone();
        let error = library.error.clone();
        let width = settings_body_width(window);
        let state = self.settings_state.as_ref().unwrap();
        state.theme_table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.rows != entries || delegate.active != active || delegate.width != width {
                delegate.rows = entries;
                delegate.active = active;
                delegate.width = width;
                table.refresh(cx);
            }
        });
        div()
            .id("theme-settings-scroll")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                setting_row(
                    "Active theme",
                    "Changes apply immediately. Theme files reload when edited.",
                    cx,
                )
                .map(|row| {
                    #[cfg(test)]
                    let row = row
                        .relative()
                        .child(crate::measure_bounds(state.theme_layout.active_row.clone()));
                    row
                })
                .child(
                    // Select's root grows to its parent width. Isolate that
                    // behavior so it cannot squeeze the setting label to zero.
                    div()
                        .relative()
                        .w(px(230.))
                        .flex_shrink_0()
                        .map(|selector| {
                            #[cfg(test)]
                            let selector = selector
                                .child(crate::measure_bounds(state.theme_layout.selector.clone()));
                            selector
                        })
                        .child(
                            Select::new(&state.themes)
                                .w_full()
                                .menu_width(px(230.))
                                .focus_ring(false),
                        ),
                ),
            )
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("create-theme")
                            .label("Create new theme")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let result = cx.global_mut::<ThemeLibrary>().create();
                                this.theme_result(result, window, cx);
                            })),
                    )
                    .child(
                        Button::new("reload-theme")
                            .ghost()
                            .label("Reload")
                            .on_click(|_, _, cx| theme::apply(cx)),
                    )
                    .child(
                        Button::new("restore-theme")
                            .ghost()
                            .label("Restore default")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let result = cx.global_mut::<ThemeLibrary>().restore_original();
                                this.theme_result(result, window, cx);
                            })),
                    )
                    .child(
                        Button::new("open-themes")
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .label("Open themes folder")
                            .on_click(|_, _, cx| {
                                cx.reveal_path(&cx.global::<ThemeLibrary>().directory.clone())
                            }),
                    ),
            )
            .when_some(error, |s, e| {
                s.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .map(|table| {
                        #[cfg(test)]
                        let table =
                            table.child(crate::measure_bounds(state.theme_layout.table.clone()));
                        table
                    })
                    .child(
                        DataTable::new(&state.theme_table)
                            .with_size(px(42.))
                            .stripe(true)
                            .bordered(false),
                    ),
            )
            .into_any_element()
    }

    fn prompt_theme_rename(&mut self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = cx
            .global::<ThemeLibrary>()
            .entries
            .iter()
            .find(|e| e.file == file)
            .cloned()
        else {
            return;
        };
        let owner = cx.weak_entity();
        let editor = cx.new(|cx| ThemeRename::new(entry, owner, window, cx));
        editor.read(cx).input.focus_handle(cx).focus(window, cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Rename theme")
                .w(px(440.))
                .child(editor.clone())
        });
    }

    fn theme_result(
        &mut self,
        result: Result<(), String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                theme::apply(cx);
                self.settings_state.as_mut().unwrap().error = None;
            }
            Err(error) => self.settings_state.as_mut().unwrap().error = Some(error),
        }
        cx.notify();
    }

    fn render_keymap(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self
            .settings_state
            .as_ref()
            .unwrap()
            .key_filter
            .read(cx)
            .value()
            .to_lowercase();
        let map = cx.global::<Keymap>();
        let actions = keybinds::actions();
        let mut rows: Vec<_> = map
            .effective()
            .into_iter()
            .map(|(binding, user)| {
                key_row(binding, if user { "User" } else { "Default" }, &actions)
            })
            .collect();
        rows.extend(
            map.changes
                .iter()
                .filter(|b| b.disabled)
                .map(|b| key_row(b.clone(), "Disabled", &actions)),
        );
        for action in &actions {
            if !rows
                .iter()
                .any(|r| r.binding.action == action.id && !r.binding.disabled)
            {
                rows.push(KeyRow {
                    label: action.label.clone(),
                    reference: action.reference.clone(),
                    source: "-",
                    binding: Binding {
                        action: action.id.clone(),
                        keys: String::new(),
                        context: action.context.clone(),
                        disabled: false,
                    },
                });
            }
        }
        rows.retain(|r| {
            format!(
                "{} {} {} {} {}",
                r.label, r.reference, r.binding.action, r.binding.keys, r.binding.context
            )
            .to_lowercase()
            .contains(&query)
        });
        let rows = group_key_rows(rows);
        let width = settings_body_width(window);
        let error = map.error.clone();
        let state = self.settings_state.as_mut().unwrap();
        state.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.rows != rows || delegate.width != width {
                delegate.rows = rows;
                delegate.width = width;
                table.refresh(cx);
            }
        });
        let selected = state.selected.is_some();
        div().flex_1().min_h_0().flex().flex_col().gap_3()
            .child(settings_input(&state.key_filter,window,cx))
            .child(div().flex().flex_wrap().gap_2()
                .child(Button::new("add-keybind").label("Create keybinding").on_click(cx.listener(|this,_,window,cx| this.edit_keybinding(true,window,cx))))
                .child(Button::new("edit-keybind").ghost().label("Edit").disabled(!selected).on_click(cx.listener(|this,_,window,cx| this.edit_keybinding(false,window,cx))))
                .child(Button::new("remove-keybind").ghost().label("Remove").disabled(!selected).on_click(cx.listener(|this,_,_,cx| this.change_binding(false,false,cx))))
                .child(Button::new("reset-keybind").ghost().label("Reset action").disabled(!selected).on_click(cx.listener(|this,_,_,cx| this.change_binding(true,false,cx))))
                .child(Button::new("reset-all-keybinds").ghost().label("Reset all").on_click(cx.listener(|_,_,window,cx| {
                    let owner=cx.weak_entity();
                    window.open_dialog(cx,move |dialog,_,_| { let owner=owner.clone(); dialog.title("Reset all keybindings?").child("This removes all your shortcut changes.").on_ok(move |_,_,cx| {
                        let _=owner.update(cx,|this,cx| this.change_binding(true,true,cx)); true
                    }) });
                }))))
            .when_some(error,|s,e| s.child(div().text_sm().text_color(cx.theme().danger).child(format!("keybinds.json: {e}"))))
            .child(div().flex_1().min_h_0().child(DataTable::new(&state.table).stripe(false).bordered(false)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Double-click to edit. Sequences wait up to one second between keys. Only user changes are saved."))
            .into_any_element()
    }

    fn change_binding(&mut self, reset: bool, all: bool, cx: &mut Context<Self>) {
        let selected = self.settings_state.as_ref().unwrap().selected.clone();
        let map = cx.global_mut::<Keymap>();
        let result = if all {
            map.reset(None)
        } else if let Some(row) = selected {
            if reset {
                map.reset(Some(&row.binding))
            } else if row.binding.keys.is_empty() {
                Ok(())
            } else {
                map.remove(&row.binding)
            }
        } else {
            return;
        };
        let state = self.settings_state.as_mut().unwrap();
        state.error = result.err();
        state.selected = None;
        cx.notify();
    }

    fn edit_keybinding(&mut self, addition: bool, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.settings_state.as_ref().unwrap().selected.clone();
        let seed = row.as_ref().map(|r| r.binding.clone());
        let old = if addition {
            None
        } else {
            row.filter(|r| !r.binding.keys.is_empty())
                .map(|r| r.binding)
        };
        let editor = cx.new(|cx| BindingEditor::new(old, seed, window, cx));
        let subscription =
            cx.subscribe_in(&editor, window, |this, _, binding: &Binding, window, cx| {
                let editor = this
                    .settings_state
                    .as_ref()
                    .unwrap()
                    .editor
                    .as_ref()
                    .unwrap()
                    .clone();
                let old = editor.read(cx).old.clone();
                let result = cx
                    .global_mut::<Keymap>()
                    .save(old.as_ref(), binding.clone());
                match result {
                    Ok(()) => {
                        this.settings_state.as_mut().unwrap().selected = None;
                        window.close_dialog(cx);
                        this.focus.focus(window, cx);
                    }
                    Err(error) => editor.update(cx, |e, cx| {
                        e.error = Some(error);
                        cx.notify();
                    }),
                }
                cx.notify();
            });
        let state = self.settings_state.as_mut().unwrap();
        state.editor_subscription = Some(subscription);
        state.editor = Some(editor.clone());
        let focus = editor.read(cx).focus.clone();
        focus.focus(window, cx);
        window.open_dialog(cx, move |dialog, _, _| {
            let closing = editor.clone();
            dialog
                .title("Keybinding")
                .keyboard(false)
                .w(px(600.))
                .child(editor.clone())
                .on_close(move |_, _, cx| closing.update(cx, |e, _| e.recording = false))
        });
    }

    pub fn record_media_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let editor = self.settings_state.as_ref().and_then(|s| s.editor.clone());
        if let Some(editor) = editor {
            return editor.update(cx, |e, cx| {
                if e.recording {
                    if e.recorded.len() < 4 {
                        e.recorded.push(key.into());
                    }
                    cx.notify();
                    true
                } else {
                    false
                }
            });
        }
        false
    }

    pub fn record_keyboard_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let editor = self.settings_state.as_ref().and_then(|s| s.editor.clone());
        if let Some(editor) = editor {
            return editor.update(cx, |e, cx| {
                if !e.recording {
                    return false;
                }
                if !event.is_held && e.recorded.len() < 4 {
                    e.recorded.push(keybinds::stroke(&event.keystroke));
                }
                cx.notify();
                true
            });
        }
        false
    }

    pub fn extend_scope(&self, scope: &mut keybinds::Scope) {
        if self.folder.is_some() {
            scope.values.push("folder");
        }
        if !self.selected.is_empty() {
            scope.values.push("selected");
        }
        if self.settings
            && let Some(state) = &self.settings_state
            && state.page == Page::Keys
        {
            scope.values.push("KeybindEditor");
        }
    }

    pub fn run_library_action(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "select_all" => {
                self.selected = self
                    .rows
                    .iter()
                    .filter_map(|r| match r {
                        Row::Clip(c) => Some(c.path.clone()),
                        _ => None,
                    })
                    .collect()
            }
            "deselect" => self.deselect(cx),
            "delete" => self.confirm_delete(window, cx),
            "add_folder" => self.add_folder(window, cx),
            "detect" => self.worker.send(Request::Detect),
            "steam_search" => self.approve_steam_scan(None, window, cx),
            "steam_folder" => self.select_steam_folder(window, cx),
            "new_theme" if cx.has_global::<ThemeLibrary>() => {
                let result = cx.global_mut::<ThemeLibrary>().create();
                if self.settings_state.is_some() {
                    self.theme_result(result, window, cx);
                }
            }
            "restore_theme" if cx.has_global::<ThemeLibrary>() => {
                let result = cx.global_mut::<ThemeLibrary>().restore_original();
                if self.settings_state.is_some() {
                    self.theme_result(result, window, cx);
                }
            }
            "rename" if self.selected.len() == 1 => {
                self.prompt_rename(self.selected.iter().next().unwrap().clone(), window, cx);
            }
            "play_selected" => {
                if let Some(path) = self.selected.iter().next() {
                    self.play(path.clone(), cx);
                }
            }
            "mark_watched" => self.mark_selection(true, cx),
            "mark_unwatched" => self.mark_selection(false, cx),
            "refresh_library" => {
                self.requested.clear();
                self.worker.send(Request::Refresh);
            }
            "sort_name" | "sort_newest" | "sort_count" => self.worker.send(Request::Sort(
                match action {
                    "sort_name" => "name",
                    "sort_count" => "clip_count",
                    _ => "newest_clip",
                }
                .into(),
            )),
            "folder_up" => {
                let parent = self
                    .folder
                    .as_ref()
                    .and_then(|p| p.parent())
                    .map(Path::to_owned)
                    .filter(|p| {
                        self.config
                            .clip_directories
                            .iter()
                            .any(|r| p.starts_with(r))
                    });
                self.navigate(parent, cx);
            }
            "custom_icon" => {
                if let Some(folder) = self.folder.clone() {
                    self.custom_icon(folder, window, cx);
                }
            }
            "choose_icon" => {
                if let Some(folder) = self.folder.clone() {
                    self.choose_game_icon(folder, window, cx);
                }
            }
            "restore_icon" => {
                if let Some(folder) = self.folder.clone() {
                    self.icon_worker.send(IconRequest::Restore(folder));
                }
            }
            "toggle_discovery" => {
                let next = self.config.clip_discovery != Discovery::Enabled;
                self.steam_prompt_pending = next;
                self.worker.send(Request::Consent(next));
            }
            "toggle_steam" => {
                if self.config.steam_discovery == Discovery::Enabled {
                    self.worker.send(Request::SteamConsent(false));
                } else {
                    self.approve_steam_scan(None, window, cx);
                }
            }
            "settings_library" | "settings_theme" | "settings_keys" | "settings_about" => {
                self.ensure_settings(window, cx);
                self.settings_state.as_mut().unwrap().page = match action {
                    "settings_library" => Page::Library,
                    "settings_theme" => Page::Theme,
                    "settings_keys" => Page::Keys,
                    _ => Page::About,
                };
            }
            "theme_folder" if cx.has_global::<ThemeLibrary>() => {
                cx.reveal_path(&cx.global::<ThemeLibrary>().directory.clone());
            }
            "rename_theme" => {
                let active = cx.global::<ThemeLibrary>().active.clone();
                self.prompt_theme_rename(&active, window, cx);
            }
            "create_keybinding" => self.edit_keybinding(true, window, cx),
            "edit_keybinding" => self.edit_keybinding(false, window, cx),
            "remove_keybinding" => self.change_binding(false, false, cx),
            "reset_keybinding" => self.change_binding(true, false, cx),
            _ => {}
        }
        cx.notify();
    }
}

struct ThemeRename {
    entry: ThemeEntry,
    input: Entity<InputState>,
    error: Option<String>,
    owner: WeakEntity<ClipBrowser>,
    _subscription: Subscription,
}
impl ThemeRename {
    fn new(
        entry: ThemeEntry,
        owner: WeakEntity<ClipBrowser>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(entry.name.clone()));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.save(window, cx),
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            });
        Self {
            entry,
            input,
            error: None,
            owner,
            _subscription: subscription,
        }
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.input.read(cx).value().to_string();
        let library = cx.global_mut::<ThemeLibrary>();
        let result = if library.active == self.entry.file {
            library.rename(&name)
        } else {
            library.rename_file(&self.entry.file, &name)
        };
        match result {
            Ok(()) => {
                theme::apply(cx);
                window.close_dialog(cx);
                let _ = self.owner.update(cx, |this, cx| {
                    this.settings_state.as_mut().unwrap().error = None;
                    cx.notify();
                });
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}
impl Render for ThemeRename {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child(format!("Rename {}", self.entry.name)))
            .child(settings_input(&self.input, window, cx))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Updates the theme name and filename. Other themes are never overwritten.",
                    ),
            )
            .when_some(self.error.clone(), |s, error| {
                s.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-theme-rename")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-theme-rename")
                            .label("Rename")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

struct BindingEditor {
    old: Option<Binding>,
    action: Entity<SelectState<SearchableVec<ActionChoice>>>,
    preset: Entity<SelectState<Vec<SharedString>>>,
    context: Entity<InputState>,
    keys: Entity<InputState>,
    focus: FocusHandle,
    recording: bool,
    recorded: Vec<String>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<Binding> for BindingEditor {}
impl BindingEditor {
    fn new(
        old: Option<Binding>,
        seed: Option<Binding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let actions = keybinds::actions();
        let selected = seed
            .as_ref()
            .and_then(|b| actions.iter().position(|a| a.id == b.action))
            .unwrap_or(0);
        let choices: Vec<_> = actions
            .into_iter()
            .map(|a| ActionChoice {
                name: a.label.into(),
                id: a.id,
                reference: a.reference,
            })
            .collect();
        let action = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(choices),
                Some(IndexPath::default().row(selected)),
                window,
                cx,
            )
            .searchable(true)
        });
        let preset = cx.new(|cx| {
            SelectState::new(
                keybinds::PRESETS
                    .iter()
                    .map(|s| SharedString::from(*s))
                    .collect::<Vec<_>>(),
                None,
                window,
                cx,
            )
        });
        let context = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                seed.as_ref()
                    .map(|b| b.context.clone())
                    .unwrap_or_else(|| "Application".into()),
            )
        });
        let keys = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("ctrl-k ctrl-s")
                .default_value(old.as_ref().map(|b| b.keys.clone()).unwrap_or_default())
        });
        let subscriptions = vec![
            cx.subscribe(&keys, |this, _, _: &InputEvent, cx| {
                this.error = None;
                cx.notify();
            }),
            cx.subscribe(&context, |this, _, _: &InputEvent, cx| {
                this.error = None;
                cx.notify();
            }),
            cx.subscribe_in(&preset, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.context
                        .update(cx, |s, cx| s.set_value(value.clone(), window, cx));
                }
            }),
            cx.subscribe_in(&action, window, |this, _, event, window, cx| {
                if this.old.is_none()
                    && let SelectEvent::Confirm(Some(id)) = event
                    && let Some(a) = keybinds::actions().into_iter().find(|a| a.id == *id)
                {
                    this.context
                        .update(cx, |s, cx| s.set_value(a.context, window, cx));
                }
                cx.notify();
            }),
        ];
        Self {
            old,
            action,
            preset,
            context,
            keys,
            focus: cx.focus_handle(),
            recording: false,
            recorded: vec![],
            error: None,
            _subscriptions: subscriptions,
        }
    }
    fn binding(&self, cx: &App) -> Result<Binding, String> {
        let action = self
            .action
            .read(cx)
            .selected_value()
            .cloned()
            .ok_or("Choose an action")?;
        let keys = keybinds::normalize_keys(&self.keys.read(cx).value())?;
        let context = self.context.read(cx).value().to_string();
        keybinds::validate_context(&context)?;
        Ok(Binding {
            action,
            keys,
            context,
            disabled: false,
        })
    }
}
impl Render for BindingEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let context_text = self.context.read(cx).value().to_string();
        let token_start = context_text
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_alphanumeric())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        let prefix = &context_text[token_start..];
        let suggestions: Vec<_> = keybinds::CONTEXTS
            .iter()
            .filter(|name| {
                !prefix.is_empty()
                    && name.to_lowercase().starts_with(&prefix.to_lowercase())
                    && **name != prefix
            })
            .take(6)
            .copied()
            .collect();
        let validation = self.binding(cx);
        let conflicts = validation
            .as_ref()
            .map(|b| cx.global::<Keymap>().conflicts(b, self.old.as_ref()))
            .unwrap_or_default();
        let error = self
            .error
            .clone()
            .or_else(|| validation.as_ref().err().cloned());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.recording {
                    if !event.is_held && this.recorded.len() < 4 {
                        this.recorded.push(keybinds::stroke(&event.keystroke));
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child("Action")
            .child(Select::new(&self.action).focus_ring(false))
            .child("Context")
            .child(
                Select::new(&self.preset)
                    .focus_ring(false)
                    .placeholder("Choose a predefined context"),
            )
            .child(settings_input(&self.context, window, cx))
            .when(!suggestions.is_empty(), |s| {
                s.child(div().flex().flex_wrap().gap_1().children(
                    suggestions.into_iter().enumerate().map(|(index, name)| {
                        let completed = format!("{}{name}", &context_text[..token_start]);
                        Button::new(("context-completion", index))
                            .ghost()
                            .label(name)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.context.update(cx, |input, cx| {
                                    input.set_value(completed.clone(), window, cx);
                                    input.focus_handle(cx).focus(window, cx);
                                });
                            }))
                    }),
                ))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Combine with &&, ||, ! and parentheses. Available: {}",
                        keybinds::CONTEXTS.join(", ")
                    )),
            )
            .child("Keystrokes")
            .child(settings_input(&self.keys, window, cx).disabled(self.recording))
            .when(self.recording, |s| {
                s.child(div().text_sm().child(if self.recorded.is_empty() {
                    "Press your shortcut or sequence…".into()
                } else {
                    self.recorded.join(" ")
                }))
            })
            .child(
                Button::new("record-shortcut")
                    .label(if self.recording {
                        "Finish recording"
                    } else {
                        "Record shortcut"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.recording = !this.recording;
                        if this.recording {
                            this.recorded.clear();
                            this.focus.focus(window, cx);
                        } else {
                            let keys = this.recorded.join(" ");
                            this.keys.update(cx, |s, cx| s.set_value(keys, window, cx));
                        }
                        cx.notify();
                    })),
            )
            .when_some(error, |s, e| {
                s.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .when(!conflicts.is_empty(), |s| {
                s.child(div().text_sm().text_color(cx.theme().danger).child(format!(
                    "Overlapping shortcuts or sequence prefixes:\n{}",
                    conflicts.join("\n")
                )))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-keybind")
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.recording = false;
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("save-keybind")
                            .label("Save")
                            .disabled(validation.is_err() || self.recording)
                            .on_click(cx.listener(|this, _, _, cx| match this.binding(cx) {
                                Ok(binding) => cx.emit(binding),
                                Err(error) => {
                                    this.error = Some(error);
                                    cx.notify();
                                }
                            })),
                    ),
            )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use gpui_kit::InputEvent as _;

    #[::core::prelude::v1::test]
    fn keybinding_stripes_restart_after_each_context_header() {
        let actions = keybinds::actions();
        let defaults = Keymap::defaults();
        let rows = ["open", "settings", "select_all"]
            .into_iter()
            .map(|id| {
                key_row(
                    defaults
                        .iter()
                        .find(|binding| binding.action == id)
                        .unwrap()
                        .clone(),
                    "Default",
                    &actions,
                )
            })
            .collect();
        let table = KeyTable {
            rows: group_key_rows(rows),
            width: 1100.,
        };
        assert_eq!(
            (0..table.rows.len())
                .map(|index| table.row_band(index))
                .collect::<Vec<_>>(),
            [
                KeyRowBand::Context,
                KeyRowBand::First,
                KeyRowBand::Second,
                KeyRowBand::Context,
                KeyRowBand::First
            ]
        );
    }

    #[::core::prelude::v1::test]
    fn keybindings_are_grouped_by_context_and_keep_action_references() {
        let actions = keybinds::actions();
        let defaults = Keymap::defaults();
        let rows = ["pause", "open", "select_all"]
            .into_iter()
            .map(|id| {
                key_row(
                    defaults.iter().find(|b| b.action == id).unwrap().clone(),
                    "Default",
                    &actions,
                )
            })
            .collect();
        let grouped = group_key_rows(rows);
        let contexts: Vec<_> = grouped
            .iter()
            .filter_map(|r| match r {
                KeyTableRow::Context(c) => Some(c.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contexts, ["Application", "ClipLibrary", "Player"]);
        assert!(
            grouped
                .iter()
                .filter(|r| matches!(r, KeyTableRow::Context(_)))
                .all(|r| r.binding().is_none())
        );
        assert_eq!(
            grouped.last().unwrap().binding().unwrap().reference,
            "player.pause"
        );
    }

    #[::core::prelude::v1::test]
    fn action_search_matches_names_and_references() {
        let action = ActionChoice {
            name: "Pause playback".into(),
            id: "pause".into(),
            reference: "player.pause".into(),
        };
        assert!(action.matches("PAUSE"));
        assert!(action.matches("player.pause"));
        assert!(!action.matches("unrelated"));
    }

    #[::core::prelude::v1::test]
    #[ignore = "interactive settings page rendering, input safety and shortcut recording"]
    fn settings_pages_and_recording() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().to_owned();
        let result = Arc::new(std::sync::Mutex::new(None));
        let report = result.clone();
        application()
            .with_assets(crate::app_assets::AppAssets)
            .run(move |cx| {
                init(cx);
                cx.set_global(ThemeLibrary::open(store.join("themes")).unwrap());
                theme::apply(cx);
                assert!(cx.theme().tokens.sidebar==cx.theme().colors.sidebar.into());
                assert!(cx.theme().tokens.list_hover==cx.theme().colors.list_hover.into());
                cx.set_global(Keymap::open(store.join("keybinds.json")));
                let mut shell = None;
                let handle = cx
                    .open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(100.), px(100.)),
                                size(px(960.), px(700.)),
                            ))),
                            ..Default::default()
                        },
                        |window, cx| {
                            let browser = cx.new(|cx| ClipBrowser::for_test(store, cx));
                            let player = cx.new(|cx| {
                                let mut player =
                                    crate::PlayerShell::with_browser(browser, window, cx);
                                player.set_settings(true, window, cx);
                                player
                            });
                            shell = Some(player.clone());
                            crate::player_root(player, window, cx)
                        },
                    )
                    .unwrap();
                let shell = shell.unwrap();
                cx.activate(true);
                cx.spawn(async move |cx| {
                    let outcome: Result<(), String> = async {
                        for page in [Page::Player, Page::Library, Page::Theme, Page::About, Page::Keys] {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(250))
                                .await;
                            cx.update_window(handle.into(), |_, window, cx| {
                                let browser = shell.read(cx).browser.clone();
                                browser.update(cx, |this, cx| {
                                    this.ensure_settings(window, cx);
                                    this.settings_state.as_mut().unwrap().page = page;
                                    cx.notify();
                                });
                            })
                            .map_err(|e| e.to_string())?;
                        }
                        for width in [680.,960.,1600.] {
                        cx.update_window(handle.into(),|_,window,cx| {
                            window.resize(size(px(width),px(700.)));
                            let browser=shell.read(cx).browser.clone();
                            browser.update(cx,|browser,cx| { browser.settings_state.as_mut().unwrap().page=Page::Theme;cx.notify(); });
                        }).map_err(|e| e.to_string())?;
                        cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                        cx.update_window(handle.into(),|_,window,cx| {
                            let browser=shell.read(cx).browser.read(cx);
                            let layout=&browser.settings_state.as_ref().unwrap().theme_layout;
                            let row=layout.active_row.get();let selector=layout.selector.get();let table=layout.table.get();
                            if row.size.width<px(400.) || row.size.height>px(140.) || selector.size.width!=px(230.) || table.size.height<px(100.) || table.bottom()>window.viewport_size().height {
                                return Err(format!("Collapsed theme layout: row={row:?}, selector={selector:?}, table={table:?}"));
                            }
                            Ok(())
                        }).map_err(|e| e.to_string())??;
                        }
                        cx.update_window(handle.into(),|_,window,cx| {
                            window.resize(size(px(960.),px(700.)));
                            let browser=shell.read(cx).browser.clone();
                            browser.update(cx,|browser,cx| { browser.settings_state.as_mut().unwrap().page=Page::Keys;cx.notify(); });
                        }).map_err(|e| e.to_string())?;
                        cx.update_window(handle.into(),|_,_,cx| {
                            cx.global_mut::<ThemeLibrary>().create().unwrap();theme::apply(cx);
                            let library=cx.global::<ThemeLibrary>();
                            theme::atomic_json(&library.directory.join(&library.active),&serde_json::json!({"name":"Edited live","colors":{"primary":"#33aaff"}}),false).unwrap();
                        }).map_err(|e| e.to_string())?;
                        cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
                        cx.update_window(handle.into(),|_,_,cx| {
                            if cx.theme().primary!=Hsla::from(rgb(0x33aaff)) { return Err("Active theme edits were not applied live".to_owned()); }
                            let library=cx.global::<ThemeLibrary>();
                            std::fs::remove_file(library.directory.join(&library.active)).unwrap();
                            Ok(())
                        }).map_err(|e| e.to_string())??;
                        cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
                        cx.update_window(handle.into(),|_,_,cx| {
                            if cx.global::<ThemeLibrary>().active!="spacer-original.json" || cx.theme().primary!=Hsla::from(rgb(0xa50036)) { return Err("Deleting the active theme did not restore Spacer original".to_owned()); }
                            Ok(())
                        }).map_err(|e| e.to_string())??;
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(250))
                            .await;
                        cx.update_window(handle.into(), |_, window, cx| {
                            cx.global_mut::<Keymap>()
                                .save(
                                    None,
                                    Binding {
                                        action: "settings_about".into(),
                                        keys: "ctrl-alt-b".into(),
                                        context: "Dialog".into(),
                                        disabled: false,
                                    },
                                )
                                .unwrap();
                            let browser = shell.read(cx).browser.clone();
                            browser.update(cx, |this, cx| this.edit_keybinding(true, window, cx));
                        })
                        .map_err(|e| e.to_string())?;
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(250))
                            .await;
                        cx.update_window(handle.into(), |_, window, cx| {
                            window.dispatch_event(
                                KeyDownEvent {
                                    keystroke: Keystroke::parse("ctrl-alt-b").unwrap(),
                                    is_held: false,
                                    prefer_character_input: false,
                                }
                                .to_platform_input(),
                                cx,
                            );
                            let browser = shell.read(cx).browser.clone();
                            if browser.read(cx).settings_state.as_ref().unwrap().page != Page::About
                            {
                                return Err(
                                    "Custom Dialog context shortcut was not dispatched".to_owned()
                                );
                            }
                            let editor = browser
                                .read(cx)
                                .settings_state
                                .as_ref()
                                .unwrap()
                                .editor
                                .clone()
                                .unwrap();
                            editor.update(cx, |e, cx| {
                                e.recording = true;
                                e.focus.focus(window, cx);
                                cx.notify();
                            });
                            Ok(())
                        })
                        .map_err(|e| e.to_string())??;
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(150))
                            .await;
                        cx.update_window(handle.into(), |_, window, cx| {
                            let browser = shell.read(cx).browser.clone();
                            let editor = browser
                                .read(cx)
                                .settings_state
                                .as_ref()
                                .unwrap()
                                .editor
                                .clone()
                                .unwrap();
                            for key in ["ctrl-k", "ctrl-s", "escape"] {
                                window.dispatch_event(
                                    KeyDownEvent {
                                        keystroke: Keystroke::parse(key).unwrap(),
                                        is_held: false,
                                        prefer_character_input: false,
                                    }
                                    .to_platform_input(),
                                    cx,
                                );
                            }
                            if editor.read(cx).recorded != ["ctrl-k", "ctrl-s", "escape"] {
                                return Err(format!(
                                    "Recording missed keys: {:?}",
                                    editor.read(cx).recorded
                                ));
                            }
                            if !window.has_active_dialog(cx) {
                                return Err("Recording closed the dialog".into());
                            }
                            editor.update(cx, |e, cx| {
                                e.recording = false;
                                cx.emit(Binding {
                                    action: "settings_library".into(),
                                    keys: "k".into(),
                                    context: "Settings".into(),
                                    disabled: false,
                                });
                            });
                            Ok(())
                        })
                        .map_err(|e| e.to_string())??;
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(150))
                            .await;
                        cx.update_window(handle.into(), |_, window, cx| {
                            if window.has_active_dialog(cx) {
                                return Err("Saving did not close the keybinding editor".to_owned());
                            }
                            if !cx
                                .global::<Keymap>()
                                .changes
                                .iter()
                                .any(|b| b.action == "settings_library")
                            {
                                return Err("The keybinding editor did not save the binding".into());
                            }
                            window.dispatch_event(
                                KeyDownEvent {
                                    keystroke: Keystroke::parse("k").unwrap(),
                                    is_held: false,
                                    prefer_character_input: false,
                                }
                                .to_platform_input(),
                                cx,
                            );
                            let browser = shell.read(cx).browser.clone();
                            if browser.read(cx).settings_state.as_ref().unwrap().page
                                != Page::Library
                            {
                                return Err("Saved binding was not applied immediately".into());
                            }
                            browser.update(cx, |this, cx| {
                                this.settings_state.as_mut().unwrap().page = Page::Keys;
                                this.settings_state
                                    .as_ref()
                                    .unwrap()
                                    .search
                                    .focus_handle(cx)
                                    .focus(window, cx);
                                cx.notify();
                            });
                            Ok(())
                        })
                        .map_err(|e| e.to_string())??;
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(150))
                            .await;
                        cx.update_window(handle.into(), |_, window, cx| {
                            window.dispatch_event(
                                KeyDownEvent {
                                    keystroke: Keystroke::parse("k").unwrap(),
                                    is_held: false,
                                    prefer_character_input: false,
                                }
                                .to_platform_input(),
                                cx,
                            );
                            let browser = shell.read(cx).browser.clone();
                            if browser.read(cx).settings_state.as_ref().unwrap().page != Page::Keys
                            {
                                return Err("Typing triggered a settings shortcut".to_owned());
                            }
                            Ok(())
                        })
                        .map_err(|e| e.to_string())??;
                        Ok(())
                    }
                    .await;
                    *report.lock().unwrap() = Some(outcome);
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
        result
            .lock()
            .unwrap()
            .take()
            .expect("Settings test completed")
            .unwrap();
    }
}
