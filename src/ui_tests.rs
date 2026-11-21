//! Headless integration tests use the real CHK interface and app assets with
//! in-memory AIR. No window, network, filesystem cache or shared crate edits.
use crate::{
    FigFiles, browser,
    collaboration::{self, MemberStatus, SharedAction, SharedSpace},
    contacts::{self, AddressBook},
    contacts_ui,
    drafts::{self, EditDraft},
    invitation_delivery::{DeliverLink, JobAction, LinkJob, LinkJobs, Purpose, Status},
    invitations::{self, FileInvitation, Invitation, LinkSource},
    preview, runtime,
    sharing::{
        self, Transfer, TransferAction, TransferInit, TransferItem, TransferKind, TransferStatus,
    },
    sharing_ui,
    storage::{self, FileBlob, Vault},
};
use chk::blocking::Block;
use chk::messages::{ChatRoom, ContactNotificationState, RoomNotificationState};
use chk::profiles::{Profile, ProfileAction};
use chk::{App, Assets, Bumper, Context, Display, Icons, Page, PageType, Theme};
use futures::FutureExt;
use prism::{
    Window,
    air::{Contract, ContractType, Id, Metadata, Name, Request, Value},
    drawable::{Drawable, Instruction, Item},
    hardware, traits,
};
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

type ContractStore = HashMap<(Id, Id), Box<dyn Any + Send + Sync>>;
type ShareLog = Arc<Mutex<Option<Vec<(Id, Id, Name)>>>>;

#[derive(Clone, Default)]
struct TestKeyboard;
impl traits::Keyboard for TestKeyboard {
    fn show(&self, _: f32) {}
    fn hide(&self) {}
    fn visible(&self) -> bool {
        false
    }
}
type HardwareLog = Arc<Mutex<Vec<(&'static str, String)>>>;

#[derive(Clone, Default)]
struct TestHardware {
    commands: Option<HardwareLog>,
    clipboard: Option<String>,
}
struct CancelledPick<T>(PhantomData<T>);
impl<T: Clone + 'static> traits::Picked<T> for CancelledPick<T> {
    async fn get(&mut self) -> hardware::PickResult<T> {
        Err(hardware::Cancelled)
    }
}
impl<T: Clone + 'static> traits::Picker<T> for TestHardware {
    type Picked = CancelledPick<T>;
    fn pick(&self) -> Self::Picked {
        panic!("Building or rendering a page must not open a native picker")
    }
}
impl traits::Hardware for TestHardware {
    fn keyboard(&self) -> hardware::Keyboard {
        hardware::Keyboard::new(TestKeyboard)
    }
    fn player(&self, _: media::Player) -> Result<hardware::Player, String> {
        Err("No playback in UI smoke tests".into())
    }
    fn start_camera(&self) -> Result<hardware::Camera, String> {
        Err("No camera".into())
    }
    fn start_microphone(&self) -> Result<hardware::Microphone, String> {
        Err("No microphone".into())
    }
    fn start_capture(&self) -> Result<hardware::Capture, String> {
        Err("No capture".into())
    }
    fn share(&self, data: &hardware::Data) {
        let Some(commands) = &self.commands else {
            panic!("Rendering must not share files");
        };
        let hardware::Data::Text(value) = data else {
            panic!("Invitation actions must share a text link");
        };
        commands.lock().unwrap().push(("share", value.clone()));
    }
    fn set_clipboard(&self, data: &hardware::Data) {
        let Some(commands) = &self.commands else {
            panic!("Rendering must not write the clipboard");
        };
        let hardware::Data::Text(value) = data else {
            panic!("Invitation actions must copy a text link");
        };
        commands.lock().unwrap().push(("copy", value.clone()));
    }
    fn get_clipboard(&self) -> Option<hardware::Data> {
        self.clipboard.clone().map(hardware::Data::Text)
    }
    fn set_cache(&self, _: &str, _: &hardware::Data) -> Result<(), String> {
        Ok(())
    }
    fn get_cache(&self, _: &str) -> Option<hardware::Data> {
        None
    }
    fn trigger_haptic(&self) {}
}

#[derive(Clone, Debug)]
struct MemoryAir(
    Arc<Mutex<ContractStore>>,
    ShareLog,
    Name,
    Arc<Mutex<Vec<prism::service::Key>>>,
);
impl MemoryAir {
    fn with_name(name: Name) -> Self {
        Self(
            Default::default(),
            Default::default(),
            name,
            Default::default(),
        )
    }
}
impl Default for MemoryAir {
    fn default() -> Self {
        Self::with_name(me())
    }
}
fn me() -> Name {
    "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        .parse()
        .unwrap()
}
fn metadata() -> Metadata {
    Metadata {
        signer: me(),
        timestamp: 1_000_000_000,
        confirmed: true,
    }
}
fn peer() -> Name {
    "0379be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        .parse()
        .unwrap()
}
fn transfer_fixture(
    ctx: &Context,
    air: &MemoryAir,
    incoming: bool,
    folder: bool,
    ready: bool,
    status: TransferStatus,
) -> prism::air::Instance<Transfer> {
    let (sender, recipient) = if incoming {
        (peer(), me())
    } else {
        (me(), peer())
    };
    let direction = if incoming { "received" } else { "sent" };
    let content = b"A shared note".to_vec();
    let file = TransferItem {
        parent: folder.then_some(0),
        name: format!("{direction}-note.txt"),
        kind: TransferKind::File {
            size: content.len() as u64,
            mime: "text/plain".into(),
        },
    };
    let items = if folder {
        vec![
            TransferItem {
                parent: None,
                name: format!("{direction}-folder"),
                kind: TransferKind::Folder,
            },
            file,
        ]
    } else {
        vec![file]
    };
    let init = TransferInit {
        recipient,
        nonce: Id::hash(&(direction, folder, ready)),
        items,
    };
    let id = Id::hash(&(sender, &init));
    let mut transfer = Transfer::init(
        init,
        Metadata {
            signer: sender,
            ..metadata()
        },
    );
    if ready {
        transfer
            .apply(
                TransferAction::Upload {
                    index: u32::from(folder),
                    bytes: content,
                },
                Metadata {
                    signer: sender,
                    ..metadata()
                },
            )
            .unwrap();
        transfer
            .apply(
                TransferAction::Seal,
                Metadata {
                    signer: sender,
                    ..metadata()
                },
            )
            .unwrap();
    }
    let decision = match status {
        TransferStatus::Pending => None,
        TransferStatus::Accepted => Some(TransferAction::Accept),
        TransferStatus::Declined => Some(TransferAction::Decline),
    };
    if let Some(decision) = decision {
        transfer
            .apply(
                decision,
                Metadata {
                    signer: recipient,
                    ..metadata()
                },
            )
            .unwrap();
    }
    air.0.lock().unwrap().insert(
        (Transfer::id(), id),
        Box::new(Stored {
            id,
            state: Arc::new(Mutex::new(transfer)),
            signer: ctx.air.me(),
            shares: air.1.clone(),
        }),
    );
    ctx.air.instances::<Transfer>().list().remove(&id).unwrap()
}

fn shared_fixture(
    ctx: &Context,
    air: &MemoryAir,
    incoming: bool,
    folder: bool,
    status: MemberStatus,
) -> prism::air::Instance<SharedSpace> {
    let owner = if incoming { peer() } else { me() };
    let recipient = if incoming { me() } else { peer() };
    let source = MemoryAir::with_name(owner);
    *source.1.lock().unwrap() = Some(Vec::new());
    let source_air = prism::air::Air::new(source);
    let mut vault = storage::open(&source_air);
    let prefix = if incoming { "received" } else { "owned" };
    let parent = folder
        .then(|| storage::create_folder(&mut vault, None, &format!("{prefix}-folder")).unwrap());
    let file = storage::import_file(
        &source_air,
        &mut vault,
        parent,
        &format!("{prefix}-note.txt"),
        b"Collaborative content".to_vec(),
    )
    .unwrap();
    let space =
        collaboration::share(&source_air, &mut vault, parent.unwrap_or(file), recipient).unwrap();
    let mut snapshot = space.pending().clone();
    let action = match status {
        MemberStatus::Active => Some(SharedAction::Join),
        MemberStatus::Declined => Some(SharedAction::Decline),
        MemberStatus::Invited => None,
    };
    if let Some(action) = action {
        snapshot
            .apply(
                action,
                Metadata {
                    signer: recipient,
                    ..metadata()
                },
            )
            .unwrap();
    }
    air.0.lock().unwrap().insert(
        (SharedSpace::id(), space.id()),
        Box::new(Stored {
            id: space.id(),
            state: Arc::new(Mutex::new(snapshot)),
            signer: ctx.air.me(),
            shares: air.1.clone(),
        }),
    );
    ctx.air
        .instances::<SharedSpace>()
        .list()
        .remove(&space.id())
        .unwrap()
}

fn stored_fixture<C: Contract>(
    ctx: &Context,
    air: &MemoryAir,
    id: Id,
    state: C,
) -> prism::air::Instance<C> {
    air.0.lock().unwrap().insert(
        (C::id(), id),
        Box::new(Stored {
            id,
            state: Arc::new(Mutex::new(state)),
            signer: ctx.air.me(),
            shares: air.1.clone(),
        }),
    );
    ctx.air.instances::<C>().list().remove(&id).unwrap()
}

impl traits::Air for MemoryAir {
    fn me(&self) -> Name {
        self.2
    }
    fn instances(&self, contract: ContractType) -> Box<dyn traits::DynInstances> {
        if contract.id == Vault::id() {
            Box::new(Instances::<Vault>(self.clone(), PhantomData))
        } else if contract.id == FileBlob::id() {
            Box::new(Instances::<FileBlob>(self.clone(), PhantomData))
        } else if contract.id == Transfer::id() {
            Box::new(Instances::<Transfer>(self.clone(), PhantomData))
        } else if contract.id == SharedSpace::id() {
            Box::new(Instances::<SharedSpace>(self.clone(), PhantomData))
        } else if contract.id == EditDraft::id() {
            Box::new(Instances::<EditDraft>(self.clone(), PhantomData))
        } else if contract.id == LinkJob::id() {
            Box::new(Instances::<LinkJob>(self.clone(), PhantomData))
        } else if contract.id == AddressBook::id() {
            Box::new(Instances::<AddressBook>(self.clone(), PhantomData))
        } else if contract.id == Profile::id() {
            Box::new(Instances::<Profile>(self.clone(), PhantomData))
        } else if contract.id == Block::id() {
            Box::new(Instances::<Block>(self.clone(), PhantomData))
        } else if contract.id == ContactNotificationState::id() {
            Box::new(Instances::<ContactNotificationState>(
                self.clone(),
                PhantomData,
            ))
        } else if contract.id == RoomNotificationState::id() {
            Box::new(Instances::<RoomNotificationState>(
                self.clone(),
                PhantomData,
            ))
        } else if contract.id == ChatRoom::id() {
            Box::new(Instances::<ChatRoom>(self.clone(), PhantomData))
        } else {
            panic!("Fig Files must not start unused contracts: {}", contract.id)
        }
    }
    fn start(&self, service: prism::service::ServiceInit) {
        assert!(
            service.key == prism::service::Key::new::<LinkJobs>(self.2, &())
                || service.key.0 == TypeId::of::<DeliverLink>(),
            "Unexpected background service: {:?}",
            service.key,
        );
        // Record scheduling, but never poll a service or connect to the network.
        self.3.lock().unwrap().push(service.key);
    }
    fn shutdown(&self, _: prism::service::Key) {
        panic!("Unexpected background service")
    }
}
#[derive(Clone, Debug)]
struct Instances<C: Contract>(MemoryAir, PhantomData<C>);
impl<C: Contract> traits::DynInstances for Instances<C> {
    fn create(&self, init: Request) -> Box<dyn traits::DynInstance> {
        let init = *init.downcast::<C::Init>().unwrap();
        let id = Id::hash(&(self.0.2, &init));
        let mut all = self.0.0.lock().unwrap();
        let entry = all.entry((C::id(), id)).or_insert_with(|| {
            Box::new(Stored {
                id,
                state: Arc::new(Mutex::new(C::init(
                    init,
                    Metadata {
                        signer: self.0.2,
                        ..metadata()
                    },
                ))),
                signer: self.0.2,
                shares: self.0.1.clone(),
            })
        });
        Box::new(entry.downcast_ref::<Stored<C>>().unwrap().clone())
    }
    fn list(&self) -> HashMap<Id, Box<dyn traits::DynInstance>> {
        self.0
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|((kind, _), _)| *kind == C::id())
            .map(|((_, id), stored)| {
                (
                    *id,
                    Box::new(stored.downcast_ref::<Stored<C>>().unwrap().clone())
                        as Box<dyn traits::DynInstance>,
                )
            })
            .collect()
    }
    fn try_next(&mut self) -> Option<Box<dyn traits::DynInstance>> {
        None
    }
    fn next(&mut self) -> traits::BoxFuture<'_, Box<dyn traits::DynInstance>> {
        Box::pin(std::future::pending())
    }
}
#[derive(Clone, Debug)]
struct Stored<C> {
    id: Id,
    state: Arc<Mutex<C>>,
    signer: Name,
    shares: ShareLog,
}
#[derive(Debug)]
struct Snapshot<T>(T);
impl<T: std::fmt::Debug + Send + Sync + 'static> traits::DynRef for Snapshot<T> {
    fn value(&self) -> &(dyn Any + Send + Sync) {
        &self.0
    }
}
impl<C: Contract> traits::DynInstance for Stored<C> {
    fn id(&self) -> Id {
        self.id
    }
    fn notify(&self, enabled: bool) {
        assert!(!enabled, "Fig Files must not enable chat notifications")
    }
    fn share(&self, recipient: Name) {
        let mut shares = self.shares.lock().unwrap();
        let log = shares
            .as_mut()
            .expect("Private files must not be shared by rendering");
        log.push((C::id(), self.id, recipient));
    }
    fn send(&mut self, message: Request) -> Box<dyn traits::DynPending> {
        Box::new(Pending(self.state.lock().unwrap().apply(
            *message.downcast::<C::Message>().unwrap(),
            Metadata {
                signer: self.signer,
                ..metadata()
            },
        )))
    }
    fn pending(&self) -> Box<dyn traits::DynRef + '_> {
        Box::new(Snapshot(self.state.lock().unwrap().clone()))
    }
    fn confirmed(&self) -> Option<Box<dyn traits::DynRef + '_>> {
        Some(self.pending())
    }
    fn try_next(&mut self) -> Option<traits::ContractUpdate> {
        None
    }
    fn next(&mut self) -> traits::BoxFuture<'_, traits::ContractUpdate> {
        Box::pin(std::future::pending())
    }
}
#[derive(Clone, Debug)]
struct Pending<T>(T);
impl<T: Clone + std::fmt::Debug + Send + Sync + 'static> traits::DynPending for Pending<T> {
    fn pending(&self) -> Box<dyn traits::DynRef + '_> {
        Box::new(Snapshot(self.0.clone()))
    }
    fn try_confirmed(&self) -> Option<Value> {
        Some(Arc::new(self.0.clone()))
    }
    fn try_next(&mut self) -> Option<bool> {
        None
    }
    fn next(&mut self) -> traits::BoxFuture<'_, bool> {
        Box::pin(std::future::pending())
    }
}
fn context(mobile: bool) -> Context {
    context_with_air(mobile).0
}
fn context_with_air(mobile: bool) -> (Context, MemoryAir) {
    let air = MemoryAir::default();
    let ctx = Context::new(
        TestHardware::default(),
        prism::air::Air::new(air.clone()),
        Window {
            size: if mobile {
                (390.0, 844.0)
            } else {
                (1280.0, 900.0)
            },
            is_mobile: mobile,
            ..Window::default()
        },
    );
    (ctx, air)
}
fn theme() -> Theme {
    FigFiles
        .theme()
        .build::<chk::PelicanUi>(Assets::new(vec![include_dir::include_dir!(
            "$CARGO_MANIFEST_DIR/resources"
        )]))
}

/// Inspect CHK's public page description so placement regressions are checked
/// independently of Pelican's drawing/layout implementation.
fn page_description(page: Page, ctx: &mut Context, theme: &Theme) -> PageType {
    match page {
        Page::Static(page) => *page,
        Page::Refreshing(mut builder) => builder.build(ctx, theme),
    }
}

fn header_description(page: PageType, ctx: &mut Context, theme: &Theme) -> (Icons, String) {
    let header = match page {
        PageType::Content { header, .. } | PageType::EdgeToEdge { header, .. } => header,
        _ => panic!("Expected a content or edge-to-edge page"),
    };
    let action = match header {
        chk::design_system::Header::Stack { action, .. }
        | chk::design_system::Header::Home { action, .. } => action,
        _ => None,
    };
    let (icon, mut action) = action.expect("This page needs a header action");
    let page = PageType::display("Test", vec![], None, Bumper::None, chk::Offset::Start)
        .build_root(ctx, theme);
    let mut view = theme.interface(
        ctx,
        vec![chk::RootInfo {
            icon: chk::RootIcon::Icon(Icons::Info),
            label: "Test".into(),
            page,
        }],
        None,
    );
    draw(view.as_mut(), ctx);
    action(ctx, theme);
    (icon, text(&draw(view.as_mut(), ctx)))
}

fn is_loading_preview(page: &PageType) -> bool {
    matches!(page, PageType::Content { items, .. } if items.iter().any(|item| matches!(item, chk::Item::Display(Display::Text { text, .. }) if text.contains("Loading preview"))))
}

fn wait_for_preview(
    builder: &mut dyn chk::PageBuilder,
    ctx: &mut Context,
    theme: &Theme,
) -> PageType {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let _ = builder.next().now_or_never();
        let page = builder.build(ctx, theme);
        if !is_loading_preview(&page) {
            return page;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Preview did not complete within five seconds"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn page_body(page: &PageType) -> Vec<&Display> {
    match page {
        PageType::Content { items, .. } => items
            .iter()
            .filter_map(|item| match item {
                chk::Item::Display(display) => Some(display),
                chk::Item::Input(_) => None,
            })
            .collect(),
        _ => panic!("Expected a root or display page"),
    }
}

fn form_value(id: &str, state: chk::State) -> chk::FormValues {
    chk::FormValues::new([(id.into(), state)])
}

fn primary_label(page: &PageType) -> Option<&str> {
    match page {
        PageType::Content {
            bumper: Bumper::Footer(chk::design_system::Footer::Home { first, .. }),
            ..
        } => first.as_ref().map(|(label, _)| label.as_str()),
        PageType::Content {
            bumper: Bumper::Custom { label, .. },
            ..
        } => Some(label.as_str()),
        PageType::Content { .. } => None,
        _ => panic!("Expected a root or display page"),
    }
}

fn cta_action_labels(page: &PageType) -> Vec<&str> {
    page_body(page)
        .into_iter()
        .filter_map(|display| match display {
            Display::Cta { label, actions, .. } if !actions.is_empty() => Some(label.as_str()),
            _ => None,
        })
        .collect()
}

fn manual_recipient_page(selector: PageType, ctx: &mut Context, theme: &Theme) -> PageType {
    let flow = match selector {
        PageType::Form {
            id,
            title,
            item: chk::Input::Selection(selector),
            ..
        } => {
            assert_eq!(id, "recipients");
            assert_eq!(title, "Share access");
            // CHK's selector handles are public; its quick-action navigation
            // is also exercised by taps in the populated-contact flow below.
            chk::Flow::from_form(chk::Form::select_contact(
                &invitations::contacts(),
                theme,
                selector.id,
                vec![],
            ))
        }
        PageType::Content { header, items, .. } => {
            assert_eq!(header.title(), "Share access");
            let actions = items
                .into_iter()
                .find_map(|item| match item {
                    chk::Item::Display(Display::Cta { label, actions, .. })
                        if label == "New contact" =>
                    {
                        Some(actions)
                    }
                    _ => None,
                })
                .expect("Empty contact choices must expose the New contact CTA");
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0].presentation().label,
                chk::ActionItem::format_label("New contact")
            );
            // The CTA's private ActionItem flow is exercised through actual
            // New contact / Paste / Continue taps below; reuse the same CHK
            // form here only to inspect its standard text controls.
            chk::Flow::from_form(chk::Form::new_contact(&invitations::contacts()))
        }
        _ => panic!("Sharing offers contact selection or New contact/Create link choices"),
    };
    assert_eq!(flow.0.len(), 1);
    let form = page_description(flow.0[0].clone(), ctx, theme);
    let PageType::Form {
        item: chk::Input::Text {
            actions: Some(actions),
            ..
        },
        ..
    } = &form
    else {
        panic!("Manual receiving names use CHK's text form");
    };
    assert_text_actions(actions);
    assert!(
        matches!(&form, PageType::Form { id, title, .. } if id == "airname" && title == "New contact")
    );
    form
}

