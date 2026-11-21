//! Shared files use CHK pages while all changes target the same AIR contract.
use crate::{
    browser,
    collaboration::{self, ReplaceOutcome, SharedSpace},
    runtime::FileEvent,
    sharing_ui,
    storage::{self, Entry, EntryId},
};
use chk::{
    Action, ActionItem, AvatarContent, AvatarIconStyle, Bumper, Context, Display, Flow, Form, FormComplete,
    FormItem, FormValidState, FormValues, Icons, ListItem, Offset, Page, PageType, State, Theme,
    ValueItem, ContractWatch, ListWatch,
};
use prism::air::{Id, Instance};

const MAX_EDIT_TEXT: usize = 256 * 1024;

fn native(event: FileEvent) -> Action {
    Action::custom(move |ctx: &mut Context, _: &Theme| ctx.emit(event.clone()))
}

fn unavailable(body: &str) -> PageType {
    PageType::display(
        "Shared item unavailable",
        vec![Display::instructions(body)],
        None,
        Bumper::None,
        Offset::Start,
    )
}

/// A private library entry is only a link. Its displayed contents stay subscribed
/// to the canonical shared contract, including after a recipient joins.
pub(crate) fn mount(space: Id) -> Page {
    let fallback = Page::watching(
        ListWatch::<SharedSpace>::default(),
        move |ctx: &mut Context, theme: &Theme, spaces: Vec<Instance<SharedSpace>>| {
            let Some(instance) = spaces.into_iter().find(|instance| instance.id() == space) else {
                return unavailable(
                    "This shared item is still syncing. Keep Fig Files open and try again.",
                );
            };
            let root = instance.pending().root;
            render_item(ctx, theme, instance, root)
        },
    );
    crate::preview::page(crate::preview::Source::Mount(space), fallback)
}

pub(crate) fn item(space: Instance<SharedSpace>, id: EntryId) -> Page {
    let fallback = Page::watching(
        ContractWatch::new(space.clone()),
        move |ctx: &mut Context, theme: &Theme, space: Instance<SharedSpace>| {
            render_item(ctx, theme, space, id)
        },
    );
    crate::preview::page(crate::preview::Source::Shared(space, id), fallback)
}

pub(crate) fn folder(space: Instance<SharedSpace>, id: EntryId) -> Page {
    item(space, id)
}

pub(crate) fn render_item(
    ctx: &mut Context,
    theme: &Theme,
    space: Instance<SharedSpace>,
    id: EntryId,
) -> PageType {
    let state = space.pending();
    if state.validate().is_err() || !state.is_ready() {
        return unavailable("This shared item is still syncing or could not be read.");
    }
    if state.validate().is_err() || !state.can_write(ctx.air.me()) {
        return PageType::display(
            "Shared access",
            vec![Display::instructions(
                "Join this shared item to open and edit it with the other collaborators.",
            )],
            None,
            Bumper::custom(
                "View invitation",
                Action::flow(Flow::new(vec![sharing_ui::invitation_detail(
                    space.clone(),
                )])),
            ),
            Offset::Start,
        );
    }
    let Some(entry) = state.tree.get(id).cloned() else {
        return unavailable("This item has been removed by a collaborator.");
    };
    let is_folder = entry.is_folder() && !state.tree.is_trashed(id);
    drop(state);
    if is_folder {
        render_folder(ctx, theme, space, entry)
    } else {
        render_detail(ctx, theme, space, entry)
    }
}

fn row(theme: &Theme, entry: &Entry, subtitle: &str, page: Page) -> ListItem {
    ListItem::avatar(
        theme
            .image(browser::entry_art(entry))
            .map(AvatarContent::image)
            .unwrap_or_else(|| {
                AvatarContent::icon(
                    if entry.is_folder() {
                        Icons::Home
                    } else {
                        Icons::Credential
                    },
                    AvatarIconStyle::Brand,
                )
            }),
        &entry.name,
        subtitle,
        None,
        Some(Action::flow(Flow::new(vec![page]))), false, None,
    )
}

