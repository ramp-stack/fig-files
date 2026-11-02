//! Private saved contacts and sharing history, presented with public CHK pages.
use crate::{
    browser,
    collaboration::{MemberStatus, SharedSpace},
    contacts::{self, AddressBook},
    sharing::{Transfer, TransferStatus},
    sharing_ui,
};
use chk::{
    Action, AvatarContent, AvatarIconStyle, Bumper, Context, Display, Flow, Form, FormAction,
    FormComplete, FormItem, FormValidState, Icons, ListItem, Offset, Page, PageType, State, Theme,
    Watch,
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

fn short_name(name: Name) -> String {
    let name = name.to_string();
    format!("{}…{}", &name[..10], &name[name.len() - 8..])
}

fn item_count(count: usize) -> String {
    if count == 0 {
        return "No shared files yet".into();
    }
    format!(
        "{count} shared {}",
        if count == 1 { "item" } else { "items" }
    )
}

/// Snapshot the roster when opening Orange's CHK search form. Watching updates
/// here would rebuild the form and discard its selected contacts.
pub(crate) fn search_items(ctx: &Context) -> Vec<(ListItem, Name)> {
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
    roster(ctx.air.me(), &books, &spaces, &transfers)
        .into_iter()
        .map(|contact| {
            (
                ListItem::avatar(
                    AvatarContent::icon(Icons::Profile, AvatarIconStyle::Brand),
                    &short_name(contact.name),
                    &contact.name.to_string(),
                    None,
                    None,
                ),
                contact.name,
            )
        })
        .collect()
}

/// Match Orange's nested New contact form: save, return to the existing search
/// page, and select the new name without losing contacts already selected.
pub(crate) fn new_contact(theme: &Theme) -> Form {
    Form::flow(
        theme,
        vec![FormItem::text(
            "New contact",
            "Fig Files receiving name",
            Some(vec![
                FormAction::Paste,
                FormAction::scan_qr_code("Scan a Fig Files receiving name"),
            ]),
            |ctx: &mut Context, text: String| {
                if text.is_empty() {
                    FormValidState::Invalid
                } else {
                    match contacts::parse(&text, ctx.air.me()) {
                        Ok(_) => FormValidState::Valid,
                        Err(error) => FormValidState::InvalidWithData(error.to_string()),
                    }
                }
            },
        )],
        None,
        None,
        Box::new(|ctx: &mut Context, states: &Vec<State>| {
            let Some(State::Text(text)) = states.first() else {
                return FormComplete::None;
            };
            let Ok(name) = contacts::parse(text, ctx.air.me()) else {
                return FormComplete::None;
            };
            match contacts::add(&ctx.air, name) {
                Ok(name) => {
                    FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
                        (Action::choose_search(name).get())(ctx, theme)
                    }))
                }
                Err(error) => FormComplete::Next(browser::message(
                    "Could not add contact",
                    &error.to_string(),
                )),
            }
        }),
    )
}

pub(crate) fn home() -> Page {
    Page::watching(
        (
            Watch::list::<AddressBook>(),
            Watch::list::<SharedSpace>(),
            Watch::list::<Transfer>(),
        ),
        |ctx: &mut Context,
         _: &Theme,
         (books, spaces, transfers): (
            Vec<Instance<AddressBook>>,
            Vec<Instance<SharedSpace>>,
            Vec<Instance<Transfer>>,
        )| {
            let rows = roster(ctx.air.me(), &books, &spaces, &transfers)
                .into_iter()
                .map(|contact| {
                    ListItem::avatar(
                        AvatarContent::icon(Icons::Profile, AvatarIconStyle::Brand),
                        &short_name(contact.name),
                        &item_count(contact.history.len()),
                        None,
                        Some(Flow::new(vec![detail(contact.name)])),
                    )
                })
                .collect();
            PageType::root(
                "Contacts",
                vec![],
                vec![Display::list(
                    None,
                    rows,
                    Some(
                        "Add someone by their Fig Files receiving name. People you share with also appear here.",
                    ),
                )],
                None,
                Some((
                    "Add contact".into(),
                    Box::new(|_: &mut Context, theme: &Theme| add_flow(theme)),
                )),
                None,
            )
        },
    )
}

pub(crate) fn detail(name: Name) -> Page {
    Page::watching(
        (
            Watch::list::<AddressBook>(),
            Watch::list::<SharedSpace>(),
            Watch::list::<Transfer>(),
        ),
        move |ctx: &mut Context,
              _: &Theme,
              (books, spaces, transfers): (
            Vec<Instance<AddressBook>>,
            Vec<Instance<SharedSpace>>,
            Vec<Instance<Transfer>>,
        )| {
            let Some(contact) = roster(ctx.air.me(), &books, &spaces, &transfers)
                .into_iter()
                .find(|contact| contact.name == name)
            else {
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
            let rows = contact
                .history
                .iter()
                .map(|history| {
                    let flow = match history.source {
                        HistorySource::Shared => spaces
                            .iter()
                            .find(|space| space.id() == history.id)
                            .map(|space| {
                                Flow::new(vec![sharing_ui::invitation_detail(space.clone())])
                            }),
                        HistorySource::Legacy => transfers
                            .iter()
                            .find(|transfer| transfer.id() == history.id)
                            .map(|transfer| Flow::new(vec![sharing_ui::detail(transfer.clone())])),
                    };
                    ListItem::plain(&history.title, &history.description, None, flow)
                })
                .collect();
            let full = name.to_string();
            PageType::display(
                "Contact",
                vec![
                    Display::icon(Icons::Profile),
                    Display::label("Fig Files receiving name"),
                    Display::text(&full),
                    Display::instructions(&item_count(contact.history.len())),
                    Display::list(
                        Some("Sharing history"),
                        rows,
                        Some("Share a file or folder with this contact."),
                    ),
                ],
                None,
                Bumper::double(
                    "Share files",
                    Action::flow(Flow::new(vec![sharing_ui::send_picker_for(name)])),
                    "Copy name",
                    Action::copy(&full),
                ),
                Offset::Start,
            )
        },
    )
}

pub(crate) fn add_flow(theme: &Theme) -> Flow {
    Flow::from_form(Form::flow(
        theme,
        vec![FormItem::text(
            "Add contact",
            "Fig Files receiving name",
            Some(vec![
                FormAction::Paste,
                FormAction::scan_qr_code("Scan a Fig Files receiving name"),
            ]),
            |ctx: &mut Context, text: String| match contacts::parse(&text, ctx.air.me()) {
                Ok(_) => FormValidState::Valid,
                Err(error) => FormValidState::InvalidWithData(error.to_string()),
            },
        )],
        None,
        None,
        Box::new(|ctx: &mut Context, states: &Vec<State>| {
            let Some(State::Text(text)) = states.first() else {
                return FormComplete::None;
            };
            match contacts::parse(text, ctx.air.me()).and_then(|name| contacts::add(&ctx.air, name))
            {
                Ok(name) => FormComplete::Next(detail(name)),
                Err(error) => FormComplete::Next(browser::message(
                    "Could not add contact",
                    &error.to_string(),
                )),
            }
        }),
    ))
}