fn assert_text_actions(actions: &[chk::ActionItem]) {
    let presentation = actions
        .iter()
        .map(|action| {
            let item = action.presentation();
            (item.label, item.active, item.icon)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        presentation,
        vec![
            (
                "Paste Clipboard".into(),
                Some("Pasted".into()),
                Icons::Paste
            ),
            ("Scan QR Code".into(), None, Icons::QrCode),
        ]
    );
}

fn settle(view: &mut dyn Drawable, ctx: &mut Context) {
    for _ in 0..80 {
        view.request_width();
        view.request_height(ctx.window.size.0);
        let layout = view.layout(ctx.window.size);
        let events = ctx.events();
        if !events.is_empty() {
            for event in events {
                assert!(
                    event.downcast_ref::<runtime::FileEvent>().is_none(),
                    "Rendering must not emit a native file action"
                );
                view.event(ctx, &layout, event);
            }
        } else if let Some(callback) = view.changed().now_or_never() {
            callback(view.as_any_mut(), ctx);
        } else {
            return;
        }
    }
    panic!("The UI did not settle");
}
fn draw(view: &mut dyn Drawable, ctx: &mut Context) -> Vec<Instruction> {
    settle(view, ctx);
    view.request_width();
    view.request_height(ctx.window.size.0);
    let layout = view.layout(ctx.window.size);
    let drawing = view.draw(
        &layout,
        (0.0, 0.0),
        (0.0, 0.0, ctx.window.size.0, ctx.window.size.1),
    );
    assert!(!drawing.is_empty(), "The page must render");
    for instruction in &drawing {
        assert!(instruction.offset.0.is_finite() && instruction.offset.1.is_finite());
        assert!(instruction.bound.0.is_finite() && instruction.bound.1.is_finite());
        assert!(instruction.bound.2.is_finite() && instruction.bound.3.is_finite());
    }
    drawing
}
fn text(drawing: &[Instruction]) -> String {
    drawing
        .iter()
        .filter_map(|i| match &i.item {
            Item::Text(t) => Some(t.spans.iter().map(|s| s.text.as_str()).collect::<String>()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// CHK owns button-caption casing; model labels and user filenames stay exact.
fn has_button_label(visible: &str, label: &str) -> bool {
    visible
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(label))
}

/// Tap the actual rendered label through Prism's public pointer events, so row
/// navigation is exercised without reading CHK's private ListItem flow field.
fn tap_text(view: &mut dyn Drawable, ctx: &mut Context, label: &str) {
    tap_text_offset(view, ctx, label, (8.0, 8.0));
}

fn tap_text_offset(view: &mut dyn Drawable, ctx: &mut Context, label: &str, offset: (f32, f32)) {
    use prism::events::{MouseButton, MouseEvent, MouseState};
    let drawing = draw(view, ctx);
    let at = drawing
        .iter()
        .rev()
        .find_map(|instruction| {
            let Item::Text(text) = &instruction.item else {
                return None;
            };
            let content = text
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            content
                .trim()
                .eq_ignore_ascii_case(label)
                .then_some(instruction.offset)
        })
        .unwrap_or_else(|| panic!("No tappable label {label}: {}", text(&drawing)));
    for state in [
        MouseState::Pressed(MouseButton::Left),
        MouseState::Released(MouseButton::Left),
    ] {
        let layout = view.layout(ctx.window.size);
        view.event(
            ctx,
            &layout,
            Box::new(MouseEvent {
                position: Some((at.0 + offset.0, at.1 + offset.1)),
                state,
            }),
        );
        settle(view, ctx);
    }
}

#[test]
fn pdf_page_selector_renders_one_based_page_and_only_available_navigation_actions() {
    for mobile in [true, false] {
        for (page, count, expected_actions) in [
            (0, 3, vec![("Next page", Icons::Forward)]),
            (
                1,
                3,
                vec![
                    ("Previous page", Icons::Back),
                    ("Next page", Icons::Forward),
                ],
            ),
            (2, 3, vec![("Previous page", Icons::Back)]),
            (0, 1, vec![]),
        ] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let control = preview::PageControl::new();
            control.select(page);
            let flow = preview::page_selector(control.clone(), page, count, &theme);
            assert_eq!(flow.0.len(), 1, "Page selection is one focused CHK form");
            let form = page_description(flow.0.into_iter().next().unwrap(), &mut ctx, &theme);
            let PageType::Form {
                title,
                item: chk::Input::Text {
                    preset, actions, ..
                },
                on_submit,
                ..
            } = &form
            else {
                panic!("PDF page selection must use CHK's text form");
            };
            let heading = format!("Page {} of {count}", page + 1);
            assert_eq!(title, &heading);
            assert_eq!(preset, &Some((page + 1).to_string()));
            assert!(
                on_submit.is_some(),
                "Entering a page must have a submit action"
            );
            let actual = actions.as_deref().unwrap_or_default();
            let expected_actions: Vec<_> = expected_actions
                .into_iter()
                .map(|(label, icon)| (chk::ActionItem::format_label(label), icon))
                .collect();
            let actual: Vec<_> = actual
                .iter()
                .map(|action| {
                    let item = action.presentation();
                    (item.label, item.icon)
                })
                .collect();
            assert_eq!(actual, expected_actions);
            assert_eq!(
                control.page(),
                page,
                "Building page navigation must not select a different page"
            );
            let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(
                visible.contains(&heading),
                "Missing page position: {visible}"
            );
            assert_eq!(has_button_label(&visible, "Previous page"), page > 0);
            assert_eq!(has_button_label(&visible, "Next page"), page + 1 < count);
            assert!(
                air.0.lock().unwrap().is_empty(),
                "Rendering must not create or modify AIR data"
            );
            assert!(air.1.lock().unwrap().is_none());
        }
    }
}

#[test]
fn real_application_and_all_empty_roots_render_on_phone_and_desktop() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let mut app = <FigFiles as prism::Application>::new(&mut ctx);
        assert!(
            text(&draw(&mut app, &mut ctx))
                .lines()
                .any(|line| line == "Files")
        );
        let vault = storage::open(&ctx.air);
        for (location, expected) in [
            (browser::Location::Folder(None), "No files yet."),
            (browser::Location::Trash, "Trash is empty."),
        ] {
            let mut page =
                browser::listing(vault.clone(), location, true).build_root(&mut ctx, &theme());
            let visible = text(&draw(page.as_mut(), &mut ctx));
            assert!(
                visible.contains(expected),
                "Root should show {expected}: {visible}"
            );
        }
    }
}

#[test]
fn navigation_roots_keep_files_shared_contacts_trash_and_add_standard_profile() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let roots = FigFiles.roots(&mut ctx, &theme(), None);
        let actual: Vec<_> = roots
            .iter()
            .map(|root| {
                (
                    root.label.as_str(),
                    match &root.icon {
                        chk::RootIcon::Icon(icon) => Some(*icon),
                        chk::RootIcon::Avatar(_) => None,
                    },
                )
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                ("Files", Some(Icons::Home)),
                ("Shared", Some(Icons::Send)),
                ("Contacts", Some(Icons::Group)),
                ("Trash", Some(Icons::Delete)),
                ("Profile", Some(Icons::Profile)),
            ]
        );
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn contact_history_includes_known_peers_and_deduplicates_contract_records() {
    use contacts_ui::{ContactIndex, HistorySource};
    let (ctx, air) = context_with_air(false);
    let received = shared_fixture(&ctx, &air, true, true, MemberStatus::Active);
    let active_peer = air::Secret::new().name();
    let unaccepted_peer = air::Secret::new().name();
    let mut state = received.pending().clone();
    state.members.insert(active_peer, MemberStatus::Active);
    state.members.insert(unaccepted_peer, MemberStatus::Invited);
    let root = state.root;
    state.tree.entries.get_mut(&root).unwrap().modified_at = 42;
    let sent = transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Declined);
    let mut index = ContactIndex::new(me());
    index.add_shared(received.id(), &state);
    index.add_shared(received.id(), &state);
    // Shared and legacy contract IDs occupy separate history namespaces.
    index.add_transfer(received.id(), &sent.pending());
    index.add_transfer(received.id(), &sent.pending());
    let contacts = index.finish();
    assert_eq!(contacts.len(), 2);
    assert!(
        !contacts
            .iter()
            .any(|contact| contact.name == me() || contact.name == unaccepted_peer)
    );
    let owner = contacts
        .iter()
        .find(|contact| contact.name == peer())
        .unwrap();
    assert_eq!(owner.last_interaction, 42);
    assert_eq!(owner.history.len(), 2);
    assert!(
        owner
            .history
            .iter()
            .any(|history| matches!(history.source, HistorySource::Shared))
    );
    assert!(
        owner
            .history
            .iter()
            .any(|history| matches!(history.source, HistorySource::Legacy))
    );
    let joined_peer = contacts
        .iter()
        .find(|contact| contact.name == active_peer)
        .unwrap();
    assert_eq!(joined_peer.history.len(), 1);
    assert_eq!(joined_peer.last_interaction, 42);
    let mut ordered_names = vec![peer(), active_peer];
    ordered_names.sort();
    assert_eq!(
        contacts
            .iter()
            .map(|contact| contact.name)
            .collect::<Vec<_>>(),
        ordered_names
    );
}

#[test]
fn contacts_require_opt_in_for_incoming_history_and_reject_invalid_or_unrelated_records() {
    use contacts_ui::ContactIndex;
    for incoming in [true, false] {
        for status in [
            MemberStatus::Invited,
            MemberStatus::Active,
            MemberStatus::Declined,
        ] {
            let (ctx, air) = context_with_air(false);
            let shared = shared_fixture(&ctx, &air, incoming, false, status);
            let mut index = ContactIndex::new(me());
            index.add_shared(shared.id(), &shared.pending());
            let contacts = index.finish();
            let expected = !incoming || status == MemberStatus::Active;
            assert_eq!(
                contacts.len(),
                usize::from(expected),
                "incoming={incoming}, status={status:?}"
            );
            if expected {
                assert_eq!(contacts[0].name, peer());
            }
        }
        for status in [
            TransferStatus::Pending,
            TransferStatus::Accepted,
            TransferStatus::Declined,
        ] {
            let (ctx, air) = context_with_air(false);
            let transfer = transfer_fixture(&ctx, &air, incoming, false, true, status);
            let mut index = ContactIndex::new(me());
            index.add_transfer(transfer.id(), &transfer.pending());
            let contacts = index.finish();
            let expected = !incoming || status == TransferStatus::Accepted;
            assert_eq!(
                contacts.len(),
                usize::from(expected),
                "incoming={incoming}, status={status:?}"
            );
            if expected {
                assert_eq!(contacts[0].name, peer());
            }
        }
    }
    let (ctx, air) = context_with_air(false);
    let shared = shared_fixture(&ctx, &air, true, true, MemberStatus::Active);
    let accepted = transfer_fixture(&ctx, &air, true, false, true, TransferStatus::Accepted);
    let mut index = ContactIndex::new(me());
    let mut invalid_shared = shared.pending().clone();
    invalid_shared
        .tree
        .entries
        .get_mut(&invalid_shared.root)
        .unwrap()
        .name = "bad\u{0007}name".into();
    index.add_shared(Id::random(), &invalid_shared);
    let mut unrelated_shared = shared.pending().clone();
    unrelated_shared.members.remove(&me());
    index.add_shared(Id::random(), &unrelated_shared);
    let mut invalid_transfer = accepted.pending().clone();
    invalid_transfer.items[0].name = "invalid/name.txt".into();
    index.add_transfer(Id::random(), &invalid_transfer);
    let mut self_transfer = accepted.pending().clone();
    self_transfer.sender = me();
    self_transfer.recipient = me();
    index.add_transfer(Id::random(), &self_transfer);
    let mut unrelated_transfer = accepted.pending().clone();
    unrelated_transfer.recipient = air::Secret::new().name();
    index.add_transfer(Id::random(), &unrelated_transfer);
    assert!(index.finish().is_empty());
}

#[test]
fn contacts_empty_and_populated_pages_render_without_sharing_or_changing_history() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let empty = page_description(contacts_ui::home(), &mut ctx, &theme);
        assert!(matches!(&empty, PageType::Content {
            header: chk::design_system::Header::Home {title, action: Some((Icons::Search, _))},
            bumper: Bumper::Footer(chk::design_system::Footer::Home {second: None, ..}), ..
        } if title == "Contacts"));
        assert_eq!(primary_label(&empty), Some("New Contact"));
        let mut rendered = Page::Static(Box::new(empty)).build_root(&mut ctx, &theme);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            visible.contains("No contacts yet.")
                && visible.contains("Get started by adding a friend.")
        );
        assert!(
            air.0.lock().unwrap().is_empty(),
            "Empty Contacts must not create records"
        );

        let outgoing = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        let incoming = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let legacy = transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted);
        let unaccepted = transfer_fixture(&ctx, &air, true, true, true, TransferStatus::Pending);
        let mut invalid = legacy.pending().clone();
        invalid.items[0].name = "invalid/contact-title".into();
        stored_fixture(&ctx, &air, Id::random(), invalid);
        contacts_ui::sync_profiles(&mut ctx);
        let mut profile = Profile::try_from_name(&mut ctx, peer()).unwrap();
        profile.send(ProfileAction::ChangeUsername("Casey".into()));
        let before = air.0.lock().unwrap().len();
        let members = outgoing.pending().members.clone();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        let titles = list_titles(&home);
        assert_eq!(
            titles,
            ["Casey"],
            "One CHK profile represents repeated sharing"
        );
        let mut rendered = Page::Static(Box::new(home)).build_root(&mut ctx, &theme);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Casey"));

        let detail = page_description(contacts_ui::detail(peer()), &mut ctx, &theme);
        assert_profile_contact(&detail, peer());
        let titles = list_titles(&detail);
        for expected in ["owned-folder", "received-note.txt", "sent-note.txt"] {
            assert!(titles.iter().any(|title| title == expected));
        }
        assert!(
            !titles
                .iter()
                .any(|title| title == "received-folder" || title == "invalid/contact-title")
        );
        let (icon, editor) = header_description(detail, &mut ctx, &theme);
        assert_eq!(icon, Icons::Edit);
        assert!(editor.contains("Edit profile") && editor.contains("Username"));
        let absent = page_description(
            contacts_ui::detail(air::Secret::new().name()),
            &mut ctx,
            &theme,
        );
        assert_eq!(primary_label(&absent), None);
        assert!(
            matches!(absent, PageType::Content { header, .. } if header.title() == "Contact unavailable")
        );
        assert_eq!(outgoing.pending().members, members);
        assert_eq!(
            incoming.pending().members.get(&me()),
            Some(&MemberStatus::Active)
        );
        assert_eq!(unaccepted.pending().status, TransferStatus::Pending);
        assert_eq!(air.0.lock().unwrap().len(), before);
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
        assert!(ctx.air.instances::<Block>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

fn list_titles(page: &PageType) -> Vec<String> {
    page_body(page)
        .into_iter()
        .filter_map(|display| match display {
            Display::List { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .map(|item| item.title.clone())
        .collect()
}

fn assert_profile_contact(page: &PageType, name: Name) {
    let body = page_body(page);
    assert!(
        body.iter()
            .any(|item| matches!(item, Display::Avatar { .. }))
    );
    assert!(
        body.iter()
            .any(|item| matches!(item, Display::Cta {label, ..} if label == "About me"))
    );
    let Some(Display::Cta {
        instructions,
        actions,
        ..
    }) = body
        .iter()
        .copied()
        .find(|item| matches!(item, Display::Cta {label, ..} if label == "AirName"))
    else {
        panic!("Contacts must retain CHK's AirName and copy/QR controls");
    };
    assert_eq!(instructions, &name.to_string());
    let labels: Vec<_> = actions
        .iter()
        .map(|action| action.presentation().label)
        .collect();
    assert_eq!(labels, ["Copy", "Display QR Code"]);
    let actions: Vec<_> = body
        .iter()
        .filter_map(|item| match item {
            Display::Cta { label, actions, .. } if label == "Files" => Some(actions),
            _ => None,
        })
        .flatten()
        .map(|action| action.presentation().label)
        .collect();
    assert_eq!(
        actions,
        ["Share Files"],
        "File contact actions must not create chats or block channels"
    );
}

#[test]
fn adding_a_contact_uses_the_standard_airname_form_and_only_persists_on_valid_submission() {
    for mobile in [true, false] {
        let (mut navigation_ctx, navigation_air) = context_with_air(mobile);
        let navigation_theme = theme();
        let mut app = <FigFiles as prism::Application>::new(&mut navigation_ctx);
        draw(&mut app, &mut navigation_ctx);
        let before = navigation_air.0.lock().unwrap().len();
        browser::show(&mut navigation_ctx, &navigation_theme, contacts_ui::home());
        tap_text(&mut app, &mut navigation_ctx, "New Contact");
        let visible = text(&draw(&mut app, &mut navigation_ctx));
        assert!(visible.contains("New contact") && visible.contains("AirName"));
        assert!(
            has_button_label(&visible, "Paste Clipboard")
                && has_button_label(&visible, "Scan QR Code")
        );
        tap_text(&mut app, &mut navigation_ctx, "Scan QR Code");
        assert!(text(&draw(&mut app, &mut navigation_ctx)).contains("AirName"));
        assert_eq!(navigation_air.0.lock().unwrap().len(), before);
        assert!(navigation_air.1.lock().unwrap().is_none());

        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let flow = contacts_ui::add_flow(&theme);
        assert_eq!(flow.0.len(), 1);
        let form = page_description(flow.0[0].clone(), &mut ctx, &theme);
        let PageType::Form {
            id,
            title,
            item:
                chk::Input::Text {
                    actions: Some(actions),
                    ..
                },
            on_submit: Some(mut submit),
            ..
        } = form.clone()
        else {
            panic!("Adding a contact uses CHK's AirName form");
        };
        assert_eq!(id, "airname");
        assert_eq!(title, "New contact");
        assert_text_actions(&actions);
        let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("AirName"));
        assert!(air.0.lock().unwrap().is_empty());
        for input in [
            "".to_string(),
            "not-an-air-name".into(),
            "03".into(),
            me().to_string(),
        ] {
            submit(&mut ctx, &form_value("airname", chk::State::Text(input)));
            assert!(
                air.0.lock().unwrap().is_empty(),
                "Invalid names cannot create a profile"
            );
            ctx.events();
        }
        let result = submit(
            &mut ctx,
            &form_value("airname", chk::State::Text(format!("  \n{}\t ", peer()))),
        );
        assert!(matches!(result, chk::FormComplete::Next(_)));
        let profile = Profile::try_from_name(&mut ctx, peer()).unwrap();
        let saved = profile.pending().clone();
        assert_eq!(saved.name, Some(peer()));
        assert!(!saved.username.is_empty());
        submit(
            &mut ctx,
            &form_value("airname", chk::State::Text(peer().to_string())),
        );
        assert_eq!(*profile.pending(), saved);
        assert_eq!(ctx.air.instances::<Profile>().list().len(), 1);
        assert_eq!(
            air.0.lock().unwrap().len(),
            1,
            "Only the private CHK profile is stored"
        );
        assert!(ctx.air.instances::<AddressBook>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
        assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn standard_profiles_render_and_merge_with_later_sharing_history() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut profile = Profile::from_name(&mut ctx, peer());
        profile.send(ProfileAction::ChangeUsername("Alex Example".into()));
        let before = air.0.lock().unwrap().len();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        assert_eq!(primary_label(&home), Some("New Contact"));
        assert_eq!(list_titles(&home), ["Alex Example"]);
        let detail = page_description(contacts_ui::detail(peer()), &mut ctx, &theme);
        assert_profile_contact(&detail, peer());
        assert!(list_titles(&detail).is_empty());
        assert_eq!(air.0.lock().unwrap().len(), before);
        let mut app = <FigFiles as prism::Application>::new(&mut ctx);
        draw(&mut app, &mut ctx);
        let after_startup = air.0.lock().unwrap().len();
        browser::show(&mut ctx, &theme, Page::Static(Box::new(detail)));
        tap_text(&mut app, &mut ctx, "Share Files");
        assert!(text(&draw(&mut app, &mut ctx)).contains("Share files"));
        assert!(air.1.lock().unwrap().is_none());
        // Startup creates the user's own profile/library; contact rendering and
        // the file picker must leave the selected contact's metadata untouched.
        assert_eq!(profile.pending().username, "Alex Example");
        assert_eq!(air.0.lock().unwrap().len(), after_startup);
        shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted);
        contacts_ui::sync_profiles(&mut ctx);
        let before = air.0.lock().unwrap().len();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        assert_eq!(list_titles(&home), ["Alex Example"]);
        let detail = page_description(contacts_ui::detail(peer()), &mut ctx, &theme);
        let titles = list_titles(&detail);
        assert!(
            titles.contains(&"owned-folder".into()) && titles.contains(&"sent-note.txt".into())
        );
        assert_eq!(air.0.lock().unwrap().len(), before);
        assert!(
            ctx.air
                .instances::<AddressBook>()
                .list()
                .values()
                .all(|book| book.pending().contacts.is_empty())
        );
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn legacy_contacts_migrate_privately_without_overwriting_profiles_or_adding_unaccepted_senders() {
    let (mut ctx, air) = context_with_air(false);
    let saved_name = air::Secret::new().name();
    contacts::add(&ctx.air, saved_name).unwrap();
    let mut profile = Profile::from_name(&mut ctx, saved_name);
    profile.send(ProfileAction::ChangeUsername("My colleague".into()));
    profile.send(ProfileAction::ChangeNotes("Private notes preserved".into()));
    profile.send(ProfileAction::ChangeAvatar(chk::AvatarContent::icon(
        Icons::Group,
        chk::AvatarIconStyle::Brand,
    )));
    let custom = profile.pending().clone();
    let outgoing = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
    let unknown = air::Secret::new().name();
    let mut pending = transfer_fixture(&ctx, &air, true, true, true, TransferStatus::Pending)
        .pending()
        .clone();
    pending.sender = unknown;
    stored_fixture(&ctx, &air, Id::random(), pending);
    let mut invalid = transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted)
        .pending()
        .clone();
    invalid.recipient = air::Secret::new().name();
    let invalid_name = invalid.recipient;
    invalid.items[0].name = "invalid/name".into();
    stored_fixture(&ctx, &air, Id::random(), invalid);
    let memberships = outgoing.pending().members.clone();
    contacts_ui::sync_profiles(&mut ctx);
    assert!(Profile::try_from_name(&mut ctx, peer()).is_some());
    assert!(Profile::try_from_name(&mut ctx, unknown).is_none());
    assert!(Profile::try_from_name(&mut ctx, invalid_name).is_none());
    assert_eq!(*profile.pending(), custom);
    let count = air.0.lock().unwrap().len();
    contacts_ui::sync_profiles(&mut ctx);
    assert_eq!(air.0.lock().unwrap().len(), count);
    assert_eq!(*profile.pending(), custom);
    assert_eq!(outgoing.pending().members, memberships);
    assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
    assert!(ctx.air.instances::<Block>().list().is_empty());
    assert!(air.1.lock().unwrap().is_none());
}

#[test]
fn standard_profile_editing_saves_alias_biography_and_avatar_privately() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut root = FigFiles.roots(&mut ctx, &theme, None).pop().unwrap();
        assert_eq!(root.label, "Profile");
        assert!(text(&draw(root.page.as_mut(), &mut ctx)).contains("My profile"));
        let own_contact = page_description(contacts_ui::detail(me()), &mut ctx, &theme);
        assert!(
            matches!(own_contact, PageType::Edit { ref title, .. } if title == "My profile"),
            "Selecting your own AirName opens your profile editor, not a self-sharing action"
        );
        let page = page_description(
            chk::Root::profile(&mut ctx, &chk::ContactInvitations::default()).0,
            &mut ctx,
            &theme,
        );
        let PageType::Edit {
            title,
            items,
            display,
            mut on_save,
            ..
        } = page
        else {
            panic!("Profile reuses CHK's standard edit page");
        };
        assert_eq!(title, "My profile");
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["avatar", "username", "about"]
        );
        assert!(display.iter().any(|item| matches!(item, Display::Cta {label, instructions, ..} if label == "AirName" && instructions == &me().to_string())));
        let records = air.0.lock().unwrap().len();
        let avatar = chk::AvatarContent::icon(Icons::Profile, chk::AvatarIconStyle::Brand);
        let values = chk::FormValues::new([
            ("username".into(), chk::State::Text("My Fig Profile".into())),
            (
                "about".into(),
                chk::State::Text("Personal biography".into()),
            ),
            ("avatar".into(), chk::State::Avatar(avatar.clone())),
        ]);
        on_save(&mut ctx, &values);
        let profile = Profile::try_from_name(&mut ctx, me()).unwrap();
        assert_eq!(profile.pending().username, "My Fig Profile");
        assert_eq!(profile.pending().notes, "Personal biography");
        assert_eq!(profile.pending().avatar, avatar);
        assert_eq!(air.0.lock().unwrap().len(), records);
        assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn sharing_again_with_a_contact_skips_name_entry_but_requires_explicit_confirmation() {
    for mobile in [true, false] {
        for folder in [true, false] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            let id = if folder {
                let parent = storage::create_folder(&mut vault, None, "Repeat project").unwrap();
                storage::create_folder(&mut vault, Some(parent), "Empty folder").unwrap();
                parent
            } else {
                storage::import_file(
                    &ctx.air,
                    &mut vault,
                    None,
                    "repeat.txt",
                    b"Repeat sharing".to_vec(),
                )
                .unwrap()
            };
            contacts::add(&ctx.air, peer()).unwrap();
            let before = vault.pending().entries.clone();
            let without_contact =
                sharing_ui::picker_selection(&mut ctx, vault.clone(), id, None, &theme);
            let selector = page_description(without_contact.0[0].clone(), &mut ctx, &theme);
            assert!(matches!(
                &selector,
                PageType::Form {
                    item: chk::Input::Selection(_),
                    ..
                }
            ));
            let mut selected_contact =
                Page::Static(Box::new(selector.clone())).build(&mut ctx, &theme, None, 1);
            let short = Profile::try_from_name(&mut ctx, peer())
                .unwrap()
                .pending()
                .username
                .clone();
            assert_eq!(
                text(&draw(selected_contact.as_mut(), &mut ctx))
                    .lines()
                    .filter(|line| *line == short)
                    .count(),
                1
            );
            manual_recipient_page(selector, &mut ctx, &theme);

            let picker = page_description(sharing_ui::send_picker_for(peer()), &mut ctx, &theme);
            let mut rendered = Page::Static(Box::new(picker)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains(if folder {
                "Repeat project"
            } else {
                "repeat.txt"
            }));
            assert!(
                visible
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&peer().to_string())
            );

            let selected =
                sharing_ui::picker_selection(&mut ctx, vault.clone(), id, Some(peer()), &theme);
            assert_eq!(selected.0.len(), 1);
            let confirmation = page_description(selected.0[0].clone(), &mut ctx, &theme);
            assert_eq!(primary_label(&confirmation), Some("Share access"));
            assert!(
                matches!(&confirmation, PageType::Content { .. }),
                "Known recipients must not be asked for their name again"
            );
            let mut rendered =
                Page::Static(Box::new(confirmation.clone())).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("Read & write access"));
            assert!(
                visible
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&peer().to_string())
            );
            assert_eq!(vault.pending().entries, before);
            assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
            assert!(air.1.lock().unwrap().is_none());

            // Explicit confirmation uses the same collaborative share action;
            // only the new SharedSpace capability leaves the private library.
            *air.1.lock().unwrap() = Some(Vec::new());
            let PageType::Content {
                bumper: Bumper::Custom {
                    action, secondary, ..
                },
                ..
            } = confirmation
            else {
                unreachable!()
            };
            assert_eq!(
                secondary.as_ref().map(|(label, _)| label.as_str()),
                Some("Cancel")
            );
            (action.get())(&mut ctx, &theme);
            let spaces = ctx.air.instances::<SharedSpace>().list();
            assert_eq!(spaces.len(), 1);
            let space = spaces.values().next().unwrap();
            assert_eq!(
                space.pending().members.get(&peer()),
                Some(&MemberStatus::Invited)
            );
            assert!(
                matches!(vault.pending().entries[&id].kind, storage::EntryKind::Shared { space: channel, .. } if channel == space.id())
            );
            assert_eq!(
                *air.1.lock().unwrap(),
                Some(vec![(SharedSpace::id(), space.id(), peer())])
            );
            assert!(ctx.air.instances::<Transfer>().list().is_empty());
        }
    }
}

