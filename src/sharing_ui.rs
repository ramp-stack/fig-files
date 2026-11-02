//! Collaborative sharing and older file copies through public CHK pages.
use crate::{
    browser,
    collaboration::{self, MemberStatus, SharedSpace},
    collaboration_ui, contacts_ui,
    drafts::EditDraft,
    sharing::{self, Transfer, TransferStatus},
    storage::{self, EntryId, Vault},
};
use chk::{
    Action, AvatarContent, AvatarIconStyle, Bumper, Context, Display, Flow, Form, FormAction,
    FormComplete, FormItem, Icons, ListItem, Offset, Page, PageType, State, Theme, Watch,
};
use prism::air::{Instance, Name};

fn short_name(name: Name) -> String {
    let full = name.to_string();
    format!("{}…{}", &full[..10], &full[full.len() - 8..])
}

pub fn receive_name(name: Name) -> Page {
    let full = name.to_string();
    Page::Static(Box::new(PageType::display(
        "Receive files",
        vec![
            Display::qr_code(&full, "Your Fig Files receiving name"),
            Display::text(&full),
            Display::instructions("Share this name to receive files and folders here."),
        ],
        Some((
            Icons::Info,
            Box::new(move |_: &mut Context, _: &Theme| Flow::new(vec![name_help(name)])),
        )),
        Bumper::custom("Copy name", Action::copy(&full)),
        Offset::Start,
    )))
}

fn name_help(name: Name) -> Page {
    let full = name.to_string();
    Page::Static(Box::new(PageType::display(
        "Your receiving name",
        vec![Display::instructions(
            "Use the name shown in Fig Files. Your Orange app may have a different name. Invitations appear in Shared, where you can open the shared file or folder.",
        )],
        None,
        Bumper::custom("Share name", Action::share(&full)),
        Offset::Start,
    )))
}

pub fn home() -> Page {
    Page::watching(
        (
            Watch::list::<SharedSpace>(),
            Watch::list::<Transfer>(),
            Watch::list::<EditDraft>(),
        ),
        |ctx: &mut Context,
         theme: &Theme,
         (mut spaces, transfers, drafts): (
            Vec<Instance<SharedSpace>>,
            Vec<Instance<Transfer>>,
            Vec<Instance<EditDraft>>,
        )| {
            let me = ctx.air.me();
            spaces.sort_by_key(|space| std::cmp::Reverse(space.pending().created_at));
            let mut items = vec![];
            for instance in spaces {
                let space = instance.pending();
                if space.validate().is_err() {
                    continue;
                }
                let Some(member) = space.members.get(&me) else {
                    continue;
                };
                if *member == MemberStatus::Declined {
                    continue;
                }
                let owned = space.owner == me;
                let state = if !space.is_ready() {
                    "Syncing"
                } else if *member == MemberStatus::Invited {
                    "Invitation"
                } else {
                    "Read & write"
                };
                let subtitle = if owned {
                    let people = space
                        .members
                        .values()
                        .filter(|status| **status != MemberStatus::Declined)
                        .count();
                    format!("{people} people · {state}")
                } else {
                    format!("From {} · {state}", short_name(space.owner))
                };
                let art = if space.is_folder() {
                    "file-icons/folder"
                } else {
                    "file-icons/document"
                };
                let row = ListItem::avatar(
                    theme
                        .brand()
                        .images
                        .get(art)
                        .cloned()
                        .map(AvatarContent::image)
                        .unwrap_or_else(|| {
                            AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                        }),
                    space.title(),
                    &subtitle,
                    None,
                    Some(Flow::new(vec![invitation_detail(instance.clone())])),
                )
                .with_badge(!owned && *member == MemberStatus::Invited);
                items.push(row);
            }
            let displays = vec![Display::list(
                None,
                items,
                Some("Files and folders you share or receive will appear here."),
            )];
            let mut options = Vec::new();
            if drafts.iter().any(|instance| {
                let draft = instance.pending();
                draft.owner == me && !draft.resolved && draft.validate().is_ok()
            }) {
                options.push(ListItem::plain(
                    "Saved edits",
                    "Private recovery drafts",
                    None,
                    Some(Flow::new(vec![collaboration_ui::saved_edits()])),
                ));
            }
            let has_older_copies = transfers.iter().any(|instance| {
                let transfer = instance.pending();
                (transfer.sender == me || transfer.recipient == me) && transfer.validate().is_ok()
            });
            if has_older_copies {
                options.push(ListItem::plain(
                    "Older copies",
                    "Copies sent before shared access",
                    None,
                    Some(Flow::new(vec![legacy_home()])),
                ));
            }
            let header = (!options.is_empty()).then(|| {
                let options = Page::Static(Box::new(PageType::display(
                    "Shared options",
                    vec![Display::list(None, options, None)],
                    None,
                    Bumper::None,
                    Offset::Start,
                )));
                (
                    Icons::Info,
                    Box::new(move |_: &mut Context, _: &Theme| Flow::new(vec![options.clone()]))
                        as Box<dyn chk::FlowBuilder>,
                )
            });
            PageType::root(
                "Shared",
                vec![],
                displays,
                header,
                Some((
                    "Send".into(),
                    Box::new(|_: &mut Context, _: &Theme| Flow::new(vec![send_picker()])),
                )),
                Some((
                    "Receive".into(),
                    Box::new(move |_: &mut Context, _: &Theme| Flow::new(vec![receive_name(me)])),
                )),
            )
        },
    )
}