fn render_folder(
    _: &mut Context,
    theme: &Theme,
    space: Instance<SharedSpace>,
    entry: Entry,
) -> PageType {
    let state = space.pending();
    let root = state.root;
    let children = state.tree.children(Some(entry.id));
    let path = state
        .tree
        .breadcrumbs(Some(entry.id))
        .into_iter()
        .map(|e| e.name)
        .collect::<Vec<_>>()
        .join(" / ");
    let subtree = state
        .tree
        .entries
        .values()
        .filter(|child| {
            !state.tree.is_trashed(child.id)
                && (child.id == entry.id || state.tree.is_descendant(child.id, entry.id))
        })
        .collect::<Vec<_>>();
    let count = subtree.len();
    let bytes = subtree.iter().map(|child| child.size()).sum();
    drop(state);
    let rows = children
        .iter()
        .map(|child| {
            row(
                theme,
                child,
                &if child.is_folder() {
                    "Shared folder".into()
                } else {
                    storage::format_size(child.size())
                },
                item(space.clone(), child.id),
            )
        })
        .collect();
    let mut body = vec![
        Display::text(&path),
        Display::list(None, rows, Some("This folder is empty.")),
        Display::cta(
            "Folder details", None, "Share and manage this folder",
            vec![ActionItem::new(
                Action::flow(Flow::new(vec![details(space.clone(), entry.id)])),
                "Folder details", Icons::Info,
            )],
        ),
    ];
    if entry.id == root {
        body.push(Display::cta(
            "Shared Trash", None, "Restore items removed by collaborators",
            vec![ActionItem::new(
                Action::flow(Flow::new(vec![trash_page(space.clone())])),
                "Shared Trash", Icons::Delete,
            )],
        ));
    }
    body.push(Display::instructions(&format!(
        "{count} items · {} in this shared folder",
        storage::format_size(bytes)
    )));
    let search = space.clone();
    let add = space.clone();
    PageType::display(
        &entry.name,
        body,
        Some((
            Icons::Search,
            Action::flow(search_flow(search.clone(), entry.id, theme)),
        )),
        Bumper::custom(
            "Add files",
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                browser::show(ctx, theme, add_page(add.clone(), entry.id, theme))
            }),
        ),
        Offset::Start,
    )
}

pub(crate) fn details(space: Instance<SharedSpace>, id: EntryId) -> Page {
    Page::watching(
        ContractWatch::new(space),
        move |ctx: &mut Context, theme: &Theme, space: Instance<SharedSpace>| {
            let state = space.pending();
            if state.validate().is_err() || !state.can_write(ctx.air.me()) || !state.is_ready() {
                return unavailable("Join this item from Shared first.");
            }
            let Some(entry) = state.tree.get(id).cloned() else {
                return unavailable("This item was removed.");
            };
            drop(state);
            render_detail(ctx, theme, space, entry)
        },
    )
}