#[test]
fn header_search_filters_in_place_clears_and_opens_results() {
    fn key(view: &mut dyn Drawable, ctx: &mut Context, key: prism::events::Key) -> String {
        let layout = view.layout(ctx.window.size);
        view.event(
            ctx,
            &layout,
            Box::new(prism::events::KeyboardEvent {
                key,
                state: prism::events::KeyboardState::Pressed,
                modifiers: prism::events::Modifiers::default(),
            }),
        );
        text(&draw(view, ctx))
    }

    for mobile in [true, false] {
        for mode in 0..3 {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            let mut space =
                (mode == 1).then(|| shared_fixture(&ctx, &air, true, true, MemberStatus::Active));
            let (source, matched, other, title) = if let Some(space) = &mut space {
                let root = space.pending().root;
                collaboration::import_file(&ctx.air, space, root, "other.txt", b"Other".to_vec())
                    .unwrap();
                (
                    crate::collaboration_ui::item(space.clone(), root),
                    "received-note.txt",
                    "other.txt",
                    "Search shared folder",
                )
            } else if mode == 2 {
                let mut profile = Profile::from_name(&mut ctx, peer());
                profile.send(ProfileAction::ChangeUsername("Note keeper".into()));
                let mut other = Profile::from_name(&mut ctx, air::Secret::new().name());
                other.send(ProfileAction::ChangeUsername("Morgan".into()));
                (
                    contacts_ui::home(),
                    "Note keeper",
                    "Morgan",
                    "Search contacts",
                )
            } else {
                storage::import_file(&ctx.air, &mut vault, None, "note.txt", b"Notes".to_vec())
                    .unwrap();
                storage::import_file(&ctx.air, &mut vault, None, "other.txt", b"Other".to_vec())
                    .unwrap();
                let trashed = storage::import_file(
                    &ctx.air,
                    &mut vault,
                    None,
                    "trashed-note.txt",
                    b"Old".to_vec(),
                )
                .unwrap();
                storage::trash(&mut vault, trashed).unwrap();
                (
                    browser::listing(vault.clone(), browser::Location::Folder(None), true),
                    "note.txt",
                    "other.txt",
                    "Search files",
                )
            };
            let header = match page_description(source, &mut ctx, &theme) {
                PageType::Content { header, .. } => header,
                _ => panic!("Search must be reachable from the page header"),
            };
            let mut search = match header {
                chk::design_system::Header::Home {
                    action: Some((Icons::Search, action)),
                    ..
                }
                | chk::design_system::Header::Stack {
                    action: Some((Icons::Search, action)),
                    ..
                } => action,
                _ => panic!("Missing Search header action"),
            };
            let mut app = <FigFiles as prism::Application>::new(&mut ctx);
            draw(&mut app, &mut ctx);
            search(&mut ctx, &theme);
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                visible.contains(title) && visible.contains(matched) && visible.contains(other),
                "{visible}"
            );
            assert!(!has_button_label(&visible, "Continue"));

            let visible = key(
                &mut app,
                &mut ctx,
                prism::events::Key::Character("note".into()),
            );
            assert!(
                visible.contains(title) && visible.contains(matched),
                "{visible}"
            );
            assert!(
                !visible.contains(other) && !visible.contains("trashed-note.txt"),
                "{visible}"
            );
            assert!(!has_button_label(&visible, "Continue"));

            for _ in 0..4 {
                key(&mut app, &mut ctx, prism::events::Key::Backspace);
            }
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                visible.contains(matched) && visible.contains(other),
                "Clearing restores all results: {visible}"
            );
            let visible = key(
                &mut app,
                &mut ctx,
                prism::events::Key::Character("no-match".into()),
            );
            assert!(
                visible.contains(if mode == 2 {
                    "No matching contacts."
                } else {
                    "No matching files or folders."
                }),
                "{visible}"
            );
            for _ in 0..8 {
                key(&mut app, &mut ctx, prism::events::Key::Backspace);
            }
            key(
                &mut app,
                &mut ctx,
                prism::events::Key::Character("note".into()),
            );
            tap_text(&mut app, &mut ctx, matched);
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                !visible.contains(title),
                "A result must open directly: {visible}"
            );
            if mode == 2 {
                assert!(
                    visible.contains("AirName") && has_button_label(&visible, "Share files"),
                    "{visible}"
                );
                assert!(!has_button_label(&visible, "Message"));
            } else {
                assert!(visible.contains(matched), "{visible}");
            }
            assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
            assert!(ctx.air.instances::<LinkJob>().list().is_empty());
            assert!(air.1.lock().unwrap().is_none());
        }
    }
}

#[test]
fn folder_header_search_contains_only_descendants_and_root_search_stays_global() {
    fn search(page: Page, ctx: &mut Context, theme: &Theme) -> Box<dyn Drawable> {
        let PageType::Content { header, .. } = page_description(page, ctx, theme) else {
            panic!("A folder must expose its Search header");
        };
        let mut action = match header {
            chk::design_system::Header::Home {
                action: Some((Icons::Search, action)),
                ..
            }
            | chk::design_system::Header::Stack {
                action: Some((Icons::Search, action)),
                ..
            } => action,
            _ => panic!("Missing Search header action"),
        };
        let mut app = <FigFiles as prism::Application>::new(ctx);
        draw(&mut app, ctx);
        action(ctx, theme);
        draw(&mut app, ctx);
        Box::new(app)
    }
    fn query(view: &mut dyn Drawable, ctx: &mut Context, previous: &str, next: &str) -> String {
        use prism::events::{Key, KeyboardEvent, KeyboardState, Modifiers};
        for key in std::iter::repeat_n(Key::Backspace, previous.len())
            .chain(std::iter::once(Key::Character(next.into())))
        {
            let layout = view.layout(ctx.window.size);
            view.event(
                ctx,
                &layout,
                Box::new(KeyboardEvent {
                    key,
                    state: KeyboardState::Pressed,
                    modifiers: Modifiers::default(),
                }),
            );
            draw(view, ctx);
        }
        text(&draw(view, ctx))
    }
    let (mut ctx, air) = context_with_air(false);
    let theme = theme();
    let mut vault = storage::open(&ctx.air);
    let parent = storage::create_folder(&mut vault, None, "Parent").unwrap();
    let scope = storage::create_folder(&mut vault, Some(parent), "Working folder").unwrap();
    let sibling = storage::create_folder(&mut vault, Some(parent), "Sibling").unwrap();
    let deeper = storage::create_folder(&mut vault, Some(scope), "Private deeper").unwrap();
    for (parent, name) in [
        (Some(scope), "note-private-child.txt"),
        (Some(deeper), "note-private-grandchild.txt"),
        (None, "note-outside-root.txt"),
        (Some(parent), "note-outside-parent.txt"),
        (Some(sibling), "note-outside-sibling.txt"),
    ] {
        storage::import_file(&ctx.air, &mut vault, parent, name, b"Test".to_vec()).unwrap();
    }
    let discarded = storage::create_folder(&mut vault, Some(scope), "Discarded").unwrap();
    storage::import_file(
        &ctx.air,
        &mut vault,
        Some(discarded),
        "note-trash-private.txt",
        b"Old".to_vec(),
    )
    .unwrap();
    storage::trash(&mut vault, discarded).unwrap();

    let mut space = shared_fixture(&ctx, &air, true, true, MemberStatus::Active);
    let root = space.pending().root;
    let root_file = space.pending().tree.children(Some(root))[0].id;
    collaboration::rename(&ctx.air, &mut space, root_file, "note-shared-parent.txt").unwrap();
    let shared_scope =
        collaboration::create_folder(&ctx.air, &mut space, root, "Shared scope").unwrap();
    let shared_deeper =
        collaboration::create_folder(&ctx.air, &mut space, shared_scope, "Shared deeper").unwrap();
    let shared_sibling =
        collaboration::create_folder(&ctx.air, &mut space, root, "Shared sibling").unwrap();
    for (parent, name) in [
        (shared_scope, "note-shared-child.txt"),
        (shared_deeper, "note-shared-grandchild.txt"),
        (shared_sibling, "note-shared-sibling.txt"),
    ] {
        collaboration::import_file(&ctx.air, &mut space, parent, name, b"Test".to_vec()).unwrap();
    }
    let discarded =
        collaboration::create_folder(&ctx.air, &mut space, shared_scope, "Discarded shared")
            .unwrap();
    collaboration::import_file(
        &ctx.air,
        &mut space,
        discarded,
        "note-trash-shared.txt",
        b"Old".to_vec(),
    )
    .unwrap();
    collaboration::trash(&ctx.air, &mut space, discarded).unwrap();
    let mount = storage::mount_shared(&mut vault, space.id(), "Mounted", true).unwrap();
    storage::move_entry(&mut vault, mount, Some(scope)).unwrap();
    let mut other_space = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
    let other_root = other_space.pending().root;
    let other_file = other_space.pending().tree.children(Some(other_root))[0].id;
    collaboration::rename(
        &ctx.air,
        &mut other_space,
        other_file,
        "note-outside-mount.txt",
    )
    .unwrap();
    let other_mount =
        storage::mount_shared(&mut vault, other_space.id(), "Other mount", true).unwrap();
    storage::move_entry(&mut vault, other_mount, Some(sibling)).unwrap();

    let scoped = browser::listing(vault.clone(), browser::Location::Folder(Some(scope)), false);
    // Folder identity, rather than its original name, defines the scope.
    storage::rename(&mut vault, scope, "Renamed working folder").unwrap();
    let mut view = search(scoped, &mut ctx, &theme);
    let initial = text(&draw(view.as_mut(), &mut ctx));
    assert!(
        initial.contains("Private deeper") && !initial.contains("Sibling"),
        "{initial}"
    );
    let visible = query(view.as_mut(), &mut ctx, "", "note-private");
    assert!(
        visible.contains("note-private-child.txt")
            && visible.contains("note-private-grandchild.txt"),
        "{visible}"
    );
    let visible = query(view.as_mut(), &mut ctx, "note-private", "note-shared");
    for name in [
        "note-shared-child.txt",
        "note-shared-grandchild.txt",
        "note-shared-parent.txt",
        "note-shared-sibling.txt",
    ] {
        assert!(
            visible.contains(name),
            "Mounted descendants must be searched inside their private folder: {visible}"
        );
    }
    let visible = query(view.as_mut(), &mut ctx, "note-shared", "note-outside");
    assert!(
        visible.contains("No matching files or folders."),
        "{visible}"
    );
    let visible = query(view.as_mut(), &mut ctx, "note-outside", "note-trash");
    assert!(
        visible.contains("No matching files or folders."),
        "{visible}"
    );

    let mut view = search(
        crate::collaboration_ui::item(space.clone(), shared_scope),
        &mut ctx,
        &theme,
    );
    let initial = text(&draw(view.as_mut(), &mut ctx));
    assert!(
        initial.contains("Shared deeper") && !initial.contains("Shared sibling"),
        "{initial}"
    );
    let visible = query(view.as_mut(), &mut ctx, "", "note-shared");
    assert!(
        visible.contains("note-shared-child.txt") && visible.contains("note-shared-grandchild.txt"),
        "{visible}"
    );
    assert!(
        !visible.contains("note-shared-parent.txt") && !visible.contains("note-shared-sibling.txt"),
        "{visible}"
    );
    let visible = query(view.as_mut(), &mut ctx, "note-shared", "note-trash");
    assert!(
        visible.contains("No matching files or folders."),
        "{visible}"
    );

    let mut view = search(
        browser::listing(vault.clone(), browser::Location::Folder(None), true),
        &mut ctx,
        &theme,
    );
    let visible = query(view.as_mut(), &mut ctx, "", "note-outside");
    for name in [
        "note-outside-root.txt",
        "note-outside-parent.txt",
        "note-outside-sibling.txt",
        "note-outside-mount.txt",
    ] {
        assert!(
            visible.contains(name),
            "Files root must retain global search: {visible}"
        );
    }
    assert!(air.1.lock().unwrap().is_none());
}

#[test]
fn populated_folders_search_and_trash_render_actual_entries() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let mut vault = storage::open(&ctx.air);
        let folder = storage::create_folder(&mut vault, None, "Documents").unwrap();
        let file = storage::import_file(
            &ctx.air,
            &mut vault,
            Some(folder),
            "notes.txt",
            b"Fig Files notes".to_vec(),
        )
        .unwrap();
        storage::set_favorite(&mut vault, file, true).unwrap();
        let discarded =
            storage::import_file(&ctx.air, &mut vault, None, "discarded.txt", b"Old".to_vec())
                .unwrap();
        storage::trash(&mut vault, discarded).unwrap();
        for (location, expected, absent) in [
            (browser::Location::Folder(None), "Documents", "notes.txt"),
            (
                browser::Location::Folder(Some(folder)),
                "notes.txt",
                "discarded.txt",
            ),
            (
                browser::Location::Search("notes".into()),
                "notes.txt",
                "discarded.txt",
            ),
            (browser::Location::Trash, "discarded.txt", "notes.txt"),
        ] {
            let mut page =
                browser::listing(vault.clone(), location, false).build(&mut ctx, &theme(), None, 1);
            let visible = text(&draw(page.as_mut(), &mut ctx));
            assert!(visible.contains(expected), "Expected {expected}: {visible}");
            assert!(!visible.contains(absent), "Unexpected {absent}: {visible}");
        }
        let mut page = browser::info(vault.clone(), file).build(&mut ctx, &theme(), None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in ["notes.txt", "Share", "Move", "Delete"] {
            assert!(
                visible.contains(expected),
                "File details should include {expected}: {visible}"
            );
        }
        for secondary in [
            "Rename",
            "Move to Trash",
            "Share / Export",
            "Send to Orange name",
        ] {
            assert!(
                !visible.lines().any(|line| line == secondary),
                "Secondary controls belong in focused pages: {visible}"
            );
        }
        let mut page = browser::detail(vault, folder).build(&mut ctx, &theme(), None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        assert!(
            visible.contains("Share"),
            "Folders should support AIR sharing: {visible}"
        );
    }
}

