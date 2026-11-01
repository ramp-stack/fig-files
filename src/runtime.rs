//! Native file interactions, owned by Fig Files rather than the shared UI crates.
use crate::{
    browser, collaboration, collaboration_ui, drafts,
    storage::{self, Entry, EntryId, MAX_FILE_SIZE},
};
use chk::{Context, Theme};
use prism::{
    air::{Air, Id},
    drawable::{Area, Component, Drawable, Layout},
    events::{ChildEvent, DynEvent, Event, OnChange, OnEvent, on_change, on_event},
    hardware::{Data, Image, PickResult, Picked},
};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub enum FileEvent {
    Import(Option<EntryId>),
    Preview(EntryId),
    PdfPage(EntryId, usize),
    Export(EntryId),
    SharedImport(Id, EntryId),
    SharedPreview(Id, EntryId),
    SharedPdfPage(Id, EntryId, usize),
    SharedExport(Id, EntryId),
    SharedReplace(Id, EntryId),
    SharedExportConflict(Id, EntryId, Id),
    DraftExport(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileSource {
    Private(EntryId),
    Shared(Id, EntryId),
}
impl FileSource {
    fn pdf_event(self, page: usize) -> FileEvent {
        match self {
            Self::Private(id) => FileEvent::PdfPage(id, page),
            Self::Shared(space, id) => FileEvent::SharedPdfPage(space, id, page),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PickerDestination {
    Private(Option<EntryId>),
    Shared {
        space: Id,
        parent: EntryId,
    },
    Replacement {
        space: Id,
        entry: EntryId,
        expected: Id,
    },
}
impl Event for FileEvent {
    fn pass(&self, _: &mut Context, children: &mut [ChildEvent<'_, Self>]) {
        for child in children {
            *child.event = Some(self.clone());
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Fill;
impl Layout for Fill {
    fn request_width(&mut self, children: Vec<(f32, f32)>) -> (f32, f32) {
        children.first().copied().unwrap_or((0.0, f32::MAX))
    }
    fn assign_width(&mut self, width: f32) -> Vec<f32> {
        vec![width]
    }
    fn request_height(&mut self, _: f32, children: Vec<(f32, f32)>) -> (f32, f32) {
        children.first().copied().unwrap_or((0.0, f32::MAX))
    }
    fn layout(&mut self, size: (f32, f32)) -> Vec<Area> {
        vec![Area {
            offset: (0.0, 0.0),
            size,
        }]
    }
}

#[derive(Clone, Component)]
pub struct FileRuntime {
    layout: Fill,
    interface: Box<dyn Drawable>,
    #[skip]
    theme: Theme,
    #[skip]
    picker: Option<Picked<Vec<PathBuf>>>,
    #[skip]
    destination: Option<PickerDestination>,
}
impl fmt::Debug for FileRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRuntime").finish_non_exhaustive()
    }
}
impl FileRuntime {
    pub fn new(interface: Box<dyn Drawable>, theme: Theme) -> Self {
        Self {
            layout: Fill,
            interface,
            theme,
            picker: None,
            destination: None,
        }
    }

    fn notice(&self, ctx: &mut Context, title: &str, body: &str) {
        browser::show(ctx, &self.theme, browser::message(title, body));
    }

    fn import(&self, ctx: &mut Context, paths: Vec<PathBuf>, parent: Option<EntryId>) {
        let mut vault = storage::open(&ctx.air);
        let mut failures = Vec::new();
        let mut imported = 0;
        for path in paths {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Invalid file name");
            let result = read_import(&path).and_then(|bytes| {
                storage::import_file(&ctx.air, &mut vault, parent, name, bytes)
                    .map_err(|error| error.to_string())
            });
            match result {
                Ok(_) => imported += 1,
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        let failure = (!failures.is_empty()).then(|| {
            format!(
                "{imported} {} imported.\n\n{}",
                if imported == 1 { "file" } else { "files" },
                failures.join("\n\n")
            )
        });
        // Finish the Add flow after the picker returns. Reopen the destination
        // folder so imported items are visible instead of leaving a chooser up.
        chk::FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
            if parent.is_some() {
                browser::show(
                    ctx,
                    theme,
                    browser::listing(vault.clone(), browser::Location::Folder(parent), false),
                );
            }
            if let Some(body) = &failure {
                browser::show(ctx, theme, browser::message("Import results", body));
            }
        }))
        .run(ctx, &self.theme);
    }

    fn import_shared(&self, ctx: &mut Context, paths: Vec<PathBuf>, space_id: Id, parent: EntryId) {
        let mut space = match collaboration::get(&ctx.air, space_id) {
            Ok(space) => space,
            Err(error) => {
                self.notice(ctx, "Shared folder unavailable", &error.to_string());
                return;
            }
        };
        let mut failures = Vec::new();
        let mut imported = 0;
        for path in paths {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Invalid file name");
            let result = read_import(&path).and_then(|bytes| {
                collaboration::import_file(&ctx.air, &mut space, parent, name, bytes)
                    .map_err(|error| error.to_string())
            });
            match result {
                Ok(_) => imported += 1,
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        let failure = (!failures.is_empty()).then(|| {
            format!(
                "{imported} {} imported.\n\n{}",
                if imported == 1 { "file" } else { "files" },
                failures.join("\n\n")
            )
        });
        chk::FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
            browser::show(ctx, theme, collaboration_ui::folder(space.clone(), parent));
            if let Some(body) = &failure {
                browser::show(ctx, theme, browser::message("Import results", body));
            }
        }))
        .run(ctx, &self.theme);
    }

    fn replace_shared(
        &self,
        ctx: &mut Context,
        paths: Vec<PathBuf>,
        space_id: Id,
        id: EntryId,
        expected: Id,
    ) {
        let result = read_replacement(&paths).and_then(|bytes| {
            let mut space =
                collaboration::get(&ctx.air, space_id).map_err(|error| error.to_string())?;
            let outcome = collaboration::replace_file(&ctx.air, &mut space, id, expected, bytes)
                .map_err(|error| error.to_string())?;
            Ok((space, outcome))
        });
        match result {
            Ok((space, outcome)) => {
                chk::FormComplete::Return(Box::new(move |ctx: &mut Context, theme: &Theme| {
                    browser::show(ctx, theme, collaboration_ui::item(space.clone(), id));
                    if let collaboration::ReplaceOutcome::SavedConflict(_) = outcome {
                        browser::show(ctx, theme, browser::message(
                            "Replacement saved separately",
                            "Someone changed this file while you were choosing a replacement. Your replacement was saved as a conflicting version. The current file was kept.",
                        ));
                    }
                })).run(ctx, &self.theme);
            }
            Err(error) => self.notice(ctx, "Could not replace file", &error),
        }
    }

    fn pick_shared_import(&mut self, ctx: &mut Context, space_id: Id, parent: EntryId) {
        if self.picker.is_some() {
            return;
        }
        let result = collaboration::get(&ctx.air, space_id)
            .map_err(|error| error.to_string())
            .and_then(|space| {
                let snapshot = space.pending();
                snapshot.validate().map_err(|error| error.to_string())?;
                if !snapshot.can_write(ctx.air.me()) {
                    return Err("Join this shared folder before adding files.".to_string());
                }
                if snapshot.tree.is_trashed(parent)
                    || !snapshot
                        .tree
                        .get(parent)
                        .is_some_and(|entry| entry.is_folder())
                {
                    return Err("This shared folder is no longer available.".to_string());
                }
                Ok(())
            });
        if let Err(error) = result {
            self.notice(ctx, "Shared folder unavailable", &error);
            return;
        }
        self.destination = Some(PickerDestination::Shared {
            space: space_id,
            parent,
        });
        self.picker = Some(ctx.hardware.pick::<Vec<PathBuf>>());
    }

    fn pick_shared_replacement(&mut self, ctx: &mut Context, space: Id, entry: EntryId) {
        if self.picker.is_some() {
            return;
        }
        // Capture the content revision before opening a native picker. Any
        // remote edit while it is open must be preserved by replace_file.
        let expected = collaboration::get(&ctx.air, space)
            .map_err(|error| error.to_string())
            .and_then(|space| {
                let snapshot = space.pending();
                snapshot.validate().map_err(|error| error.to_string())?;
                if !snapshot.can_write(ctx.air.me()) {
                    return Err("Join this shared item before replacing files.".to_string());
                }
                snapshot
                    .content_hash(entry)
                    .map_err(|error| error.to_string())
            });
        match expected {
            Ok(expected) => {
                self.destination = Some(PickerDestination::Replacement {
                    space,
                    entry,
                    expected,
                });
                self.picker = Some(ctx.hardware.pick::<Vec<PathBuf>>());
            }
            Err(error) => self.notice(ctx, "File unavailable", &error.to_string()),
        }
    }

    fn export_conflict(&self, ctx: &mut Context, space: Id, id: EntryId, conflict: Id) {
        let result = collaboration::get(&ctx.air, space)
            .map_err(|error| error.to_string())
            .and_then(|space| {
                let snapshot = space.pending();
                snapshot.validate().map_err(|error| error.to_string())?;
                if !snapshot.can_write(ctx.air.me()) {
                    return Err("Join this shared item before exporting its edits.".into());
                }
                let entry = snapshot
                    .tree
                    .get(id)
                    .filter(|_| !snapshot.tree.is_trashed(id))
                    .ok_or_else(|| "This file is no longer available.".to_string())?;
                let version = snapshot
                    .conflicts(id)
                    .into_iter()
                    .find(|version| version.id == conflict)
                    .ok_or_else(|| {
                        "This conflicting version is no longer available.".to_string()
                    })?;
                write_export(&entry.name, &version.bytes).map_err(|error| error.to_string())
            });
        match result {
            Ok(path) => ctx.hardware.share(&Data::File(path)),
            Err(error) => self.notice(ctx, "Could not export version", &error),
        }
    }

    fn export_draft(&self, ctx: &mut Context, id: Id) {
        let result = drafts::get(&ctx.air, id)
            .map_err(|error| error.to_string())
            .and_then(|draft| {
                let draft = draft.pending();
                write_export(&draft.name, &draft.bytes).map_err(|error| error.to_string())
            });
        match result {
            Ok(path) => ctx.hardware.share(&Data::File(path)),
            Err(error) => self.notice(ctx, "Could not export saved edit", &error),
        }
    }

    fn open_file(&self, ctx: &mut Context, source: FileSource, preview: bool) {
        let (entry, bytes) = match load_file(&ctx.air, source) {
            Ok(file) => file,
            Err(error) => {
                self.notice(ctx, "File unavailable", &error);
                return;
            }
        };
        if preview {
            if entry.mime() == "image/svg+xml" || entry.extension() == "svg" {
                let font = self
                    .theme
                    .fonts()
                    .get_font("FontStyle::Text")
                    .map(|font| font.bytes());
                match crate::svg_preview::render_with_font(&bytes, font) {
                    Ok(image) => self.show_preview(ctx, &image, None),
                    Err(error) => self.notice(ctx, "Preview unavailable", &error),
                }
                return;
            }
            if entry.mime() == "application/pdf" || entry.extension() == "pdf" {
                self.show_pdf(ctx, source, &bytes, 0);
                return;
            }
            if entry.mime().starts_with("image/") {
                if !safe_image_dimensions(&bytes) {
                    self.notice(ctx, "Preview unavailable", "This image is too large or its format is unsupported. Use Share → Export file to open the original.");
                    return;
                }
                if let Ok(image) = Image::read(bytes.as_slice())
                    && let Some(photo) = chk::air::messages::Photo::from_image(&image)
                {
                    browser::show(
                        ctx,
                        &self.theme,
                        chk::Page::Static(Box::new(chk::PageType::photo(photo, None))),
                    );
                    return;
                }
            }
            if let Ok(text) = std::str::from_utf8(&bytes) {
                if !text.contains('\0') {
                    let mut excerpt: String = text.chars().take(16_000).collect();
                    if text.chars().nth(16_000).is_some() {
                        excerpt.push_str("\n\n[Preview shortened. Export for the complete file.]");
                    }
                    if excerpt.is_empty() {
                        excerpt = "This file is empty.".into();
                    }
                    browser::show(
                        ctx,
                        &self.theme,
                        chk::Page::Static(Box::new(chk::PageType::display(
                            &entry.name,
                            vec![chk::Display::text(&excerpt)],
                            None,
                            chk::Bumper::Done,
                            chk::Offset::Start,
                        ))),
                    );
                    return;
                }
            }
            self.notice(
                ctx,
                "Preview unavailable",
                "Use Share → Export file to open this file in an app that supports its format.",
            );
        } else {
            match write_export(&entry.name, &bytes) {
                Ok(path) => ctx.hardware.share(&Data::File(path)),
                Err(error) => self.notice(ctx, "Could not export", &error.to_string()),
            }
        }
    }

    fn show_preview(
        &self,
        ctx: &mut Context,
        image: &Image,
        header: Option<(chk::Icons, Box<dyn chk::FlowBuilder>)>,
    ) {
        if let Some(photo) = chk::air::messages::Photo::encode(image, 1600, 95) {
            browser::show(
                ctx,
                &self.theme,
                chk::Page::Static(Box::new(chk::PageType::photo(photo, header))),
            );
        } else {
            self.notice(
                ctx,
                "Preview unavailable",
                "This document could not be displayed.",
            );
        }
    }

    fn show_pdf(&self, ctx: &mut Context, source: FileSource, bytes: &[u8], page: usize) {
        match crate::pdf_preview::render(bytes, page) {
            Ok((image, count)) => {
                let header = (count > 1).then(|| {
                    (
                        chk::Icons::Credential,
                        Box::new(move |_: &mut Context, theme: &Theme| match source {
                            FileSource::Private(id) => pdf_pages(id, page, count, theme),
                            FileSource::Shared(_, _) => pdf_pages_for(source, page, count, theme),
                        }) as Box<dyn chk::FlowBuilder>,
                    )
                });
                self.show_preview(ctx, &image, header);
            }
            Err(error) => self.notice(ctx, "Preview unavailable", &error),
        }
    }

    fn open_pdf_page(&self, ctx: &mut Context, source: FileSource, page: usize) {
        match load_file(&ctx.air, source) {
            Ok((_, bytes)) => {
                // Page selection closes the previous viewer/selector flow.
                // Rebuild a constant two-page navigation stack, with file
                // details behind the image, rather than stacking each page.
                let detail = match source {
                    FileSource::Private(id) => browser::detail(storage::open(&ctx.air), id),
                    FileSource::Shared(space, id) => match collaboration::get(&ctx.air, space) {
                        Ok(space) => collaboration_ui::item(space, id),
                        Err(error) => {
                            self.notice(ctx, "File unavailable", &error.to_string());
                            return;
                        }
                    },
                };
                browser::show(ctx, &self.theme, detail);
                self.show_pdf(ctx, source, &bytes, page);
            }
            Err(error) => self.notice(ctx, "Preview unavailable", &error.to_string()),
        }
    }
}

fn load_file(air: &Air, source: FileSource) -> Result<(Entry, Vec<u8>), String> {
    match source {
        FileSource::Private(id) => {
            let vault = storage::open(air);
            let snapshot = vault.pending();
            let entry = snapshot
                .get(id)
                .filter(|_| !snapshot.is_trashed(id))
                .ok_or_else(|| "This file is no longer available.".to_string())?
                .clone();
            let bytes = storage::read_file(air, &entry).map_err(|error| error.to_string())?;
            Ok((entry, bytes))
        }
        FileSource::Shared(space, id) => {
            let space = collaboration::get(air, space).map_err(|error| error.to_string())?;
            let snapshot = space.pending();
            if !snapshot.can_write(air.me()) {
                return Err("Join this shared item before opening its files.".into());
            }
            let entry = snapshot
                .tree
                .get(id)
                .filter(|_| !snapshot.tree.is_trashed(id))
                .ok_or_else(|| "This file is no longer available.".to_string())?
                .clone();
            let bytes = snapshot.read_file(id).map_err(|error| error.to_string())?;
            Ok((entry, bytes))
        }
    }
}

fn page_selection(source: FileSource, page: usize) -> chk::FormComplete {
    chk::FormComplete::Return(Box::new(move |ctx: &mut Context, _: &Theme| {
        ctx.emit(source.pdf_event(page));
    }))
}

pub(crate) fn pdf_pages(id: EntryId, page: usize, count: usize, theme: &Theme) -> chk::Flow {
    pdf_pages_for(FileSource::Private(id), page, count, theme)
}

fn pdf_pages_for(source: FileSource, page: usize, count: usize, theme: &Theme) -> chk::Flow {
    use chk::{Action, Form, FormAction, FormItem, FormValidState, Icons, State};
    let mut actions = Vec::new();
    if page > 0 {
        actions.push(FormAction::Custom(
            "Previous page".into(),
            None,
            Icons::Back,
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                page_selection(source, page - 1).run(ctx, theme)
            }),
        ));
    }
    if page + 1 < count {
        actions.push(FormAction::Custom(
            "Next page".into(),
            None,
            Icons::Forward,
            Action::custom(move |ctx: &mut Context, theme: &Theme| {
                page_selection(source, page + 1).run(ctx, theme)
            }),
        ));
    }
    chk::Flow::from_form(Form::flow(
        theme,
        vec![FormItem::text_with_preset(
            &format!("Page {} of {count}", page + 1),
            &(page + 1).to_string(),
            Some(actions),
            move |_: &mut Context, text: String| match parse_page(&text, count) {
                Some(_) => FormValidState::Valid,
                None => FormValidState::InvalidWithData(format!("Enter a page from 1 to {count}.")),
            },
        )],
        None,
        None,
        Box::new(move |_: &mut Context, states: &Vec<State>| {
            let Some(State::Text(text)) = states.first() else {
                return chk::FormComplete::None;
            };
            parse_page(text, count)
                .map(|page| page_selection(source, page))
                .unwrap_or(chk::FormComplete::None)
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

#[on_event]
impl OnEvent<FileEvent> for FileRuntime {
    fn on(&mut self, ctx: &mut Context, event: FileEvent) -> Vec<Box<dyn DynEvent>> {
        match event {
            FileEvent::Import(parent) => {
                if self.picker.is_none() {
                    self.destination = Some(PickerDestination::Private(parent));
                    self.picker = Some(ctx.hardware.pick::<Vec<PathBuf>>());
                }
            }
            FileEvent::Preview(id) => self.open_file(ctx, FileSource::Private(id), true),
            FileEvent::PdfPage(id, page) => self.open_pdf_page(ctx, FileSource::Private(id), page),
            FileEvent::Export(id) => self.open_file(ctx, FileSource::Private(id), false),
            FileEvent::SharedImport(space, parent) => self.pick_shared_import(ctx, space, parent),
            FileEvent::SharedPreview(space, id) => {
                self.open_file(ctx, FileSource::Shared(space, id), true)
            }
            FileEvent::SharedPdfPage(space, id, page) => {
                self.open_pdf_page(ctx, FileSource::Shared(space, id), page)
            }
            FileEvent::SharedExport(space, id) => {
                self.open_file(ctx, FileSource::Shared(space, id), false)
            }
            FileEvent::SharedReplace(space, id) => self.pick_shared_replacement(ctx, space, id),
            FileEvent::SharedExportConflict(space, id, conflict) => {
                self.export_conflict(ctx, space, id, conflict)
            }
            FileEvent::DraftExport(id) => self.export_draft(ctx, id),
        }
        vec![]
    }
}

#[on_change(async {
    match &mut self.picker { Some(picker) => picker.next().await, None => std::future::pending().await }
})]
impl OnChange<PickResult<Vec<PathBuf>>> for FileRuntime {
    fn on_change(&mut self, ctx: &mut Context, result: PickResult<Vec<PathBuf>>) {
        self.picker = None;
        let destination = self.destination.take();
        if let (Ok(paths), Some(destination)) = (result, destination) {
            match destination {
                PickerDestination::Private(parent) => self.import(ctx, paths, parent),
                PickerDestination::Shared { space, parent } => {
                    self.import_shared(ctx, paths, space, parent)
                }
                PickerDestination::Replacement {
                    space,
                    entry,
                    expected,
                } => self.replace_shared(ctx, paths, space, entry, expected),
            }
        }
    }
}

/// Reject folders, non-UTF8 names and oversized files before allocating their contents.
fn safe_image_dimensions(bytes: &[u8]) -> bool {
    chk::image::io::Reader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(width, height)| {
            width > 0
                && height > 0
                && width <= 8192
                && height <= 8192
                && u64::from(width) * u64::from(height) <= 16_000_000
        })
}

fn read_import(path: &Path) -> Result<Vec<u8>, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("The filename is not valid Unicode.")?;
    storage::validate_name(name).map_err(|error| error.to_string())?;
    read_contents(path)
}

/// Replacement changes the contents only; the existing shared filename stays intact.
fn read_replacement(paths: &[PathBuf]) -> Result<Vec<u8>, String> {
    let [path] = paths else {
        return Err("Choose exactly one replacement file.".into());
    };
    read_contents(path)
}

fn read_contents(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| format!("Could not read the file: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Select individual files; create folders inside Fig Files.".into());
    }
    if metadata.len() > MAX_FILE_SIZE as u64 {
        return Err(storage::StorageError::FileTooLarge.to_string());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_FILE_SIZE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FILE_SIZE {
        return Err(storage::StorageError::FileTooLarge.to_string());
    }
    Ok(bytes)
}

/// Unique private staging directory avoids overwriting any existing user file.
fn write_export(name: &str, bytes: &[u8]) -> Result<PathBuf, std::io::Error> {
    storage::validate_name(name)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let directory =
        std::env::temp_dir().join(format!("fig-files-export-{}", prism::air::Id::random()));
    fs::create_dir(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let path = directory.join(name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&path)?;
    output.write_all(bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn pdf_navigation_keeps_shared_space_and_file_scope() {
        let space = Id::random();
        let entry = Id::random();
        assert!(matches!(
            FileSource::Private(entry).pdf_event(2),
            FileEvent::PdfPage(actual, 2) if actual == entry
        ));
        assert!(matches!(
            FileSource::Shared(space, entry).pdf_event(4),
            FileEvent::SharedPdfPage(actual_space, actual_entry, 4)
                if actual_space == space && actual_entry == entry
        ));
    }

    #[test]
    fn replacement_reads_one_file_without_adopting_its_name() {
        let bytes = b"\0replacement\xff\r\n";
        let path = write_export("different-name.bin", bytes).unwrap();
        assert_eq!(
            read_replacement(std::slice::from_ref(&path)).unwrap(),
            bytes
        );
        assert!(read_replacement(&[]).unwrap_err().contains("exactly one"));
        assert!(
            read_replacement(&[path.clone(), path.clone()])
                .unwrap_err()
                .contains("exactly one")
        );
        assert!(read_replacement(&[path.parent().unwrap().to_owned()]).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn import_export_preserves_binary_and_never_overwrites() {
        let bytes = b"\0Fig\xff\r\n";
        let first = write_export("sample.bin", bytes).unwrap();
        let second = write_export("sample.bin", b"different").unwrap();
        assert_ne!(first, second);
        assert_eq!(read_import(&first).unwrap(), bytes);
        assert_eq!(read_import(&second).unwrap(), b"different");
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
        fs::remove_dir_all(second.parent().unwrap()).unwrap();
    }

    #[test]
    fn export_rejects_path_traversal_and_import_rejects_directories() {
        assert!(write_export("../escape.txt", b"data").is_err());
        assert!(write_export("nested/file.txt", b"data").is_err());
        assert!(read_import(&std::env::temp_dir()).is_err());
    }

    #[test]
    fn importer_rejects_oversized_files() {
        let path = write_export("large.bin", b"").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_FILE_SIZE as u64 + 1)
            .unwrap();
        assert!(read_import(&path).unwrap_err().contains("8 MiB"));
        assert!(
            read_replacement(std::slice::from_ref(&path))
                .unwrap_err()
                .contains("8 MiB")
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
