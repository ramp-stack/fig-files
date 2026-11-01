//! File-manager pages expressed entirely through CHK's public page/form API.
use crate::collaboration::SharedSpace;
use crate::runtime::FileEvent;
use crate::storage::{self, Entry, EntryId, EntryKind, Vault};
use chk::{
    Action, AvatarContent, AvatarIconStyle, Bumper, Context, Display, Flow, FlowBuilder, Form,
    FormComplete, FormItem, FormValidState, Icons, ListItem, Offset, Page, PageType, State,
    TableItem, Theme, Watch,
};
use prism::air::Instance;

#[derive(Clone, Debug)]
pub enum Location {
    Folder(Option<EntryId>),
    Trash,
    Search(String),
}

pub fn show(ctx: &mut Context, theme: &Theme, page: Page) {
    Flow::new(vec![page]).build(ctx, theme)(ctx, theme);
}

pub fn message(title: &str, body: &str) -> Page {
    Page::Static(Box::new(PageType::display(
        title,
        vec![Display::instructions(body)],
        None,
        Bumper::Done,
        Offset::Center,
    )))
}

fn file_action(event: FileEvent) -> Action {
    Action::custom(move |ctx: &mut Context, _: &Theme| ctx.emit(event.clone()))
}

pub fn listing(vault: Instance<Vault>, location: Location, root: bool) -> Page {
    Page::watching(
        (Watch::contract(vault), Watch::list::<SharedSpace>()),
        move |_ctx: &mut Context,
              theme: &Theme,
              (vault, spaces): (Instance<Vault>, Vec<Instance<SharedSpace>>)| {
            let snapshot = vault.pending();
            let parent = match location {
                Location::Folder(parent) => parent,
                _ => None,
            };
            if let Some(id) = parent {
                if let Some(Entry {
                    kind: EntryKind::Shared { space, .. },
                    ..
                }) = snapshot.get(id)
                    && !snapshot.is_trashed(id)
                {
                    if let Some(instance) = spaces.iter().find(|instance| instance.id() == *space) {
                        let root = instance.pending().root;
                        drop(snapshot);
                        return crate::collaboration_ui::render_item(
                            _ctx,
                            theme,
                            instance.clone(),
                            root,
                        );
                    }
                    return PageType::display(
                        "Shared folder syncing",
                        vec![Display::instructions(
                            "This folder now uses shared access. Keep Fig Files open while its contents sync.",
                        )],
                        None,
                        Bumper::None,
                        Offset::Start,
                    );
                }
                if snapshot.get(id).is_none() || snapshot.is_trashed(id) {
                    return PageType::display(
                        "Folder unavailable",
                        vec![Display::instructions(
                            "This folder was moved to Trash or removed. Return to Files to continue.",
                        )],
                        None,
                        Bumper::Done,
                        Offset::Center,
                    );
                }
            }
            let title = match &location {
                Location::Folder(None) => "Fig Files".into(),
                Location::Folder(Some(id)) => snapshot
                    .get(*id)
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| "Folder".into()),
                Location::Trash => "Trash".into(),
                Location::Search(query) => format!("Results for “{query}”"),
            };
            let entries = library_entries(&snapshot, &spaces, &location);
            let (count, total) = library_totals(&snapshot, &spaces);
            let trail = if let Location::Folder(Some(_)) = location {
                let names: Vec<_> = snapshot
                    .breadcrumbs(parent)
                    .into_iter()
                    .map(|e| e.name)
                    .collect();
                Some(format!("Files / {}", names.join(" / ")))
            } else if matches!(location, Location::Trash) {
                Some("Restore items here, or remove them from your library.".into())
            } else {
                None
            };
            drop(snapshot);
            let mut displays = trail
                .iter()
                .map(|text| Display::text(text))
                .collect::<Vec<_>>();
            let header = if !matches!(location, Location::Trash) {
                let search_vault = vault.clone();
                Some((
                    Icons::Search,
                    Box::new(move |_: &mut Context, theme: &Theme| {
                        search_flow(search_vault.clone(), theme)
                    }) as Box<dyn FlowBuilder>,
                ))
            } else {
                None
            };
            if entries.is_empty() {
                let (art, label, hint) = match location {
                    Location::Trash => (
                        "brand/empty-trash",
                        "All clear",
                        "Files you move to Trash will appear here.",
                    ),
                    Location::Search(_) => (
                        "brand/empty-files",
                        "No matching files",
                        "Try another file or folder name.",
                    ),
                    _ => (
                        "brand/empty-files",
                        "Room for your next idea",
                        "Import your first file or create a folder. Files up to 8 MiB are supported.",
                    ),
                };
                displays.extend([
                    Display::image(art, (160.0, 160.0)),
                    Display::label(label),
                    Display::instructions(hint),
                ]);
            } else {
                let rows = entries
                    .into_iter()
                    .map(|(entry, shared)| {
                        let page = if let Some(space) = shared {
                            if let Some(instance) =
                                spaces.iter().find(|instance| instance.id() == space)
                            {
                                crate::collaboration_ui::item(instance.clone(), entry.id)
                            } else {
                                crate::collaboration_ui::mount(space)
                            }
                        } else if let EntryKind::Shared { space, .. } = entry.kind {
                            if matches!(location, Location::Trash) {
                                detail(vault.clone(), entry.id)
                            } else {
                                crate::collaboration_ui::mount(space)
                            }
                        } else if entry.is_folder() && !matches!(location, Location::Trash) {
                            listing(vault.clone(), Location::Folder(Some(entry.id)), false)
                        } else {
                            detail(vault.clone(), entry.id)
                        };
                        let subtitle =
                            if shared.is_some() || matches!(entry.kind, EntryKind::Shared { .. }) {
                                if entry.is_folder() {
                                    "Shared folder · Can edit".into()
                                } else {
                                    "Shared file · Can edit".into()
                                }
                            } else if entry.is_folder() {
                                "Folder".into()
                            } else {
                                format!("{} · {}", file_kind(&entry), format_size(entry.size()))
                            };
                        ListItem::avatar(
                            theme
                                .brand()
                                .images
                                .get(entry_art(&entry))
                                .cloned()
                                .map(AvatarContent::image)
                                .unwrap_or_else(|| {
                                    AvatarContent::icon(entry_icon(&entry), AvatarIconStyle::Brand)
                                }),
                            &entry.name,
                            &subtitle,
                            None,
                            Some(Flow::new(vec![page])),
                        )
                    })
                    .collect();
                displays.push(Display::list(None, rows, None));
            }
            if let Some(id) = parent {
                displays.push(Display::list(
                    None,
                    vec![ListItem::plain(
                        "Folder details",
                        "Share and manage this folder",
                        None,
                        Some(Flow::new(vec![detail(vault.clone(), id)])),
                    )],
                    None,
                ));
            }
            displays.push(Display::instructions(&format!(
                "{count} {} · {} in your library",
                if count == 1 { "item" } else { "items" },
                format_size(total),
            )));
            let can_add = matches!(location, Location::Folder(_));
            if root {
                let add = can_add.then(|| {
                    let vault = vault.clone();
                    (
                        "Add files".into(),
                        Box::new(move |_: &mut Context, theme: &Theme| {
                            Flow::new(vec![add_page(vault.clone(), parent, theme)])
                        }) as Box<dyn FlowBuilder>,
                    )
                });
                PageType::root(&title, vec![], displays, header, add, None)
            } else {
                let bumper = if can_add {
                    Bumper::custom(
                        "Add files",
                        Action::custom(move |ctx: &mut Context, theme: &Theme| {
                            show(ctx, theme, add_page(vault.clone(), parent, theme));
                        }),
                    )
                } else {
                    Bumper::None
                };
                PageType::display(&title, displays, header, bumper, Offset::Start)
            }
        },
    )
}

