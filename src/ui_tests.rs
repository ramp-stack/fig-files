//! Headless integration tests use the real CHK interface and app assets with
//! in-memory AIR. No window, network, filesystem cache or shared crate edits.
use crate::{
    FigFiles, browser,
    collaboration::{self, MemberStatus, SharedAction, SharedSpace},
    contacts::{self, AddressBook},
    contacts_ui,
    drafts::{self, EditDraft},
    runtime,
    sharing::{
        self, Transfer, TransferAction, TransferInit, TransferItem, TransferKind, TransferStatus,
    },
    sharing_ui,
    storage::{self, FileBlob, Vault},
};
use chk::{App, Assets, Bumper, Context, Display, Icons, Page, PageType, Theme};
use futures::FutureExt;
use prism::{
    Application, Window,
    air::{Contract, ContractType, Id, Metadata, Name, Request, Value},
    drawable::{Drawable, Instruction, Item},
    hardware, traits,
};
use std::{
    any::Any,
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct TestKeyboard;
impl traits::Keyboard for TestKeyboard {
    fn show(&self, _: f32) {}
    fn hide(&self) {}
    fn visible(&self) -> bool {
        false
    }
}
#[derive(Clone, Default)]
struct TestHardware;
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
    fn share(&self, _: &hardware::Data) {
        panic!("Rendering must not share files")
    }
    fn set_clipboard(&self, _: &hardware::Data) {
        panic!("Rendering must not write the clipboard")
    }
    fn get_clipboard(&self) -> Option<hardware::Data> {
        None
    }
    fn set_cache(&self, _: &str, _: &hardware::Data) -> Result<(), String> {
        Ok(())
    }
    fn get_cache(&self, _: &str) -> Option<hardware::Data> {
        None
    }
    fn trigger_haptic(&self) {}
}