#[test]
fn library_commands_are_scoped_to_folders_and_totals_are_the_last_content() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let folder = storage::create_folder(&mut vault, None, "Documents").unwrap();
        storage::create_folder(&mut vault, Some(folder), "Reference").unwrap();
        let file = storage::import_file(
            &ctx.air,
            &mut vault,
            Some(folder),
            "notes.txt",
            b"abc".to_vec(),
        )
        .unwrap();
        storage::set_favorite(&mut vault, file, true).unwrap();
        storage::import_file(&ctx.air, &mut vault, None, "other.txt", b"12345".to_vec()).unwrap();
        let removed = storage::import_file(
            &ctx.air,
            &mut vault,
            None,
            "removed.txt",
            b"not counted".to_vec(),
        )
        .unwrap();
        storage::trash(&mut vault, removed).unwrap();
        for root in [true, false] {
            for location in [
                browser::Location::Folder(None),
                browser::Location::Folder(Some(folder)),
                browser::Location::Search("notes".into()),
                browser::Location::Trash,
            ] {
                let is_folder = matches!(location, browser::Location::Folder(_));
                let is_trash = matches!(location, browser::Location::Trash);
                let page = page_description(
                    browser::listing(vault.clone(), location, root),
                    &mut ctx,
                    &theme,
                );
                assert_eq!(
                    primary_label(&page),
                    is_folder.then_some(if root { "Add Files" } else { "Add files" }),
                );
                let body = page_body(&page);
                assert!(
                    !body
                        .iter()
                        .any(|display| matches!(display, Display::Actions { .. })),
                    "Library pages should not repeat commands in inline action rows"
                );
                let Some(Display::Text { text, .. }) = body.last() else {
                    panic!("Library totals belong at the bottom of the content");
                };
                assert!(
                    text.contains("4 items"),
                    "Totals must describe the library, not the current filtered list: {text}"
                );
                assert!(
                    text.contains("8 B"),
                    "Trashed content must be excluded from library bytes: {text}"
                );
                let header = match &page {
                    PageType::Content {
                        header,
                        bumper: Bumper::Footer(chk::design_system::Footer::Home { second, .. }),
                        ..
                    } => {
                        assert!(second.is_none());
                        header
                    }
                    PageType::Content { header, .. } => header,
                    _ => unreachable!(),
                };
                assert_eq!(
                    match header {
                        chk::design_system::Header::Stack { action, .. }
                        | chk::design_system::Header::Home { action, .. } =>
                            action.as_ref().map(|(icon, _)| *icon),
                        _ => None,
                    },
                    (!is_trash).then_some(Icons::Search)
                );
            }
        }
    }
}

#[test]
fn collaborative_browser_open_folder_switches_to_live_contents_after_promotion() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let folder = storage::create_folder(&mut vault, None, "Project").unwrap();
        storage::import_file(
            &ctx.air,
            &mut vault,
            Some(folder),
            "notes.txt",
            b"abc".to_vec(),
        )
        .unwrap();
        let open_page = browser::listing(
            vault.clone(),
            browser::Location::Folder(Some(folder)),
            false,
        );
        let mut before = open_page.clone().build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(before.as_mut(), &mut ctx)).contains("Files / Project"));
        *air.1.lock().unwrap() = Some(Vec::new());
        let space = collaboration::share(&ctx.air, &mut vault, folder, peer()).unwrap();
        *air.1.lock().unwrap() = None;
        let page = page_description(open_page, &mut ctx, &theme);
        assert_eq!(primary_label(&page), Some("Add files"));
        let mut after = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(after.as_mut(), &mut ctx));
        for expected in [
            "notes.txt",
            "Shared Trash",
            "2 items · 3 B in this shared folder",
        ] {
            assert!(
                visible.contains(expected),
                "Open folder did not switch to shared content: {visible}"
            );
        }
        assert!(!visible.contains("Files / Project"));
        assert!(!visible.contains("Import your first file"));
        let add = page_description(
            crate::collaboration_ui::add_page(space.clone(), folder, &theme),
            &mut ctx,
            &theme,
        );
        let PageType::Content {
            bumper: Bumper::Custom { action, .. },
            ..
        } = add
        else {
            panic!("Shared import needs an action");
        };
        ctx.events();
        (action.get())(&mut ctx, &theme);
        let events = ctx.events();
        assert!(events.iter().any(|event| matches!(event.downcast_ref::<runtime::FileEvent>(), Some(runtime::FileEvent::SharedImport(id,parent)) if *id == space.id() && *parent == folder)));
        assert!(!events.iter().any(|event| matches!(
            event.downcast_ref::<runtime::FileEvent>(),
            Some(runtime::FileEvent::Import(_))
        )));
        assert_eq!(
            vault.pending().entries.len(),
            1,
            "Shared children are not private children"
        );
    }
}

#[test]
fn collaborative_browser_search_follows_shared_edits_and_keeps_totals_last() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut space = shared_fixture(&ctx, &air, true, true, MemberStatus::Active);
        let mut vault = storage::open(&ctx.air);
        collaboration::join(&ctx.air, &mut vault, &mut space).unwrap();
        storage::import_file(&ctx.air, &mut vault, None, "private.txt", b"12345".to_vec()).unwrap();
        let root = space.pending().root;
        let child = space.pending().tree.children(Some(root))[0].id;
        let search = browser::listing(
            vault.clone(),
            browser::Location::Search("note".into()),
            false,
        );
        let mut rendered = search.clone().build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("received-note.txt"));
        collaboration::rename(&ctx.air, &mut space, child, "updated-note.txt").unwrap();
        let mut renamed = search.clone().build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(renamed.as_mut(), &mut ctx));
        assert!(visible.contains("updated-note.txt") && !visible.contains("received-note.txt"));
        let added = collaboration::import_file(
            &ctx.air,
            &mut space,
            root,
            "added-note.txt",
            b"abcd".to_vec(),
        )
        .unwrap();
        collaboration::trash(&ctx.air, &mut space, child).unwrap();
        let page = page_description(search, &mut ctx, &theme);
        assert_eq!(primary_label(&page), None);
        let Some(Display::Text { text: footer, .. }) = page_body(&page).into_iter().last() else {
            panic!("Totals belong last");
        };
        assert!(
            footer.contains("3 items") && footer.contains("9 B"),
            "Live library totals are incorrect: {footer}"
        );
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("added-note.txt"));
        assert!(!visible.contains("updated-note.txt") && !visible.contains("received-note.txt"));
        assert!(vault.pending().get(added).is_none());
        assert!(vault.pending().get(child).is_none());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn collaborative_browser_private_move_excludes_shared_shortcuts() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let shared = storage::create_folder(&mut vault, None, "Shared destination").unwrap();
        let personal = storage::create_folder(&mut vault, None, "Personal").unwrap();
        let file = storage::import_file(&ctx.air, &mut vault, None, "private.txt", b"abc".to_vec())
            .unwrap();
        *air.1.lock().unwrap() = Some(Vec::new());
        let space = collaboration::share(&ctx.air, &mut vault, shared, peer()).unwrap();
        *air.1.lock().unwrap() = None;
        let before = space.pending().tree.clone();
        let page = page_description(
            browser::move_flow(vault.clone(), file, &theme).0.remove(0),
            &mut ctx,
            &theme,
        );
        let PageType::Form {
            item: chk::Input::Enumerator { items },
            on_submit: Some(mut submit),
            ..
        } = page.clone()
        else {
            panic!("Move uses a CHK folder selector");
        };
        assert_eq!(
            items,
            vec![chk::ValueItem::new("Files / Personal", "Folder")]
        );
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Files / Personal"));
        assert!(!visible.contains("Shared destination"));
        submit(
            &mut ctx,
            &form_value("destination", chk::State::Enumerator(0)),
        );
        assert_eq!(vault.pending().get(file).unwrap().parent, Some(personal));
        assert_eq!(space.pending().tree, before);
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn collaborative_browser_share_picker_uses_live_names_but_shares_the_private_mount() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut space = shared_fixture(&ctx, &air, true, true, MemberStatus::Active);
        let root = space.pending().root;
        let mut vault = storage::open(&ctx.air);
        let mount = collaboration::join(&ctx.air, &mut vault, &mut space).unwrap();
        assert_ne!(mount, root, "Recipient mounts have private IDs");
        let picker = sharing_ui::send_picker();
        collaboration::rename(&ctx.air, &mut space, root, "Renamed project").unwrap();
        let mut rendered = picker.build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Renamed project") && !visible.contains("received-folder"));
        let selector = page_description(
            sharing_ui::send_flow(&mut ctx, vault.clone(), mount, &theme)
                .0
                .remove(0),
            &mut ctx,
            &theme,
        );
        let form = manual_recipient_page(selector, &mut ctx, &theme);
        let PageType::Form {
            on_submit: Some(mut submit),
            ..
        } = form
        else {
            panic!("The manual-entry bumper opens recipient input");
        };
        let recipient = ::air::Secret::new().name();
        // Saving the receiving name prepares the selection; sharing still
        // requires the explicit app-owned confirmation below.
        submit(
            &mut ctx,
            &form_value("airname", chk::State::Text(recipient.to_string())),
        );
        assert!(air.1.lock().unwrap().is_none());
        let confirmation = sharing_ui::send_confirmation(vault.clone(), mount, recipient);
        collaboration::rename(&ctx.air, &mut space, root, "Latest project name").unwrap();
        let page = page_description(confirmation, &mut ctx, &theme);
        let mut rendered = Page::Static(Box::new(page.clone())).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Latest project name") && !visible.contains("received-folder"));
        assert!(
            air.1.lock().unwrap().is_none(),
            "Rendering must not invite anyone"
        );
        let PageType::Content {
            bumper: Bumper::Custom { action, .. },
            ..
        } = page
        else {
            panic!("Confirmation needs a share action");
        };
        *air.1.lock().unwrap() = Some(Vec::new());
        (action.get())(&mut ctx, &theme);
        assert_eq!(
            air.1.lock().unwrap().as_deref().unwrap(),
            &[(SharedSpace::id(), space.id(), recipient)]
        );
        assert_eq!(ctx.air.instances::<SharedSpace>().list().len(), 1);
        assert_eq!(
            space.pending().members.get(&recipient),
            Some(&MemberStatus::Invited)
        );
        assert_eq!(vault.pending().entries.len(), 1);
        assert!(
            matches!(vault.pending().get(mount).unwrap().kind, storage::EntryKind::Shared { space:id, .. } if id == space.id())
        );
    }
}

#[test]
fn file_preview_opens_info_with_metadata_and_focused_actions() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let folder = storage::create_folder(&mut vault, None, "Documents").unwrap();
        let file = storage::import_file(
            &ctx.air,
            &mut vault,
            Some(folder),
            "notes.txt",
            b"notes".to_vec(),
        )
        .unwrap();
        storage::set_favorite(&mut vault, file, true).unwrap();
        let before = vault.pending().entries.clone();

        let file_page = page_description(browser::detail(vault.clone(), file), &mut ctx, &theme);
        assert!(is_loading_preview(&file_page));
        assert_eq!(primary_label(&file_page), None);
        assert!(
            !page_body(&file_page)
                .into_iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let (icon, info) = header_description(file_page, &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        assert!(!has_button_label(&info, "Continue"));
        for expected in ["Share", "Move", "Delete", "Export file"] {
            assert!(
                has_button_label(&info, expected),
                "Missing file information action {expected}"
            );
        }
        let visible = info;
        for expected in ["Name", "Kind", "Size", "Location", "Access", "Only you"] {
            assert!(
                visible.contains(expected),
                "Missing metadata {expected}: {visible}"
            );
        }

        let folder_page = page_description(browser::info(vault.clone(), folder), &mut ctx, &theme);
        assert_eq!(primary_label(&folder_page), None);
        assert_eq!(
            cta_action_labels(&folder_page),
            vec!["Share", "Move", "Delete"]
        );
        let (icon, rename) = header_description(folder_page, &mut ctx, &theme);
        assert_eq!(icon, Icons::Edit);
        assert!(rename.contains("Edit folder") && rename.contains("Folder name"));
        assert!(has_button_label(&rename, "Save"));
        assert!(!has_button_label(&rename, "Continue"));
        assert!(!rename.contains("Manage item"));

        let file_info = page_description(browser::info(vault.clone(), file), &mut ctx, &theme);
        assert_eq!(primary_label(&file_info), None);
        let (icon, rename) = header_description(file_info, &mut ctx, &theme);
        assert_eq!(icon, Icons::Edit);
        assert!(rename.contains("Edit file") && rename.contains("File name"));
        assert!(has_button_label(&rename, "Save"));
        assert!(!has_button_label(&rename, "Continue"));
        assert_eq!(
            vault.pending().entries,
            before,
            "Inspecting menus must not perform their actions"
        );
        assert!(ctx.air.instances::<Transfer>().list().is_empty());

        storage::trash(&mut vault, file).unwrap();
        let trashed = page_description(browser::info(vault.clone(), file), &mut ctx, &theme);
        assert_eq!(primary_label(&trashed), Some("Restore"));
        let (icon, confirmation) = header_description(trashed, &mut ctx, &theme);
        assert_eq!(icon, Icons::Delete);
        assert!(has_button_label(&confirmation, "Delete permanently"));
        assert!(
            vault.pending().get(file).is_some(),
            "Opening confirmation must not delete the item"
        );
    }
}

#[test]
fn opening_private_text_and_images_automatically_loads_content_with_an_info_header() {
    for mobile in [true, false] {
        for image in [false, true] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            let (name, bytes) = if image {
                ("preview.svg", br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#.to_vec())
            } else {
                ("preview.txt", b"Automatically loaded file content".to_vec())
            };
            let id = storage::import_file(&ctx.air, &mut vault, None, name, bytes).unwrap();
            let before = vault.pending().entries.clone();
            let Page::Refreshing(mut builder) = browser::detail(vault.clone(), id) else {
                panic!("File previews must refresh asynchronously");
            };
            assert!(
                ctx.events().is_empty(),
                "Constructing a preview must not trigger native work"
            );
            let loading = builder.build(&mut ctx, &theme);
            assert!(
                is_loading_preview(&loading),
                "The first frame must show the loading placeholder"
            );
            assert_eq!(
                header_description(loading.clone(), &mut ctx, &theme).0,
                Icons::Info
            );
            let mut rendered = Page::Static(Box::new(loading)).build(&mut ctx, &theme, None, 1);
            assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Loading preview"));
            let ready = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
            assert_eq!(matches!(&ready, PageType::EdgeToEdge { .. }), image);
            let (icon, info) = header_description(ready.clone(), &mut ctx, &theme);
            assert_eq!(icon, Icons::Info);
            assert!(!has_button_label(&info, "Continue"));
            let mut rendered = Page::Static(Box::new(ready)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            if !image {
                assert!(visible.contains("Automatically loaded file content"));
            }
            assert!(!visible.lines().any(|line| line == "Preview"));
            assert_eq!(vault.pending().entries, before);
            assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
            assert!(air.1.lock().unwrap().is_none());
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[test]
fn native_pdf_info_page_selection_reopens_the_automatic_preview() {
    fn tap_info(view: &mut dyn Drawable, ctx: &mut Context, theme: &Theme) {
        use prism::events::{MouseButton, MouseEvent, MouseState};
        let native = theme
            .downcast_ref::<chk::PelicanUi>()
            .unwrap()
            .native_theme();
        let icon = native.icons().get(Icons::Info);
        let drawing = draw(view, ctx);
        let at = drawing
            .iter()
            .rev()
            .find_map(|instruction| match &instruction.item {
                Item::Svg(_, svg, _) if svg == &icon => Some(instruction.offset),
                _ => None,
            })
            .expect("The rendered PDF viewer must have its Info action");
        for state in [
            MouseState::Pressed(MouseButton::Left),
            MouseState::Released(MouseButton::Left),
        ] {
            let layout = view.layout(ctx.window.size);
            view.event(
                ctx,
                &layout,
                Box::new(MouseEvent {
                    position: Some((at.0 + 8.0, at.1 + 8.0)),
                    state,
                }),
            );
            settle(view, ctx);
        }
    }

    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let id = storage::import_file(
            &ctx.air,
            &mut vault,
            None,
            "three-pages.pdf",
            crate::pdf_preview::tests::fixture(),
        )
        .unwrap();
        let before = vault.pending().entries.clone();
        let Page::Refreshing(mut builder) = browser::detail(vault.clone(), id) else {
            panic!("Opening a PDF must use the automatic preview");
        };
        assert!(is_loading_preview(&builder.build(&mut ctx, &theme)));
        let ready = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
        assert!(matches!(&ready, PageType::EdgeToEdge { .. }));
        assert_eq!(header_description(ready, &mut ctx, &theme).0, Icons::Info);

        let mut app = <FigFiles as prism::Application>::new(&mut ctx);
        draw(&mut app, &mut ctx);
        browser::show(&mut ctx, &theme, Page::Refreshing(builder));
        assert!(
            text(&draw(&mut app, &mut ctx))
                .lines()
                .any(|line| line == "Photo")
        );
        tap_info(&mut app, &mut ctx, &theme);
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(visible.contains("PDF pages") && visible.contains("Page 1 of 3"));
        assert!(visible.contains("Share") && visible.contains("Delete"));
        tap_text(&mut app, &mut ctx, "PDF pages");
        assert!(text(&draw(&mut app, &mut ctx)).contains("Page 1 of 3"));

        // The real form action emits CHK's reset-with-callback navigation event.
        // Processing it must reopen the selected preview without a FileEvent.
        tap_text(&mut app, &mut ctx, "Next page");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let drawing = loop {
            let drawing = draw(&mut app, &mut ctx);
            if text(&drawing).lines().any(|line| line == "Photo") {
                break drawing;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Selecting the next page did not reopen the PDF: {}",
                text(&drawing)
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let rendered = drawing
            .iter()
            .find_map(|instruction| match &instruction.item {
                Item::Image(_, image, _) if image.dimensions == (200, 400) => Some(image),
                _ => None,
            })
            .expect("The automatic viewer must display the selected PDF page");
        let pixel = rendered[(100 * 200 + 50) as usize];
        assert!(
            pixel.blue > 200 && pixel.red < 40 && pixel.green < 40,
            "Page two has a blue upper-left quadrant; page one is red"
        );
        tap_info(&mut app, &mut ctx, &theme);
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(visible.contains("PDF pages") && visible.contains("Page 2 of 3"));
        assert!(visible.contains("Share") && visible.contains("Delete"));
        tap_text(&mut app, &mut ctx, "PDF pages");
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(visible.contains("Page 2 of 3"));
        assert!(
            has_button_label(&visible, "Previous page") && has_button_label(&visible, "Next page")
        );
        assert_eq!(vault.pending().entries, before);
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn asynchronous_shared_preview_rechecks_the_current_bytes_before_showing_a_result() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut space = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let id = space.pending().root;
        let Page::Refreshing(mut builder) = crate::collaboration_ui::item(space.clone(), id) else {
            panic!("Shared files need the asynchronous preview router");
        };
        let first = builder.build(&mut ctx, &theme);
        assert!(is_loading_preview(&first));
        assert_eq!(header_description(first, &mut ctx, &theme).0, Icons::Info);
        let expected = space.pending().content_hash(id).unwrap();
        space
            .send(SharedAction::Replace {
                id,
                expected,
                version: Id::random(),
                bytes: b"Latest collaborative preview content".to_vec(),
            })
            .pending()
            .clone()
            .unwrap();
        let ready = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
        let (icon, info) = header_description(ready.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        let mut rendered = Page::Static(Box::new(ready)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Latest collaborative preview content"));
        assert!(
            !visible.contains("Collaborative content"),
            "An old worker result must not overwrite newer content"
        );
        let visible = info;
        for label in [
            "Name", "Kind", "Size", "Location", "Access", "Share", "Delete",
        ] {
            assert!(
                visible.contains(label),
                "Missing shared metadata/action {label}: {visible}"
            );
        }
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn delayed_file_bytes_and_unsupported_previews_keep_information_accessible() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let id = storage::import_file(
            &ctx.air,
            &mut vault,
            None,
            "arriving.txt",
            b"The bytes have arrived".to_vec(),
        )
        .unwrap();
        let storage::EntryKind::File { blob, .. } = vault.pending().entries[&id].kind else {
            unreachable!()
        };
        let delayed = air
            .0
            .lock()
            .unwrap()
            .remove(&(FileBlob::id(), blob))
            .unwrap();
        let Page::Refreshing(mut builder) = browser::detail(vault.clone(), id) else {
            unreachable!()
        };
        let waiting = builder.build(&mut ctx, &theme);
        assert!(is_loading_preview(&waiting));
        assert_eq!(header_description(waiting, &mut ctx, &theme).0, Icons::Info);
        air.0
            .lock()
            .unwrap()
            .insert((FileBlob::id(), blob), delayed);
        let ready = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
        let mut rendered = Page::Static(Box::new(ready)).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("The bytes have arrived"));

        let unsupported = storage::import_file(
            &ctx.air,
            &mut vault,
            None,
            "binary.bin",
            vec![0, 255, 0, 128],
        )
        .unwrap();
        let Page::Refreshing(mut builder) = browser::detail(vault, unsupported) else {
            unreachable!()
        };
        assert!(is_loading_preview(&builder.build(&mut ctx, &theme)));
        let unavailable = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
        assert_eq!(
            header_description(unavailable.clone(), &mut ctx, &theme).0,
            Icons::Info
        );
        let mut rendered = Page::Static(Box::new(unavailable)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Preview unavailable"));
        assert!(visible.contains("Open Info to export"));
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn a_preview_finishing_after_trash_or_deletion_does_not_show_stale_file_contents() {
    for mobile in [true, false] {
        for permanently in [false, true] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            let id = storage::import_file(
                &ctx.air,
                &mut vault,
                None,
                "removed.txt",
                b"Do not reveal this stale preview".to_vec(),
            )
            .unwrap();
            let Page::Refreshing(mut builder) = browser::detail(vault.clone(), id) else {
                unreachable!()
            };
            assert!(is_loading_preview(&builder.build(&mut ctx, &theme)));
            storage::trash(&mut vault, id).unwrap();
            if permanently {
                storage::delete_permanently(&mut vault, id).unwrap();
            }
            let result = wait_for_preview(builder.as_mut(), &mut ctx, &theme);
            if !permanently {
                assert_eq!(primary_label(&result), Some("Restore"));
            }
            let mut rendered = Page::Static(Box::new(result)).build(&mut ctx, &theme, None, 1);
            assert!(
                !text(&draw(rendered.as_mut(), &mut ctx))
                    .contains("Do not reveal this stale preview")
            );
            assert!(air.1.lock().unwrap().is_none());
        }
    }
}

#[test]
fn file_info_ctas_navigate_without_mutation_and_header_rename_requires_save() {
    for mobile in [true, false] {
        for mode in 0..3 {
            let shared = mode != 0;
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            storage::create_folder(&mut vault, None, "Destination").unwrap();
            let mut space =
                shared.then(|| shared_fixture(&ctx, &air, true, mode == 1, MemberStatus::Active));
            let id = if let Some(space) = &mut space {
                collaboration::join(&ctx.air, &mut vault, space).unwrap();
                let root = space.pending().root;
                if mode == 1 {
                    let file = space.pending().tree.children(Some(root))[0].id;
                    collaboration::create_folder(&ctx.air, space, root, "Shared destination")
                        .unwrap();
                    file
                } else {
                    root
                }
            } else {
                storage::import_file(
                    &ctx.air,
                    &mut vault,
                    None,
                    "info.txt",
                    b"Info action test".to_vec(),
                )
                .unwrap()
            };
            let info = if let Some(space) = &space {
                crate::collaboration_ui::details(space.clone(), id)
            } else {
                browser::info(vault.clone(), id)
            };
            let before_vault = vault.pending().entries.clone();
            let before_shared = space.as_ref().map(|space| space.pending().tree.clone());
            let before_members = space.as_ref().map(|space| space.pending().members.clone());
            let original_name = space
                .as_ref()
                .map(|space| space.pending().tree.get(id).unwrap().name.clone())
                .unwrap_or_else(|| vault.pending().get(id).unwrap().name.clone());
            let page = page_description(info.clone(), &mut ctx, &theme);
            let mut expected = vec!["Share", "Move", "Delete"];
            if shared {
                expected.push("File contents");
            }
            expected.push("Export file");
            assert_eq!(cta_action_labels(&page), expected);
            assert!(
                list_titles(&page).is_empty(),
                "File actions belong in CHK CTAs"
            );
            assert_eq!(primary_label(&page), None);
            let PageType::Content {
                header:
                    chk::design_system::Header::Stack {
                        action: Some((Icons::Edit, mut rename)),
                        ..
                    },
                ..
            } = page
            else {
                panic!("A file's Edit header must open its rename form directly");
            };
            let mut app = <FigFiles as prism::Application>::new(&mut ctx);
            draw(&mut app, &mut ctx);
            browser::show(&mut ctx, &theme, info.clone());
            tap_text(&mut app, &mut ctx, "Share");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains("Share access") && has_button_label(&visible, "New contact"));
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().members.clone()), before_members);
            }
            assert!(air.1.lock().unwrap().is_none());

            browser::show(&mut ctx, &theme, info.clone());
            tap_text(&mut app, &mut ctx, "Move");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                visible.contains(if mode == 1 {
                    "Shared destination"
                } else {
                    "Files / Destination"
                }),
                "{visible}"
            );
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }

            browser::show(&mut ctx, &theme, info.clone());
            tap_text(&mut app, &mut ctx, "Delete");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                has_button_label(
                    &visible,
                    match mode {
                        0 => "Move to Trash",
                        1 => "Move to Shared Trash",
                        _ => "Remove shortcut",
                    }
                ),
                "{visible}"
            );
            assert!(visible.contains("Cancel"));
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            assert!(air.1.lock().unwrap().is_none());

            if shared {
                browser::show(&mut ctx, &theme, info.clone());
                let visible = text(&draw(&mut app, &mut ctx));
                assert!(
                    has_button_label(&visible, "Edit text")
                        && has_button_label(&visible, "Replace file"),
                    "{visible}"
                );
                tap_text(&mut app, &mut ctx, "Edit text");
                let visible = text(&draw(&mut app, &mut ctx));
                assert!(
                    visible.contains("Edit shared text")
                        && visible.contains("Collaborative content"),
                    "{visible}"
                );
                assert_eq!(
                    Some(space.as_ref().unwrap().pending().tree.clone()),
                    before_shared
                );
                assert!(drafts::list(&ctx.air).is_empty());
            }

            browser::show(&mut ctx, &theme, info);
            draw(&mut app, &mut ctx);
            let layout = app.layout(ctx.window.size);
            let center = (ctx.window.size.0 * 0.5, ctx.window.size.1 * 0.5);
            app.event(
                &mut ctx,
                &layout,
                Box::new(prism::events::MouseEvent {
                    position: Some(center),
                    state: prism::events::MouseState::Scroll(0.0, 300.0),
                }),
            );
            draw(&mut app, &mut ctx);
            tap_text(&mut app, &mut ctx, "Export file");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains("Export file"));
            assert!(
                !visible.contains("File contents"),
                "Export CTA opens a focused confirmation: {visible}"
            );
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            assert!(air.1.lock().unwrap().is_none());

            // Exercise the captured Edit header in a fresh navigation surface;
            // prior scrolling/export navigation must not retain its focus.
            let mut app = <FigFiles as prism::Application>::new(&mut ctx);
            draw(&mut app, &mut ctx);
            rename(&mut ctx, &theme);
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(
                visible.contains("Edit file") && visible.contains("File name"),
                "{visible}"
            );
            assert!(has_button_label(&visible, "Save"));
            assert!(!has_button_label(&visible, "Continue"));
            tap_text(&mut app, &mut ctx, &original_name);
            let layout = app.layout(ctx.window.size);
            app.event(
                &mut ctx,
                &layout,
                Box::new(prism::events::KeyboardEvent {
                    key: prism::events::Key::Character("Renamed".into()),
                    state: prism::events::KeyboardState::Pressed,
                    modifiers: prism::events::Modifiers::default(),
                }),
            );
            let typed = text(&draw(&mut app, &mut ctx));
            assert!(
                typed.contains("Renamed"),
                "Renaming must edit the focused File name: {typed}"
            );
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            tap_text(&mut app, &mut ctx, "Save");
            assert!(text(&draw(&mut app, &mut ctx)).contains("Edit file"));
            let renamed = space
                .as_ref()
                .map(|space| space.pending().tree.get(id).unwrap().name.clone())
                .unwrap_or_else(|| vault.pending().get(id).unwrap().name.clone());
            assert_ne!(renamed, original_name);
            assert!(renamed.contains("Renamed"));
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().members.clone()), before_members);
                assert_eq!(
                    space.pending().read_file(id).unwrap(),
                    b"Collaborative content"
                );
            }
        }
    }
}