pub(crate) fn render_detail(
    ctx: &mut Context,
    theme: &Theme,
    space: Instance<SharedSpace>,
    entry: Entry,
) -> PageType {
    let state = space.pending();
    if state.validate().is_err() || !state.is_ready() || !state.can_write(ctx.air.me()) {
        return unavailable("Join this item from Shared first.");
    }
    let Some(entry) = state.tree.get(entry.id).cloned() else {
        return unavailable("This item was removed.");
    };
    let root = state.root;
    let trashed = state.tree.is_trashed(entry.id);
    let conflict_count = state.conflicts(entry.id).len();
    let path = state
        .tree
        .breadcrumbs(entry.parent)
        .into_iter()
        .map(|parent| parent.name)
        .collect::<Vec<_>>()
        .join(" / ");
    let root_name = state.title().to_owned();
    let shared_folder = state.is_folder();
    let can_edit_text = !entry.is_folder()
        && !trashed
        && entry.size() <= MAX_EDIT_TEXT as u64
        && state.read_file(entry.id).is_ok_and(|bytes| {
            std::str::from_utf8(&bytes).is_ok() && !bytes.contains(&0)
        });
    drop(state);
    let mut body = if !entry.is_folder() && !trashed {
        Vec::new()
    } else {
        vec![
            Display::image(browser::entry_art(&entry), 112.0),
            Display::label(&entry.name),
            Display::instructions(&if entry.is_folder() {
                "Shared folder · Everyone can edit".into()
            } else {
                format!("{} · Everyone can edit", storage::format_size(entry.size()))
            }),
        ]
    };
    let conflict_notice = (conflict_count > 0).then(|| {
        Display::cta(
            "Edits to review", None,
            &format!("{conflict_count} simultaneous edits kept safely"),
            vec![ActionItem::new(
                Action::flow(Flow::new(vec![conflicts_page(space.clone(), entry.id)])),
                "Edits to review", Icons::Info,
            )],
        )
    });
    if (entry.is_folder() || trashed)
        && let Some(notice) = &conflict_notice
    {
        body.push(notice.clone());
    }
    if trashed {
        body.push(Display::instructions(
            "This item is in Shared Trash for everyone.",
        ));
        let restoring = space.clone();
        let deleting = space.clone();
        return PageType::display(
            &entry.name,
            body,
            Some((
                Icons::Delete,
                Action::flow({
                    Flow::new(vec![delete_confirmation(deleting.clone(), entry.id, true)])
                }),
            )),
            Bumper::custom(
                "Restore for everyone",
                Action::custom(
                    move |ctx: &mut Context, theme: &Theme| match collaboration::restore(
                        &ctx.air,
                        &mut restoring.clone(),
                        entry.id,
                    ) {
                        Ok(_) => return_to(restoring.clone(), entry.id).run(ctx, theme),
                        Err(error) => browser::show(
                            ctx,
                            theme,
                            browser::message("Could not restore", &error.to_string()),
                        ),
                    },
                ),
            ),
            Offset::Start,
        );
    }
    if !entry.is_folder() {
        if let Some(notice) = conflict_notice {
            body.push(notice);
        }
        body.push(Display::table(
            "Details",
            vec![
                ValueItem::new("Name", &entry.name),
                ValueItem::new("Kind", entry.mime()),
                ValueItem::new("Size", &storage::format_size(entry.size())),
                ValueItem::new(
                    "Location",
                    &if path.is_empty() {
                        "Shared".into()
                    } else {
                        format!("Shared / {path}")
                    },
                ),
                ValueItem::new("Access", "Read and write for everyone with access"),
            ],
            None,
        ));
        let (move_hint, moving, delete_hint, delete) = if entry.id == root {
            (
                "Move your Files shortcut",
                root_move_flow(ctx, space.id(), theme),
                "Remove your Files shortcut; shared content stays available",
                root_delete_page(ctx, space.id()),
            )
        } else {
            (
                "Move within this shared folder",
                move_flow(space.clone(), entry.id, theme),
                "Move this file to Shared Trash for everyone",
                delete_confirmation(space.clone(), entry.id, false),
            )
        };
        let share_hint = if shared_folder {
            format!("Share the entire shared folder “{root_name}”, including all files")
        } else {
            "Give contacts read and write access to this shared file".into()
        };
        let mut contents_actions = Vec::new();
        if can_edit_text {
            let edit_space = space.clone();
            let edit_id = entry.id;
            contents_actions.push(ActionItem::new(
                Action::deferred_flow(move |_: &mut Context, theme: &Theme| {
                    edit_text_flow(edit_space.clone(), edit_id, theme)
                }),
                "Edit text", Icons::Edit,
            ));
        }
        contents_actions.push(ActionItem::new(
            Action::flow(Flow::new(vec![replace_page(space.clone(), entry.id)])),
            "Replace file", Icons::Up,
        ));
        body.extend([
            Display::cta(
                "Share", None, &share_hint,
                vec![ActionItem::new(
                    Action::flow(sharing_ui::invite_flow(ctx, space.clone(), theme)),
                    "Share", Icons::Send,
                )],
            ),
            Display::cta(
                "Move", None, move_hint,
                vec![ActionItem::new(Action::flow(moving), "Move", Icons::Forward)],
            ),
            Display::cta(
                "Delete", None, delete_hint,
                vec![ActionItem::new(
                    Action::flow(Flow::new(vec![delete])),
                    "Delete", Icons::Delete,
                )],
            ),
            Display::cta(
                "File contents", None, "Changes apply to everyone with access.",
                contents_actions,
            ),
            Display::cta(
                "Export file", None, "Open a local copy in another app",
                vec![ActionItem::new(
                    Action::flow(Flow::new(vec![export_page(space.clone(), entry.id)])),
                    "Export file", Icons::Forward,
                )],
            ),
        ]);
        return PageType::display(
            &entry.name,
            body,
            Some((Icons::Edit, Action::flow(name_flow(space, entry.id, false, theme)))),
            Bumper::None,
            Offset::Start,
        );
    }
    let (move_hint, moving, delete_hint, deleting) = if entry.id == root {
        (
            "Move your Files shortcut",
            root_move_flow(ctx, space.id(), theme),
            "Remove your Files shortcut; shared content stays available",
            root_delete_page(ctx, space.id()),
        )
    } else {
        (
            "Move within this shared folder",
            move_flow(space.clone(), entry.id, theme),
            "Move this folder to Shared Trash for everyone",
            delete_confirmation(space.clone(), entry.id, false),
        )
    };
    body.extend([
        Display::cta(
            "Share",
            None,
            &format!("Share the entire shared folder “{root_name}”, including all files"),
            vec![ActionItem::new(
                Action::flow(sharing_ui::invite_flow(ctx, space.clone(), theme)),
                "Share",
                Icons::Send,
            )],
        ),
        Display::cta(
            "Move",
            None,
            move_hint,
            vec![ActionItem::new(Action::flow(moving), "Move", Icons::Forward)],
        ),
        Display::cta(
            "Delete",
            None,
            delete_hint,
            vec![ActionItem::new(
                Action::flow(Flow::new(vec![deleting])),
                "Delete",
                Icons::Delete,
            )],
        ),
    ]);
    PageType::display(
        &entry.name,
        body,
        Some((Icons::Edit, Action::flow(name_flow(space, entry.id, false, theme)))),
        Bumper::None,
        Offset::Start,
    )
}

