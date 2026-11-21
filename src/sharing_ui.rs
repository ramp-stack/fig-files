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
    Action, ActionItem, AvatarContent, AvatarIconStyle, Bumper, ContactInvitations, Context,
    ContractWatch, Display, Flow, Form, FormAction, FormComplete, FormValues, Icons, Input,
    ListItem, ListWatch, Offset, Page, PageType, Theme,
};
use prism::air::{Instance, Name};

fn short_name(name: Name) -> String {
    let full = name.to_string();
    format!("{}…{}", &full[..10], &full[full.len() - 8..])
}

fn airname_item(name: Name, role: &str) -> Display {
    let name = name.to_string();
    Display::cta(
        "AirName",
        Some(role),
        &name,
        vec![ActionItem::new_with_active(
            Action::copy(&name), "Copy", "Copied", Icons::Copy,
        )],
    )
}

pub fn receive_name(name: Name) -> Page {
    let invitations = crate::invitations::contacts();
    let mut page = PageType::qr_page(&invitations, name);
    if let PageType::Content { items, .. } = &mut page {
        for item in items {
            if let chk::Item::Display(Display::QRCode { data, .. }) = item {
                *data = invitations.contact_url(name);
            }
        }
    }
    Page::Static(Box::new(page))
}

pub fn home() -> Page {
    Page::watching(
        (
            ListWatch::<SharedSpace>::default(),
            ListWatch::<Transfer>::default(),
            ListWatch::<EditDraft>::default(),
        ),
        |ctx: &mut Context,
         theme: &Theme,
         (mut spaces, transfers, drafts): (Vec<Instance<SharedSpace>>, _, _)| {
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
                    state.to_owned()
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
                        .image(art)
                        .map(AvatarContent::image)
                        .unwrap_or_else(|| {
                            AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                        }),
                    space.title(),
                    &subtitle,
                    None,
                    Some(Action::flow(Flow::new(vec![invitation_detail(
                        instance.clone(),
                    )]))),
                    !owned && *member == MemberStatus::Invited,
                    None,
                );
                items.push(row);
            }
            let displays = vec![Display::list(
                None,
                items,
                Some("No shared files yet."),
            )];
            let mut options = Vec::new();
            if drafts.iter().any(|instance| {
                let draft = instance.pending();
                draft.owner == me && !draft.resolved && draft.validate().is_ok()
            }) {
                options.push(Display::cta(
                    "Saved edits",
                    None,
                    "Private recovery drafts",
                    vec![ActionItem::new(
                        Action::flow(Flow::new(vec![collaboration_ui::saved_edits()])),
                        "Saved edits", Icons::Edit,
                    )],
                ));
            }
            let has_older_copies = transfers.iter().any(|instance| {
                let transfer = instance.pending();
                (transfer.sender == me || transfer.recipient == me) && transfer.validate().is_ok()
            });
            if has_older_copies {
                options.push(Display::cta(
                    "Older copies",
                    None,
                    "Copies sent before shared access",
                    vec![ActionItem::new(
                        Action::flow(Flow::new(vec![legacy_home()])),
                        "Older copies", Icons::Copy,
                    )],
                ));
            }
            let header = (!options.is_empty()).then(|| {
                let options = Page::Static(Box::new(PageType::display(
                    "Shared options",
                    options,
                    None,
                    Bumper::None,
                    Offset::Start,
                )));
                (Icons::Info, Action::flow(Flow::new(vec![options])))
            });
            PageType::root(
                "Shared",
                vec![],
                displays,
                header,
                Some(("Send".into(), Action::flow(Flow::new(vec![send_picker()])))),
                Some((
                    "Receive".into(),
                    Action::flow(crate::invitations::receive_flow(me)),
                )),
            )
        },
    )
}