#[test]
fn add_and_folder_info_pages_keep_secondary_choices_separate_without_side_effects() {
    let mut ctx = context(false);
    let theme = theme();
    let mut vault = storage::open(&ctx.air);
    let folder = storage::create_folder(&mut vault, None, "Documents").unwrap();
    let before = vault.pending().entries.clone();
    let add = page_description(
        browser::add_page(vault.clone(), Some(folder), &theme),
        &mut ctx,
        &theme,
    );
    assert_eq!(primary_label(&add), Some("Import files"));
    assert!(
        !page_body(&add)
            .into_iter()
            .any(|display| matches!(display, Display::Actions { .. }))
    );
    let mut rendered = Page::Static(Box::new(add)).build(&mut ctx, &theme, None, 1);
    let visible = text(&draw(rendered.as_mut(), &mut ctx));
    assert!(visible.contains("New folder"));

    let folder_info = page_description(browser::info(vault.clone(), folder), &mut ctx, &theme);
    assert_eq!(primary_label(&folder_info), None);
    assert_eq!(
        cta_action_labels(&folder_info),
        vec!["Share", "Move", "Delete"]
    );
    let (icon, rename) = header_description(folder_info, &mut ctx, &theme);
    assert_eq!(icon, Icons::Edit);
    assert!(rename.contains("Edit folder") && rename.contains("Folder name"));
    assert!(has_button_label(&rename, "Save"));
    assert!(!has_button_label(&rename, "Continue"));
    assert_eq!(vault.pending().entries, before);
    assert!(ctx.air.instances::<Transfer>().list().is_empty());
}

#[test]
fn folder_details_offer_direct_actions_and_preserve_shared_root_contents() {
    fn edit_key(view: &mut dyn Drawable, ctx: &mut Context, key: prism::events::Key) {
        let layout = view.layout(ctx.window.size);
        view.event(
            ctx,
            &layout,
            Box::new(prism::events::KeyboardEvent {
                key,
                state: prism::events::KeyboardState::Pressed,
                modifiers: prism::events::Modifiers::default(),
            }),
        );
        draw(view, ctx);
    }
    for mobile in [true, false] {
        for mode in 0..3 {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            storage::create_folder(&mut vault, None, "Destination").unwrap();
            let mut space =
                (mode != 0).then(|| shared_fixture(&ctx, &air, true, true, MemberStatus::Active));
            let mut shortcut = None;
            let id = if let Some(space) = &mut space {
                shortcut = Some(collaboration::join(&ctx.air, &mut vault, space).unwrap());
                let root = space.pending().root;
                collaboration::create_folder(&ctx.air, space, root, "Shared destination").unwrap();
                if mode == 1 {
                    collaboration::create_folder(&ctx.air, space, root, "Nested folder").unwrap()
                } else {
                    root
                }
            } else {
                storage::create_folder(&mut vault, None, "Working folder").unwrap()
            };
            let info = || match &space {
                Some(space) => crate::collaboration_ui::details(space.clone(), id),
                None => browser::info(vault.clone(), id),
            };
            let original_name = space
                .as_ref()
                .map(|space| space.pending().tree.get(id).unwrap().name.clone())
                .unwrap_or_else(|| vault.pending().get(id).unwrap().name.clone());
            let before_vault = vault.pending().entries.clone();
            let before_shared = space.as_ref().map(|space| space.pending().tree.clone());
            let before_members = space.as_ref().map(|space| space.pending().members.clone());
            let page = page_description(info(), &mut ctx, &theme);
            assert_eq!(primary_label(&page), None);
            assert_eq!(cta_action_labels(&page), ["Share", "Move", "Delete"]);
            let PageType::Content {
                header:
                    chk::design_system::Header::Stack {
                        action: Some((Icons::Edit, mut rename)),
                        ..
                    },
                ..
            } = page
            else {
                panic!("A folder's header must rename it directly");
            };
            let mut app = <FigFiles as prism::Application>::new(&mut ctx);
            draw(&mut app, &mut ctx);
            browser::show(&mut ctx, &theme, info());
            tap_text(&mut app, &mut ctx, "Share");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains("Share access"));
            assert!(!visible.contains("No items yet"));
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
                assert_eq!(Some(space.pending().members.clone()), before_members);
            }
            assert!(air.1.lock().unwrap().is_none());

            browser::show(&mut ctx, &theme, info());
            tap_text(&mut app, &mut ctx, "Move");
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains(if mode == 1 {
                "Shared destination"
            } else {
                "Files / Destination"
            }));
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }

            rename(&mut ctx, &theme);
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains("Edit folder") && visible.contains("Folder name"));
            assert!(has_button_label(&visible, "Save"));
            assert!(!has_button_label(&visible, "Continue"));
            // An unchanged edit cannot save, and invalid input keeps the
            // editor/draft open without mutating either private or shared data.
            tap_text(&mut app, &mut ctx, "Save");
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            tap_text(&mut app, &mut ctx, &original_name);
            edit_key(
                &mut app,
                &mut ctx,
                prism::events::Key::Character("/".into()),
            );
            assert!(text(&draw(&mut app, &mut ctx)).contains("Edit folder"));
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            edit_key(&mut app, &mut ctx, prism::events::Key::Backspace);
            edit_key(
                &mut app,
                &mut ctx,
                prism::events::Key::Character("Renamed".into()),
            );
            assert_eq!(vault.pending().entries, before_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_shared);
            }
            tap_text(&mut app, &mut ctx, "Save");
            let saved_editor = text(&draw(&mut app, &mut ctx));
            assert!(
                saved_editor.contains("Edit folder"),
                "Saving keeps the standard editor open"
            );
            assert!(!has_button_label(&saved_editor, "Continue"));
            let renamed = space
                .as_ref()
                .map(|space| space.pending().tree.get(id).unwrap().name.clone())
                .unwrap_or_else(|| vault.pending().get(id).unwrap().name.clone());
            assert_ne!(renamed, original_name);
            assert!(renamed.contains("Renamed"));

            let before_delete_vault = vault.pending().entries.clone();
            let before_delete_shared = space.as_ref().map(|space| space.pending().tree.clone());
            browser::show(&mut ctx, &theme, info());
            tap_text(&mut app, &mut ctx, "Delete");
            let visible = text(&draw(&mut app, &mut ctx));
            let confirm = match mode {
                0 => "Move to Trash",
                1 => "Move to Shared Trash",
                _ => "Remove shortcut",
            };
            assert!(has_button_label(&visible, confirm));
            assert!(has_button_label(&visible, "Cancel"));
            assert_eq!(vault.pending().entries, before_delete_vault);
            if let Some(space) = &space {
                assert_eq!(Some(space.pending().tree.clone()), before_delete_shared);
            }
            tap_text(&mut app, &mut ctx, confirm);
            draw(&mut app, &mut ctx);
            if let Some(space) = &space {
                assert!(!space.pending().tree.is_trashed(space.pending().root));
                assert_eq!(Some(space.pending().members.clone()), before_members);
                if mode == 1 {
                    assert!(space.pending().tree.is_trashed(id));
                    assert_eq!(vault.pending().entries, before_delete_vault);
                } else {
                    assert_eq!(Some(space.pending().tree.clone()), before_delete_shared);
                    assert!(vault.pending().is_trashed(shortcut.unwrap()));
                }
            } else {
                assert!(vault.pending().is_trashed(id));
            }
            assert!(air.1.lock().unwrap().is_none());
            assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        }
    }
}

#[test]
fn empty_shared_root_and_receiving_name_render_on_phone_and_desktop_without_sharing() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let theme = theme();
        let mut page = sharing_ui::home().build_root(&mut ctx, &theme);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in ["Shared", "Send", "Receive", "No shared files yet."] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        let mut page = sharing_ui::receive_name(me()).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        assert!(
            visible.contains("Share AirName") && visible.contains("Scan to share this AirName.")
        );
        assert!(has_button_label(&visible, "Share"));
        let description = page_description(sharing_ui::receive_name(me()), &mut ctx, &theme);
        assert!(
            page_body(&description).into_iter().any(
                |item| matches!(item, Display::QRCode {data, ..} if data == &Invitation::Contact(me()).url())
            )
        );
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
    }
}

#[test]
fn shared_send_and_receive_bumpers_open_their_flows_without_side_effects() {
    for mobile in [true, false] {
        for only_trashed in [false, true] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            if only_trashed {
                let mut vault = storage::open(&ctx.air);
                let id = storage::create_folder(&mut vault, None, "Removed folder").unwrap();
                storage::import_file(
                    &ctx.air,
                    &mut vault,
                    Some(id),
                    "hidden.txt",
                    b"No live items".to_vec(),
                )
                .unwrap();
                storage::trash(&mut vault, id).unwrap();
            }
            let vault_count = ctx.air.instances::<Vault>().list().len();
            let home = page_description(sharing_ui::home(), &mut ctx, &theme);
            assert_eq!(primary_label(&home), Some("Send"));
            let PageType::Content {
                header: chk::design_system::Header::Home { action: None, .. },
                bumper:
                    Bumper::Footer(chk::design_system::Footer::Home {
                        first: Some((send_label, mut send)),
                        second: Some((receive_label, mut receive)),
                    }),
                ..
            } = home
            else {
                panic!("Shared needs labeled Send and Receive root bumpers");
            };
            assert_eq!(send_label, "Send");
            assert_eq!(receive_label, "Receive");
            let root = PageType::display("Test", vec![], None, Bumper::None, chk::Offset::Start)
                .build_root(&mut ctx, &theme);
            let mut interface = theme.interface(
                &mut ctx,
                vec![chk::RootInfo {
                    icon: chk::RootIcon::Icon(Icons::Info),
                    label: "Test".into(),
                    page: root,
                }],
                None,
            );
            draw(interface.as_mut(), &mut ctx);
            send(&mut ctx, &theme);
            let visible = text(&draw(interface.as_mut(), &mut ctx));
            assert!(
                visible.contains("Share files")
                    && visible.contains("Add a file or folder in Files")
            );
            let picker = page_description(sharing_ui::send_picker(), &mut ctx, &theme);
            assert!(matches!(
                &picker,
                PageType::Content { header, bumper: Bumper::None, .. } if header.title() == "Share files"
            ));
            assert!(
                !page_body(&picker)
                    .into_iter()
                    .any(|display| matches!(display, Display::Actions { .. }))
            );
            let lists: Vec<_> = page_body(&picker)
                .into_iter()
                .filter_map(|display| match display {
                    Display::List { items, .. } => Some(items),
                    _ => None,
                })
                .collect();
            assert_eq!(lists.len(), 1);
            assert!(
                lists[0].is_empty(),
                "A picker must not invent selectable items"
            );
            let mut rendered = Page::Static(Box::new(picker)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("Add a file or folder in Files"));
            assert!(!visible.contains("Removed folder"));
            assert!(!visible.contains("hidden.txt"));

            receive(&mut ctx, &theme);
            let visible = text(&draw(interface.as_mut(), &mut ctx));
            assert!(visible.contains("Receive"));
            for label in ["Paste clipboard", "Scan QR code", "Show my QR code"] {
                assert!(
                    has_button_label(&visible, label),
                    "Missing {label}: {visible}"
                );
            }
            assert!(!visible.contains("Share AirName"));
            tap_text(interface.as_mut(), &mut ctx, "Show my QR code");
            let visible = text(&draw(interface.as_mut(), &mut ctx));
            assert!(visible.contains("Share AirName") && has_button_label(&visible, "Share"));
            let receiving = page_description(sharing_ui::receive_name(me()), &mut ctx, &theme);
            assert!(
                matches!(&receiving, PageType::Content { header, .. } if header.title() == "Share AirName")
            );
            assert_eq!(primary_label(&receiving), Some("Share"));
            assert!(page_body(&receiving).into_iter().any(
                |item| matches!(item, Display::QRCode {data, ..} if data == &Invitation::Contact(me()).url())
            ));
            let mut rendered = Page::Static(Box::new(receiving)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("Scan to share this AirName."));
            assert_eq!(ctx.air.instances::<Vault>().list().len(), vault_count);
            assert!(ctx.air.instances::<Transfer>().list().is_empty());
            assert!(air.1.lock().unwrap().is_none());
        }
    }
}