fn root_shortcut(ctx: &Context, space: Id) -> Option<(Instance<storage::Vault>, EntryId)> {
    ctx.air.instances::<storage::Vault>().list().into_values().find_map(|vault| {
        let state = vault.pending();
        if state.owner != ctx.air.me() { return None; }
        let id = state.entries.values().find(|entry| {
            matches!(entry.kind, storage::EntryKind::Shared { space: existing, .. } if existing == space)
                && !state.is_trashed(entry.id)
        }).map(|entry| entry.id);
        drop(state);
        id.map(|id| (vault, id))
    })
}

fn root_move_flow(ctx: &Context, space: Id, theme: &Theme) -> Flow {
    if let Some((vault, id)) = root_shortcut(ctx, space) {
        browser::move_flow(vault, id, theme)
    } else {
        Flow::new(vec![browser::message(
            "No Files shortcut",
            "Add this shared item to Files before moving its shortcut.",
        )])
    }
}

fn root_delete_page(ctx: &Context, space: Id) -> Page {
    if let Some((vault, id)) = root_shortcut(ctx, space) {
        browser::shortcut_delete_confirmation(vault, id)
    } else {
        Page::Static(Box::new(PageType::display(
            "Shared item",
            vec![Display::instructions(
                "This shared item has no shortcut in your Files. The shared root cannot be deleted here and remains available to everyone with access.",
            )],
            None,
            Bumper::None,
            Offset::Start,
        )))
    }
}

pub(crate) fn add_page(space: Instance<SharedSpace>, parent: EntryId, theme: &Theme) -> Page {
    Page::Static(Box::new(PageType::display(
        "Add to shared folder",
        vec![
            Display::instructions(
                "New files and folders are available to everyone with access. Files can be up to 8 MiB each.",
            ),
            Display::cta(
                "New folder", None, "Create a shared folder here",
                vec![ActionItem::new(
                    Action::flow(name_flow(space.clone(), parent, true, theme)),
                    "New folder", Icons::Add,
                )],
            ),
        ],
        None,
        Bumper::custom(
            "Import files",
            native(FileEvent::SharedImport(space.id(), parent)),
        ),
        Offset::Start,
    )))
}

fn export_page(space: Instance<SharedSpace>, id: EntryId) -> Page {
    Page::Static(Box::new(PageType::display(
        "Export file",
        vec![Display::instructions(
            "Export the current file to another app. To share changes back, use Replace file here.",
        )],
        None,
        Bumper::custom(
            "Export file",
            native(FileEvent::SharedExport(space.id(), id)),
        ),
        Offset::Start,
    )))
}
fn replace_page(space: Instance<SharedSpace>, id: EntryId) -> Page {
    Page::Static(Box::new(PageType::display(
        "Replace shared file",
        vec![Display::instructions(
            "Choose one updated file. Its contents will replace this file for everyone; the existing filename stays the same. Simultaneous edits are kept for review.",
        )],
        None,
        Bumper::custom(
            "Choose updated file",
            native(FileEvent::SharedReplace(space.id(), id)),
        ),
        Offset::Start,
    )))
}

fn valid_name(name: String) -> FormValidState {
    match storage::validate_name(&name) {
        Ok(_) => FormValidState::Valid,
        Err(error) => FormValidState::InvalidWithData(error.to_string()),
    }
}
fn name_flow(space: Instance<SharedSpace>, id: EntryId, create: bool, _theme: &Theme) -> Flow {
    let (name, folder) = space
        .pending()
        .tree
        .get(id)
        .map(|entry| (entry.name.clone(), entry.is_folder()))
        .unwrap_or_default();
    if !create {
        return Flow::from_form(Form::page(
            if folder { "Edit folder" } else { "Edit file" },
            vec![FormItem::text_with_preset(
                "name", uuid::Uuid::new_v4(),
                if folder { "Folder name" } else { "File name" },
                &name, None, |_: &mut Context, text| valid_name(text),
            )],
            vec![Display::instructions("Changes apply to everyone with access.")],
            Box::new(move |ctx: &mut Context, states: &FormValues| {
                let Some(State::Text(name)) = states.get("name") else {
                    return FormComplete::None;
                };
                match collaboration::rename(&ctx.air, &mut space.clone(), id, name) {
                    Ok(_) => FormComplete::None,
                    Err(error) => FormComplete::Error(error.to_string()),
                }
            }),
        ));
    }
    Flow::from_form(Form::flow(
        vec![FormItem::text(
            "name", uuid::Uuid::new_v4(), "New folder",
            "folder name",
            None,
            |_: &mut Context, text| valid_name(text),
        )],
        None,
        None,
        Box::new(move |ctx: &mut Context, states: &FormValues| {
            let Some(State::Text(name)) = states.get("name") else {
                return FormComplete::None;
            };
            match collaboration::create_folder(&ctx.air, &mut space.clone(), id, name) {
                Ok(_) => return_to(space.clone(), id),
                Err(error) => {
                    FormComplete::Next(browser::message("Could not save", &error.to_string()))
                }
            }
        }),
    ))
}

