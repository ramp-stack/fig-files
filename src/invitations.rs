//! Fig Files links use CHK routing and authenticated invitation delivery.
use crate::{
    browser,
    collaboration::{self, SharedSpace},
    collaboration_ui, contacts_ui,
    invitation_delivery::{self, LinkJob, Purpose, Status},
    storage::{EntryId, Vault},
};
use chk::{
    Action, ActionItem, Bumper, ContactInvitations, Context, ContractWatch, Display, Flow, Form,
    FormAction, FormComplete, FormItem, FormValidState, FormValues, Icons, Invitations, ListWatch,
    Offset, Page, PageType, Theme,
};
use prism::air::{Id, Instance, Name};
use serde::{Deserialize, Serialize};
use std::fmt;

const BASE: &str = "https://therampstack.com/fig-files/invite.html";
pub(crate) const LINK_ACCESS: &str =
    "Anyone with this link can read and edit this file or folder, and share the link with others.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invitation {
    Contact(Name),
    File(FileInvitation),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileInvitation {
    pub publisher: Name,
    pub space: Id,
    pub token: Id,
}
impl fmt::Debug for FileInvitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileInvitation")
            .field("publisher", &self.publisher)
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}
impl Invitation {
    pub(crate) fn url(self) -> String {
        match self {
            Self::Contact(name) => format!("{BASE}#contact={name}"),
            Self::File(file) => format!(
                "{BASE}#file={}.{}.{}",
                file.publisher, file.space, file.token
            ),
        }
    }
    pub(crate) fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if let Some(name) = input.strip_prefix("fig-files://contact/") {
            return canonical_name(name).map(Self::Contact);
        }
        if let Some(value) = input.strip_prefix("fig-files://file/") {
            return parse_file(value, '/').map(Self::File);
        }
        let fragment = [BASE, "https://www.therampstack.com/fig-files/invite.html"]
            .into_iter()
            .find_map(|base| input.strip_prefix(base)?.strip_prefix('#'))?;
        if let Some(name) = fragment.strip_prefix("contact=") {
            canonical_name(name).map(Self::Contact)
        } else {
            parse_file(fragment.strip_prefix("file=")?, '.').map(Self::File)
        }
    }
}
fn lowercase_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn canonical_name(value: &str) -> Option<Name> {
    if !lowercase_hex(value, 66) {
        return None;
    }
    let name: Name = value.parse().ok()?;
    (name.to_string() == value).then_some(name)
}
fn parse_file(value: &str, separator: char) -> Option<FileInvitation> {
    let mut parts = value.split(separator);
    let publisher = canonical_name(parts.next()?)?;
    let space = parts.next()?;
    let token = parts.next()?;
    if parts.next().is_some() || !lowercase_hex(space, 64) || !lowercase_hex(token, 64) {
        return None;
    }
    Some(FileInvitation {
        publisher,
        space: space.parse().ok()?,
        token: token.parse().ok()?,
    })
}
pub(crate) fn router() -> Invitations<Invitation> {
    Invitations::new(Invitation::url, Invitation::parse, open)
}
pub(crate) fn contacts() -> ContactInvitations {
    ContactInvitations::new(Invitations::new(
        |name| Invitation::Contact(name).url(),
        |input| match Invitation::parse(input)? {
            Invitation::Contact(name) => Some(name),
            Invitation::File(_) => None,
        },
        |ctx, name| open(ctx, Invitation::Contact(name)),
    ))
}
pub(crate) fn open(ctx: &mut Context, invitation: Invitation) -> Action {
    let page = match invitation {
        Invitation::Contact(name) => {
            if chk::blocking::Block::am_i_blocked(ctx, name) {
                return Action::flow(Flow::new(vec![browser::message(
                    "Contact unavailable",
                    "This user blocked you. You cannot add them as a contact.",
                )]));
            }
            chk::profiles::Profile::from_name(ctx, name);
            contacts_ui::detail(name)
        }
        Invitation::File(file) => accept_page(file),
    };
    Action::flow(Flow::new(vec![page]))
}
pub(crate) fn receive_flow(name: Name) -> Flow {
    Flow::from_form(Form::flow(
        vec![FormItem::text(
            "invitation",
            uuid::Uuid::new_v4(),
            "Receive",
            "Invitation link",
            Some(vec![
                FormAction::Paste,
                FormAction::ScanQRCode("Scan a Fig Files invitation".into()),
                FormAction::Custom(ActionItem::new(
                    Action::flow(Flow::new(vec![crate::sharing_ui::receive_name(name)])),
                    "Show my QR code",
                    Icons::QrCode,
                )),
            ]),
            |_: &mut Context, value: String| {
                if value.trim().is_empty() {
                    FormValidState::Invalid
                } else if Invitation::parse(&value).is_some() {
                    FormValidState::Valid
                } else {
                    FormValidState::InvalidWithData(
                        "Enter a valid Fig Files invitation link.".into(),
                    )
                }
            },
        )],
        None,
        None,
        Box::new(|ctx: &mut Context, values: &FormValues| {
            match values.text("invitation").and_then(Invitation::parse) {
                Some(invitation) => FormComplete::Action(open(ctx, invitation)),
                None => FormComplete::Error("Enter a valid Fig Files invitation link.".into()),
            }
        }),
    ))
}