#[test]
fn shared_send_picker_lists_live_files_and_folders_with_parent_paths_without_sending() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let work = storage::create_folder(&mut vault, None, "Work").unwrap();
        let personal = storage::create_folder(&mut vault, None, "Personal").unwrap();
        let empty = storage::create_folder(&mut vault, Some(work), "Empty").unwrap();
        let selected_file = storage::import_file(
            &ctx.air,
            &mut vault,
            Some(work),
            "notes.txt",
            b"Work notes".to_vec(),
        )
        .unwrap();
        storage::import_file(
            &ctx.air,
            &mut vault,
            Some(personal),
            "notes.txt",
            b"Personal notes".to_vec(),
        )
        .unwrap();
        storage::import_file(&ctx.air, &mut vault, None, "overview.txt", b"Root".to_vec()).unwrap();
        let discarded = storage::import_file(
            &ctx.air,
            &mut vault,
            Some(work),
            "discarded.txt",
            b"Discarded".to_vec(),
        )
        .unwrap();
        storage::trash(&mut vault, discarded).unwrap();
        let removed_folder = storage::create_folder(&mut vault, None, "Removed folder").unwrap();
        storage::import_file(
            &ctx.air,
            &mut vault,
            Some(removed_folder),
            "hidden-descendant.txt",
            b"Hidden".to_vec(),
        )
        .unwrap();
        storage::trash(&mut vault, removed_folder).unwrap();
        let before = vault.pending().entries.clone();
        let picker = page_description(sharing_ui::send_picker(), &mut ctx, &theme);
        let titles: Vec<_> = page_body(&picker)
            .into_iter()
            .filter_map(|display| match display {
                Display::List { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .map(|item| item.title.as_str())
            .collect();
        assert_eq!(titles.len(), 6);
        assert_eq!(
            titles.iter().filter(|title| **title == "notes.txt").count(),
            2
        );
        for expected in ["Work", "Personal", "Empty", "overview.txt"] {
            assert!(
                titles.contains(&expected),
                "Missing selectable {expected}: {titles:?}"
            );
        }
        for excluded in ["discarded.txt", "Removed folder", "hidden-descendant.txt"] {
            assert!(
                !titles.contains(&excluded),
                "Trashed items cannot be sent: {titles:?}"
            );
        }
        let mut rendered = Page::Static(Box::new(picker)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        for expected in ["Share files", "Files / Work", "Files / Personal", "Folder"] {
            assert!(
                visible.contains(expected),
                "Missing picker context {expected}: {visible}"
            );
        }
        // Both kinds open contact selection, with manual Paste/QR entry available.
        // Building either page must not send or invoke a native picker.
        for id in [selected_file, empty] {
            let flow = sharing_ui::send_flow(&mut ctx, vault.clone(), id, &theme);
            assert_eq!(flow.0.len(), 1);
            let selector = page_description(flow.0.into_iter().next().unwrap(), &mut ctx, &theme);
            assert_eq!(
                cta_action_labels(&selector),
                vec!["New contact", "Create link"]
            );
            let form = manual_recipient_page(selector, &mut ctx, &theme);
            let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("New contact"));
        }
        assert_eq!(vault.pending().entries, before);
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn collaborative_home_and_invitation_pages_render_without_joining_or_sharing() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let invited = shared_fixture(&ctx, &air, true, true, MemberStatus::Invited);
        let received = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let owned = shared_fixture(&ctx, &air, false, true, MemberStatus::Invited);
        for (space, created_at) in [(&invited, 10), (&owned, 20), (&received, 30)] {
            let mut state = space.pending().clone();
            state.created_at = created_at;
            stored_fixture(&ctx, &air, space.id(), state);
        }
        transfer_fixture(&ctx, &air, true, false, true, TransferStatus::Pending);
        let file = received.pending().root;
        drafts::save(
            &ctx.air,
            received.id(),
            file,
            "received-note.txt",
            received.pending().content_hash(file).unwrap(),
            b"Saved edit".to_vec(),
        )
        .unwrap();
        let before = invited.pending().members.clone();
        let root = page_description(sharing_ui::home(), &mut ctx, &theme);
        assert_eq!(
            page_body(&root).len(),
            1,
            "Shared shows exactly one file list, with utilities in its Info header"
        );
        let Display::List { label, items, .. } = page_body(&root).into_iter().next().unwrap()
        else {
            panic!("Shared must be a file list");
        };
        assert_eq!(label, &None);
        assert_eq!(
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["received-note.txt", "owned-folder", "received-folder"]
        );
        assert!(items[2].badge);
        assert!(!items[0].badge);
        assert_eq!(primary_label(&root), Some("Send"));
        assert!(matches!(&root, PageType::Content {
                bumper: Bumper::Footer(chk::design_system::Footer::Home {
                    second: Some((label, _)), ..
                }), ..
            } if label == "Receive"));
        let (icon, options) = header_description(root.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        let options_visible = options;
        assert!(
            options_visible.contains("Saved edits") && options_visible.contains("Older copies")
        );
        assert!(!options_visible.contains("Open invitation"));
        let mut root = Page::Static(Box::new(root)).build_root(&mut ctx, &theme);
        let visible = text(&draw(root.as_mut(), &mut ctx));
        for expected in [
            "received-folder",
            "received-note.txt",
            "owned-folder",
            "Invitation",
            "Read & write",
            "From ",
        ] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        for removed in [
            "Received",
            "Shared by you",
            "Saved edits",
            "Older copies",
            "2 people",
        ] {
            assert!(
                !visible.contains(removed),
                "Main file list contains an obsolete section: {visible}"
            );
        }
        let page = page_description(
            sharing_ui::invitation_detail(invited.clone()),
            &mut ctx,
            &theme,
        );
        assert_eq!(primary_label(&page), Some("Join shared folder"));
        assert!(
            !page_body(&page)
                .into_iter()
                .any(|item| matches!(item, Display::Actions { .. }))
        );
        let (icon, info) = header_description(page.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        assert!(has_button_label(&info, "Decline invitation"));
        assert!(has_button_label(&info, "Copy"));
        assert!(
            info.contains("Shared by") && info.contains(&peer().to_string()),
            "Sender attribution stays available: {info}"
        );
        assert!(!info.contains("People with access"));
        for participant_status in ["Owner", "Joined", "Invited"] {
            assert!(
                !info.lines().any(|line| line == participant_status),
                "Sharing information must not list participants: {info}"
            );
        }
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Read & write access"));
        assert!(visible.contains("Everyone with access can edit this folder"));
        assert_eq!(invited.pending().members, before);
        assert!(!invited.pending().can_write(me()));
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(ctx.air.instances::<FileBlob>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn joining_shared_access_creates_only_a_shortcut_and_opens_the_live_item() {
    for mobile in [true, false] {
        for folder in [true, false] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let space = shared_fixture(&ctx, &air, true, folder, MemberStatus::Invited);
            let page = page_description(
                sharing_ui::invitation_detail(space.clone()),
                &mut ctx,
                &theme,
            );
            let PageType::Content {
                bumper: Bumper::Custom { action, .. },
                ..
            } = page
            else {
                panic!("The invitation needs a primary Join action");
            };
            (action.get())(&mut ctx, &theme);
            assert_eq!(
                space.pending().members.get(&me()),
                Some(&MemberStatus::Active)
            );
            assert!(space.pending().can_write(me()));
            let vault = storage::open(&ctx.air);
            assert_eq!(vault.pending().entries.len(), 1);
            let mount = vault.pending().entries.values().next().unwrap().clone();
            assert!(
                matches!(mount.kind, storage::EntryKind::Shared {space:id,folder:is_folder} if id == space.id() && is_folder == folder)
            );
            assert!(
                ctx.air.instances::<FileBlob>().list().is_empty(),
                "Joining must not copy shared bytes into private files"
            );
            let page = page_description(
                sharing_ui::invitation_detail(space.clone()),
                &mut ctx,
                &theme,
            );
            assert_eq!(
                primary_label(&page),
                Some(if folder {
                    "Open shared folder"
                } else {
                    "Open shared file"
                })
            );
            let (_, info) = header_description(page, &mut ctx, &theme);
            assert!(info.contains("Files shortcut"));
            let root = space.pending().root;
            let live = page_description(
                crate::collaboration_ui::item(space.clone(), root),
                &mut ctx,
                &theme,
            );
            assert_eq!(primary_label(&live), folder.then_some("Add files"));
            if !folder {
                assert!(is_loading_preview(&live));
                assert_eq!(
                    header_description(live.clone(), &mut ctx, &theme).0,
                    Icons::Info
                );
            }
            let mut rendered = Page::Static(Box::new(live)).build(&mut ctx, &theme, None, 1);
            assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("received-note.txt"));
            collaboration::join(&ctx.air, &mut vault.clone(), &mut space.clone()).unwrap();
            assert_eq!(
                vault.pending().entries.len(),
                1,
                "Reopening must reuse the same shortcut"
            );
            assert!(ctx.air.instances::<FileBlob>().list().is_empty());
            assert!(
                air.1.lock().unwrap().is_none(),
                "Joining must not forward access to another identity"
            );
        }
    }
}

#[test]
fn collaborative_share_and_invite_forms_are_pure_until_continue() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let id = storage::create_folder(&mut vault, None, "Project").unwrap();
        let before = vault.pending().clone();
        let selector = page_description(
            sharing_ui::send_flow(&mut ctx, vault.clone(), id, &theme)
                .0
                .remove(0),
            &mut ctx,
            &theme,
        );
        let mut rendered =
            Page::Static(Box::new(selector.clone())).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Share access") && has_button_label(&visible, "New contact"));
        assert!(has_button_label(&visible, "Create link"));
        assert_eq!(
            cta_action_labels(&selector),
            vec!["New contact", "Create link"]
        );
        assert!(!visible.contains("No items yet"));
        let manual = manual_recipient_page(selector.clone(), &mut ctx, &theme);
        let mut rendered = Page::Static(Box::new(manual.clone())).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            has_button_label(&visible, "Paste clipboard")
                && has_button_label(&visible, "Scan QR code")
        );
        assert_eq!(*vault.pending(), before);
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        ctx.hardware = hardware::Hardware::new(TestHardware {
            clipboard: Some(Invitation::Contact(peer()).url()),
            ..TestHardware::default()
        });
        let mut app = <FigFiles as prism::Application>::new(&mut ctx);
        draw(&mut app, &mut ctx);
        browser::show(&mut ctx, &theme, Page::Static(Box::new(selector)));
        tap_text(&mut app, &mut ctx, "New contact");
        tap_text(&mut app, &mut ctx, "Paste clipboard");
        tap_text(&mut app, &mut ctx, "Continue");
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(visible.contains("Share access"));
        assert!(Profile::try_from_name(&mut ctx, peer()).is_some());
        let selection = sharing_ui::send_flow(&mut ctx, vault.clone(), id, &theme)
            .0
            .remove(0);
        let selection = page_description(selection, &mut ctx, &theme);
        let PageType::Form {
            item: chk::Input::Selection(selector),
            on_submit: Some(mut share),
            ..
        } = selection
        else {
            panic!("A saved contact enables CHK's normal sharing selector");
        };
        assert!(
            selector
                .items
                .iter()
                .any(|(_, key)| key == &peer().to_string())
        );
        assert!(matches!(
            share(
                &mut ctx,
                &form_value("recipients", chk::State::Selection(vec![]))
            ),
            chk::FormComplete::Error(_)
        ));
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
        let space = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        let members = space.pending().members.clone();
        let selector = page_description(
            sharing_ui::invite_flow(&mut ctx, space.clone(), &theme)
                .0
                .remove(0),
            &mut ctx,
            &theme,
        );
        let mut rendered =
            Page::Static(Box::new(selector.clone())).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Share access"));
        assert!(has_button_label(
            &text(&draw(rendered.as_mut(), &mut ctx)),
            "Create link"
        ));
        let manual = manual_recipient_page(selector, &mut ctx, &theme);
        let mut rendered = Page::Static(Box::new(manual)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            has_button_label(&visible, "Paste clipboard")
                && has_button_label(&visible, "Scan QR code")
        );
        assert_eq!(space.pending().members, members);
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn contact_search_selects_multiple_people_and_continue_shares_only_one_space() {
    for mobile in [true, false] {
        for mode in 0..3 {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let target = air::Secret::new().name();
            contacts::add(&ctx.air, target).unwrap();
            contacts::add(&ctx.air, peer()).unwrap();
            shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
            transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted);
            let mut vault = storage::open(&ctx.air);
            let existing =
                (mode == 2).then(|| shared_fixture(&ctx, &air, false, true, MemberStatus::Active));
            let id = if let Some(space) = &existing {
                space.pending().root
            } else if mode == 1 {
                storage::create_folder(&mut vault, None, "Selected folder").unwrap()
            } else {
                storage::import_file(
                    &ctx.air,
                    &mut vault,
                    None,
                    "selected.txt",
                    b"private bytes".to_vec(),
                )
                .unwrap()
            };
            let flow = if let Some(space) = &existing {
                sharing_ui::invite_flow(&mut ctx, space.clone(), &theme)
            } else {
                sharing_ui::send_flow(&mut ctx, vault.clone(), id, &theme)
            };
            let selector = page_description(flow.0[0].clone(), &mut ctx, &theme);
            assert!(
                matches!(&selector, PageType::Form {id, item: chk::Input::Selection(_), .. } if id == "recipients")
            );
            let selected = [target, peer()];
            let before_vault = vault.pending().clone();
            let before_count = ctx.air.instances::<SharedSpace>().list().len();
            let before_members = existing
                .as_ref()
                .map(|space| space.pending().members.clone());
            let book = ctx
                .air
                .instances::<AddressBook>()
                .list()
                .into_values()
                .next()
                .unwrap();
            let before_book = book.pending().clone();
            let mut app = <FigFiles as prism::Application>::new(&mut ctx);
            draw(&mut app, &mut ctx);
            browser::show(&mut ctx, &theme, Page::Static(Box::new(selector)));
            let visible = text(&draw(&mut app, &mut ctx));
            assert!(visible.contains("Share access") && has_button_label(&visible, "New contact"));
            for recipient in selected {
                let short = Profile::try_from_name(&mut ctx, recipient)
                    .unwrap()
                    .pending()
                    .username
                    .clone();
                assert_eq!(
                    visible.lines().filter(|line| *line == short).count(),
                    1,
                    "Saved and history contacts must be deduplicated: {visible}"
                );
            }
            for recipient in selected {
                let short = Profile::try_from_name(&mut ctx, recipient)
                    .unwrap()
                    .pending()
                    .username
                    .clone();
                tap_text(&mut app, &mut ctx, &short);
                let visible = text(&draw(&mut app, &mut ctx));
                assert!(visible.contains(&short) && visible.contains("Share access"));
                assert!(
                    !visible.contains("Read & write access"),
                    "Search selection must not open an extra confirmation"
                );
            }
            assert_eq!(*vault.pending(), before_vault);
            assert_eq!(
                ctx.air.instances::<SharedSpace>().list().len(),
                before_count
            );
            if let Some(space) = &existing {
                assert_eq!(Some(space.pending().members.clone()), before_members);
            }
            assert_eq!(*book.pending(), before_book);
            assert!(ctx.air.instances::<LinkJob>().list().is_empty());
            assert!(
                air.3
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|key| key.0 != TypeId::of::<DeliverLink>())
            );
            assert!(
                air.1.lock().unwrap().is_none(),
                "Selection must not grant access before Continue"
            );
            *air.1.lock().unwrap() = Some(Vec::new());
            tap_text(&mut app, &mut ctx, "Continue");
            let shared_id = if let Some(space) = &existing {
                space.id()
            } else {
                match vault.pending().get(id).unwrap().kind {
                    storage::EntryKind::Shared { space, .. } => space,
                    _ => panic!("Continue must promote the selected private item"),
                }
            };
            let shared = collaboration::get(&ctx.air, shared_id).unwrap();
            assert_eq!(
                shared.pending().members.get(&target),
                Some(&MemberStatus::Invited)
            );
            assert!(shared.pending().members.contains_key(&peer()));
            let mut actual = air.1.lock().unwrap().clone().unwrap();
            actual.sort();
            let mut expected = selected
                .map(|name| (SharedSpace::id(), shared_id, name))
                .to_vec();
            expected.sort();
            assert_eq!(
                actual, expected,
                "Only one shared capability is sent to each selected person"
            );
            assert_eq!(
                ctx.air.instances::<SharedSpace>().list().len(),
                before_count + usize::from(existing.is_none())
            );
            assert_eq!(*book.pending(), before_book);
        }
    }
}

#[test]
fn new_contact_returns_to_search_with_previous_and_new_recipients_selected() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        contacts::add(&ctx.air, peer()).unwrap();
        let mut vault = storage::open(&ctx.air);
        let file =
            storage::import_file(&ctx.air, &mut vault, None, "draft.txt", b"private".to_vec())
                .unwrap();
        let flow = sharing_ui::send_flow(&mut ctx, vault.clone(), file, &theme);
        let selector = page_description(flow.0[0].clone(), &mut ctx, &theme);
        let new_contact = manual_recipient_page(selector.clone(), &mut ctx, &theme);
        let PageType::Form {
            on_submit: Some(mut submit),
            ..
        } = new_contact
        else {
            unreachable!()
        };
        let mut app = <FigFiles as prism::Application>::new(&mut ctx);
        draw(&mut app, &mut ctx);
        browser::show(&mut ctx, &theme, Page::Static(Box::new(selector)));
        let short_peer = Profile::try_from_name(&mut ctx, peer())
            .unwrap()
            .pending()
            .username
            .clone();
        tap_text(&mut app, &mut ctx, &short_peer);
        tap_text(&mut app, &mut ctx, "New contact");
        assert!(has_button_label(
            &text(&draw(&mut app, &mut ctx)),
            "Paste clipboard"
        ));
        // The standard CHK Back icon sits immediately before the header title.
        tap_text_offset(&mut app, &mut ctx, "New contact", (-40.0, 8.0));
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(
            visible.contains("Share access") && visible.contains(&short_peer),
            "Back must preserve the selected contact: {visible}"
        );
        assert!(!has_button_label(&visible, "Paste clipboard"));
        tap_text(&mut app, &mut ctx, "New contact");
        let before_count = air.0.lock().unwrap().len();
        for invalid in ["bad-name".to_string(), "03".to_string()] {
            submit(&mut ctx, &form_value("airname", chk::State::Text(invalid)));
            draw(&mut app, &mut ctx);
            assert_eq!(air.0.lock().unwrap().len(), before_count);
            assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        }
        let added = air::Secret::new().name();
        submit(
            &mut ctx,
            &form_value("airname", chk::State::Text(format!("  {added}\n"))),
        )
        .run(&mut ctx, &theme);
        let visible = text(&draw(&mut app, &mut ctx));
        assert!(
            visible.contains("Share access") && visible.contains(&short_peer),
            "Returning must keep the first selected chip: {visible}"
        );
        assert!(!has_button_label(&visible, "Paste clipboard"));
        let book = ctx
            .air
            .instances::<AddressBook>()
            .list()
            .into_values()
            .next()
            .unwrap();
        assert_eq!(
            book.pending().contacts.len(),
            1,
            "New contacts use CHK profiles, without rewriting the legacy book"
        );
        assert!(Profile::try_from_name(&mut ctx, added).is_some());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(
            air.1.lock().unwrap().is_none(),
            "Saving a contact must only update its private CHK profile"
        );
        *air.1.lock().unwrap() = Some(Vec::new());
        tap_text(&mut app, &mut ctx, "Continue");
        let spaces = ctx.air.instances::<SharedSpace>().list();
        assert_eq!(spaces.len(), 1);
        let shared = spaces.into_values().next().unwrap();
        assert_eq!(
            shared.pending().members.get(&peer()),
            Some(&MemberStatus::Invited)
        );
        assert_eq!(
            shared.pending().members.get(&added),
            Some(&MemberStatus::Invited)
        );
        let mut actual = air.1.lock().unwrap().clone().unwrap();
        actual.sort();
        let mut expected = vec![
            (SharedSpace::id(), shared.id(), peer()),
            (SharedSpace::id(), shared.id(), added),
        ];
        expected.sort();
        assert_eq!(
            actual, expected,
            "Continue must include both old and auto-selected new recipients"
        );
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
    }
}