fn return_to(space: Instance<SharedSpace>, id: EntryId) -> FormComplete {
    FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
        browser::show(ctx, theme, item(space.clone(), id))
    }))
}

/// Remote updates never reset the draft or its base revision. A successful Save
/// advances that base to this editor's submitted bytes for the next edit.
pub(crate) fn edit_text_flow(space: Instance<SharedSpace>, id: EntryId, _theme: &Theme) -> Flow {
    let state = space.pending();
    let draft = state
        .read_file(id)
        .ok()
        .filter(|bytes| bytes.len() <= MAX_EDIT_TEXT)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|text| !text.contains('\0'));
    let expected = state.content_hash(id);
    drop(state);
    let (Some(draft), Ok(mut expected)) = (draft, expected) else {
        return Flow::new(vec![browser::message(
            "Use Replace file",
            "The text editor supports UTF-8 files up to 256 KiB. Export this file to edit it, then use Replace file.",
        )]);
    };
    Flow::from_form(Form::page(
        "Edit shared text",
        vec![
            FormItem::text_with_preset(
                "content", uuid::Uuid::new_v4(), "File contents",
                &draft,
                None,
                |_: &mut Context, text: String| {
                    if text.len() <= MAX_EDIT_TEXT && !text.contains('\0') {
                        FormValidState::Valid
                    } else {
                        FormValidState::InvalidWithData(
                            "Use UTF-8 text up to 256 KiB without null bytes.".into(),
                        )
                    }
                },
            ),
        ],
        vec![Display::instructions(
            "Save updates this file for everyone. If someone edits it while you type, both versions are kept under Edits to review.",
        )],
        Box::new(move |ctx: &mut Context, states: &FormValues| {
            let Some(State::Text(text)) = states.get("content") else {
                return FormComplete::None;
            };
            let bytes = text.as_bytes().to_vec();
            let submitted = Id::hash(&bytes);
            match collaboration::replace_file(
                &ctx.air,
                &mut space.clone(),
                id,
                expected,
                bytes,
            ) {
                Ok(ReplaceOutcome::Updated) => {
                    expected = submitted;
                    FormComplete::None
                }
                Ok(ReplaceOutcome::SavedConflict(_)) => {
                    FormComplete::Next(conflicts_page(space.clone(), id))
                }
                Err(error) => FormComplete::Error(format!("Could not save — draft kept. {error}")),
            }
        }),
    ))
}

fn move_flow(space: Instance<SharedSpace>, id: EntryId, _theme: &Theme) -> Flow {
    let state = space.pending();
    let current = state.tree.get(id).and_then(|entry| entry.parent);
    let choices = state
        .tree
        .folders()
        .into_iter()
        .filter(|entry| {
            entry.id != id && Some(entry.id) != current && !state.tree.is_descendant(entry.id, id)
        })
        .map(|entry| {
            let path = state
                .tree
                .breadcrumbs(Some(entry.id))
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(" / ");
            (entry.id, path)
        })
        .collect::<Vec<_>>();
    drop(state);
    if choices.is_empty() {
        return Flow::new(vec![browser::message(
            "Nowhere to move yet",
            "Create another folder inside this shared folder first.",
        )]);
    }
    let field = FormItem::enumerator(
        "destination", "Move within shared folder",
        choices
            .iter()
            .map(|(_, path)| (path.as_str(), "Shared folder"))
            .collect(),
    );
    Flow::from_form(Form::flow(
        vec![field],
        None,
        None,
        Box::new(move |ctx: &mut Context, states: &FormValues| {
            let Some(State::Enumerator(index)) = states.get("destination") else {
                return FormComplete::None;
            };
            let Some((parent, _)) = choices.get(*index) else {
                return FormComplete::None;
            };
            match collaboration::move_entry(&ctx.air, &mut space.clone(), id, *parent) {
                Ok(_) => return_to(space.clone(), *parent),
                Err(error) => {
                    FormComplete::Next(browser::message("Could not move", &error.to_string()))
                }
            }
        }),
    ))
}