#[derive(Clone, Debug)]
pub(crate) enum LinkSource {
    Private(Instance<Vault>, EntryId),
    Shared(Instance<SharedSpace>),
}
/// Rendering this confirmation never publishes or promotes the item.
pub(crate) fn create_page(source: LinkSource) -> Page {
    let (title, folder) = match &source {
        LinkSource::Private(vault, id) => {
            let snapshot = vault.pending();
            let Some(entry) = snapshot.get(*id) else {
                return browser::message("Item unavailable", "This item was removed.");
            };
            (entry.name.clone(), entry.is_folder())
        }
        LinkSource::Shared(space) => {
            let state = space.pending();
            (state.title().to_owned(), state.is_folder())
        }
    };
    Page::Static(Box::new(PageType::display(
        "Create invitation link",
        vec![
            Display::label(&title),
            Display::label("Anyone with the link · Can edit"),
            Display::instructions(if folder {
                "Anyone with this link can read and edit the entire folder, including its files and subfolders, and share the link with others."
            } else {
                LINK_ACCESS
            }),
            Display::instructions(
                "This link does not expire. Access already given cannot be taken back.",
            ),
        ],
        None,
        Bumper::custom(
            "Create link",
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                let result = match &source {
                    LinkSource::Private(vault, id) => {
                        collaboration::prepare_link(&ctx.air, &mut vault.clone(), *id)
                    }
                    LinkSource::Shared(space) => {
                        collaboration::enable_link(&ctx.air, &mut space.clone())
                            .map(|token| (space.clone(), token))
                    }
                };
                let page = match result {
                    Ok((space, token)) => job_page(invitation_delivery::request(
                        &ctx.air,
                        FileInvitation {
                            publisher: ctx.air.me(),
                            space: space.id(),
                            token,
                        },
                        Purpose::Publish,
                    )),
                    Err(error) => browser::message("Could not create link", &error.to_string()),
                };
                FormComplete::Next(page).run(ctx, theme);
            }),
        ),
        Offset::Start,
    )))
}
pub(crate) fn accept_page(file: FileInvitation) -> Page {
    Page::Static(Box::new(PageType::display(
        "File invitation",
        vec![
            Display::label("Read & write access"),
            Display::instructions(LINK_ACCESS),
        ],
        None,
        Bumper::custom(
            "Join shared item",
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                let job = invitation_delivery::request(&ctx.air, file, Purpose::Join);
                FormComplete::Next(job_page(job)).run(ctx, theme);
            }),
        ),
        Offset::Center,
    )))
}
pub(crate) fn job_page(job: Instance<LinkJob>) -> Page {
    Page::watching(
        (ContractWatch::new(job), ListWatch::<SharedSpace>::default()),
        |ctx: &mut Context,
         theme: &Theme,
         (job, spaces): (Instance<LinkJob>, Vec<Instance<SharedSpace>>)| {
            let job = job.pending();
            if job.owner != ctx.air.me() {
                return PageType::display(
                    "Invitation unavailable",
                    vec![],
                    None,
                    Bumper::Done,
                    Offset::Center,
                );
            }
            match &job.status {
                Status::Ready => {
                    let url = Invitation::File(job.invitation).url();
                    PageType::display(
                        "Share invitation link",
                        vec![
                            Display::qr_code(&url, "Anyone with the link · Can edit"),
                            Display::instructions(LINK_ACCESS),
                        ],
                        Some((Icons::Copy, Action::copy(&url))),
                        Bumper::custom("Share link", Action::share(&url)),
                        Offset::Center,
                    )
                }
                Status::Joined => {
                    if let Some(space) = spaces
                        .into_iter()
                        .find(|space| space.id() == job.invitation.space)
                    {
                        let root = space.pending().root;
                        collaboration_ui::render_item(ctx, theme, space, root)
                    } else {
                        PageType::display(
                            "Shared item syncing",
                            vec![Display::instructions(
                                "Keep Fig Files open while the shared item syncs.",
                            )],
                            None,
                            Bumper::None,
                            Offset::Center,
                        )
                    }
                }
                Status::Pending => PageType::display(
                    if job.purpose == Purpose::Publish {
                        "Preparing invitation link"
                    } else {
                        "Joining shared item"
                    },
                    vec![Display::instructions(
                        "Keep Fig Files open while this invitation syncs.",
                    )],
                    None,
                    Bumper::None,
                    Offset::Center,
                ),
                Status::Failed(error) => {
                    let invitation = job.invitation;
                    let purpose = job.purpose;
                    PageType::display(
                        "Invitation unavailable",
                        vec![Display::instructions(error)],
                        None,
                        Bumper::custom(
                            "Try again",
                            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                                FormComplete::Next(job_page(invitation_delivery::request(
                                    &ctx.air, invitation, purpose,
                                )))
                                .run(ctx, theme);
                            }),
                        ),
                        Offset::Center,
                    )
                }
            }
        },
    )
}