#[test]
fn multi_recipient_helpers_reject_whole_invalid_batches_before_sharing_or_promotion() {
    let (ctx, air) = context_with_air(false);
    let mut vault = storage::open(&ctx.air);
    let file = storage::import_file(
        &ctx.air,
        &mut vault,
        None,
        "private.txt",
        b"original".to_vec(),
    )
    .unwrap();
    let before = vault.pending().clone();
    let records = air.0.lock().unwrap().len();
    let excess: Vec<_> = (0..128).map(|_| air::Secret::new().name()).collect();
    for recipients in [vec![], vec![me()], vec![peer(), me()], excess] {
        assert!(collaboration::share_with(&ctx.air, &mut vault, file, recipients).is_err());
        assert_eq!(*vault.pending(), before);
        assert_eq!(air.0.lock().unwrap().len(), records);
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
    let second = air::Secret::new().name();
    *air.1.lock().unwrap() = Some(Vec::new());
    let mut space =
        collaboration::share_with(&ctx.air, &mut vault, file, vec![peer(), second, peer()])
            .unwrap();
    assert_eq!(ctx.air.instances::<SharedSpace>().list().len(), 1);
    let mut sent = air.1.lock().unwrap().clone().unwrap();
    sent.sort();
    let mut expected = vec![
        (SharedSpace::id(), space.id(), peer()),
        (SharedSpace::id(), space.id(), second),
    ];
    expected.sort();
    assert_eq!(
        sent, expected,
        "One shared instance is sent once per distinct recipient"
    );
    let members = space.pending().members.clone();
    let source = vault.pending().clone();
    *air.1.lock().unwrap() = None;
    // This batch is individually within the recipient cap but would push the
    // existing owner + two members beyond 128; the entire batch must fail.
    let over_remaining: Vec<_> = (0..126).map(|_| air::Secret::new().name()).collect();
    for recipients in [vec![], vec![peer(), me()], over_remaining] {
        assert!(collaboration::invite_many(&ctx.air, &mut space, recipients).is_err());
        assert_eq!(space.pending().members, members);
        assert_eq!(*vault.pending(), source);
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn shared_access_helpers_keep_both_identities_on_the_same_editable_tree() {
    let source = MemoryAir::default();
    *source.1.lock().unwrap() = Some(Vec::new());
    let source_air = prism::air::Air::new(source.clone());
    let mut source_vault = storage::open(&source_air);
    let folder = storage::create_folder(&mut source_vault, None, "Project").unwrap();
    let file = storage::import_file(
        &source_air,
        &mut source_vault,
        Some(folder),
        "draft.txt",
        b"Original shared contents".to_vec(),
    )
    .unwrap();
    let private = storage::import_file(
        &source_air,
        &mut source_vault,
        None,
        "private.txt",
        b"Not shared".to_vec(),
    )
    .unwrap();
    let mut source_space =
        collaboration::share(&source_air, &mut source_vault, folder, peer()).unwrap();
    assert!(matches!(
        source_vault.pending().get(folder).unwrap().kind,
        storage::EntryKind::Shared { space, folder: true } if space == source_space.id()
    ));
    assert!(source_vault.pending().get(file).is_none());
    assert!(source_vault.pending().get(private).is_some());
    assert!(source_space.pending().tree.get(private).is_none());
    assert_eq!(
        source.1.lock().unwrap().as_deref().unwrap(),
        &[(SharedSpace::id(), source_space.id(), peer())],
        "Only the shared contract may leave the private library"
    );

    let recipient = MemoryAir::with_name(peer());
    let shared_state = source
        .0
        .lock()
        .unwrap()
        .get(&(SharedSpace::id(), source_space.id()))
        .unwrap()
        .downcast_ref::<Stored<SharedSpace>>()
        .unwrap()
        .state
        .clone();
    recipient.0.lock().unwrap().insert(
        (SharedSpace::id(), source_space.id()),
        Box::new(Stored {
            id: source_space.id(),
            state: shared_state,
            signer: peer(),
            shares: recipient.1.clone(),
        }),
    );
    let recipient_air = prism::air::Air::new(recipient.clone());
    let mut recipient_space = collaboration::get(&recipient_air, source_space.id()).unwrap();
    let mut recipient_vault = storage::open(&recipient_air);
    let shortcut =
        collaboration::join(&recipient_air, &mut recipient_vault, &mut recipient_space).unwrap();
    assert_eq!(recipient_vault.pending().entries.len(), 1);
    assert_eq!(
        collaboration::join(&recipient_air, &mut recipient_vault, &mut recipient_space).unwrap(),
        shortcut
    );
    assert!(recipient_air.instances::<FileBlob>().list().is_empty());

    collaboration::rename(&recipient_air, &mut recipient_space, file, "reviewed.txt").unwrap();
    let expected = recipient_space.pending().content_hash(file).unwrap();
    collaboration::replace_file(
        &recipient_air,
        &mut recipient_space,
        file,
        expected,
        b"Edited by recipient".to_vec(),
    )
    .unwrap();
    assert_eq!(
        source_space.pending().tree.get(file).unwrap().name,
        "reviewed.txt"
    );
    assert_eq!(
        source_space.pending().read_file(file).unwrap(),
        b"Edited by recipient"
    );

    let added = collaboration::import_file(
        &source_air,
        &mut source_space,
        folder,
        "added.bin",
        vec![0, 255, 128, 0],
    )
    .unwrap();
    assert_eq!(
        recipient_space.pending().read_file(added).unwrap(),
        vec![0, 255, 128, 0]
    );
    assert_eq!(recipient_vault.pending().entries.len(), 1);
    assert!(recipient_air.instances::<FileBlob>().list().is_empty());
    assert!(recipient.1.lock().unwrap().is_none());
}

#[test]
fn shared_text_editor_preserves_the_complete_draft_when_an_edit_races() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut space = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let file = space.pending().root;
        let original = format!(
            "{}\nEND OF ORIGINAL",
            "Original multiline text\n".repeat(800)
        );
        let expected = space.pending().content_hash(file).unwrap();
        space
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: original.as_bytes().to_vec(),
            })
            .pending()
            .clone()
            .unwrap();
        let flow = crate::collaboration_ui::edit_text_flow(space.clone(), file, &theme);
        let page = page_description(flow.0.into_iter().next().unwrap(), &mut ctx, &theme);
        let PageType::Edit {
            items, mut on_save, ..
        } = page.clone()
        else {
            panic!("Shared text uses a CHK edit form");
        };
        assert!(
            matches!(&items[0].input, chk::Input::Text { preset: Some(value), .. } if value == &original)
        );
        assert_eq!(items[0].id, "content");
        assert!(original.len() > 16_000);
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Edit shared text"));
        assert!(
            drafts::list(&ctx.air).is_empty(),
            "Opening an editor is read-only"
        );

        for revision in 1..=2 {
            let saved = format!("{original}\nSave {revision}");
            assert!(matches!(
                on_save(
                    &mut ctx,
                    &form_value("content", chk::State::Text(saved.clone()))
                ),
                chk::FormComplete::None
            ));
            assert_eq!(space.pending().read_file(file).unwrap(), saved.as_bytes());
            assert!(
                space.pending().conflicts(file).is_empty(),
                "Saving again must use the editor's last saved revision"
            );
        }

        let expected = space.pending().content_hash(file).unwrap();
        space
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"A newer collaborative edit".to_vec(),
            })
            .pending()
            .clone()
            .unwrap();
        let submitted = format!("{original}\nMY COMPLETE UNSHORTENED EDIT");
        let result = on_save(
            &mut ctx,
            &form_value("content", chk::State::Text(submitted.clone())),
        );
        assert!(matches!(result, chk::FormComplete::Next(_)));
        assert_eq!(
            space.pending().read_file(file).unwrap(),
            b"A newer collaborative edit"
        );
        assert!(
            space
                .pending()
                .conflicts(file)
                .iter()
                .any(|edit| edit.bytes == submitted.as_bytes())
        );
        let backups = drafts::list(&ctx.air);
        assert_eq!(backups.len(), 3);
        let backup = backups
            .iter()
            .find(|draft| draft.pending().bytes == submitted.as_bytes())
            .unwrap();
        assert!(!backup.pending().resolved);
        let detail = page_description(
            crate::collaboration_ui::details(space.clone(), file),
            &mut ctx,
            &theme,
        );
        assert!(cta_action_labels(&detail).contains(&"Edits to review"));
        let mut detail = Page::Static(Box::new(detail)).build(&mut ctx, &theme, None, 1);
        assert!(has_button_label(
            &text(&draw(detail.as_mut(), &mut ctx)),
            "Edits to review"
        ));
        let mut conflicts =
            crate::collaboration_ui::conflicts_page(space, file).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(conflicts.as_mut(), &mut ctx)).contains("Saved edit 1"));
        assert!(
            air.1.lock().unwrap().is_none(),
            "Drafts must remain private"
        );
    }
}

#[test]
fn saved_edits_remain_visible_after_sync_and_retry_keeps_a_private_backup() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let space = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let file = space.pending().root;
        let expected = space.pending().content_hash(file).unwrap();
        let bytes = space.pending().read_file(file).unwrap();
        let draft = drafts::save(
            &ctx.air,
            space.id(),
            file,
            "received-note.txt",
            expected,
            bytes.clone(),
        )
        .unwrap();
        assert!(
            !drafts::recoverable(&ctx.air, &draft),
            "The mock already confirms these bytes"
        );
        let home = page_description(sharing_ui::home(), &mut ctx, &theme);
        let (icon, options) = header_description(home, &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        assert!(options.contains("Saved edits"));
        assert!(!options.contains("Open invitation"));
        let mut list = crate::collaboration_ui::saved_edits().build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(list.as_mut(), &mut ctx));
        assert!(visible.contains("received-note.txt") && visible.contains("Private saved edit"));
        assert!(
            !draft.pending().resolved,
            "Passive rendering must not dismiss backups"
        );
        let page = page_description(
            crate::collaboration_ui::saved_edit(draft.clone()),
            &mut ctx,
            &theme,
        );
        assert_eq!(primary_label(&page), Some("Retry saved edit"));
        let (icon, discard) = header_description(page.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Delete);
        assert!(has_button_label(&discard, "Discard saved edit"));
        let mut rendered = Page::Static(Box::new(page.clone())).build(&mut ctx, &theme, None, 1);
        assert!(has_button_label(
            &text(&draw(rendered.as_mut(), &mut ctx)),
            "Export saved edit"
        ));
        let PageType::Content {
            bumper:
                Bumper::Custom {
                    action,
                    secondary: Some(_),
                    ..
                },
            ..
        } = page
        else {
            panic!("Retry must be the saved edit's primary action");
        };
        (action.get())(&mut ctx, &theme);
        assert!(draft.pending().resolved);
        let backups = drafts::list(&ctx.air);
        let remaining: Vec<_> = backups
            .into_iter()
            .filter(|draft| !draft.pending().resolved)
            .collect();
        assert_eq!(
            remaining.len(),
            1,
            "A retry keeps its own backup before dismissing the old one"
        );
        assert_eq!(remaining[0].pending().bytes, bytes);
        assert_eq!(space.pending().read_file(file).unwrap(), bytes);
        drafts::resolve(&mut remaining[0].clone()).unwrap();
        let home = page_description(sharing_ui::home(), &mut ctx, &theme);
        assert!(
            matches!(
                home,
                PageType::Content {
                    header: chk::design_system::Header::Home { action: None, .. },
                    ..
                }
            ),
            "Resolving all saved edits should remove the empty Shared options header"
        );
        assert!(
            air.1.lock().unwrap().is_none(),
            "Recovery drafts must not be shared"
        );
    }
}

#[test]
fn shared_root_lists_incoming_and_outgoing_files_and_folders_without_accepting_them() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        for incoming in [true, false] {
            for folder in [false, true] {
                transfer_fixture(&ctx, &air, incoming, folder, true, TransferStatus::Pending);
            }
        }
        let mut page = sharing_ui::legacy_home().build_root(&mut ctx, &theme());
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in [
            "received-note.txt",
            "received-folder",
            "sent-note.txt",
            "sent-folder",
            "Ready to save",
            "Awaiting recipient",
        ] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        assert!(storage::open(&ctx.air).pending().entries.is_empty());
        assert!(
            ctx.air
                .instances::<Transfer>()
                .list()
                .values()
                .all(|instance| instance.pending().status == TransferStatus::Pending)
        );
    }
}

#[test]
fn shared_details_distinguish_incoming_outgoing_and_transfer_states() {
    for mobile in [true, false] {
        for incoming in [true, false] {
            for folder in [false, true] {
                for (ready, status, received_status, sent_status) in [
                    (false, TransferStatus::Pending, "Receiving", "Preparing"),
                    (
                        true,
                        TransferStatus::Pending,
                        "Ready to save",
                        "Awaiting recipient",
                    ),
                    (
                        true,
                        TransferStatus::Accepted,
                        "Saved to Files",
                        "Saved by recipient",
                    ),
                    (true, TransferStatus::Declined, "Declined", "Declined"),
                ] {
                    let (mut ctx, air) = context_with_air(mobile);
                    let instance = transfer_fixture(&ctx, &air, incoming, folder, ready, status);
                    let title = instance.pending().title().to_string();
                    let mut page =
                        sharing_ui::detail(instance.clone()).build(&mut ctx, &theme(), None, 1);
                    let visible = text(&draw(page.as_mut(), &mut ctx));
                    for expected in [
                        "Older copy",
                        &title,
                        if incoming { "From" } else { "To" },
                        if incoming {
                            received_status
                        } else {
                            sent_status
                        },
                    ] {
                        assert!(visible.contains(expected), "Missing {expected}: {visible}");
                    }
                    assert_eq!(
                        visible.contains("Save to Files"),
                        incoming && ready && status == TransferStatus::Pending,
                        "Only a ready incoming transfer offers saving: {visible}"
                    );
                    assert!(
                        !visible.contains("Decline\n") && !visible.ends_with("Decline"),
                        "Declining belongs in transfer information, not the main detail: {visible}"
                    );
                    assert_eq!(instance.pending().status, status);
                    assert!(storage::open(&ctx.air).pending().entries.is_empty());
                }
            }
        }
    }
}

#[test]
fn receiving_controls_offer_paste_scan_and_own_qr_and_require_confirmation_to_join() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let home = page_description(sharing_ui::home(), &mut ctx, &theme);
        assert_eq!(primary_label(&home), Some("Send"));
        assert!(
            !page_body(&home)
                .into_iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let PageType::Content {
            header: chk::design_system::Header::Home { action: None, .. },
            bumper:
                Bumper::Footer(chk::design_system::Footer::Home {
                    first: bumper_a,
                    second: bumper_b,
                }),
            ..
        } = home
        else {
            panic!("Shared must be a CHK root");
        };
        assert_eq!(
            bumper_a.as_ref().map(|(label, _)| label.as_str()),
            Some("Send")
        );
        assert_eq!(
            bumper_b.as_ref().map(|(label, _)| label.as_str()),
            Some("Receive")
        );

        let receive = page_description(
            invitations::receive_flow(me()).0.remove(0),
            &mut ctx,
            &theme,
        );
        let PageType::Form {
            id,
            title,
            item:
                chk::Input::Text {
                    actions: Some(actions),
                    ..
                },
            on_submit: Some(mut submit),
            ..
        } = receive.clone()
        else {
            panic!("Receive must use CHK's invitation text form");
        };
        assert_eq!(id, "invitation");
        assert_eq!(title, "Receive");
        assert_eq!(actions.len(), 3);
        assert_text_actions(&actions[..2]);
        assert_eq!(
            actions[2].presentation().label,
            chk::ActionItem::format_label("Show my QR code")
        );
        assert_eq!(actions[2].presentation().icon, Icons::QrCode);
        assert!(format!("{:?}", actions[2]).contains(&format!(
            "QRCode {{ data: {:?}",
            Invitation::Contact(me()).url()
        )));
        let mut rendered = Page::Static(Box::new(receive)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        for label in ["Paste clipboard", "Scan QR code", "Show my QR code"] {
            assert!(
                has_button_label(&visible, label),
                "Missing {label}: {visible}"
            );
        }
        assert!(matches!(
            submit(
                &mut ctx,
                &form_value(
                    "invitation",
                    chk::State::Text("https://example.com/not-an-invite".into())
                )
            ),
            chk::FormComplete::Error(_)
        ));
        let file = FileInvitation {
            publisher: peer(),
            space: Id::random(),
            token: Id::random(),
        };
        let completed = submit(
            &mut ctx,
            &form_value("invitation", chk::State::Text(Invitation::File(file).url())),
        );
        let chk::FormComplete::Action(chk::Action::Flow { flow }) = completed else {
            panic!("Submitting a file invitation should navigate to a confirmation");
        };
        let confirmation = page_description(flow.0[0].clone(), &mut ctx, &theme);
        assert_eq!(primary_label(&confirmation), Some("Join shared item"));
        assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.3.lock().unwrap().is_empty());
        assert!(air.1.lock().unwrap().is_none());
        let PageType::Content {
            bumper: Bumper::Custom { action, .. },
            ..
        } = confirmation
        else {
            panic!("Receiving requires the explicit Join action");
        };
        (action.get())(&mut ctx, &theme);
        let jobs = ctx.air.instances::<LinkJob>().list();
        assert_eq!(jobs.len(), 1);
        let job = jobs.into_values().next().unwrap();
        assert_eq!(job.pending().invitation, file);
        assert_eq!(job.pending().purpose, Purpose::Join);
        assert_eq!(job.pending().status, Status::Pending);
        assert_eq!(
            *air.3.lock().unwrap(),
            vec![prism::service::Key::new::<DeliverLink>(me(), &job)]
        );
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
    }
}

#[test]
fn transfer_primary_actions_follow_state_and_decline_stays_in_information() {
    for mobile in [true, false] {
        for incoming in [true, false] {
            for (ready, status) in [
                (false, TransferStatus::Pending),
                (true, TransferStatus::Pending),
                (true, TransferStatus::Accepted),
                (true, TransferStatus::Declined),
            ] {
                let (mut ctx, air) = context_with_air(mobile);
                let theme = theme();
                let instance = transfer_fixture(&ctx, &air, incoming, false, ready, status);
                let detail =
                    page_description(sharing_ui::detail(instance.clone()), &mut ctx, &theme);
                assert_eq!(
                    primary_label(&detail),
                    (incoming && ready && status == TransferStatus::Pending)
                        .then_some("Save to Files")
                );
                if primary_label(&detail).is_none() {
                    assert!(
                        matches!(
                            &detail,
                            PageType::Content {
                                bumper: Bumper::None,
                                ..
                            }
                        ),
                        "CHK's Done bumper suppresses Info; actionless details must keep the header with no bumper"
                    );
                }
                assert!(
                    !page_body(&detail)
                        .into_iter()
                        .any(|display| matches!(display, Display::Actions { .. }))
                );
                let (icon, information) = header_description(detail, &mut ctx, &theme);
                assert_eq!(icon, Icons::Info);
                assert!(has_button_label(
                    &information,
                    if incoming && status == TransferStatus::Pending {
                        "Decline"
                    } else {
                        "Copy"
                    },
                ));
                assert_eq!(instance.pending().status, status);
                assert!(storage::open(&ctx.air).pending().entries.is_empty());
            }
        }
    }
}

#[test]
fn a_saved_transfer_offers_opening_its_existing_copy_without_saving_again() {
    for folder in [false, true] {
        let (mut ctx, air) = context_with_air(false);
        let theme = theme();
        let mut instance =
            transfer_fixture(&ctx, &air, true, folder, true, TransferStatus::Pending);
        let mut vault = storage::open(&ctx.air);
        let root = sharing::accept(&ctx.air, &mut vault, &mut instance).unwrap();
        let entries = vault.pending().entries.clone();
        let page = page_description(sharing_ui::detail(instance.clone()), &mut ctx, &theme);
        assert_eq!(primary_label(&page), Some("Open in Files"));
        let PageType::Content {
            bumper:
                Bumper::Custom {
                    action: chk::Action::Flow { flow },
                    ..
                },
            ..
        } = page
        else {
            panic!("Open in Files must navigate to the saved item");
        };
        assert_eq!(flow.0.len(), 1);
        let destination = page_description(flow.0.into_iter().next().unwrap(), &mut ctx, &theme);
        assert_eq!(
            primary_label(&destination),
            folder.then_some("Add files"),
            "Folders open their contents, while files automatically start their preview"
        );
        if !folder {
            assert!(is_loading_preview(&destination));
            assert_eq!(
                header_description(destination.clone(), &mut ctx, &theme).0,
                Icons::Info
            );
        }
        let PageType::Content { header, .. } = &destination else {
            panic!("Saved items open as a CHK display page");
        };
        assert_eq!(
            header.title(),
            if folder {
                "received-folder"
            } else {
                "received-note.txt"
            }
        );
        let mut rendered = Page::Static(Box::new(destination)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            visible.contains("received-note.txt"),
            "The saved file or folder child must be visible: {visible}"
        );
        assert_eq!(vault.pending().entries, entries);
        assert!(vault.pending().get(root).is_some());
        assert_eq!(instance.pending().status, TransferStatus::Accepted);

        storage::trash(&mut vault, root).unwrap();
        let page = page_description(sharing_ui::detail(instance.clone()), &mut ctx, &theme);
        assert_eq!(
            primary_label(&page),
            None,
            "A trashed copy cannot be opened"
        );
        assert!(matches!(
            &page,
            PageType::Content {
                bumper: Bumper::None,
                ..
            }
        ));
        assert_eq!(header_description(page, &mut ctx, &theme).0, Icons::Info);
        storage::delete_permanently(&mut vault, root).unwrap();
        let page = page_description(sharing_ui::detail(instance), &mut ctx, &theme);
        assert_eq!(
            primary_label(&page),
            None,
            "A deleted copy cannot be opened"
        );
        assert!(matches!(
            &page,
            PageType::Content {
                bumper: Bumper::None,
                ..
            }
        ));
        assert_eq!(header_description(page, &mut ctx, &theme).0, Icons::Info);
    }
}

#[test]
fn invalid_incoming_manifests_are_hidden_and_cannot_offer_transfer_actions() {
    for mobile in [true, false] {
        for attacker_title in [
            "untrusted\u{0007}manifest.txt".to_string(),
            format!("untrusted-{}", "x".repeat(256)),
        ] {
            let (mut ctx, air) = context_with_air(mobile);
            transfer_fixture(&ctx, &air, true, false, true, TransferStatus::Pending);
            let init = TransferInit {
                recipient: me(),
                nonce: Id::hash(&attacker_title),
                items: vec![TransferItem {
                    parent: None,
                    name: attacker_title,
                    kind: TransferKind::Folder,
                }],
            };
            let id = Id::hash(&(peer(), &init));
            let transfer = Transfer::init(
                init,
                Metadata {
                    signer: peer(),
                    ..metadata()
                },
            );
            assert!(transfer.validate().is_err());
            air.0.lock().unwrap().insert(
                (Transfer::id(), id),
                Box::new(Stored {
                    id,
                    state: Arc::new(Mutex::new(transfer)),
                    signer: ctx.air.me(),
                    shares: air.1.clone(),
                }),
            );
            let instance = ctx.air.instances::<Transfer>().list().remove(&id).unwrap();
            let theme = theme();
            let mut home = sharing_ui::legacy_home().build_root(&mut ctx, &theme);
            let visible = text(&draw(home.as_mut(), &mut ctx));
            assert!(visible.contains("received-note.txt"));
            assert!(
                !visible.contains("untrusted"),
                "Unchecked sender metadata must not appear in Shared: {visible}"
            );
            let mut detail = sharing_ui::detail(instance.clone()).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(detail.as_mut(), &mut ctx));
            assert!(visible.contains("Invalid transfer"));
            for forbidden in ["untrusted", "Save to Files", "Decline"] {
                assert!(
                    !visible.contains(forbidden),
                    "Invalid transfers must not expose {forbidden}: {visible}"
                );
            }
            assert_eq!(instance.pending().status, TransferStatus::Pending);
            assert!(storage::open(&ctx.air).pending().entries.is_empty());
        }
    }
}