pub fn detail(vault: Instance<Vault>, id: EntryId) -> Page {
    Page::watching(
        Watch::contract(vault),
        move |_ctx: &mut Context, _: &Theme, vault: Instance<Vault>| {
            let snapshot = vault.pending();
            let Some(entry) = snapshot.get(id).cloned() else {
                return PageType::display(
                    "Item removed",
                    vec![Display::instructions(
                        "This item is no longer in your library.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Center,
                );
            };
            let trashed = snapshot.is_trashed(id);
            if let EntryKind::Shared { space, .. } = entry.kind
                && !trashed
            {
                return PageType::display(
                    &entry.name,
                    vec![Display::instructions(
                        "This item is shared. Open it to see and edit the latest contents with everyone.",
                    )],
                    None,
                    Bumper::custom(
                        "Open shared item",
                        Action::flow(Flow::new(vec![crate::collaboration_ui::mount(space)])),
                    ),
                    Offset::Start,
                );
            }
            let path = snapshot
                .breadcrumbs(entry.parent)
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>();
            let size = if entry.is_folder() {
                format!(
                    "{} items",
                    snapshot
                        .entries
                        .values()
                        .filter(|child| child.parent == Some(id)
                            && (trashed || !snapshot.is_trashed(child.id)))
                        .count()
                )
            } else {
                format_size(entry.size())
            };
            drop(snapshot);
            let mut displays = vec![
                Display::image(entry_art(&entry), (112.0, 112.0)),
                Display::label(&entry.name),
                Display::table(
                    "Details",
                    vec![
                        TableItem::new("Kind", &file_kind(&entry)),
                        TableItem::new("Size", &size),
                        TableItem::new(
                            "Location",
                            &format!(
                                "Files{}",
                                if path.is_empty() {
                                    String::new()
                                } else {
                                    format!(" / {}", path.join(" / "))
                                }
                            ),
                        ),
                    ],
                    None,
                ),
            ];
            let (header, bumper) = if trashed {
                let delete_vault = vault.clone();
                let restore_vault = vault.clone();
                (
                    Some((
                        Icons::Delete,
                        Box::new(move |_: &mut Context, _: &Theme| {
                            Flow::new(vec![confirmation(delete_vault.clone(), id, true)])
                        }) as Box<dyn FlowBuilder>,
                    )),
                    Bumper::custom(
                        "Restore",
                        Action::custom(move |ctx: &mut Context, theme: &Theme| {
                            let result = storage::restore(&mut restore_vault.clone(), id);
                            report(
                                ctx,
                                theme,
                                result,
                                "Restored",
                                "The item is back in your library.",
                            );
                        }),
                    ),
                )
            } else if entry.is_folder() {
                let manage_vault = vault.clone();
                let share_vault = vault.clone();
                (
                    Some((
                        Icons::Settings,
                        Box::new(move |_: &mut Context, _: &Theme| {
                            Flow::new(vec![manage_page(manage_vault.clone(), id)])
                        }) as Box<dyn FlowBuilder>,
                    )),
                    Bumper::custom(
                        "Share folder",
                        Action::custom(move |ctx: &mut Context, theme: &Theme| {
                            crate::sharing_ui::send_flow(share_vault.clone(), id, theme)
                                .build(ctx, theme)(ctx, theme);
                        }),
                    ),
                )
            } else {
                displays.push(Display::list(
                    None,
                    vec![ListItem::plain(
                        "Manage file",
                        "Rename, move or delete",
                        None,
                        Some(Flow::new(vec![manage_page(vault.clone(), id)])),
                    )],
                    None,
                ));
                let share_vault = vault.clone();
                (
                    Some((
                        Icons::Send,
                        Box::new(move |_: &mut Context, theme: &Theme| {
                            Flow::new(vec![share_page(share_vault.clone(), id, theme)])
                        }) as Box<dyn FlowBuilder>,
                    )),
                    Bumper::custom("Preview", file_action(FileEvent::Preview(id))),
                )
            };
            PageType::display(&entry.name, displays, header, bumper, Offset::Start)
        },
    )
}

/// The Add button opens one small chooser; the native picker is only invoked
/// by its Import button, never while CHK constructs a page or a header.
pub(crate) fn add_page(vault: Instance<Vault>, parent: Option<EntryId>, theme: &Theme) -> Page {
    let destination = parent
        .and_then(|id| vault.pending().get(id).map(|e| e.name.clone()))
        .unwrap_or_else(|| "Files".into());
    Page::Static(Box::new(PageType::display(
        &format!("Add to {destination}"),
        vec![
            Display::instructions("Choose files from your device. Each file can be up to 8 MiB."),
            Display::list(
                None,
                vec![ListItem::plain(
                    "New folder",
                    "Create a folder here",
                    None,
                    Some(create_folder_flow(vault, parent, theme)),
                )],
                None,
            ),
        ],
        None,
        Bumper::custom("Import files", file_action(FileEvent::Import(parent))),
        Offset::Start,
    )))
}

pub(crate) fn share_page(vault: Instance<Vault>, id: EntryId, _theme: &Theme) -> Page {
    Page::watching(
        Watch::contract(vault),
        move |_: &mut Context, _: &Theme, vault: Instance<Vault>| {
            let snapshot = vault.pending();
            let Some(entry) = snapshot.get(id).filter(|_| !snapshot.is_trashed(id)) else {
                return PageType::display(
                    "Item unavailable",
                    vec![Display::instructions(
                        "This item is no longer available to share.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            };
            let send_vault = vault.clone();
            let send = Action::custom(move |ctx: &mut Context, theme: &Theme| {
                crate::sharing_ui::send_flow(send_vault.clone(), id, theme).build(ctx, theme)(
                    ctx, theme,
                );
            });
            let bumper = if entry.is_folder() {
                Bumper::custom("Share access", send)
            } else {
                // CHK's secondary bumper returns to the file after opening the
                // native share sheet. It does not push another in-app flow.
                Bumper::double(
                    "Share access",
                    send,
                    "Export file",
                    file_action(FileEvent::Export(id)),
                )
            };
            PageType::display(
                "Share",
                vec![
                    Display::image(entry_art(entry), (96.0, 96.0)),
                    Display::label(&entry.name),
                    Display::instructions(
                        "Share access using a Fig Files receiving name or QR code. Everyone who joins can read and edit the same file or folder.",
                    ),
                ],
                None,
                bumper,
                Offset::Start,
            )
        },
    )
}

pub(crate) fn manage_page(vault: Instance<Vault>, id: EntryId) -> Page {
    Page::watching(
        Watch::contract(vault),
        move |_: &mut Context, theme: &Theme, vault: Instance<Vault>| {
            let snapshot = vault.pending();
            let Some(entry) = snapshot.get(id).filter(|_| !snapshot.is_trashed(id)) else {
                return PageType::display(
                    "Item unavailable",
                    vec![Display::instructions(
                        "This item was removed or moved to Trash.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            };
            if let EntryKind::Shared { space, .. } = entry.kind {
                return PageType::display(
                    "Manage shared item",
                    vec![Display::instructions(
                        "Changes to shared items apply to everyone.",
                    )],
                    None,
                    Bumper::custom(
                        "Open shared item",
                        Action::flow(Flow::new(vec![crate::collaboration_ui::mount(space)])),
                    ),
                    Offset::Start,
                );
            }
            let name = entry.name.clone();
            drop(snapshot);
            let delete_vault = vault.clone();
            PageType::display(
                "Manage",
                vec![
                    Display::label(&name),
                    Display::list(
                        None,
                        vec![
                            ListItem::plain(
                                "Rename",
                                "Change the name",
                                None,
                                Some(rename_flow(vault.clone(), id, theme)),
                            ),
                            ListItem::plain(
                                "Move",
                                "Choose another folder",
                                None,
                                Some(move_flow(vault.clone(), id, theme)),
                            ),
                        ],
                        None,
                    ),
                ],
                Some((
                    Icons::Delete,
                    Box::new(move |_: &mut Context, _: &Theme| {
                        Flow::new(vec![confirmation(delete_vault.clone(), id, false)])
                    }) as Box<dyn FlowBuilder>,
                )),
                Bumper::None,
                Offset::Start,
            )
        },
    )
}

fn validation(name: String) -> FormValidState {
    match storage::validate_name(&name) {
        Ok(_) => FormValidState::Valid,
        Err(error) => FormValidState::InvalidWithData(error.to_string()),
    }
}

fn create_folder_flow(vault: Instance<Vault>, parent: Option<EntryId>, theme: &Theme) -> Flow {
    Flow::from_form(Form::flow(
        theme,
        vec![FormItem::text(
            "New folder",
            "folder name",
            None,
            |_: &mut Context, name: String| validation(name),
        )],
        None,
        None,
        Box::new(move |_: &mut Context, states: &Vec<State>| {
            let Some(State::Text(name)) = states.first() else {
                return FormComplete::None;
            };
            match storage::create_folder(&mut vault.clone(), parent, name) {
                Ok(_) => {
                    let vault = vault.clone();
                    FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
                        if parent.is_some() {
                            show(
                                ctx,
                                theme,
                                listing(vault.clone(), Location::Folder(parent), false),
                            );
                        }
                    }))
                }
                Err(error) => {
                    FormComplete::Next(message("Could not create folder", &error.to_string()))
                }
            }
        }),
    ))
}

fn rename_flow(vault: Instance<Vault>, id: EntryId, theme: &Theme) -> Flow {
    let name = vault
        .pending()
        .get(id)
        .map(|e| e.name.clone())
        .unwrap_or_default();
    Flow::from_form(Form::flow(
        theme,
        vec![FormItem::text_with_preset(
            "Rename",
            &name,
            None,
            |_: &mut Context, name: String| validation(name),
        )],
        None,
        None,
        Box::new(move |_: &mut Context, states: &Vec<State>| {
            let Some(State::Text(name)) = states.first() else {
                return FormComplete::None;
            };
            match storage::rename(&mut vault.clone(), id, name) {
                Ok(_) => FormComplete::Return(Box::new(|_: &mut Context, _: &Theme| {})),
                Err(error) => FormComplete::Next(message("Could not rename", &error.to_string())),
            }
        }),
    ))
}

pub(crate) fn move_flow(vault: Instance<Vault>, id: EntryId, theme: &Theme) -> Flow {
    let snapshot = vault.pending();
    let current_parent = snapshot.get(id).and_then(|e| e.parent);
    let folders: Vec<_> = snapshot
        .folders()
        .into_iter()
        .filter(|e| {
            e.is_folder()
                && !matches!(e.kind, EntryKind::Shared { .. })
                && e.id != id
                && !snapshot
                    .breadcrumbs(Some(e.id))
                    .iter()
                    .any(|ancestor| ancestor.id == id)
        })
        .collect();
    let mut destinations = vec![(None, "Files".to_string())];
    destinations.extend(folders.into_iter().map(|e| {
        let path = snapshot
            .breadcrumbs(Some(e.id))
            .into_iter()
            .map(|e| e.name)
            .collect::<Vec<_>>()
            .join(" / ");
        (Some(e.id), format!("Files / {path}"))
    }));
    destinations.retain(|(parent, _)| *parent != current_parent);
    drop(snapshot);
    if destinations.is_empty() {
        return Flow::new(vec![message(
            "Nowhere to move yet",
            "Create another folder first.",
        )]);
    }
    let choices: Vec<_> = destinations
        .iter()
        .map(|(_, label)| (label.as_str(), "Folder"))
        .collect();
    let input = FormItem::enumerator("Move to folder", choices);
    Flow::from_form(Form::flow(
        theme,
        vec![input],
        None,
        None,
        Box::new(move |_: &mut Context, states: &Vec<State>| {
            let Some(State::Enumerator(index)) = states.first() else {
                return FormComplete::None;
            };
            let Some((parent, _)) = destinations.get(*index) else {
                return FormComplete::None;
            };
            match storage::move_entry(&mut vault.clone(), id, *parent) {
                Ok(_) => FormComplete::Return(Box::new(|_: &mut Context, _: &Theme| {})),
                Err(error) => FormComplete::Next(message("Could not move", &error.to_string())),
            }
        }),
    ))
}

fn search_flow(vault: Instance<Vault>, theme: &Theme) -> Flow {
    Flow::from_form(Form::flow(
        theme,
        vec![FormItem::text(
            "Search files",
            "file or folder name",
            None,
            |_: &mut Context, query: String| {
                if query.trim().is_empty() {
                    FormValidState::Invalid
                } else {
                    FormValidState::Valid
                }
            },
        )],
        None,
        None,
        Box::new(move |_: &mut Context, states: &Vec<State>| {
            let Some(State::Text(query)) = states.first() else {
                return FormComplete::None;
            };
            FormComplete::Next(listing(
                vault.clone(),
                Location::Search(query.trim().to_string()),
                false,
            ))
        }),
    ))
}

fn confirmation(vault: Instance<Vault>, id: EntryId, permanent: bool) -> Page {
    let name = vault
        .pending()
        .get(id)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "this item".into());
    let label = if permanent {
        "Delete permanently"
    } else {
        "Move to Trash"
    };
    let explanation = if permanent {
        "Remove this item and everything inside it from your library? This cannot be undone in Fig Files. AIR's encrypted history may retain earlier data; this is not secure erasure."
    } else {
        "Move this item and everything inside it to Trash? You can restore it later."
    };
    let callback = Action::custom(move |ctx: &mut Context, theme: &Theme| {
        let result = if permanent {
            storage::delete_permanently(&mut vault.clone(), id)
        } else {
            storage::trash(&mut vault.clone(), id)
        };
        report(
            ctx,
            theme,
            result,
            if permanent {
                "Deleted"
            } else {
                "Moved to Trash"
            },
            if permanent {
                "The item has been removed from your library."
            } else {
                "You can restore it from the Trash tab."
            },
        );
    });
    Page::Static(Box::new(PageType::display(
        label,
        vec![Display::label(&name), Display::instructions(explanation)],
        None,
        Bumper::double(label, callback, "Cancel", Action::None),
        Offset::Center,
    )))
}

fn report(
    ctx: &mut Context,
    theme: &Theme,
    result: Result<EntryId, storage::StorageError>,
    title: &str,
    body: &str,
) {
    let page = match result {
        Ok(_) => message(title, body),
        Err(error) => message("Could not complete action", &error.to_string()),
    };
    FormComplete::Next(page).run(ctx, theme);
}

pub(crate) fn entry_art(entry: &Entry) -> &'static str {
    if entry.is_folder() {
        "file-icons/folder"
    } else if entry.mime().starts_with("image/") {
        "file-icons/image"
    } else if entry.mime().starts_with("video/") {
        "file-icons/video"
    } else if entry.mime().starts_with("audio/") {
        "file-icons/audio"
    } else if matches!(
        entry.extension().as_str(),
        "zip" | "tar" | "gz" | "7z" | "rar"
    ) {
        "file-icons/archive"
    } else {
        "file-icons/document"
    }
}

fn entry_icon(entry: &Entry) -> Icons {
    if entry.is_folder() {
        Icons::Home
    } else if entry.mime().starts_with("image/") {
        Icons::Photos
    } else if entry.mime().starts_with("video/") {
        Icons::Play
    } else if entry.mime().starts_with("audio/") {
        Icons::Microphone
    } else {
        Icons::Credential
    }
}

fn file_kind(entry: &Entry) -> String {
    if entry.is_folder() {
        "Folder".into()
    } else {
        entry.mime().to_string()
    }
}

pub fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0))
    }
}

/// Resolve library links on every shared update, including live shared
/// descendants in global search.
fn library_entries(
    vault: &Vault,
    spaces: &[Instance<SharedSpace>],
    location: &Location,
) -> Vec<(Entry, Option<EntryId>)> {
    let base = match location {
        Location::Folder(parent) => vault.children(*parent),
        Location::Trash => vault.trash(),
        Location::Search(_) => vault
            .entries
            .values()
            .filter(|entry| !vault.is_trashed(entry.id))
            .cloned()
            .collect(),
    };
    let mut rows = base
        .into_iter()
        .map(|entry| {
            if !matches!(location, Location::Trash)
                && let EntryKind::Shared { space, .. } = entry.kind
                && let Some(instance) = spaces.iter().find(|instance| instance.id() == space)
            {
                let state = instance.pending();
                if state.is_ready()
                    && state.can_write(vault.owner)
                    && let Some(root) = state.tree.get(state.root)
                {
                    return (root.clone(), Some(space));
                }
            }
            (entry, None)
        })
        .collect::<Vec<_>>();
    if matches!(location, Location::Search(_)) {
        for mount in vault
            .entries
            .values()
            .filter(|entry| !vault.is_trashed(entry.id))
        {
            let EntryKind::Shared { space, .. } = mount.kind else {
                continue;
            };
            let Some(instance) = spaces.iter().find(|instance| instance.id() == space) else {
                continue;
            };
            let state = instance.pending();
            if !state.is_ready() || !state.can_write(vault.owner) {
                continue;
            }
            rows.extend(
                state
                    .tree
                    .entries
                    .values()
                    .filter(|entry| entry.id != state.root && !state.tree.is_trashed(entry.id))
                    .cloned()
                    .map(|entry| (entry, Some(space))),
            );
        }
    }
    if let Location::Search(query) = location {
        let query = query.to_lowercase();
        rows.retain(|(entry, _)| entry.name.to_lowercase().contains(&query));
    }
    rows.sort_by(|(a, _), (b, _)| {
        b.is_folder()
            .cmp(&a.is_folder())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

fn library_totals(vault: &Vault, spaces: &[Instance<SharedSpace>]) -> (usize, u64) {
    let mut count = vault.item_count();
    let mut bytes = vault.total_size();
    for entry in vault
        .entries
        .values()
        .filter(|entry| !vault.is_trashed(entry.id))
    {
        if let EntryKind::Shared { space, .. } = entry.kind
            && let Some(instance) = spaces.iter().find(|instance| instance.id() == space)
        {
            let state = instance.pending();
            if state.is_ready() && state.can_write(vault.owner) {
                count = count
                    .saturating_sub(1)
                    .saturating_add(state.tree.item_count());
                bytes = bytes.saturating_add(state.tree.total_size());
            }
        }
    }
    (count, bytes)
}

/// Personal placement and trash affect only the shortcut, never the
/// collaborators' shared root or its contents.
pub(crate) fn shortcut_page(vault: Instance<Vault>, id: EntryId) -> Page {
    Page::watching(
        Watch::contract(vault),
        move |_: &mut Context, theme: &Theme, vault: Instance<Vault>| {
            let state = vault.pending();
            let Some(entry) = state
                .get(id)
                .filter(|entry| matches!(entry.kind, EntryKind::Shared { .. }))
            else {
                return PageType::display(
                    "Shortcut removed",
                    vec![Display::instructions(
                        "You can open the shared item from Shared and add it to Files again.",
                    )],
                    None,
                    Bumper::None,
                    Offset::Start,
                );
            };
            let trashed = state.is_trashed(id);
            let name = entry.name.clone();
            drop(state);
            if trashed {
                return PageType::display(
                    "Shortcut in Trash",
                    vec![Display::instructions(
                        "Restore the shortcut from your personal Trash. The shared contents remain available to collaborators.",
                    )],
                    None,
                    Bumper::custom(
                        "Open Trash",
                        Action::flow(Flow::new(vec![listing(
                            vault.clone(),
                            Location::Trash,
                            false,
                        )])),
                    ),
                    Offset::Start,
                );
            }
            let deleting = vault.clone();
            PageType::display(
                "Files shortcut",
                vec![
                    Display::label(&name),
                    Display::instructions(
                        "These settings affect only your shortcut in Files. The shared item remains available to everyone.",
                    ),
                    Display::list(
                        None,
                        vec![ListItem::plain(
                            "Move shortcut",
                            "Choose a folder in your library",
                            None,
                            Some(move_flow(vault.clone(), id, theme)),
                        )],
                        None,
                    ),
                ],
                Some((
                    Icons::Delete,
                    Box::new(move |_: &mut Context, _: &Theme| {
                        let deleting = deleting.clone();
                        Flow::new(vec![Page::Static(Box::new(PageType::display(
                            "Remove shortcut",
                            vec![Display::instructions(
                                "Move this shortcut to your Trash? Shared files remain available to everyone, and you can still open them from Shared.",
                            )],
                            None,
                            Bumper::double(
                                "Remove shortcut",
                                Action::custom(move |ctx: &mut Context, theme: &Theme| {
                                    match storage::trash(&mut deleting.clone(), id) {
                                        Ok(_) => FormComplete::Return(Box::new(
                                            |_: &mut Context, _: &Theme| {},
                                        ))
                                        .run(ctx, theme),
                                        Err(error) => show(
                                            ctx,
                                            theme,
                                            message(
                                                "Could not remove shortcut",
                                                &error.to_string(),
                                            ),
                                        ),
                                    }
                                }),
                                "Cancel",
                                Action::None,
                            ),
                            Offset::Start,
                        )))])
                    }),
                )),
                Bumper::None,
                Offset::Start,
            )
        },
    )
}
