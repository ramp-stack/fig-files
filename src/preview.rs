//! File previews are page-local CHK updates. Workers never navigate or touch AIR.
use crate::{
    browser,
    collaboration::{self, SharedSpace},
    collaboration_ui,
    runtime::FileSource,
    storage::{self, Entry, EntryId, EntryKind, FileBlob, Vault},
};
use chk::{
    Action, ActionItem, Bumper, Context, Display, Flow, Form, FormAction, FormComplete, FormItem,
    FormValidState, FormValues, Icons, Item, Offset, Page, PageBuilder, PageType, Theme,
    ListWatch,
};
use futures::{
    FutureExt,
    channel::oneshot,
    future::{LocalBoxFuture, Shared, select_all},
};
use prism::{
    air::{Id, Instance},
    hardware::Image,
};
use std::{cell::RefCell, fmt, rc::Rc};

type Signal = Shared<LocalBoxFuture<'static, ()>>;
type Completion = Shared<LocalBoxFuture<'static, Result<Rendered, String>>>;

#[derive(Clone, Debug)]
pub(crate) enum Source {
    Private(Instance<Vault>, EntryId),
    Shared(Instance<SharedSpace>, EntryId),
    Mount(Id),
}

impl Source {
    /// Resolve again on every refresh, including after a worker finishes. A
    /// deleted file, revoked membership or newer revision wins over that result.
    fn file(&self, ctx: &Context) -> Option<(FileSource, Entry)> {
        match self {
            Self::Private(vault, id) => {
                let snapshot = vault.pending();
                let entry = snapshot.get(*id).filter(|_| !snapshot.is_trashed(*id))?;
                match entry.kind {
                    EntryKind::File { .. } => Some((FileSource::Private(*id), entry.clone())),
                    EntryKind::Shared {
                        space,
                        folder: false,
                    } => Self::Mount(space).file(ctx),
                    _ => None,
                }
            }
            Self::Shared(space, id) => shared_file(ctx, space, *id),
            Self::Mount(id) => {
                let space = collaboration::get(&ctx.air, *id).ok()?;
                let root = space.pending().root;
                shared_file(ctx, &space, root)
            }
        }
    }
}

fn shared_file(
    ctx: &Context,
    space: &Instance<SharedSpace>,
    id: EntryId,
) -> Option<(FileSource, Entry)> {
    let state = space.pending();
    if !state.is_ready() || state.validate().is_err() || !state.can_write(ctx.air.me()) {
        return None;
    }
    state
        .tree
        .get(id)
        .filter(|entry| !state.tree.is_trashed(id) && matches!(entry.kind, EntryKind::File { .. }))
        .map(|entry| (FileSource::Shared(space.id(), id), entry.clone()))
}

#[derive(Clone, Debug)]
struct Selection {
    page: usize,
    signal: Signal,
    sender: Rc<RefCell<Option<oneshot::Sender<()>>>>,
}

fn selection(page: usize) -> Selection {
    let (sender, receiver) = oneshot::channel();
    Selection {
        page,
        signal: async move {
            let _ = receiver.await;
        }
        .boxed_local()
        .shared(),
        sender: Rc::new(RefCell::new(Some(sender))),
    }
}

/// The PDF selector is an explicit navigation action; worker completion only
/// changes its own preview. Reopening a chosen page keeps the flow stack bounded.
#[derive(Clone, Debug)]
pub(crate) struct PageControl {
    selection: Rc<RefCell<Selection>>,
    destination: Option<(Source, Page)>,
}
impl PageControl {
    pub(crate) fn new() -> Self {
        Self {
            selection: Rc::new(RefCell::new(selection(0))),
            destination: None,
        }
    }
    pub(crate) fn page(&self) -> usize {
        self.selection.borrow().page
    }
    pub(crate) fn select(&self, page: usize) {
        let old = self.selection.replace(selection(page));
        if let Some(sender) = old.sender.borrow_mut().take() {
            let _ = sender.send(());
        }
    }
    fn signal(&self) -> Signal {
        self.selection.borrow().signal.clone()
    }
    fn finish(&self, page: usize) -> FormComplete {
        let control = self.clone();
        FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
            control.select(page);
            if let Some((source, fallback)) = &control.destination {
                browser::show(
                    ctx,
                    theme,
                    with_control(source.clone(), fallback.clone(), control.clone()),
                );
            }
        }))
    }
}

pub(crate) fn page(source: Source, fallback: Page) -> Page {
    let mut control = PageControl::new();
    control.destination = Some((source.clone(), fallback.clone()));
    with_control(source, fallback, control)
}

