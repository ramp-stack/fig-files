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
    hardware::{Data, PickResult, Picked},
};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub enum FileEvent {
    Import(Option<EntryId>),
    Export(EntryId),
    SharedImport(Id, EntryId),
    SharedExport(Id, EntryId),
    SharedReplace(Id, EntryId),
    SharedExportConflict(Id, EntryId, Id),
    DraftExport(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileSource {
    Private(EntryId),
    Shared(Id, EntryId),
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

    fn export_file(&self, ctx: &mut Context, source: FileSource) {
        match load_file(&ctx.air, source) {
            Ok((entry, bytes)) => match write_export(&entry.name, &bytes) {
                Ok(path) => ctx.hardware.share(&Data::File(path)),
                Err(error) => self.notice(ctx, "Could not export", &error.to_string()),
            },
            Err(error) => self.notice(ctx, "File unavailable", &error),
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
            FileEvent::Export(id) => self.export_file(ctx, FileSource::Private(id)),
            FileEvent::SharedImport(space, parent) => self.pick_shared_import(ctx, space, parent),
            FileEvent::SharedExport(space, id) => {
                self.export_file(ctx, FileSource::Shared(space, id))
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