fn search_flow(space: Instance<SharedSpace>, scope: EntryId, _theme: &Theme) -> Flow {
    Flow::new(vec![crate::search::page(
        "Shared folder",
        ContractWatch::new(space),
        move |ctx: &mut Context, theme: &Theme, space: Instance<SharedSpace>, query: &str| {
            let state = space.pending();
            if state.validate().is_err() || !state.can_write(ctx.air.me()) {
                return vec![Display::instructions("Join the shared folder first.")];
            }
            if state.tree.get(scope).is_none_or(|entry| !entry.is_folder())
                || state.tree.is_trashed(scope)
            {
                return vec![Display::instructions("This folder is no longer available.")];
            }
            let terms = query
                .split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>();
            let mut entries = state
                .tree
                .entries
                .values()
                .filter(|entry| {
                    let name = entry.name.to_lowercase();
                    !state.tree.is_trashed(entry.id)
                        && state.tree.is_descendant(entry.id, scope)
                        && terms.iter().all(|term| name.contains(term))
                })
                .cloned()
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| {
                b.is_folder()
                    .cmp(&a.is_folder())
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.id.cmp(&b.id))
            });
            drop(state);
            let rows = entries
                .iter()
                .map(|entry| row(theme, entry, "Shared item", item(space.clone(), entry.id)))
                .collect();
            vec![Display::list(
                None,
                rows,
                Some(if terms.is_empty() {
                    "No files yet."
                } else {
                    "No matching files or folders."
                }),
            )]
        },
    )])
}

pub(crate) fn trash_page(space: Instance<SharedSpace>) -> Page {
    Page::watching(
        ContractWatch::new(space),
        |ctx: &mut Context, theme: &Theme, space: Instance<SharedSpace>| {
            let state = space.pending();
            if state.validate().is_err() || !state.can_write(ctx.air.me()) {
                return unavailable("Join the shared folder first.");
            }
            let entries = state.tree.trash();
            drop(state);
            let rows = entries
                .iter()
                .map(|entry| {
                    row(
                        theme,
                        entry,
                        "Removed for everyone",
                        item(space.clone(), entry.id),
                    )
                })
                .collect();
            PageType::display(
                "Shared Trash",
                vec![Display::list(None, rows, Some("Trash is empty."))],
                None,
                Bumper::None,
                Offset::Start,
            )
        },
    )
}
fn delete_confirmation(space: Instance<SharedSpace>, id: EntryId, permanent: bool) -> Page {
    let root = space.pending().root;
    let label = if permanent {
        "Delete for everyone"
    } else {
        "Move to Shared Trash"
    };
    let body = if permanent {
        "Permanently remove this item and its contents for everyone? This cannot be undone in Fig Files. Encrypted AIR history may retain previous data."
    } else {
        "Move this item and everything inside it to Shared Trash for everyone? Any collaborator can restore it later."
    };
    Page::Static(Box::new(PageType::display(
        label,
        vec![Display::instructions(body)],
        None,
        Bumper::double(
            label,
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                let result = if permanent {
                    collaboration::delete_permanently(&ctx.air, &mut space.clone(), id)
                } else {
                    collaboration::trash(&ctx.air, &mut space.clone(), id)
                };
                match result {
                    Ok(_) => return_to(space.clone(), root).run(ctx, theme),
                    Err(error) => browser::show(
                        ctx,
                        theme,
                        browser::message("Could not remove", &error.to_string()),
                    ),
                }
            }),
            "Cancel",
            Action::None,
        ),
        Offset::Start,
    )))
}