fn with_control(source: Source, fallback: Page, control: PageControl) -> Page {
    let updates = Page::watching(
        (ListWatch::<FileBlob>::default(), ListWatch::<SharedSpace>::default()),
        |_: &mut Context, _: &Theme, _| {
            PageType::display("", vec![], None, Bumper::None, Offset::Start)
        },
    )
    .builder()
    .expect("watching page");
    Page::Refreshing(Box::new(Preview {
        source,
        fallback: fallback
            .builder()
            .expect("file pages watch their contracts"),
        updates,
        selection: control.signal(),
        control,
        key: None,
        job: None,
        rendered: None,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    source: FileSource,
    kind: EntryKind,
    extension: String,
    page: usize,
}

#[derive(Clone, Debug)]
enum Rendered {
    Photo(chk::air::messages::Photo, Option<PdfPosition>),
    Text(String),
}

#[derive(Clone, Copy, Debug)]
struct PdfPosition {
    page: usize,
    count: usize,
}

#[derive(Clone)]
struct Preview {
    source: Source,
    fallback: Box<dyn PageBuilder>,
    updates: Box<dyn PageBuilder>,
    control: PageControl,
    selection: Signal,
    key: Option<Key>,
    job: Option<Completion>,
    rendered: Option<Result<Rendered, String>>,
}
impl fmt::Debug for Preview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Preview")
            .field("source", &self.source)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl PageBuilder for Preview {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<()>> {
        Box::pin(async {
            enum Update {
                Source,
                Selection,
                Complete(Result<Rendered, String>),
            }
            let update = {
                let mut waits: Vec<LocalBoxFuture<'_, Update>> = vec![
                    async {
                        self.fallback.next().await;
                        Update::Source
                    }
                    .boxed_local(),
                    async {
                        self.updates.next().await;
                        Update::Source
                    }
                    .boxed_local(),
                    async {
                        self.selection.clone().await;
                        Update::Selection
                    }
                    .boxed_local(),
                ];
                if let Some(job) = &self.job {
                    // Keep the shared future stored until it completes: CHK can
                    // cancel this wait whenever another page becomes active.
                    waits.push(async { Update::Complete(job.clone().await) }.boxed_local());
                }
                select_all(waits).await.0
            };
            if let Update::Complete(result) = update {
                self.job = None;
                self.rendered = Some(result);
            }
            Some(())
        })
    }

    fn build(&mut self, ctx: &mut Context, theme: &Theme) -> PageType {
        let fallback = self.fallback.build(ctx, theme);
        self.updates.build(ctx, theme);
        self.selection = self.control.signal();
        let Some((source, entry)) = self.source.file(ctx) else {
            self.key = None;
            self.job = None;
            self.rendered = None;
            return fallback;
        };
        if self
            .key
            .as_ref()
            .is_some_and(|key| key.source != source || key.kind != entry.kind)
        {
            // Replacements can have fewer pages. Start the new revision at its
            // first page instead of retaining an index from the old document.
            self.control.select(0);
            self.selection = self.control.signal();
        }
        let key = Key {
            source,
            kind: entry.kind.clone(),
            extension: entry.extension(),
            page: self.control.page(),
        };
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key.clone());
            self.job = None;
            self.rendered = None;
        }
        if self.job.is_none() && self.rendered.is_none() {
            let content = match source {
                FileSource::Private(_) => {
                    storage::read_file(&ctx.air, &entry).map_err(|e| e.to_string())
                }
                FileSource::Shared(space, id) => collaboration::get(&ctx.air, space)
                    .map_err(|e| e.to_string())
                    .and_then(|space| space.pending().read_file(id).map_err(|e| e.to_string())),
            };
            if let Ok(bytes) = content {
                let font = theme
                    .fonts()
                    .get_font("FontStyle::Text")
                    .map(|font| font.bytes().to_vec());
                self.job = Some(start(entry.clone(), bytes, font, key.page));
            }
            // Unavailable private blobs can arrive after the file index. The
            // FileBlob watcher retries automatically; no polling or native I/O.
        }
        let position = match &self.rendered {
            Some(Ok(Rendered::Photo(_, position))) => *position,
            _ => None,
        };
        if let Some(position) = position
            && self.control.page() != position.page
        {
            self.control.select(position.page);
            self.selection = self.control.signal();
            if let Some(key) = &mut self.key {
                key.page = position.page;
            }
        }
        let count = position.map(|position| position.count);
        let info = match source {
            FileSource::Private(id) => browser::info(storage::open(&ctx.air), id),
            FileSource::Shared(space, id) => collaboration_ui::details(
                collaboration::get(&ctx.air, space).expect("resolved shared file"),
                id,
            ),
        };
        let info = if let Some(count) = count.filter(|count| *count > 1) {
            Page::Refreshing(Box::new(Info {
                base: info.builder().expect("watched file info"),
                control: self.control.clone(),
                count,
            }))
        } else {
            info
        };
        let header = Some((Icons::Info, Action::flow(Flow::new(vec![info]))));
        match &self.rendered {
            Some(Ok(Rendered::Photo(photo, _))) => PageType::photo(photo.clone(), header),
            Some(Ok(Rendered::Text(text))) => PageType::display(
                &entry.name,
                vec![Display::text(text)],
                header,
                Bumper::None,
                Offset::Start,
            ),
            Some(Err(error)) => PageType::display(
                &entry.name,
                vec![
                    Display::image(browser::entry_art(&entry), 160.0),
                    Display::label("Preview unavailable"),
                    Display::instructions(error),
                    Display::instructions("Open Info to export the original file."),
                ],
                header,
                Bumper::None,
                Offset::Center,
            ),
            None => PageType::display(
                &entry.name,
                vec![
                    Display::image(browser::entry_art(&entry), 160.0),
                    Display::label("Loading preview…"),
                ],
                header,
                Bumper::None,
                Offset::Center,
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct Info {
    base: Box<dyn PageBuilder>,
    control: PageControl,
    count: usize,
}
impl PageBuilder for Info {
    fn next(&mut self) -> LocalBoxFuture<'_, Option<()>> {
        self.base.next()
    }
    fn build(&mut self, ctx: &mut Context, theme: &Theme) -> PageType {
        let mut page = self.base.build(ctx, theme);
        if let PageType::Content { items, .. } = &mut page {
            items.insert(
                0,
                Item::Display(Display::cta(
                    "PDF pages", None,
                    &format!("Page {} of {}", self.control.page() + 1, self.count),
                    vec![ActionItem::new(
                        Action::flow(page_selector(
                            self.control.clone(),
                            self.control.page(),
                            self.count,
                            theme,
                        )),
                        "PDF pages", Icons::Forward,
                    )],
                )),
            );
        }
        page
    }
}

fn start(entry: Entry, bytes: Vec<u8>, font: Option<Vec<u8>>, page: usize) -> Completion {
    let (sender, receiver) = oneshot::channel();
    // Only owned bounded bytes cross the thread boundary. There is no AIR,
    // context, page, navigation callback or native share sheet on this worker.
    let _ = std::thread::Builder::new()
        .name("fig-preview".into())
        .spawn(move || {
            let _ = sender.send(render(&entry, &bytes, font.as_deref(), page));
        });
    async move {
        receiver.await.unwrap_or_else(|_| {
            Err("This preview could not be loaded. Open the file again to retry.".into())
        })
    }
    .boxed_local()
    .shared()
}

fn render(
    entry: &Entry,
    bytes: &[u8],
    font: Option<&[u8]>,
    page: usize,
) -> Result<Rendered, String> {
    let (image, pages) = if entry.mime() == "image/svg+xml" || entry.extension() == "svg" {
        (crate::svg_preview::render_with_font(bytes, font)?, None)
    } else if entry.mime() == "application/pdf" || entry.extension() == "pdf" {
        let ((image, count), page) = match crate::pdf_preview::render(bytes, page) {
            Ok(result) => (result, page),
            // An Info selector can outlive the revision it described. Recover
            // to page one if a shorter replacement arrived before selection.
            Err(_) if page > 0 => (crate::pdf_preview::render(bytes, 0)?, 0),
            Err(error) => return Err(error),
        };
        (image, Some(PdfPosition { page, count }))
    } else if entry.mime().starts_with("image/") {
        if !safe_image_dimensions(bytes) {
            return Err("This image is too large or its format is unsupported.".into());
        }
        (
            Image::read(bytes).map_err(|_| "This image could not be read.")?,
            None,
        )
    } else if let Ok(text) = std::str::from_utf8(bytes)
        && !text.contains('\0')
    {
        let mut chars = text.chars();
        let mut excerpt: String = chars.by_ref().take(16_000).collect();
        if chars.next().is_some() {
            excerpt.push_str("\n\n[Preview shortened. Export for the complete file.]");
        }
        if excerpt.is_empty() {
            excerpt = "This file is empty.".into();
        }
        return Ok(Rendered::Text(excerpt));
    } else {
        return Err("This file format does not have a preview.".into());
    };
    chk::air::messages::Photo::encode(&image, 1600, 95)
        .map(|photo| Rendered::Photo(photo, pages))
        .ok_or_else(|| "This document could not be displayed.".into())
}

fn safe_image_dimensions(bytes: &[u8]) -> bool {
    chk::image::io::Reader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(w, h)| {
            w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= 16_000_000
        })
}

pub(crate) fn page_selector(
    control: PageControl,
    page: usize,
    count: usize,
    _theme: &Theme,
) -> Flow {
    let mut actions = Vec::new();
    for (enabled, label, icon, target) in [
        (
            page > 0,
            "Previous page",
            Icons::Back,
            page.saturating_sub(1),
        ),
        (page + 1 < count, "Next page", Icons::Forward, page + 1),
    ] {
        if enabled {
            let control = control.clone();
            actions.push(FormAction::Custom(ActionItem::new(
                Action::custom(move |ctx: &mut Context, theme: &Theme| {
                    control.finish(target).run(ctx, theme)
                }),
                label,
                icon,
            )));
        }
    }
    Flow::from_form(Form::flow(
        vec![
            FormItem::text_with_preset(
                "page", uuid::Uuid::new_v4(), &format!("Page {} of {count}", page + 1),
                &(page + 1).to_string(),
                Some(actions),
                move |_: &mut Context, text: String| match parse_page(&text, count) {
                    Some(_) => FormValidState::Valid,
                    None => {
                        FormValidState::InvalidWithData(format!("Enter a page from 1 to {count}."))
                    }
                },
            ),
        ],
        None,
        None,
        Box::new(move |_: &mut Context, values: &FormValues| {
            let Some(text) = values.text("page") else {
                return FormComplete::None;
            };
            parse_page(text, count)
                .map(|page| control.finish(page))
                .unwrap_or(FormComplete::None)
        }),
    ))
}

fn parse_page(text: &str, count: usize) -> Option<usize> {
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|page| *page > 0 && *page <= count)
        .map(|page| page - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, bytes: &[u8]) -> Entry {
        Entry {
            id: Id::random(),
            parent: None,
            name: name.into(),
            kind: EntryKind::File {
                blob: Id::hash(bytes),
                size: bytes.len() as u64,
                mime: storage::mime_for_name(name).into(),
            },
            favorite: false,
            created_at: 0,
            modified_at: 0,
            trashed_at: None,
        }
    }

    #[test]
    fn text_preview_is_bounded_without_splitting_unicode() {
        let bytes = "🟣".repeat(16_001).into_bytes();
        let Rendered::Text(text) = render(&entry("notes.txt", &bytes), &bytes, None, 0).unwrap()
        else {
            panic!("text expected");
        };
        assert_eq!(text.chars().filter(|c| *c == '🟣').count(), 16_000);
        assert!(text.ends_with("[Preview shortened. Export for the complete file.]"));
        assert!(
            matches!(render(&entry("empty.txt", &[]), &[], None, 0), Ok(Rendered::Text(text)) if text == "This file is empty.")
        );
        assert!(render(&entry("binary.bin", b"\0hello"), b"\0hello", None, 0).is_err());
    }

    #[test]
    fn cloned_completion_survives_cancelled_polling() {
        let bytes = b"a background preview".to_vec();
        let job = start(entry("notes.txt", &bytes), bytes, None, 0);
        let _ = job.clone().now_or_never();
        let result = futures::executor::block_on(job.clone()).unwrap();
        assert!(matches!(result, Rendered::Text(text) if text == "a background preview"));
        assert!(
            matches!(futures::executor::block_on(job), Ok(Rendered::Text(text)) if text == "a background preview")
        );
    }

    #[test]
    fn malformed_images_fail_without_falling_back_to_text() {
        for name in ["bad.png", "bad.svg", "bad.pdf"] {
            assert!(render(&entry(name, b"not an image"), b"not an image", None, 0).is_err());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn stale_pdf_page_selection_recovers_to_first_page() {
        let bytes = crate::pdf_preview::tests::fixture();
        let Rendered::Photo(_, Some(position)) =
            render(&entry("document.pdf", &bytes), &bytes, None, 9).unwrap()
        else {
            panic!("PDF expected");
        };
        assert_eq!(position.page, 0);
        assert_eq!(position.count, 3);
        let Rendered::Photo(_, Some(position)) =
            render(&entry("document.pdf", &bytes), &bytes, None, 2).unwrap()
        else {
            panic!("PDF expected");
        };
        assert_eq!(position.page, 2);
    }

    #[test]
    fn pdf_page_numbers_are_one_based_and_bounded() {
        assert_eq!(parse_page("1", 3), Some(0));
        assert_eq!(parse_page(" 3 ", 3), Some(2));
        for input in [
            "",
            "0",
            "4",
            "-1",
            "1.5",
            "page 2",
            "999999999999999999999999999",
        ] {
            assert_eq!(parse_page(input, 3), None, "{input}");
        }
        assert_eq!(parse_page("1", 0), None);
    }
}