pub fn invitation_detail(instance: Instance<SharedSpace>) -> Page {
    Page::watching(
        Watch::contract(instance),
        |ctx: &mut Context, _: &Theme, instance: Instance<SharedSpace>| {
            let space = instance.pending();
            let me = ctx.air.me();
            if space.validate().is_err() || !space.members.contains_key(&me) {
                return PageType::display(
                    "Shared item unavailable",
                    vec![Display::instructions(
                        "This shared item cannot be opened with your Fig Files name.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            }
            let member = &space.members[&me];
            let title = space.title().to_owned();
            let kind = if space.is_folder() { "folder" } else { "file" };
            let root = space.root;
            let mut displays = vec![
                Display::image(
                    if space.is_folder() {
                        "file-icons/folder"
                    } else {
                        "file-icons/document"
                    },
                    (80.0, 80.0),
                ),
                Display::label(&title),
                Display::instructions(&format!("Shared by {}", short_name(space.owner))),
            ];
            if *member == MemberStatus::Declined {
                displays.push(Display::label("Invitation declined"));
            } else {
                displays.push(Display::label("Read & write access"));
                displays.push(Display::instructions(if space.is_folder() {
                    "Everyone with access can edit this folder and its contents. Changes are shared."
                } else {
                    "Everyone with access can edit this file. Changes are shared."
                }));
                if !space.is_ready() {
                    displays.push(Display::instructions(
                        "The shared contents are still syncing.",
                    ));
                }
            }
            let bumper = if *member == MemberStatus::Declined || !space.is_ready() {
                Bumper::None
            } else {
                let joining = instance.clone();
                let label = if *member == MemberStatus::Invited {
                    format!("Join shared {kind}")
                } else {
                    format!("Open shared {kind}")
                };
                Bumper::custom(
                    &label,
                    Action::custom(move |ctx: &mut Context, theme: &Theme| {
                        let mut vault = storage::open(&ctx.air);
                        match collaboration::join(&ctx.air, &mut vault, &mut joining.clone()) {
                            Ok(_) => {
                                FormComplete::Next(collaboration_ui::item(joining.clone(), root))
                                    .run(ctx, theme)
                            }
                            Err(error) => browser::show(
                                ctx,
                                theme,
                                browser::message("Could not open shared item", &error.to_string()),
                            ),
                        }
                    }),
                )
            };
            let info = instance.clone();
            PageType::display(
                "Shared access",
                displays,
                Some((
                    Icons::Info,
                    Box::new(move |_: &mut Context, _: &Theme| {
                        Flow::new(vec![invitation_info(info.clone())])
                    }),
                )),
                bumper,
                Offset::Start,
            )
        },
    )
}

fn invitation_info(instance: Instance<SharedSpace>) -> Page {
    Page::watching(
        (Watch::contract(instance), Watch::list::<Vault>()),
        |ctx: &mut Context,
         _: &Theme,
         (instance, vaults): (Instance<SharedSpace>, Vec<Instance<Vault>>)| {
            let space = instance.pending();
            if space.validate().is_err() || !space.members.contains_key(&ctx.air.me()) {
                return PageType::display(
                    "Shared item unavailable",
                    vec![],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            }
            let bumper = if space.members.get(&ctx.air.me()) == Some(&MemberStatus::Invited) {
                Bumper::custom(
                    "Decline invitation",
                    Action::flow(Flow::new(vec![decline_invitation(instance.clone())])),
                )
            } else {
                Bumper::custom("Copy name", Action::copy(&space.owner.to_string()))
            };
            let mut displays = vec![
                Display::label("Shared by"),
                Display::text(&space.owner.to_string()),
                Display::instructions(
                    "Read and write access applies to the entire shared item. Changes are synchronized while Fig Files is online.",
                ),
                Display::list(
                    Some("People with access"),
                    space
                        .members
                        .iter()
                        .filter(|(_, status)| **status != MemberStatus::Declined)
                        .map(|(name, status)| {
                            let label = if *name == space.owner {
                                "Owner"
                            } else if *status == MemberStatus::Active {
                                "Joined"
                            } else {
                                "Invited"
                            };
                            ListItem::plain(&short_name(*name), label, None, None)
                        })
                        .collect(),
                    None,
                ),
            ];
            let shortcut = vaults.into_iter().find_map(|vault| {
                let library = vault.pending();
                if library.owner != ctx.air.me() {return None;}
                let entry = library.entries.values().find(|entry| {
                    matches!(entry.kind, storage::EntryKind::Shared { space, .. } if space == instance.id())
                }).cloned();
                drop(library);
                entry.map(|entry| (vault, entry))
            });
            if let Some((vault, entry)) = shortcut {
                displays.push(Display::list(
                    None,
                    vec![ListItem::plain(
                        "Files shortcut",
                        &entry.name,
                        None,
                        Some(Flow::new(vec![browser::shortcut_page(vault, entry.id)])),
                    )],
                    None,
                ));
            }
            PageType::display("Sharing details", displays, None, bumper, Offset::Start)
        },
    )
}

fn decline_invitation(instance: Instance<SharedSpace>) -> Page {
    let decline = Action::custom(move |ctx: &mut Context, theme: &Theme| {
        match collaboration::decline(&ctx.air, &mut instance.clone()) {
            Ok(()) => FormComplete::Next(invitation_detail(instance.clone())).run(ctx, theme),
            Err(error) => browser::show(
                ctx,
                theme,
                browser::message("Could not decline", &error.to_string()),
            ),
        }
    });
    Page::Static(Box::new(PageType::display(
        "Decline invitation",
        vec![Display::instructions("Decline access to this shared item?")],
        None,
        Bumper::double("Decline", decline, "Cancel", Action::None),
        Offset::Center,
    )))
}

pub(crate) fn legacy_home() -> Page {
    Page::watching(
        Watch::list::<Transfer>(),
        |ctx: &mut Context, theme: &Theme, transfers: Vec<Instance<Transfer>>| {
            let me = ctx.air.me();
            let mut incoming = vec![];
            let mut outgoing = vec![];
            let mut sorted = transfers;
            sorted.sort_by_key(|instance| std::cmp::Reverse(instance.pending().created_at));
            for instance in sorted {
                let transfer = instance.pending();
                if transfer.recipient != me && transfer.sender != me {
                    continue;
                }
                // AIR authenticates the sender, but a sender can still publish
                // an invalid manifest. Do not render its unchecked metadata.
                if transfer.validate().is_err() {
                    continue;
                }
                let received = transfer.recipient == me;
                let peer = if received {
                    transfer.sender
                } else {
                    transfer.recipient
                };
                let state = status(
                    &transfer,
                    received,
                    instance
                        .confirmed()
                        .is_some_and(|confirmed| confirmed.is_ready()),
                );
                let subtitle = format!(
                    "{} {} · {}",
                    if received { "From" } else { "To" },
                    short_name(peer),
                    state
                );
                let art = if transfer.is_folder() {
                    "file-icons/folder"
                } else {
                    "file-icons/document"
                };
                let row = ListItem::avatar(
                    theme
                        .brand()
                        .images
                        .get(art)
                        .cloned()
                        .map(AvatarContent::image)
                        .unwrap_or_else(|| {
                            AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                        }),
                    transfer.title(),
                    &subtitle,
                    Some(&storage::format_size(transfer.total_size())),
                    Some(Flow::new(vec![detail(instance.clone())])),
                )
                .with_badge(received && transfer.status == TransferStatus::Pending);
                if received {
                    incoming.push(row)
                } else {
                    outgoing.push(row)
                }
            }
            PageType::display(
                "Older copies",
                vec![
                    Display::instructions(
                        "These are older copies. Changes to them are not shared.",
                    ),
                    Display::list(Some("Received"), incoming, Some("No received files yet.")),
                    Display::list(
                        Some("Sent"),
                        outgoing,
                        Some("Files and folders you send will appear here."),
                    ),
                ],
                None,
                Bumper::None,
                Offset::Start,
            )
        },
    )
}

/// Selecting a library item opens the existing recipient/confirmation flow.
/// The header only constructs this page: it never imports or shares data.
pub(crate) fn send_picker() -> Page {
    file_picker(None)
}

/// Reuse a known recipient without asking for their name again. Selecting a
/// file still opens the normal explicit read/write-access confirmation.
pub(crate) fn send_picker_for(name: Name) -> Page {
    file_picker(Some(name))
}

pub(crate) fn picker_selection(
    ctx: &mut Context,
    vault: Instance<Vault>,
    id: EntryId,
    recipient: Option<Name>,
    theme: &Theme,
) -> Flow {
    match recipient {
        Some(name) => Flow::new(vec![send_confirmation(vault, id, name)]),
        None => send_flow(ctx, vault, id, theme),
    }
}

fn file_picker(recipient: Option<Name>) -> Page {
    Page::watching(
        (Watch::list::<Vault>(), Watch::list::<SharedSpace>()),
        move |ctx: &mut Context,
              theme: &Theme,
              (vaults, spaces): (Vec<Instance<Vault>>, Vec<Instance<SharedSpace>>)| {
            let mut entries = Vec::new();
            for vault in vaults {
                let library = vault.pending();
                if library.owner != ctx.air.me() {
                    continue;
                }
                for entry in library.entries.values() {
                    if library.is_trashed(entry.id) {
                        continue;
                    }
                    let folders = library
                        .breadcrumbs(entry.parent)
                        .into_iter()
                        .map(|folder| folder.name)
                        .collect::<Vec<_>>();
                    let path = if folders.is_empty() {
                        "Files".to_owned()
                    } else {
                        format!("Files / {}", folders.join(" / "))
                    };
                    let entry = display_entry(entry, &spaces, ctx.air.me());
                    let sort_key = format!("{path} / {}", entry.name).to_lowercase();
                    entries.push((sort_key, path, entry, vault.clone()));
                }
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.id.cmp(&b.2.id)));
            let rows = entries
                .into_iter()
                .map(|(_, path, entry, vault)| {
                    let art = if entry.is_folder() {
                        "file-icons/folder"
                    } else {
                        "file-icons/document"
                    };
                    let kind = if entry.is_folder() {
                        "Folder".to_owned()
                    } else {
                        storage::format_size(entry.size())
                    };
                    ListItem::avatar(
                        theme
                            .brand()
                            .images
                            .get(art)
                            .cloned()
                            .map(AvatarContent::image)
                            .unwrap_or_else(|| {
                                AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                            }),
                        &entry.name,
                        &format!("{path} · {kind}"),
                        None,
                        Some(picker_selection(ctx, vault, entry.id, recipient, theme)),
                    )
                })
                .collect();
            let mut displays = vec![Display::instructions(
                "Choose a file or folder to share with read and write access.",
            )];
            if let Some(name) = recipient {
                displays.push(Display::label("To"));
                displays.push(Display::text(&name.to_string()));
            }
            displays.push(Display::list(
                None,
                rows,
                Some("Add a file or folder in Files, then return here to share it."),
            ));
            PageType::display("Share files", displays, None, Bumper::None, Offset::Start)
        },
    )
}

/// Resolve only presentation metadata. Sharing still targets the private mount
/// ID, which can differ from the canonical shared root after a recipient joins.
fn display_entry(
    entry: &storage::Entry,
    spaces: &[Instance<SharedSpace>],
    me: Name,
) -> storage::Entry {
    if let storage::EntryKind::Shared { space, .. } = entry.kind
        && let Some(instance) = spaces.iter().find(|instance| instance.id() == space)
    {
        let state = instance.pending();
        if state.validate().is_ok()
            && state.can_write(me)
            && state.is_ready()
            && let Some(root) = state.tree.get(state.root)
        {
            let mut displayed = root.clone();
            displayed.id = entry.id;
            displayed.parent = entry.parent;
            displayed.favorite = entry.favorite;
            return displayed;
        }
    }
    entry.clone()
}

fn status(transfer: &Transfer, received: bool, confirmed: bool) -> &'static str {
    match transfer.status {
        TransferStatus::Accepted => {
            if received {
                "Saved to Files"
            } else {
                "Saved by recipient"
            }
        }
        TransferStatus::Declined => "Declined",
        TransferStatus::Pending if !transfer.is_ready() => {
            if received {
                "Receiving"
            } else {
                "Preparing"
            }
        }
        TransferStatus::Pending if received => "Ready to save",
        TransferStatus::Pending if !confirmed => "Queued for sync",
        TransferStatus::Pending => "Awaiting recipient",
    }
}

pub fn detail(instance: Instance<Transfer>) -> Page {
    Page::watching(
        (Watch::contract(instance), Watch::list::<Vault>()),
        |ctx: &mut Context,
         _: &Theme,
         (instance, vaults): (Instance<Transfer>, Vec<Instance<Vault>>)| {
            let transfer = instance.pending();
            let me = ctx.air.me();
            if transfer.recipient != me && transfer.sender != me {
                return PageType::display(
                    "Transfer unavailable",
                    vec![Display::instructions(
                        "This transfer is addressed to another identity.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            }
            if transfer.validate().is_err() {
                return PageType::display(
                    "Invalid transfer",
                    vec![Display::instructions(
                        "This transfer has an invalid folder structure, filename, or size. Ask the sender to send a new copy from Fig Files.",
                    )],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            }
            let received = transfer.recipient == me;
            let title = transfer.title().to_owned();
            let peer = if received {
                transfer.sender
            } else {
                transfer.recipient
            };
            let state = status(
                &transfer,
                received,
                instance
                    .confirmed()
                    .is_some_and(|confirmed| confirmed.is_ready()),
            );
            let displays = vec![
                Display::image(
                    if transfer.is_folder() {
                        "file-icons/folder"
                    } else {
                        "file-icons/document"
                    },
                    (80.0, 80.0),
                ),
                Display::label(&title),
                Display::instructions(&format!(
                    "{} · {} {}",
                    storage::format_size(transfer.total_size()),
                    transfer.item_count(),
                    if transfer.item_count() == 1 {
                        "item"
                    } else {
                        "items"
                    },
                )),
                Display::instructions(&format!(
                    "{} {}",
                    if received { "From" } else { "To" },
                    short_name(peer),
                )),
                Display::label(state),
            ];
            let bumper =
                if received && transfer.status == TransferStatus::Pending && transfer.is_ready() {
                    let accepting = instance.clone();
                    Bumper::custom(
                        "Save to Files",
                        Action::custom(move |ctx: &mut Context, theme: &Theme| {
                            let mut vault = storage::open(&ctx.air);
                            match sharing::accept(&ctx.air, &mut vault, &mut accepting.clone()) {
                                Ok(id) => FormComplete::Next(saved_item(vault, id)).run(ctx, theme),
                                Err(error) => browser::show(
                                    ctx,
                                    theme,
                                    browser::message("Could not save transfer", &error.to_string()),
                                ),
                            }
                        }),
                    )
                } else if received && transfer.status == TransferStatus::Accepted {
                    let id = storage::imported_id(instance.id(), 0);
                    let vault = vaults.into_iter().find(|vault| {
                        let library = vault.pending();
                        library.owner == me && library.get(id).is_some() && !library.is_trashed(id)
                    });
                    if let Some(vault) = vault {
                        Bumper::custom(
                            "Open in Files",
                            Action::flow(Flow::new(vec![saved_item(vault, id)])),
                        )
                    } else {
                        Bumper::None
                    }
                } else {
                    Bumper::None
                };
            let info = instance.clone();
            PageType::display(
                "Older copy",
                displays,
                Some((
                    Icons::Info,
                    Box::new(move |_: &mut Context, _: &Theme| {
                        Flow::new(vec![transfer_info(info.clone())])
                    }),
                )),
                bumper,
                Offset::Start,
            )
        },
    )
}

fn saved_item(vault: Instance<Vault>, id: EntryId) -> Page {
    let is_active_folder = {
        let library = vault.pending();
        library.get(id).is_some_and(|entry| entry.is_folder()) && !library.is_trashed(id)
    };
    if is_active_folder {
        browser::listing(vault, browser::Location::Folder(Some(id)), false)
    } else {
        browser::detail(vault, id)
    }
}

fn transfer_info(instance: Instance<Transfer>) -> Page {
    Page::watching(
        Watch::contract(instance),
        |ctx: &mut Context, _: &Theme, instance: Instance<Transfer>| {
            let transfer = instance.pending();
            let received = transfer.recipient == ctx.air.me();
            if (!received && transfer.sender != ctx.air.me()) || transfer.validate().is_err() {
                return PageType::display(
                    "Transfer unavailable",
                    vec![Display::instructions("This transfer cannot be opened.")],
                    None,
                    Bumper::Done,
                    Offset::Start,
                );
            }
            let peer = if received {
                transfer.sender
            } else {
                transfer.recipient
            };
            let mut displays = vec![
                Display::label(if received { "From" } else { "To" }),
                Display::text(&peer.to_string()),
                Display::instructions("A copy from when it was sent. Later edits are not shared."),
            ];
            if received && transfer.status == TransferStatus::Pending && !transfer.is_ready() {
                displays.push(Display::instructions(
                    "Keep Fig Files open while the file contents arrive.",
                ));
            }
            let bumper = if received && transfer.status == TransferStatus::Pending {
                Bumper::custom(
                    "Decline",
                    Action::flow(Flow::new(vec![decline_confirmation(instance.clone())])),
                )
            } else {
                Bumper::custom("Copy name", Action::copy(&peer.to_string()))
            };
            PageType::display("Transfer details", displays, None, bumper, Offset::Start)
        },
    )
}

fn decline_confirmation(instance: Instance<Transfer>) -> Page {
    let action = Action::custom(move |ctx: &mut Context, theme: &Theme| {
        match sharing::decline(&ctx.air, &mut instance.clone()) {
            Ok(()) => FormComplete::Next(detail(instance.clone())).run(ctx, theme),
            Err(error) => {
                FormComplete::Next(browser::message("Could not decline", &error.to_string()))
                    .run(ctx, theme)
            }
        }
    });
    Page::Static(Box::new(PageType::display(
        "Decline transfer",
        vec![Display::instructions(
            "Decline this copy? The sender will see your response.",
        )],
        None,
        Bumper::double("Decline", action, "Cancel", Action::None),
        Offset::Center,
    )))
}

pub fn send_flow(ctx: &mut Context, vault: Instance<Vault>, id: EntryId, theme: &Theme) -> Flow {
    recipient_flow(ctx, theme, move |ctx: &mut Context, names: Vec<Name>| {
        match collaboration::share_with(&ctx.air, &mut vault.clone(), id, names) {
            Ok(space) => {
                let root = space.pending().root;
                FormComplete::Next(collaboration_ui::item(space, root))
            }
            Err(error) => {
                FormComplete::Next(browser::message("Could not share", &error.to_string()))
            }
        }
    })
}

fn recipient_flow(
    ctx: &mut Context,
    theme: &Theme,
    share: impl Fn(&mut Context, Vec<Name>) -> FormComplete + Clone + 'static,
) -> Flow {
    let items = contacts_ui::search_items(ctx);
    Flow::from_form(Form::flow(
        theme,
        vec![FormItem::search(
            "Share access",
            "contact name",
            items,
            Some(vec![FormAction::Custom(
                "New contact".into(),
                None,
                Icons::Add,
                Action::flow(Flow::from_form(contacts_ui::new_contact(theme))),
            )]),
        )],
        None,
        None,
        Box::new(move |ctx: &mut Context, states: &Vec<State>| {
            let Some(State::Search(names)) = states.first() else {
                return FormComplete::None;
            };
            if names.is_empty() {
                return FormComplete::None;
            }
            share(ctx, names.clone())
        }),
    ))
}

pub(crate) fn send_confirmation(vault: Instance<Vault>, id: EntryId, name: Name) -> Page {
    Page::watching(
        (Watch::contract(vault), Watch::list::<SharedSpace>()),
        move |ctx: &mut Context,
              _: &Theme,
              (vault, spaces): (Instance<Vault>, Vec<Instance<SharedSpace>>)| {
            let library = vault.pending();
            let Some(entry) = library
                .get(id)
                .filter(|_| library.owner == ctx.air.me() && !library.is_trashed(id))
            else {
                return PageType::display(
                    "Item unavailable",
                    vec![Display::instructions(
                        "This item was removed or moved to Trash.",
                    )],
                    None,
                    Bumper::None,
                    Offset::Start,
                );
            };
            let entry = display_entry(entry, &spaces, ctx.air.me());
            drop(library);
            let send =
                Action::custom(
                    move |ctx: &mut Context, theme: &Theme| match collaboration::share(
                        &ctx.air,
                        &mut vault.clone(),
                        id,
                        name,
                    ) {
                        Ok(space) => {
                            let root = space.pending().root;
                            FormComplete::Next(collaboration_ui::item(space, root)).run(ctx, theme);
                        }
                        Err(error) => FormComplete::Next(browser::message(
                            "Could not share",
                            &error.to_string(),
                        ))
                        .run(ctx, theme),
                    },
                );
            PageType::display(
                "Share access",
                vec![
                    Display::label(&entry.name),
                    Display::label("To"),
                    Display::text(&name.to_string()),
                    Display::label("Read & write access"),
                    Display::instructions(if entry.is_folder() {
                        "Shares the whole folder, including its files and subfolders. You will both edit the same contents."
                    } else {
                        "You will both edit the same file. Changes are shared with everyone who has access."
                    }),
                ],
                None,
                Bumper::double("Share access", send, "Cancel", Action::None),
                Offset::Start,
            )
        },
    )
}

pub fn invite_flow(ctx: &mut Context, space: Instance<SharedSpace>, theme: &Theme) -> Flow {
    recipient_flow(ctx, theme, move |ctx: &mut Context, names: Vec<Name>| {
        match collaboration::invite_many(&ctx.air, &mut space.clone(), names) {
            Ok(()) => FormComplete::Next(invitation_detail(space.clone())),
            Err(error) => FormComplete::Next(browser::message(
                "Could not share access",
                &error.to_string(),
            )),
        }
    })
}