type ShareLog = Arc<Mutex<Option<Vec<(Id, Id, Name)>>>>;
#[derive(Clone, Debug)]
struct MemoryAir(
    Arc<Mutex<HashMap<(Id, Id), Box<dyn Any + Send + Sync>>>>,
    ShareLog,
    Name,
);
impl MemoryAir {
    fn with_name(name: Name) -> Self {
        Self(Default::default(), Default::default(), name)
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
        } else if contract.id == AddressBook::id() {
            Box::new(Instances::<AddressBook>(self.clone(), PhantomData))
        } else {
            panic!("Fig Files must not start unused contracts: {}", contract.id)
        }
    }
    fn start(&self, _: prism::service::ServiceInit) {
        panic!("Unexpected background service")
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
            .filter_map(|((kind, id), stored)| {
                (*kind == C::id()).then(|| {
                    (
                        *id,
                        Box::new(stored.downcast_ref::<Stored<C>>().unwrap().clone())
                            as Box<dyn traits::DynInstance>,
                    )
                })
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
        TestHardware,
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
        .to_pelican(Assets::new(vec![include_dir::include_dir!(
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

fn header_description(page: PageType, ctx: &mut Context, theme: &Theme) -> (Icons, PageType) {
    let header = match page {
        PageType::Root { header, .. } | PageType::Display { header, .. } => header,
        _ => panic!("Expected a root or display page"),
    };
    let (icon, mut builder) = header.expect("This page needs a header action");
    let flow = builder(ctx, theme);
    assert_eq!(flow.0.len(), 1, "The header should open one focused page");
    (
        icon,
        page_description(flow.0.into_iter().next().unwrap(), ctx, theme),
    )
}

fn page_body(page: &PageType) -> &[Display] {
    match page {
        PageType::Root { display, .. } => display,
        PageType::Display { items, .. } => items,
        _ => panic!("Expected a root or display page"),
    }
}

fn primary_label(page: &PageType) -> Option<&str> {
    match page {
        PageType::Root { bumper_a, .. } => bumper_a.as_ref().map(|(label, _)| label.as_str()),
        PageType::Display {
            bumper: Bumper::Custom { label, .. },
            ..
        } => Some(label.as_str()),
        PageType::Display { .. } => None,
        _ => panic!("Expected a root or display page"),
    }
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
            let flow = runtime::pdf_pages(Id::from(123), page, count, &theme);
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
            let actual: Vec<_> = actions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|(label, _, icon, _)| (label.as_str(), *icon))
                .collect();
            assert_eq!(actual, expected_actions);
            let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(
                visible.contains(&heading),
                "Missing page position: {visible}"
            );
            assert_eq!(visible.contains("Previous page"), page > 0);
            assert_eq!(visible.contains("Next page"), page + 1 < count);
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
        let mut app = FigFiles::new(&mut ctx);
        assert!(text(&draw(&mut app, &mut ctx)).contains("Fig Files"));
        let vault = storage::open(&ctx.air);
        for (location, expected) in [
            (browser::Location::Folder(None), "Room for your next idea"),
            (browser::Location::Trash, "All clear"),
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
fn navigation_roots_are_files_shared_contacts_and_trash_in_that_order() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let roots = FigFiles.roots(&mut ctx, &theme());
        let actual: Vec<_> = roots
            .iter()
            .map(|root| (root.0.label.as_str(), root.0.icon))
            .collect();
        assert_eq!(
            actual,
            vec![
                ("Files", Icons::Home),
                ("Shared", Icons::Send),
                ("Contacts", Icons::Group),
                ("Trash", Icons::Delete),
            ]
        );
        assert!(roots.iter().all(|root| root.0.page.is_some()));
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
        assert!(
            matches!(&empty, PageType::Root { title, header: None, bumper_b: None, .. } if title == "Contacts")
        );
        assert_eq!(primary_label(&empty), Some("Add contact"));
        let mut rendered = Page::Static(Box::new(empty)).build_root(&mut ctx, &theme);
        assert!(
            text(&draw(rendered.as_mut(), &mut ctx))
                .contains("Add someone by their Fig Files receiving name.")
        );
        assert!(
            air.0.lock().unwrap().is_empty(),
            "Empty Contacts must not create AIR records"
        );

        let outgoing = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        let incoming = shared_fixture(&ctx, &air, true, false, MemberStatus::Active);
        let legacy = transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted);
        let unaccepted = transfer_fixture(&ctx, &air, true, true, true, TransferStatus::Pending);
        let mut invalid = legacy.pending().clone();
        invalid.items[0].name = "invalid/contact-title".into();
        stored_fixture(&ctx, &air, Id::random(), invalid);
        let before = air.0.lock().unwrap().len();
        let members = outgoing.pending().members.clone();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        let titles: Vec<_> = page_body(&home)
            .iter()
            .filter_map(|display| match display {
                Display::List { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .map(|item| item.title.clone())
            .collect();
        assert_eq!(
            titles.len(),
            1,
            "Repeated sharing with the same AIR name is one contact"
        );
        let mut rendered = Page::Static(Box::new(home)).build_root(&mut ctx, &theme);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("3 shared items"));

        let detail = page_description(contacts_ui::detail(peer()), &mut ctx, &theme);
        assert_eq!(primary_label(&detail), Some("Share files"));
        let PageType::Display {
            bumper:
                Bumper::Custom {
                    action: chk::Action::Flow { flow },
                    secondary,
                    ..
                },
            ..
        } = &detail
        else {
            panic!("A contact should open the file picker through a pure CHK flow");
        };
        assert_eq!(
            secondary.as_ref().map(|(label, _)| label.as_str()),
            Some("Copy name")
        );
        assert_eq!(flow.0.len(), 1);
        let picker = page_description(flow.0[0].clone(), &mut ctx, &theme);
        assert!(matches!(&picker, PageType::Display { title, .. } if title == "Share files"));
        let mut rendered = Page::Static(Box::new(detail)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            visible
                .split_whitespace()
                .collect::<String>()
                .contains(&peer().to_string())
        );
        for expected in [
            "Sharing history",
            "owned-folder",
            "received-note.txt",
            "sent-note.txt",
            "Share files",
        ] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        assert!(!visible.contains("received-folder"));
        assert!(!visible.contains("invalid/contact-title"));
        let absent = page_description(
            contacts_ui::detail(air::Secret::new().name()),
            &mut ctx,
            &theme,
        );
        assert_eq!(primary_label(&absent), None);
        assert!(
            matches!(absent, PageType::Display { title, .. } if title == "Contact unavailable")
        );
        assert_eq!(outgoing.pending().members, members);
        assert_eq!(
            incoming.pending().members.get(&me()),
            Some(&MemberStatus::Active)
        );
        assert_eq!(unaccepted.pending().status, TransferStatus::Pending);
        assert_eq!(air.0.lock().unwrap().len(), before);
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn adding_a_contact_uses_a_pure_name_form_and_only_persists_on_valid_submission() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        let PageType::Root {
            bumper_a: Some((label, mut add)),
            ..
        } = home
        else {
            panic!("Contacts must expose Add contact as its primary action");
        };
        assert_eq!(label, "Add contact");
        let flow = add(&mut ctx, &theme);
        assert_eq!(flow.0.len(), 1);
        let form = page_description(flow.0[0].clone(), &mut ctx, &theme);
        let PageType::Form {
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
            panic!("Adding a contact uses the CHK text form");
        };
        assert_eq!(title, "Add contact");
        assert_eq!(
            actions
                .iter()
                .map(|(label, _, icon, _)| (label.as_str(), *icon))
                .collect::<Vec<_>>(),
            vec![
                ("Paste clipboard", Icons::Paste),
                ("Scan QR code", Icons::QrCode)
            ]
        );
        assert!(matches!(actions[0].3, chk::Action::Paste { .. }));
        assert!(matches!(actions[1].3, chk::Action::Flow { .. }));
        let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        for expected in ["Add contact", "Paste clipboard", "Scan QR code"] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        assert!(
            air.0.lock().unwrap().is_empty(),
            "Opening and drawing Add contact must remain read-only"
        );

        for input in [
            "".to_string(),
            "not-an-air-name".into(),
            "03".into(),
            me().to_string(),
        ] {
            assert!(contacts::parse(&input, me()).is_err());
            submit(&mut ctx, &vec![chk::State::Text(input)]);
            assert!(
                air.0.lock().unwrap().is_empty(),
                "Invalid submission must not create an address book"
            );
            ctx.events();
        }
        let pasted = format!("  \n{}\t ", peer());
        assert_eq!(contacts::parse(&pasted, me()), Ok(peer()));
        submit(&mut ctx, &vec![chk::State::Text(pasted)]);
        ctx.events();
        let books = ctx.air.instances::<AddressBook>().list();
        assert_eq!(books.len(), 1);
        let state = books.values().next().unwrap().pending();
        assert_eq!(state.owner, me());
        assert_eq!(
            state.contacts.keys().copied().collect::<Vec<_>>(),
            vec![peer()]
        );
        let first_added = state.contacts[&peer()];
        drop(state);
        submit(&mut ctx, &vec![chk::State::Text(peer().to_string())]);
        ctx.events();
        assert_eq!(ctx.air.instances::<AddressBook>().list().len(), 1);
        assert_eq!(
            books.values().next().unwrap().pending().contacts[&peer()],
            first_added
        );
        assert_eq!(
            air.0.lock().unwrap().len(),
            1,
            "Adding a contact creates only the private address book"
        );
        assert!(ctx.air.instances::<Vault>().list().is_empty());
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Transfer>().list().is_empty());
        assert!(air.1.lock().unwrap().is_none());
    }
}

#[test]
fn manually_added_contacts_render_and_merge_with_later_sharing_history() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        contacts::add(&ctx.air, peer()).unwrap();
        let before = air.0.lock().unwrap().len();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        assert_eq!(primary_label(&home), Some("Add contact"));
        let rows = page_body(&home)
            .iter()
            .filter_map(|item| match item {
                Display::List { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        let mut rendered = Page::Static(Box::new(home)).build_root(&mut ctx, &theme);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("No shared files yet"));
        let detail = page_description(contacts_ui::detail(peer()), &mut ctx, &theme);
        assert_eq!(primary_label(&detail), Some("Share files"));
        let PageType::Display {
            bumper:
                Bumper::Custom {
                    action: chk::Action::Flow { flow },
                    secondary: Some((copy_label, chk::Action::Copy { data })),
                    ..
                },
            ..
        } = &detail
        else {
            panic!("Saved contacts expose Share files and Copy name");
        };
        assert_eq!(copy_label, "Copy name");
        assert_eq!(data, &peer().to_string());
        let picker = page_description(flow.0[0].clone(), &mut ctx, &theme);
        assert!(matches!(picker, PageType::Display { title, .. } if title == "Share files"));
        let mut rendered = Page::Static(Box::new(detail)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            visible
                .split_whitespace()
                .collect::<String>()
                .contains(&peer().to_string())
        );
        assert!(visible.contains("No shared files yet") && visible.contains("Share files"));
        assert_eq!(air.0.lock().unwrap().len(), before);
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        assert!(ctx.air.instances::<Transfer>().list().is_empty());

        shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        transfer_fixture(&ctx, &air, false, false, true, TransferStatus::Accepted);
        let before = air.0.lock().unwrap().len();
        let home = page_description(contacts_ui::home(), &mut ctx, &theme);
        let rows = page_body(&home)
            .iter()
            .filter_map(|item| match item {
                Display::List { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            1,
            "Manual and sharing records for the same name merge into one contact"
        );
        let mut rendered = contacts_ui::detail(peer()).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("owned-folder") && visible.contains("sent-note.txt"));
        assert!(visible.contains("2 shared items"));
        assert_eq!(ctx.air.instances::<AddressBook>().list().len(), 1);
        assert_eq!(air.0.lock().unwrap().len(), before);
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
            let before = vault.pending().entries.clone();
            let without_contact = sharing_ui::picker_selection(vault.clone(), id, None, &theme);
            assert!(matches!(
                page_description(without_contact.0[0].clone(), &mut ctx, &theme),
                PageType::Form { .. }
            ));

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

            let selected = sharing_ui::picker_selection(vault.clone(), id, Some(peer()), &theme);
            assert_eq!(selected.0.len(), 1);
            let confirmation = page_description(selected.0[0].clone(), &mut ctx, &theme);
            assert_eq!(primary_label(&confirmation), Some("Share access"));
            assert!(
                matches!(&confirmation, PageType::Display { .. }),
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
            let PageType::Display {
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
        let mut page = browser::detail(vault.clone(), file).build(&mut ctx, &theme(), None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in ["notes.txt", "Preview", "Manage file"] {
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
                assert_eq!(primary_label(&page), is_folder.then_some("Add files"));
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
                    PageType::Root {
                        header, bumper_b, ..
                    } => {
                        assert!(bumper_b.is_none());
                        header
                    }
                    PageType::Display { header, .. } => header,
                    _ => unreachable!(),
                };
                assert_eq!(
                    header.as_ref().map(|(icon, _)| *icon),
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
            "Everyone who joins can read and edit",
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
        let PageType::Display {
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
        let Some(Display::Text { text: footer, .. }) = page_body(&page).last() else {
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
            vec![chk::EnumItem::new("Files / Personal", "Folder")]
        );
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(visible.contains("Files / Personal"));
        assert!(!visible.contains("Shared destination"));
        submit(&mut ctx, &vec![chk::State::Enumerator(0)]);
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
        let form = page_description(
            sharing_ui::send_flow(vault.clone(), mount, &theme)
                .0
                .remove(0),
            &mut ctx,
            &theme,
        );
        let PageType::Form {
            on_submit: Some(mut submit),
            ..
        } = form
        else {
            panic!("Share begins with recipient input");
        };
        let recipient = ::air::Secret::new().name();
        // CHK's Form::flow executes Next and returns None from its wrapper.
        // Inspect the same app-owned destination without relying on private
        // navigation event types or invoking any sharing during construction.
        submit(&mut ctx, &vec![chk::State::Text(recipient.to_string())]);
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
        let PageType::Display {
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
fn file_details_and_management_use_focused_chk_action_slots() {
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
        assert_eq!(primary_label(&file_page), Some("Preview"));
        assert!(
            !page_body(&file_page)
                .iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let (icon, share) = header_description(file_page, &mut ctx, &theme);
        assert_eq!(icon, Icons::Send);
        assert_eq!(primary_label(&share), Some("Share access"));
        let PageType::Display {
            bumper: Bumper::Custom { secondary, .. },
            ..
        } = share
        else {
            panic!("Sharing needs a CHK bumper");
        };
        assert_eq!(
            secondary.as_ref().map(|(label, _)| label.as_str()),
            Some("Export file")
        );

        let folder_page =
            page_description(browser::detail(vault.clone(), folder), &mut ctx, &theme);
        assert_eq!(primary_label(&folder_page), Some("Share folder"));
        let (icon, management) = header_description(folder_page, &mut ctx, &theme);
        assert_eq!(icon, Icons::Settings);
        assert_eq!(primary_label(&management), None);

        let manage = page_description(browser::manage_page(vault.clone(), file), &mut ctx, &theme);
        assert_eq!(primary_label(&manage), None);
        assert!(
            !page_body(&manage)
                .iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let mut rendered = Page::Static(Box::new(manage.clone())).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(rendered.as_mut(), &mut ctx));
        for expected in ["Rename", "Move"] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        let (icon, confirmation) = header_description(manage, &mut ctx, &theme);
        assert_eq!(icon, Icons::Delete);
        assert_eq!(primary_label(&confirmation), Some("Move to Trash"));
        assert_eq!(
            vault.pending().entries,
            before,
            "Inspecting menus must not perform their actions"
        );
        assert!(ctx.air.instances::<Transfer>().list().is_empty());

        storage::trash(&mut vault, file).unwrap();
        let trashed = page_description(browser::detail(vault.clone(), file), &mut ctx, &theme);
        assert_eq!(primary_label(&trashed), Some("Restore"));
        let (icon, confirmation) = header_description(trashed, &mut ctx, &theme);
        assert_eq!(icon, Icons::Delete);
        assert_eq!(primary_label(&confirmation), Some("Delete permanently"));
        assert!(
            vault.pending().get(file).is_some(),
            "Opening confirmation must not delete the item"
        );
    }
}

#[test]
fn add_and_share_pages_keep_secondary_choices_separate_without_side_effects() {
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
            .iter()
            .any(|display| matches!(display, Display::Actions { .. }))
    );
    let mut rendered = Page::Static(Box::new(add)).build(&mut ctx, &theme, None, 1);
    let visible = text(&draw(rendered.as_mut(), &mut ctx));
    assert!(visible.contains("New folder"));

    let share = page_description(
        browser::share_page(vault.clone(), folder, &theme),
        &mut ctx,
        &theme,
    );
    let PageType::Display {
        bumper: Bumper::Custom {
            label, secondary, ..
        },
        ..
    } = share
    else {
        panic!("Folder sharing needs a CHK bumper");
    };
    assert_eq!(label, "Share access");
    assert!(
        secondary.is_none(),
        "A folder cannot use the file export command"
    );
    assert_eq!(vault.pending().entries, before);
    assert!(ctx.air.instances::<Transfer>().list().is_empty());
}

#[test]
fn empty_shared_root_and_receiving_name_render_on_phone_and_desktop_without_sharing() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let theme = theme();
        let mut page = sharing_ui::home().build_root(&mut ctx, &theme);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in [
            "Shared",
            "Send",
            "Receive",
            "Files and folders you share or receive will appear here.",
        ] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        let mut page = sharing_ui::receive_name(me()).build(&mut ctx, &theme, None, 1);
        let visible = text(&draw(page.as_mut(), &mut ctx));
        for expected in ["Receive files", "Copy name"] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        // Wrapping may split the public key across drawing instructions.
        let compact = visible.split_whitespace().collect::<String>();
        assert!(compact.contains(&me().to_string()));
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
            let PageType::Root {
                header,
                bumper_a: Some((send_label, mut send)),
                bumper_b: Some((receive_label, mut receive)),
                ..
            } = home
            else {
                panic!("Shared needs labeled Send and Receive root bumpers");
            };
            assert!(header.is_none());
            assert_eq!(send_label, "Send");
            assert_eq!(receive_label, "Receive");
            let send_flow = send(&mut ctx, &theme);
            assert_eq!(send_flow.0.len(), 1);
            let picker = page_description(send_flow.0[0].clone(), &mut ctx, &theme);
            assert!(matches!(
                &picker,
                PageType::Display { title, bumper: Bumper::None, .. } if title == "Share files"
            ));
            assert!(
                !page_body(&picker)
                    .iter()
                    .any(|display| matches!(display, Display::Actions { .. }))
            );
            let lists: Vec<_> = page_body(&picker)
                .iter()
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

            let receive_flow = receive(&mut ctx, &theme);
            assert_eq!(receive_flow.0.len(), 1);
            let receiving = page_description(receive_flow.0[0].clone(), &mut ctx, &theme);
            assert!(
                matches!(&receiving, PageType::Display { title, .. } if title == "Receive files")
            );
            assert_eq!(primary_label(&receiving), Some("Copy name"));
            let mut rendered = Page::Static(Box::new(receiving)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(
                visible
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&me().to_string())
            );
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
            .iter()
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
        // The existing recipient form remains the next step for both kinds of
        // selectable item. Building it must not send or invoke a native picker.
        for id in [selected_file, empty] {
            let flow = sharing_ui::send_flow(vault.clone(), id, &theme);
            assert_eq!(flow.0.len(), 1);
            let form = page_description(flow.0.into_iter().next().unwrap(), &mut ctx, &theme);
            assert!(matches!(&form, PageType::Form { title, .. } if title == "Share access"));
            let mut rendered = Page::Static(Box::new(form)).build(&mut ctx, &theme, None, 1);
            let visible = text(&draw(rendered.as_mut(), &mut ctx));
            assert!(visible.contains("Share access"));
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
        let Display::List { label, items, .. } = &page_body(&root)[0] else {
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
        assert_eq!(items[2], items[2].clone().with_badge(true));
        assert_eq!(items[0], items[0].clone().with_badge(false));
        assert_eq!(primary_label(&root), Some("Send"));
        assert!(
            matches!(&root, PageType::Root { bumper_b: Some((label, _)), .. } if label == "Receive")
        );
        let (icon, options) = header_description(root.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        let mut rendered = Page::Static(Box::new(options)).build(&mut ctx, &theme, None, 1);
        let options_visible = text(&draw(rendered.as_mut(), &mut ctx));
        assert!(
            options_visible.contains("Saved edits") && options_visible.contains("Older copies")
        );
        let mut root = Page::Static(Box::new(root)).build_root(&mut ctx, &theme);
        let visible = text(&draw(root.as_mut(), &mut ctx));
        for expected in [
            "received-folder",
            "received-note.txt",
            "owned-folder",
            "Invitation",
            "Read & write",
            "From ",
            "2 people",
        ] {
            assert!(visible.contains(expected), "Missing {expected}: {visible}");
        }
        for removed in ["Received", "Shared by you", "Saved edits", "Older copies"] {
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
                .iter()
                .any(|item| matches!(item, Display::Actions { .. }))
        );
        let (icon, info) = header_description(page.clone(), &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        assert_eq!(primary_label(&info), Some("Decline invitation"));
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
            let PageType::Display {
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
            let mut rendered = Page::Static(Box::new(info)).build(&mut ctx, &theme, None, 1);
            assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Files shortcut"));
            let root = space.pending().root;
            let live = page_description(
                crate::collaboration_ui::item(space.clone(), root),
                &mut ctx,
                &theme,
            );
            assert_eq!(
                primary_label(&live),
                Some(if folder { "Add files" } else { "Preview" })
            );
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
fn collaborative_share_and_invite_forms_are_pure_until_confirmation() {
    for mobile in [true, false] {
        let (mut ctx, air) = context_with_air(mobile);
        let theme = theme();
        let mut vault = storage::open(&ctx.air);
        let id = storage::create_folder(&mut vault, None, "Project").unwrap();
        let before = vault.pending().clone();
        let form = sharing_ui::send_flow(vault.clone(), id, &theme).0.remove(0);
        let mut rendered = form.build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Share access"));
        assert_eq!(*vault.pending(), before);
        assert!(ctx.air.instances::<SharedSpace>().list().is_empty());
        let space = shared_fixture(&ctx, &air, false, true, MemberStatus::Active);
        let members = space.pending().members.clone();
        let form = sharing_ui::invite_flow(space.clone(), &theme).0.remove(0);
        let mut rendered = form.build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Share access"));
        assert_eq!(space.pending().members, members);
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
            input, mut on_save, ..
        } = page.clone()
        else {
            panic!("Shared text uses a CHK edit form");
        };
        assert!(
            matches!(&input[0], chk::Input::Text { preset: Some(value), .. } if value == &original)
        );
        assert!(original.len() > 16_000);
        let mut rendered = Page::Static(Box::new(page)).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Edit shared text"));
        assert!(
            drafts::list(&ctx.air).is_empty(),
            "Opening an editor is read-only"
        );

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
        let result = on_save(&mut ctx, &vec![chk::State::Text(submitted.clone())]);
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
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].pending().bytes, submitted.as_bytes());
        assert!(!backups[0].pending().resolved);
        let mut detail =
            crate::collaboration_ui::details(space.clone(), file).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(detail.as_mut(), &mut ctx)).contains("Edits to review"));
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
        let mut options = Page::Static(Box::new(options)).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(options.as_mut(), &mut ctx)).contains("Saved edits"));
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
        assert_eq!(primary_label(&discard), Some("Discard saved edit"));
        let mut rendered = Page::Static(Box::new(page.clone())).build(&mut ctx, &theme, None, 1);
        assert!(text(&draw(rendered.as_mut(), &mut ctx)).contains("Export saved edit"));
        let PageType::Display {
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
            matches!(home, PageType::Root { header: None, .. }),
            "Empty Shared options must not leave a header"
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
                    let instance =
                        transfer_fixture(&ctx, &air, incoming, folder, ready, status.clone());
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
fn receiving_controls_use_primary_and_information_slots_without_inline_action_rows() {
    for mobile in [true, false] {
        let mut ctx = context(mobile);
        let theme = theme();
        let home = page_description(sharing_ui::home(), &mut ctx, &theme);
        assert_eq!(primary_label(&home), Some("Send"));
        assert!(
            !page_body(&home)
                .iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let PageType::Root {
            header,
            bumper_a,
            bumper_b,
            ..
        } = home
        else {
            panic!("Shared must be a CHK root");
        };
        assert!(header.is_none());
        assert_eq!(
            bumper_a.as_ref().map(|(label, _)| label.as_str()),
            Some("Send")
        );
        assert_eq!(
            bumper_b.as_ref().map(|(label, _)| label.as_str()),
            Some("Receive")
        );

        let receive = page_description(sharing_ui::receive_name(me()), &mut ctx, &theme);
        assert_eq!(primary_label(&receive), Some("Copy name"));
        assert!(
            !page_body(&receive)
                .iter()
                .any(|display| matches!(display, Display::Actions { .. }))
        );
        let (icon, information) = header_description(receive, &mut ctx, &theme);
        assert_eq!(icon, Icons::Info);
        assert_eq!(primary_label(&information), Some("Share name"));
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
                            PageType::Display {
                                bumper: Bumper::None,
                                ..
                            }
                        ),
                        "CHK's Done bumper suppresses Info; actionless details must keep the header with no bumper"
                    );
                }
                assert!(
                    !page_body(&detail)
                        .iter()
                        .any(|display| matches!(display, Display::Actions { .. }))
                );
                let (icon, information) = header_description(detail, &mut ctx, &theme);
                assert_eq!(icon, Icons::Info);
                assert_eq!(
                    primary_label(&information),
                    Some(if incoming && status == TransferStatus::Pending {
                        "Decline"
                    } else {
                        "Copy name"
                    })
                );
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
        let PageType::Display {
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
            Some(if folder { "Add files" } else { "Preview" }),
            "Folders must open their contents, while files open their details"
        );
        let PageType::Display { title, .. } = &destination else {
            panic!("Saved items open as a CHK display page");
        };
        assert_eq!(
            title,
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
            PageType::Display {
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
            PageType::Display {
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
        sharing_ui::recipient(&format!("  {}\n", peer()), me()),
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
            sharing_ui::recipient(invalid, me()).is_err(),
            "An invalid recipient must never be accepted: {invalid}"
        );
    }
    assert!(sharing_ui::recipient(&me().to_string(), me()).is_err());
}