pub(crate) fn conflicts_page(space: Instance<SharedSpace>, id: EntryId) -> Page {
    Page::watching(
        ContractWatch::new(space),
        move |ctx: &mut Context, _: &Theme, space: Instance<SharedSpace>| {
            let state = space.pending();
            if state.validate().is_err() || !state.can_write(ctx.air.me()) {
                return unavailable("Join the shared item first.");
            }
            let versions = state.conflicts(id);
            drop(state);
            let rows = versions
                .iter()
                .enumerate()
                .map(|(index, version)| {
                    ListItem::plain(
                        &format!("Saved edit {}", index + 1),
                        &format!(
                            "{} · {}",
                            storage::format_size(version.bytes.len() as u64),
                            short_name(version.author)
                        ),
                        None,
                        Some(Action::flow(Flow::new(vec![conflict_page(
                            space.clone(),
                            id,
                            version.id,
                        )]))), false, None,
                    )
                })
                .collect();
            PageType::display(
                "Edits to review",
                vec![
                    Display::instructions(
                        "Someone changed this file before another edit arrived. The current file is unchanged and the other edits are kept here. Review an edit before making it current.",
                    ),
                    Display::list(None, rows, Some("No simultaneous edits to review.")),
                ],
                None,
                Bumper::None,
                Offset::Start,
            )
        },
    )
}
fn short_name(name: prism::air::Name) -> String {
    let name = name.to_string();
    format!("{}…{}", &name[..10], &name[name.len() - 8..])
}
fn conflict_page(space: Instance<SharedSpace>, id: EntryId, version: Id) -> Page {
    Page::watching(
        ContractWatch::new(space),
        move |ctx: &mut Context, _: &Theme, space: Instance<SharedSpace>| {
            let state = space.pending();
            if state.validate().is_err() || !state.can_write(ctx.air.me()) {
                return unavailable("Join the shared item first.");
            }
            let Some(saved) = state
                .conflicts(id)
                .into_iter()
                .find(|saved| saved.id == version)
            else {
                return unavailable("This edit has already been resolved.");
            };
            let expected = state.content_hash(id);
            drop(state);
            let mut body = vec![
                Display::label("Saved simultaneous edit"),
                Display::instructions(&format!(
                    "{} · by {}",
                    storage::format_size(saved.bytes.len() as u64),
                    short_name(saved.author)
                )),
            ];
            if let Ok(text) = std::str::from_utf8(&saved.bytes)
                && !text.contains('\0')
            {
                let excerpt: String = text.chars().take(16_000).collect();
                body.push(Display::text(&excerpt));
                if text.chars().nth(16_000).is_some() {
                    body.push(Display::instructions(
                        "Preview shortened. Export this edit for the complete file.",
                    ));
                }
            } else {
                body.push(Display::instructions(
                    "Export this edit to review it in another app.",
                ));
            }
            // Resolution replaces the canonical file, so the final review makes
            // the scope and preservation of the displaced current bytes explicit.
            body.push(Display::instructions("Making this edit current updates the file for everyone. The previous current file is kept as an edit to review."));
            let applying = space.clone();
            let resolve = Action::custom(move |ctx: &mut Context, theme: &Theme| {
                let Ok(expected) = expected else {
                    return;
                };
                match collaboration::resolve_conflict(
                    &ctx.air,
                    &mut applying.clone(),
                    id,
                    version,
                    expected,
                ) {
                    Ok(ReplaceOutcome::Updated) => return_to(applying.clone(), id).run(ctx, theme),
                    Ok(ReplaceOutcome::SavedConflict(_)) => browser::show(
                        ctx,
                        theme,
                        browser::message(
                            "File changed again",
                            "Your saved edit is still available. Review the latest file before trying again.",
                        ),
                    ),
                    Err(error) => browser::show(
                        ctx,
                        theme,
                        browser::message("Could not apply edit", &error.to_string()),
                    ),
                }
            });
            let discarding = space.clone();
            PageType::display(
                "Review edit",
                body,
                Some((
                    Icons::Delete,
                    Action::flow({
                        Flow::new(vec![discard_confirmation(discarding.clone(), id, version)])
                    }),
                )),
                Bumper::double(
                    "Make current",
                    resolve,
                    "Export this edit",
                    native(FileEvent::SharedExportConflict(space.id(), id, version)),
                ),
                Offset::Start,
            )
        },
    )
}

fn discard_confirmation(space: Instance<SharedSpace>, id: EntryId, version: Id) -> Page {
    Page::Static(Box::new(PageType::display(
        "Discard saved edit",
        vec![Display::instructions(
            "Remove this saved edit for everyone? The current file stays unchanged. Export the edit first if you want to keep it.",
        )],
        None,
        Bumper::double(
            "Discard edit",
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                match collaboration::discard_conflict(&ctx.air, &mut space.clone(), id, version) {
                    Ok(()) => FormComplete::Next(conflicts_page(space.clone(), id)).run(ctx, theme),
                    Err(error) => browser::show(
                        ctx,
                        theme,
                        browser::message("Could not discard", &error.to_string()),
                    ),
                }
            }),
            "Cancel",
            Action::None,
        ),
        Offset::Start,
    )))
}

