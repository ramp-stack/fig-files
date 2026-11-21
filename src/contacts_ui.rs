//! CHK contacts and profiles, with private legacy migration and file actions.
use crate::{
    collaboration::{MemberStatus, SharedSpace},
    contacts::AddressBook,
    sharing::{Transfer, TransferStatus},
    sharing_ui,
};
use chk::{
    Action, ActionItem, Bumper, Context, Display, Flow, Form, FormComplete, FormValues, Icons,
    Item, ListItem, ListWatch, Offset, Page, PageType, Root, Theme,
    design_system::{Footer, Header},
    profiles::Profile,
};
use prism::air::{Id, Instance, Name};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HistorySource {
    Shared,
    Legacy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContactHistory {
    pub id: Id,
    pub source: HistorySource,
    pub title: String,
    pub last_interaction: u64,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Contact {
    pub name: Name,
    pub last_interaction: u64,
    pub history: Vec<ContactHistory>,
}

/// Adds validated metadata one instance at a time. Callers can release each AIR
/// reference immediately; file contents are never cloned into the contact index.
pub(crate) struct ContactIndex {
    me: Name,
    saved: BTreeMap<Name, u64>,
    histories: BTreeMap<Name, BTreeMap<(HistorySource, Id), ContactHistory>>,
}

impl ContactIndex {
    pub(crate) fn new(me: Name) -> Self {
        Self {
            me,
            saved: BTreeMap::new(),
            histories: BTreeMap::new(),
        }
    }

    pub(crate) fn add_saved(&mut self, book: &AddressBook) {
        if book.owner != self.me || book.validate().is_err() {
            return;
        }
        for (&name, &timestamp) in &book.contacts {
            if name == self.me {
                continue;
            }
            self.saved
                .entry(name)
                .and_modify(|time| *time = (*time).max(timestamp))
                .or_insert(timestamp);
            self.histories.entry(name).or_default();
        }
    }

    pub(crate) fn add_shared(&mut self, id: Id, space: &SharedSpace) {
        if space.validate().is_err() {
            return;
        }
        let owned = space.owner == self.me;
        if !owned && space.members.get(&self.me) != Some(&MemberStatus::Active) {
            return;
        }
        // Membership events have no separate timestamp in the existing wire
        // format. Use the latest recorded activity in the shared item.
        let last_interaction = space
            .tree
            .entries
            .values()
            .map(|entry| entry.modified_at)
            .max()
            .unwrap_or(space.created_at)
            .max(space.created_at);
        for (&name, &status) in &space.members {
            if !owned && name != space.owner && status != MemberStatus::Active {
                continue;
            }
            let description = if owned {
                match status {
                    MemberStatus::Invited => "Shared by you · Invited",
                    MemberStatus::Active => "Shared by you · Joined",
                    MemberStatus::Declined => "Shared by you · Invitation declined",
                }
            } else if name == space.owner {
                "Shared with you · Joined"
            } else {
                "Shared together · Joined"
            };
            self.add(
                name,
                ContactHistory {
                    id,
                    source: HistorySource::Shared,
                    title: space.title().to_owned(),
                    last_interaction,
                    description: description.into(),
                },
            );
        }
    }

    pub(crate) fn add_transfer(&mut self, id: Id, transfer: &Transfer) {
        if transfer.validate().is_err() {
            return;
        }
        let (name, description) = if transfer.sender == self.me {
            (
                transfer.recipient,
                match transfer.status {
                    TransferStatus::Pending => "Older copy · Sent",
                    TransferStatus::Accepted => "Older copy · Saved by recipient",
                    TransferStatus::Declined => "Older copy · Declined",
                },
            )
        } else if transfer.recipient == self.me && transfer.status == TransferStatus::Accepted {
            (transfer.sender, "Older copy · Received")
        } else {
            return;
        };
        self.add(
            name,
            ContactHistory {
                id,
                source: HistorySource::Legacy,
                title: transfer.title().to_owned(),
                last_interaction: transfer.created_at,
                description: description.into(),
            },
        );
    }

    fn add(&mut self, name: Name, history: ContactHistory) {
        if name == self.me {
            return;
        }
        let histories = self.histories.entry(name).or_default();
        let key = (history.source, history.id);
        if histories
            .get(&key)
            .is_none_or(|previous| previous.last_interaction <= history.last_interaction)
        {
            histories.insert(key, history);
        }
    }

    pub(crate) fn finish(self) -> Vec<Contact> {
        let saved = self.saved;
        let mut contacts: Vec<_> = self
            .histories
            .into_iter()
            .map(|(name, history)| {
                let mut history: Vec<_> = history.into_values().collect();
                history.sort_by(|a, b| {
                    b.last_interaction
                        .cmp(&a.last_interaction)
                        .then_with(|| a.source.cmp(&b.source))
                        .then_with(|| a.id.cmp(&b.id))
                });
                Contact {
                    name,
                    last_interaction: history
                        .first()
                        .map(|item| item.last_interaction)
                        .unwrap_or_default()
                        .max(saved.get(&name).copied().unwrap_or_default()),
                    history,
                }
            })
            .collect();
        contacts.sort_by(|a, b| {
            b.last_interaction
                .cmp(&a.last_interaction)
                .then_with(|| a.name.cmp(&b.name))
        });
        contacts
    }
}

fn roster(
    me: Name,
    books: &[Instance<AddressBook>],
    spaces: &[Instance<SharedSpace>],
    transfers: &[Instance<Transfer>],
) -> Vec<Contact> {
    let mut index = ContactIndex::new(me);
    for instance in books {
        index.add_saved(&instance.pending());
    }
    for instance in spaces {
        index.add_shared(instance.id(), &instance.pending());
    }
    for instance in transfers {
        index.add_transfer(instance.id(), &instance.pending());
    }
    index.finish()
}

/// Import saved AirNames and accepted sharing history into CHK's private profile
/// store. Existing aliases, notes and avatars always win; no channel is shared.
pub(crate) fn sync_profiles(ctx: &mut Context) {
    let books = ctx
        .air
        .instances::<AddressBook>()
        .list()
        .into_values()
        .collect::<Vec<_>>();
    let spaces = ctx
        .air
        .instances::<SharedSpace>()
        .list()
        .into_values()
        .collect::<Vec<_>>();
    let transfers = ctx
        .air
        .instances::<Transfer>()
        .list()
        .into_values()
        .collect::<Vec<_>>();
    for contact in roster(ctx.air.me(), &books, &spaces, &transfers) {
        Profile::from_name(ctx, contact.name);
    }
}

/// Keep CHK's Contacts home, rows, ordering and empty-state benefits. The only
/// app-specific substitutions are destinations for file sharing and lookup.
pub(crate) fn home() -> Page {
    let mut standard = Root::contacts(&crate::invitations::contacts())
        .0
        .builder()
        .unwrap();
    Page::watching(
        (
            ListWatch::<AddressBook>::default(),
            ListWatch::<SharedSpace>::default(),
            ListWatch::<Transfer>::default(),
            ListWatch::<Profile>::default(),
        ),
        move |ctx: &mut Context, theme: &Theme, _| {
            sync_profiles(ctx);
            let mut page = standard.build(ctx, theme);
            if let PageType::Content {
                items,
                header,
                bumper,
                ..
            } = &mut page
            {
                for item in items {
                    if let Item::Display(Display::List { items: rows, .. }) = item {
                        for row in rows {
                            if let Some(Action::Flow { flow }) = &mut row.action {
                                flow.0 = flow.0.iter().cloned().map(profile_page).collect();
                            }
                        }
                    }
                }
                if let Header::Home { action, .. } = header {
                    *action = Some((Icons::Search, Action::deferred_flow(search_flow).get()));
                }
                if let Bumper::Footer(Footer::Home {
                    first: Some((_, action)),
                    ..
                }) = bumper
                {
                    *action =
                        Action::deferred_flow(|_: &mut Context, theme: &Theme| add_flow(theme))
                            .get();
                }
            }
            page
        },
    )
}

/// The shared CHK editor remains responsible for avatar, username and biography.
/// Adapt its profile view only where a file manager needs a different action.
fn profile_page(standard: Page) -> Page {
    Page::watching(
        (
            ListWatch::<Profile>::default(),
            ListWatch::<SharedSpace>::default(),
            ListWatch::<Transfer>::default(),
        ),
        move |ctx: &mut Context,
              theme: &Theme,
              (_, spaces, transfers): (Vec<Instance<Profile>>, _, _)| {
            let mut page = match &standard {
                Page::Static(page) => *page.clone(),
                Page::Refreshing(builder) => builder.clone().build(ctx, theme),
            };
            adapt_profile(&mut page, ctx, &spaces, &transfers);
            page
        },
    )
}

fn adapt_profile(
    page: &mut PageType,
    ctx: &Context,
    spaces: &[Instance<SharedSpace>],
    transfers: &[Instance<Transfer>],
) {
    adapt_airname_qr(page);
    let PageType::Content { items, .. } = page else {
        return;
    };
    // The stock profile descriptor exposes its full identity in the AirName
    // item, independently of the user-editable username and biography.
    let Some(name) = items.iter().find_map(|item| match item {
        Item::Display(Display::Cta {
            label,
            instructions,
            ..
        }) if label == "AirName" => instructions.parse::<Name>().ok(),
        _ => None,
    }) else {
        return;
    };
    for item in items.iter_mut() {
        if matches!(item, Item::Display(Display::Actions { .. })) {
            *item = Item::Display(Display::cta(
                "Files",
                None,
                "",
                vec![ActionItem::new(
                    Action::flow(Flow::new(vec![sharing_ui::send_picker_for(name)])),
                    "Share files",
                    Icons::Send,
                )],
            ));
        }
    }
    let history = roster(ctx.air.me(), &[], spaces, transfers)
        .into_iter()
        .find(|contact| contact.name == name)
        .map(|contact| contact.history)
        .unwrap_or_default();
    let rows = history
        .into_iter()
        .map(|history| {
            let page = match history.source {
                HistorySource::Shared => spaces
                    .iter()
                    .find(|space| space.id() == history.id)
                    .map(|space| sharing_ui::invitation_detail(space.clone())),
                HistorySource::Legacy => transfers
                    .iter()
                    .find(|transfer| transfer.id() == history.id)
                    .map(|transfer| sharing_ui::detail(transfer.clone())),
            };
            ListItem::plain(
                &history.title,
                &history.description,
                None,
                page.map(|page| Action::flow(Flow::new(vec![page]))),
                false,
                None,
            )
        })
        .collect();
    items.push(Item::Display(Display::list(
        Some("Sharing history"),
        rows,
        Some("No shared files yet."),
    )));
}

/// Preserve CHK's own-profile editor and its actions, substituting only the QR
/// destination so cameras can open the same invitation link as the Share action.
pub(crate) fn profile_root(ctx: &mut Context) -> Root {
    let mut root = Root::profile(ctx, &crate::invitations::contacts());
    if let Page::Static(page) = &mut root.0 {
        adapt_airname_qr(page);
    }
    root
}

fn adapt_airname_qr(page: &mut PageType) {
    let replace = |display: &mut Display| {
        let Display::Cta {
            label,
            instructions,
            actions,
            ..
        } = display
        else {
            return;
        };
        if label != "AirName" {
            return;
        }
        let Ok(name) = instructions.parse::<Name>() else {
            return;
        };
        for action in actions {
            if action.presentation().icon == Icons::QrCode {
                *action = ActionItem::new(
                    Action::flow(Flow::new(vec![sharing_ui::receive_name(name)])),
                    "Display QR Code",
                    Icons::QrCode,
                );
            }
        }
    };
    match page {
        PageType::Edit { display, .. } => display.iter_mut().for_each(replace),
        PageType::Content { items, .. } => {
            for item in items {
                if let Item::Display(display) = item {
                    replace(display);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn detail(name: Name) -> Page {
    Page::watching(
        (
            ListWatch::<Profile>::default(),
            ListWatch::<AddressBook>::default(),
            ListWatch::<SharedSpace>::default(),
            ListWatch::<Transfer>::default(),
        ),
        move |ctx: &mut Context,
              _: &Theme,
              (_, _, spaces, transfers): (
            Vec<Instance<Profile>>,
            Vec<Instance<AddressBook>>,
            _,
            _,
        )| {
            sync_profiles(ctx);
            let Some(profile) = Profile::try_from_name(ctx, name) else {
                return PageType::display(
                    "Contact unavailable",
                    vec![Display::instructions(
                        "This contact is not saved and has no sharing history.",
                    )],
                    None,
                    Bumper::None,
                    Offset::Start,
                );
            };
            if name == ctx.air.me() {
                let mut page = Profile::editing(true, profile, ctx.window.is_mobile, None);
                adapt_airname_qr(&mut page);
                return page;
            }
            let mut page = Profile::profile_view(&crate::invitations::contacts(), ctx, profile);
            adapt_profile(&mut page, ctx, &spaces, &transfers);
            page
        },
    )
}

pub(crate) fn add_flow(_: &Theme) -> Flow {
    let mut form = Form::new_contact(&crate::invitations::contacts());
    if let Form::Flow { on_submit, .. } = &mut form {
        let mut standard = on_submit.clone();
        *on_submit =
            Box::new(
                move |ctx: &mut Context, values: &FormValues| match standard(ctx, values) {
                    FormComplete::Next(page) => FormComplete::Next(profile_page(page)),
                    completion => completion,
                },
            );
    }
    Flow::from_form(form)
}

/// Reuse CHK's contact rows and live-search layout, keeping file actions on profiles.
fn search_flow(ctx: &mut Context, _: &Theme) -> Flow {
    sync_profiles(ctx);
    Flow::new(vec![crate::search::page(
        "Contacts",
        ListWatch::<Profile>::default(),
        |ctx, _, profiles, query| {
            let query = query.to_lowercase();
            let terms = query.split_whitespace().collect::<Vec<_>>();
            let mut rows =
                chk::SearchSource::contact_items(&crate::invitations::contacts(), ctx, profiles);
            rows.retain(|row| {
                let text = format!("{} {}", row.title, row.subtitle).to_lowercase();
                terms.iter().all(|term| text.contains(term))
            });
            for row in &mut rows {
                if let Some(Action::Flow { flow }) = &mut row.action {
                    flow.0 = flow.0.iter().cloned().map(profile_page).collect();
                }
            }
            let empty = if terms.is_empty() {
                "No contacts yet."
            } else {
                "No matching contacts."
            };
            vec![Display::list(None, rows, Some(empty))]
        },
    )])
}