pub fn invitation_detail(instance: Instance<SharedSpace>) -> Page {
    Page::watching(
        ContractWatch::new(instance),
        |ctx: &mut Context, _: &Theme, instance: Instance<SharedSpace>| {
            let space = instance.pending();
            let me = ctx.air.me();
            if space.validate().is_err() || !space.members.contains_key(&me) {
                return PageType::display(
                    "Shared item unavailable",
                    vec![Display::instructions(
                        "This shared item cannot be opened with your AirName.",
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
                    80.0,
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
                    Action::flow(Flow::new(vec![invitation_info(info)])),
                )),
                bumper,
                Offset::Start,
            )
        },
    )
}

fn invitation_info(instance: Instance<SharedSpace>) -> Page {
    Page::watching(
        (ContractWatch::new(instance), ListWatch::<Vault>::default()),
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
                Bumper::None
            };
            let mut displays = vec![
                airname_item(space.owner, "Shared by"),
                Display::instructions(
                    "Read and write access applies to the entire shared item. Changes are synchronized while Fig Files is online.",
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
                displays.push(Display::cta(
                    "Files shortcut",
                    Some(&entry.name),
                    "",
                    vec![ActionItem::new(
                        Action::flow(Flow::new(vec![browser::shortcut_page(vault, entry.id)])),
                        "Files shortcut", Icons::Home,
                    )],
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
        ListWatch::<Transfer>::default(),
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
                        .image(art)
                        .map(AvatarContent::image)
                        .unwrap_or_else(|| {
                            AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                        }),
                    transfer.title(),
                    &subtitle,
                    Some(&storage::format_size(transfer.total_size())),
                    Some(Action::flow(Flow::new(vec![detail(instance.clone())]))),
                    received && transfer.status == TransferStatus::Pending,
                    None,
                );
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
        (
            ListWatch::<Vault>::default(),
            ListWatch::<SharedSpace>::default(),
        ),
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
                            .image(art)
                            .map(AvatarContent::image)
                            .unwrap_or_else(|| {
                                AvatarContent::icon(Icons::Send, AvatarIconStyle::Brand)
                            }),
                        &entry.name,
                        &format!("{path} · {kind}"),
                        None,
                        Some(Action::flow(picker_selection(
                            ctx, vault, entry.id, recipient, theme,
                        ))),
                        false,
                        None,
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
        (ContractWatch::new(instance), ListWatch::<Vault>::default()),
        |ctx: &mut Context,
         _: &Theme,
         (instance, vaults): (Instance<Transfer>, Vec<Instance<Vault>>)| {
            let transfer = instance.pending();
            let me = ctx.air.me();
            if transfer.recipient != me && transfer.sender != me {
                return PageType::display(
                    "Transfer unavailable",
                    vec![Display::instructions(
                        "This transfer is addressed to another AirName.",
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
                    80.0,
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
                    Action::flow(Flow::new(vec![transfer_info(info)])),
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
        ContractWatch::new(instance),
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
                airname_item(peer, if received { "From" } else { "To" }),
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
                Bumper::None
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
    let link = crate::invitations::LinkSource::Private(vault.clone(), id);
    recipient_flow(
        ctx,
        theme,
        link,
        move |ctx: &mut Context, names: Vec<Name>| match collaboration::share_with(
            &ctx.air,
            &mut vault.clone(),
            id,
            names,
        ) {
            Ok(space) => {
                let root = space.pending().root;
                FormComplete::Next(collaboration_ui::item(space, root))
            }
            Err(error) => {
                FormComplete::Next(browser::message("Could not share", &error.to_string()))
            }
        },
    )
}

fn recipient_flow(
    ctx: &mut Context,
    theme: &Theme,
    link: crate::invitations::LinkSource,
    share: impl Fn(&mut Context, Vec<Name>) -> FormComplete + Clone + 'static,
) -> Flow {
    contacts_ui::sync_profiles(ctx);
    let mut form = Form::new_message(&crate::invitations::contacts(), ctx, theme);
    let Form::Flow {
        inputs, on_submit, ..
    } = &mut form
    else {
        unreachable!("CHK's new-message form is a flow");
    };
    let me = ctx.air.me().to_string();
    let mut has_contacts = false;
    for input in inputs.iter_mut().filter(|input| input.id == "recipients") {
        input.title = "Share access".into();
        if let Input::Selection(selector) = &mut input.input {
            selector.items.retain(|(_, name)| name != &me);
            has_contacts = !selector.items.is_empty();
            selector.actions.push(FormAction::Custom(ActionItem::new(
                Action::flow(Flow::new(vec![crate::invitations::create_page(
                    link.clone(),
                )])),
                "Create link",
                Icons::Link,
            )));
        }
    }
    if !has_contacts {
        return empty_recipient_flow(theme, link, share);
    }
    // Keep CHK's selector and New contact flow intact. Only the final submit
    // changes from creating a conversation to sharing this file or folder.
    *on_submit = Box::new(move |ctx: &mut Context, values: &FormValues| {
        let Some(keys) = values.selection("recipients") else {
            return FormComplete::None;
        };
        let names = match ContactInvitations::selected_recipients(keys) {
            Ok(names) => names,
            Err(error) => return FormComplete::Error(error.into()),
        };
        if names
            .iter()
            .any(|name| chk::blocking::Block::am_i_blocked(ctx, *name))
        {
            return FormComplete::Error("A selected user has blocked you.".into());
        }
        let names = names
            .into_iter()
            .map(|name| crate::contacts::parse(&name.to_string(), ctx.air.me()))
            .collect::<Result<Vec<_>, _>>();
        match names {
            Ok(names) => share(ctx, names),
            Err(error) => FormComplete::Error(error.to_string()),
        }
    });
    Flow::from_form(form)
}

/// CHK's empty selector has fixed status text. Show its real entry actions
/// instead, then use the unchanged selector once there is a contact to choose.
fn empty_recipient_flow(
    theme: &Theme,
    link: crate::invitations::LinkSource,
    share: impl Fn(&mut Context, Vec<Name>) -> FormComplete + Clone + 'static,
) -> Flow {
    let mut contact_form = Form::new_contact(&crate::invitations::contacts());
    if let Form::Flow { on_submit, .. } = &mut contact_form {
        let mut save_contact = on_submit.clone();
        let theme = theme.clone();
        let link = link.clone();
        *on_submit = Box::new(move |ctx: &mut Context, values: &FormValues| {
            match save_contact(ctx, values) {
                FormComplete::Next(_) => {
                    // The CHK recipient form has one selection page. Restart
                    // the contact flow there without selecting or sharing yet.
                    let page = recipient_flow(ctx, &theme, link.clone(), share.clone())
                        .0
                        .into_iter()
                        .next()
                        .expect("The recipient flow has a page");
                    FormComplete::Next(page)
                }
                result => result,
            }
        });
    }
    Flow::new(vec![Page::Static(Box::new(PageType::display(
        "Share access",
        vec![
            Display::cta(
                "New contact",
                None,
                "",
                vec![ActionItem::new(
                    Action::flow(Flow::from_form(contact_form)),
                    "New contact",
                    Icons::Add,
                )],
            ),
            Display::cta(
                "Create link",
                None,
                "",
                vec![ActionItem::new(
                    Action::flow(Flow::new(vec![crate::invitations::create_page(link)])),
                    "Create link",
                    Icons::Link,
                )],
            ),
        ],
        None,
        Bumper::None,
        Offset::Start,
    )))])
}

pub(crate) fn send_confirmation(vault: Instance<Vault>, id: EntryId, name: Name) -> Page {
    Page::watching(
        (
            ContractWatch::new(vault),
            ListWatch::<SharedSpace>::default(),
        ),
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
    let link = crate::invitations::LinkSource::Shared(space.clone());
    recipient_flow(
        ctx,
        theme,
        link,
        move |ctx: &mut Context, names: Vec<Name>| match collaboration::invite_many(
            &ctx.air,
            &mut space.clone(),
            names,
        ) {
            Ok(()) => FormComplete::Next(invitation_detail(space.clone())),
            Err(error) => FormComplete::Next(browser::message(
                "Could not share access",
                &error.to_string(),
            )),
        },
    )
}