#[test]
fn send_and_accept_helpers_copy_only_the_selected_live_tree_with_owned_binary_contents() {
    let source = MemoryAir::default();
    // Only this workflow explicitly permits sharing. Rendering fixtures keep
    // None and still panic if any contract attempts to share a channel.
    *source.1.lock().unwrap() = Some(Vec::new());
    let source_air = prism::air::Air::new(source.clone());
    let mut source_vault = storage::open(&source_air);
    let private = storage::create_folder(&mut source_vault, None, "Private library").unwrap();
    let project = storage::create_folder(&mut source_vault, Some(private), "Project").unwrap();
    let nested = storage::create_folder(&mut source_vault, Some(project), "Nested").unwrap();
    storage::create_folder(&mut source_vault, Some(nested), "Empty").unwrap();
    let bytes = vec![0, 255, 128, 13, 10, 42, 0, 255];
    let binary = storage::import_file(
        &source_air,
        &mut source_vault,
        Some(nested),
        "payload.bin",
        bytes.clone(),
    )
    .unwrap();
    storage::import_file(
        &source_air,
        &mut source_vault,
        Some(project),
        "zero.bin",
        Vec::new(),
    )
    .unwrap();
    storage::import_file(
        &source_air,
        &mut source_vault,
        Some(private),
        "private-sibling.txt",
        b"Never shared".to_vec(),
    )
    .unwrap();
    let discarded = storage::import_file(
        &source_air,
        &mut source_vault,
        Some(project),
        "discarded.txt",
        b"Discarded".to_vec(),
    )
    .unwrap();
    storage::trash(&mut source_vault, discarded).unwrap();
    let hidden = storage::create_folder(&mut source_vault, Some(project), "Hidden").unwrap();
    storage::import_file(
        &source_air,
        &mut source_vault,
        Some(hidden),
        "hidden-descendant.txt",
        b"Hidden".to_vec(),
    )
    .unwrap();
    storage::trash(&mut source_vault, hidden).unwrap();

    let transfer = sharing::send(&source_air, &source_vault, project, peer()).unwrap();
    assert_eq!(
        *source.1.lock().unwrap(),
        Some(vec![(Transfer::id(), transfer.id(), peer())]),
        "Only the transfer channel may be shared; the vault and blobs stay private"
    );
    let snapshot = transfer.pending().clone();
    assert!(snapshot.is_ready());
    assert_eq!(snapshot.sender, me());
    assert_eq!(snapshot.recipient, peer());
    assert_eq!(snapshot.title(), "Project");
    assert_eq!(snapshot.item_count(), 5);
    assert_eq!(snapshot.total_size(), bytes.len() as u64);
    let by_name: HashMap<_, _> = snapshot
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.name.as_str(), (index as u32, item)))
        .collect();
    assert_eq!(by_name["Project"].1.parent, None);
    assert_eq!(by_name["Nested"].1.parent, Some(by_name["Project"].0));
    assert_eq!(by_name["Empty"].1.parent, Some(by_name["Nested"].0));
    assert_eq!(by_name["payload.bin"].1.parent, Some(by_name["Nested"].0));
    assert_eq!(by_name["zero.bin"].1.parent, Some(by_name["Project"].0));
    for excluded in [
        "Private library",
        "private-sibling.txt",
        "discarded.txt",
        "Hidden",
        "hidden-descendant.txt",
    ] {
        assert!(!by_name.contains_key(excluded));
    }
    storage::rename(&mut source_vault, binary, "source-renamed.bin").unwrap();

    // Simulate delivery of the sealed snapshot to a separate AIR identity.
    // Contract encryption/transport is exercised by the backend tests; here
    // actual app helpers must recreate private, recipient-owned blobs.
    let destination = MemoryAir::with_name(peer());
    destination.0.lock().unwrap().insert(
        (Transfer::id(), transfer.id()),
        Box::new(Stored {
            id: transfer.id(),
            state: Arc::new(Mutex::new(snapshot)),
            signer: peer(),
            shares: destination.1.clone(),
        }),
    );
    let recipient_air = prism::air::Air::new(destination.clone());
    let mut received = recipient_air
        .instances::<Transfer>()
        .list()
        .remove(&transfer.id())
        .unwrap();
    let mut recipient_vault = storage::open(&recipient_air);
    let existing = storage::create_folder(&mut recipient_vault, None, "Project").unwrap();
    let accepted = sharing::accept(&recipient_air, &mut recipient_vault, &mut received).unwrap();
    let copied = recipient_vault.pending().clone();
    assert_eq!(copied.owner, peer());
    assert_eq!(copied.get(existing).unwrap().name, "Project");
    assert_eq!(copied.get(accepted).unwrap().name, "Project (2)");
    assert_eq!(copied.get(accepted).unwrap().parent, None);
    assert_eq!(copied.entries.len(), 6);
    let child = |parent, name: &str| {
        copied
            .children(Some(parent))
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap()
    };
    let nested_copy = child(accepted, "Nested");
    let empty_copy = child(nested_copy.id, "Empty");
    assert!(empty_copy.is_folder());
    assert!(copied.children(Some(empty_copy.id)).is_empty());
    let binary_copy = child(nested_copy.id, "payload.bin");
    assert_eq!(
        storage::read_file(&recipient_air, &binary_copy).unwrap(),
        bytes
    );
    let zero_copy = child(accepted, "zero.bin");
    assert!(
        storage::read_file(&recipient_air, &zero_copy)
            .unwrap()
            .is_empty()
    );
    assert!(
        recipient_air
            .instances::<FileBlob>()
            .list()
            .values()
            .all(|blob| blob.pending().owner == peer())
    );
    assert_eq!(received.pending().status, TransferStatus::Accepted);
    let blob_count = recipient_air.instances::<FileBlob>().list().len();
    assert_eq!(
        sharing::accept(&recipient_air, &mut recipient_vault, &mut received).unwrap(),
        accepted
    );
    assert_eq!(recipient_vault.pending().entries, copied.entries);
    assert_eq!(
        recipient_air.instances::<FileBlob>().list().len(),
        blob_count
    );
    assert!(destination.1.lock().unwrap().is_none());
}

#[test]
fn accept_and_decline_helpers_preserve_existing_files_and_retry_without_duplicates() {
    let (ctx, air) = context_with_air(false);
    let mut vault = storage::open(&ctx.air);
    let existing = storage::import_file(
        &ctx.air,
        &mut vault,
        None,
        "received-note.txt",
        b"Keep the original".to_vec(),
    )
    .unwrap();
    let mut incoming = transfer_fixture(&ctx, &air, true, false, true, TransferStatus::Pending);
    let accepted = sharing::accept(&ctx.air, &mut vault, &mut incoming).unwrap();
    let copied = vault.pending().get(accepted).unwrap().clone();
    assert_eq!(copied.name, "received-note (2).txt");
    assert_eq!(copied.parent, None);
    assert_eq!(
        storage::read_file(&ctx.air, &copied).unwrap(),
        b"A shared note"
    );
    assert_eq!(
        storage::read_file(&ctx.air, vault.pending().get(existing).unwrap()).unwrap(),
        b"Keep the original"
    );
    assert_eq!(incoming.pending().status, TransferStatus::Accepted);
    let original_entries = vault.pending().entries.clone();
    let original_blobs = ctx.air.instances::<FileBlob>().list().len();
    assert_eq!(
        sharing::accept(&ctx.air, &mut vault, &mut incoming).unwrap(),
        accepted
    );
    assert_eq!(vault.pending().entries, original_entries);
    assert_eq!(ctx.air.instances::<FileBlob>().list().len(), original_blobs);

    let mut declined = transfer_fixture(&ctx, &air, true, true, true, TransferStatus::Pending);
    sharing::decline(&ctx.air, &mut declined).unwrap();
    assert_eq!(declined.pending().status, TransferStatus::Declined);
    assert!(sharing::accept(&ctx.air, &mut vault, &mut declined).is_err());
    assert_eq!(vault.pending().entries, original_entries);
    assert_eq!(ctx.air.instances::<FileBlob>().list().len(), original_blobs);
    assert!(air.1.lock().unwrap().is_none());
}

#[test]
fn receiving_name_validation_rejects_nicknames_invalid_keys_and_self() {
    assert_eq!(
        contacts::parse(&format!("  {}\n", peer()), me()),
        Ok(peer())
    );
    for invalid in [
        "",
        "alice",
        "orange:alice",
        "0279be667e",
        "0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        "02ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    ] {
        assert!(
            contacts::parse(invalid, me()).is_err(),
            "An invalid recipient must never be accepted: {invalid}"
        );
    }
    assert!(contacts::parse(&me().to_string(), me()).is_err());
}

#[test]
fn invitation_links_roundtrip_strictly_and_keep_bearer_tokens_out_of_debug() {
    let file = FileInvitation {
        publisher: peer(),
        space: "ab".repeat(32).parse().unwrap(),
        token: "cd".repeat(32).parse().unwrap(),
    };
    let contact = Invitation::Contact(peer());
    let shared = Invitation::File(file);
    let router = invitations::router();
    for invitation in [contact, shared] {
        let url = invitation.url();
        assert!(url.starts_with("https://therampstack.com/fig-files/invite.html#"));
        assert!(!url.contains('?'));
        assert_eq!(router.parse_invite(&url), Some(invitation));
        assert_eq!(router.parse_invite(&format!("  {url}\n")), Some(invitation));
        assert_eq!(
            router.parse_invite(
                &url.replace("https://therampstack.com", "https://www.therampstack.com")
            ),
            Some(invitation),
        );
    }
    assert_eq!(
        Invitation::parse(&format!("fig-files://contact/{}", peer())),
        Some(contact)
    );
    assert_eq!(
        Invitation::parse(&format!(
            "fig-files://file/{}/{}/{}",
            peer(),
            file.space,
            file.token
        )),
        Some(shared)
    );
    let contacts = invitations::contacts();
    assert_eq!(contacts.parse_contact(&contact.url()), Some(peer()));
    assert_eq!(contacts.parse_contact(&peer().to_string()), Some(peer()));
    assert_eq!(contacts.parse_contact(&shared.url()), None);
    for invalid in [
        contact.url().replace("https:", "http:"),
        contact.url().replace("therampstack.com", "therampstack.com.evil.example"),
        contact.url().replace("therampstack.com", "therampstack.com@evil.example"),
        contact.url().replace("/fig-files/invite.html", "/invite.html"),
        contact.url().replace("#contact=", "?contact="),
        contact.url().replace("#contact=", "?tracking=1#contact="),
        contact.url().replace(&peer().to_string(), &peer().to_string().to_uppercase()),
        format!("{}&contact={}", contact.url(), me()),
        format!("{}#extra", contact.url()),
        format!("{}%0a", contact.url()),
        format!("{}&redirect=https://example.com", shared.url()),
        shared.url().replace(&file.token.to_string(), &file.token.to_string().to_uppercase()),
        shared.url().replace(&format!(".{}", file.token), ""),
        format!("{}.{}", shared.url(), file.token),
        format!("fig-files://file/{}/{}/{}/extra", peer(), file.space, file.token),
        format!("fig-files://contact/{}?file={}", peer(), file.space),
        "https://therampstack.com/fig-files/invite.html#contact=02ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".into(),
    ] {
        assert_eq!(router.parse_invite(&invalid), None, "Accepted malformed invitation: {invalid}");
    }
    assert!(!format!("{shared:?}").contains(&file.token.to_string()));
    assert!(!format!("{file:?}").contains(&file.token.to_string()));
}

#[test]
fn opening_contact_invitations_saves_only_a_private_profile() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let chk::Action::Flow { flow } = invitations::open(&mut ctx, Invitation::Contact(peer()))
        else {
            panic!("A contact invitation should open its profile");
        };
        let profile = Profile::try_from_name(&mut ctx, peer()).unwrap();
        let alias = profile.pending().username.clone();
        let mut page = flow.0[0].clone().build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        assert!(visible.contains(&alias));
        assert!(!has_button_label(&visible, "Message"));
        assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
        assert!(air.3.lock().unwrap().is_empty());
        invitations::open(&mut ctx, Invitation::Contact(peer()));
        assert_eq!(ctx.air.instances::<Profile>().list().len(), 1);
    }
}

#[test]
fn invitation_publication_requires_explicit_creation_and_reuses_the_same_shared_item() {
    for mobile in [true, false] {
        for folder in [false, true] {
            let (mut ctx, air) = context_with_air(mobile);
            let theme = theme();
            let mut vault = storage::open(&ctx.air);
            let id = if folder {
                storage::create_folder(&mut vault, None, "Linked folder").unwrap()
            } else {
                storage::import_file(
                    &ctx.air,
                    &mut vault,
                    None,
                    "linked.txt",
                    b"shared bytes".to_vec(),
                )
                .unwrap()
            };
            let before = vault.pending().clone();
            let confirmation = page_description(
                invitations::create_page(LinkSource::Private(vault.clone(), id)),
                &mut ctx,
                &theme,
            );
            let mut rendered =
                Page::Static(Box::new(confirmation.clone())).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("Anyone with the link · Can edit"));
            assert!(visible.contains("Anyone with this link can read and edit"));
            assert_eq!(*vault.pending(), before);
            assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
            assert!(ctx.air.instances::<LinkJob>().list().is_empty());
            assert!(air.3.lock().unwrap().is_empty());
            assert!(air.1.lock().unwrap().is_none());
            let PageType::Content {
                bumper: Bumper::Custom { action, .. },
                ..
            } = confirmation
            else {
                panic!("Creating an invitation must be an explicit bumper action");
            };
            (action.get())(&mut ctx, &theme);
            let jobs = ctx.air.instances::<LinkJob>().list();
            assert_eq!(jobs.len(), 1);
            let job = jobs.into_values().next().unwrap();
            let invitation = job.pending().invitation;
            assert_eq!(job.pending().purpose, Purpose::Publish);
            assert_eq!(job.pending().status, Status::Pending);
            let space = collaboration::get(&ctx.air, invitation.space).unwrap();
            assert!(space.pending().permits_link(invitation.token));
            assert_eq!(space.pending().members.len(), 1);
            assert_eq!(
                space.pending().members.get(&me()),
                Some(&MemberStatus::Active)
            );
            assert!(
                matches!(vault.pending().get(id).unwrap().kind, storage::EntryKind::Shared { space: shared, .. } if shared == space.id())
            );
            assert_eq!(
                *air.3.lock().unwrap(),
                vec![prism::service::Key::new::<DeliverLink>(me(), &job)]
            );
            assert!(
                air.1.lock().unwrap().is_none(),
                "The fixture must not poll publication transport"
            );
            (action.get())(&mut ctx, &theme);
            assert_eq!(ctx.air.instances::<SharedSpace>().list().len(), 1);
            assert_eq!(ctx.air.instances::<LinkJob>().list().len(), 1);
        }
    }
}

#[test]
fn opening_file_invitations_does_not_receive_or_join_until_the_user_accepts() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let file = FileInvitation {
            publisher: peer(),
            space: Id::random(),
            token: Id::random(),
        };
        let chk::Action::Flow { flow } = invitations::open(&mut ctx, Invitation::File(file)) else {
            panic!("A file invitation needs a confirmation flow");
        };
        let page = page_description(flow.0[0].clone(), &mut ctx, &theme);
        let mut rendered = Page::Static(Box::new(page.clone())).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Read & write access"));
        assert!(visible.contains("Anyone with this link can read and edit"));
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
        assert!(air.3.lock().unwrap().is_empty());
        let PageType::Content {
            bumper: Bumper::Custom { action, .. },
            ..
        } = page
        else {
            panic!("Joining an invitation must be explicit");
        };
        (action.get())(&mut ctx, &theme);
        let jobs = ctx.air.instances::<LinkJob>().list();
        assert_eq!(jobs.len(), 1);
        let job = jobs.into_values().next().unwrap();
        assert_eq!(job.pending().invitation, file);
        assert_eq!(job.pending().purpose, Purpose::Join);
        assert_eq!(job.pending().status, Status::Pending);
        assert_eq!(
            *air.3.lock().unwrap(),
            vec![prism::service::Key::new::<DeliverLink>(me(), &job)]
        );
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn ready_invitation_page_copies_and_shares_the_same_fragment_link_only_on_action() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let commands: HardwareLog = Default::default();
        ctx.hardware = hardware::Hardware::new(TestHardware {
            commands: Some(commands.clone()),
            ..TestHardware::default()
        });
        let theme = theme();
        let file = FileInvitation {
            publisher: me(),
            space: Id::random(),
            token: Id::random(),
        };
        let mut job = ctx
            .air
            .instances::<LinkJob>()
            .create((file, Purpose::Publish));
        job.send(JobAction::Complete(Ok(())));
        let page = page_description(invitations::job_page(job), &mut ctx, &theme);
        let url = Invitation::File(file).url();
        assert!(
            page_body(&page)
                .iter()
                .any(|item| matches!(item, Display::QRCode { data, .. } if data == &url))
        );
        let mut rendered = Page::Static(Box::new(page.clone())).build(&mut ctx, &theme, None, 1);
        draw(rendered.as_mut(), &mut ctx);
        assert!(commands.lock().unwrap().is_empty());
        let PageType::Content {
            header:
                chk::design_system::Header::Stack {
                    action: Some((Icons::Copy, mut copy)),
                    ..
                },
            bumper: Bumper::Custom { action: share, .. },
            ..
        } = page
        else {
            panic!("A ready invitation needs Copy and Share actions");
        };
        copy(&mut ctx, &theme);
        (share.get())(&mut ctx, &theme);
        assert_eq!(
            *commands.lock().unwrap(),
            vec![("copy", url.clone()), ("share", url.clone())]
        );
        let (request_url, fragment) = url.split_once('#').unwrap();
        assert!(!request_url.contains(&file.token.to_string()));
        assert!(fragment.ends_with(&file.token.to_string()));
        assert!(air.1.lock().unwrap().is_none());
        assert!(air.3.lock().unwrap().is_empty());
    }
}

#[test]
fn profile_airname_actions_keep_raw_copy_but_open_invitation_link_qr_codes() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let commands: HardwareLog = Default::default();
        ctx.hardware = hardware::Hardware::new(TestHardware {
            commands: Some(commands.clone()),
            ..TestHardware::default()
        });
        Profile::from_name(&mut ctx, peer());
        let pages = [
            (me(), contacts_ui::profile_root(&mut ctx).0),
            (me(), contacts_ui::detail(me())),
            (peer(), contacts_ui::detail(peer())),
        ];
        for (name, page) in pages {
            let page = page_description(page, &mut ctx, &theme);
            let displays = match &page {
                PageType::Edit { display, .. } => display.iter().collect(),
                PageType::Content { .. } => page_body(&page),
                _ => panic!("A profile must expose CHK's standard AirName item"),
            };
            let actions = displays
                .into_iter()
                .find_map(|display| match display {
                    Display::Cta {
                        label,
                        instructions,
                        actions,
                        ..
                    } if label == "AirName" => {
                        assert_eq!(instructions, &name.to_string());
                        Some(actions)
                    }
                    _ => None,
                })
                .expect("Profiles keep their AirName actions");
            let mut copy = actions
                .iter()
                .map(chk::ActionItem::presentation)
                .find(|action| action.icon == Icons::Copy)
                .unwrap()
                .on_click;
            copy(&mut ctx, &theme);
            assert_eq!(
                commands.lock().unwrap().pop(),
                Some(("copy", name.to_string()))
            );
            let qr_action = actions
                .iter()
                .find(|action| action.presentation().icon == Icons::QrCode)
                .unwrap();
            let url = Invitation::Contact(name).url();
            // CHK keeps ActionItem's nested flow private but exposes its
            // descriptor through Debug. Check the QR payload specifically,
            // then activate the real callbacks below to verify navigation.
            assert!(format!("{qr_action:?}").contains(&format!("QRCode {{ data: {url:?}")));
            let mut display_qr = qr_action.presentation().on_click;
            let root = PageType::display("Test", vec![], None, Bumper::None, chk::Offset::Start)
                .build_root(&mut ctx, &theme);
            let mut interface = theme.interface(
                &mut ctx,
                vec![chk::RootInfo {
                    icon: chk::RootIcon::Icon(Icons::Profile),
                    label: "Profile".into(),
                    page: root,
                }],
                None,
            );
            draw(interface.as_mut(), &mut ctx);
            display_qr(&mut ctx, &theme);
            let drawing = draw(interface.as_mut(), &mut ctx);
            assert!(text(&drawing).contains("Share AirName"));

            tap_text(interface.as_mut(), &mut ctx, "Share");
            assert_eq!(commands.lock().unwrap().pop(), Some(("share", url)));
        }
        assert!(air.1.lock().unwrap().is_none());
        assert!(ctx.air.instances::<LinkJob>().list().is_empty());
        assert!(ctx.air.instances::<ChatRoom>().list().is_empty());
    }
}