pub(crate) fn saved_edits() -> Page {
    Page::watching(
        ListWatch::<crate::drafts::EditDraft>::default(),
        |ctx: &mut Context, _: &Theme, mut drafts: Vec<Instance<crate::drafts::EditDraft>>| {
            drafts.sort_by_key(|draft| std::cmp::Reverse(draft.pending().created_at));
            let rows = drafts
                .into_iter()
                .filter_map(|draft| {
                    let state = draft.pending();
                    if state.owner != ctx.air.me() || state.resolved || state.validate().is_err() {
                        return None;
                    }
                    let row = ListItem::plain(
                        &state.name,
                        &format!(
                            "{} · Private saved edit",
                            storage::format_size(state.bytes.len() as u64)
                        ),
                        None,
                        Some(Action::flow(Flow::new(vec![saved_edit(draft.clone())]))), false, None,
                    );
                    Some(row)
                })
                .collect();
            PageType::display(
                "Saved edits",
                vec![
                    Display::instructions(
                        "Your submitted edits are kept privately for recovery, including edits queued offline. They may already be synced. Check the current file before retrying or discard a backup when you no longer need it.",
                    ),
                    Display::list(None, rows, Some("No saved edits.")),
                ],
                None,
                Bumper::None,
                Offset::Start,
            )
        },
    )
}
pub(crate) fn saved_edit(draft: Instance<crate::drafts::EditDraft>) -> Page {
    Page::watching(
        (ContractWatch::new(draft), ListWatch::<SharedSpace>::default()),
        |ctx: &mut Context,
         _: &Theme,
         (draft, spaces): (
            Instance<crate::drafts::EditDraft>,
            Vec<Instance<SharedSpace>>,
        )| {
            let state = draft.pending();
            if state.owner != ctx.air.me() || state.resolved || state.validate().is_err() {
                return unavailable("This saved edit is no longer available.");
            }
            let mut body = vec![
                Display::label(&state.name),
                Display::instructions(
                    "This is your private saved edit. Export it to keep a local file, or retry saving it to the shared item.",
                ),
            ];
            if let Ok(text) = std::str::from_utf8(&state.bytes)
                && !text.contains('\0')
            {
                body.push(Display::text(
                    &text.chars().take(16_000).collect::<String>(),
                ));
                if text.chars().nth(16_000).is_some() {
                    body.push(Display::instructions(
                        "Preview shortened. Export includes the complete file.",
                    ));
                }
            }
            let space = spaces
                .into_iter()
                .find(|space| space.id() == state.space && space.pending().can_write(ctx.air.me()));
            let file = state.file;
            let expected = state.expected;
            let bytes = state.bytes.clone();
            let title = state.name.clone();
            if let Some(space) = &space {
                body.push(Display::cta(
                    "Current shared file", None, "Check the latest contents before retrying",
                    vec![ActionItem::new(
                        Action::flow(Flow::new(vec![item(space.clone(), file)])),
                        "Open file", Icons::Forward,
                    )],
                ));
                body.push(Display::instructions("Retry uses the version you originally edited. If the file has changed, your edit is kept under Edits to review."));
            }
            drop(state);
            let retrying = draft.clone();
            let discarding = draft.clone();
            let bumper = if let Some(space) = space {
                Bumper::double(
                    "Retry saved edit",
                    Action::custom(move |ctx: &mut Context, theme: &Theme| {
                        match collaboration::replace_file(
                            &ctx.air,
                            &mut space.clone(),
                            file,
                            expected,
                            bytes.clone(),
                        ) {
                            Ok(_) => {
                                // The retry saves its own durable backup before enqueueing.
                                let _ = crate::drafts::resolve(&mut retrying.clone());
                                return_to(space.clone(), file).run(ctx, theme);
                            }
                            Err(error) => browser::show(
                                ctx,
                                theme,
                                browser::message("Saved edit kept", &error.to_string()),
                            ),
                        }
                    }),
                    "Export saved edit",
                    native(FileEvent::DraftExport(draft.id())),
                )
            } else {
                Bumper::custom(
                    "Export saved edit",
                    native(FileEvent::DraftExport(draft.id())),
                )
            };
            PageType::display(
                &title,
                body,
                Some((
                    Icons::Delete,
                    Action::flow({
                        let discarding = discarding.clone();
                        Flow::new(vec![Page::Static(Box::new(PageType::display(
                            "Discard private saved edit",
                            vec![Display::instructions(
                                "Remove this recovery draft? The shared file stays unchanged. If this edit has not synced, export it first to keep a copy.",
                            )],
                            None,
                            Bumper::double(
                                "Discard saved edit",
                                Action::custom(move |ctx: &mut Context, theme: &Theme| {
                                    match crate::drafts::resolve(&mut discarding.clone()) {
                                        Ok(()) => FormComplete::Next(saved_edits()).run(ctx, theme),
                                        Err(error) => browser::show(
                                            ctx,
                                            theme,
                                            browser::message(
                                                "Could not discard",
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
                bumper,
                Offset::Start,
            )
        },
    )
}
