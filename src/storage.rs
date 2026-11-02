//! Private AIR contracts for the file index and immutable file contents.
//!
//! The index contains no machine-local paths. File bytes live in separate AIR
//! instances, so renaming a file never re-uploads its contents. AIR retains its
//! encrypted history; removing an entry is not a secure erase of that history.

use ramp::prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type EntryId = Id;
pub const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_TREE_ITEMS: usize = 128;
pub const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_TREE_DEPTH: usize = 32;

#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub struct ImportItem {
    pub parent: Option<u32>,
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Folder,
    File { blob: Id, size: u64, mime: String },
    Shared { space: Id, folder: bool },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub parent: Option<EntryId>,
    pub name: String,
    pub kind: EntryKind,
    pub favorite: bool,
    /// Unix seconds.
    pub created_at: u64,
    pub modified_at: u64,
    /// Explicitly trashed ancestor roots are shown in Trash. Descendants follow
    /// their ancestor without losing their original folder relationships.
    pub trashed_at: Option<u64>,
}

impl Entry {
    pub fn is_folder(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::Folder | EntryKind::Shared { folder: true, .. }
        )
    }
    pub fn size(&self) -> u64 {
        match self.kind {
            EntryKind::Folder => 0,
            EntryKind::File { size, .. } => size,
            EntryKind::Shared { .. } => 0,
        }
    }
    pub fn mime(&self) -> &str {
        match &self.kind {
            EntryKind::Folder => "inode/directory",
            EntryKind::File { mime, .. } => mime,
            EntryKind::Shared { folder: true, .. } => "inode/directory",
            EntryKind::Shared { folder: false, .. } => "application/octet-stream",
        }
    }
    pub fn extension(&self) -> String {
        self.name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_lowercase())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vault {
    pub owner: Name,
    pub entries: BTreeMap<EntryId, Entry>,
}

#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub enum VaultAction {
    Create {
        id: EntryId,
        parent: Option<EntryId>,
        name: String,
        kind: EntryKind,
    },
    Rename {
        id: EntryId,
        name: String,
    },
    Move {
        id: EntryId,
        parent: Option<EntryId>,
    },
    Favorite {
        id: EntryId,
        favorite: bool,
    },
    Trash(EntryId),
    Restore(EntryId),
    DeletePermanently(EntryId),
    // Append variants to retain the existing AIR action wire format.
    ImportSnapshot {
        transfer: Id,
        items: Vec<ImportItem>,
    },
    PromoteShared {
        id: EntryId,
        space: Id,
        expected: Id,
    },
    MountShared {
        space: Id,
        name: String,
        folder: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageError {
    Unauthorized,
    InvalidName,
    NameExists,
    MissingEntry,
    InvalidFolder,
    FolderCycle,
    InTrash,
    NotInTrash,
    NotAFile,
    FileTooLarge,
    ContentUnavailable,
    IdExists,
    InvalidSnapshot,
    Changed,
    SharedMount,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unauthorized => "This library belongs to another identity.",
            Self::InvalidName => {
                "Use a name of 1–255 bytes without control characters or / \\ : * ? \" < > |."
            }
            Self::NameExists => "An item with this name already exists in that folder.",
            Self::MissingEntry => "This item no longer exists.",
            Self::InvalidFolder => "Choose an existing folder outside Trash.",
            Self::FolderCycle => "A folder cannot be moved into itself or one of its subfolders.",
            Self::InTrash => "Restore this item from Trash first.",
            Self::NotInTrash => "Move this item to Trash before removing it permanently.",
            Self::NotAFile => "Choose a file to export.",
            Self::FileTooLarge => "Files can be up to 8 MiB each.",
            Self::ContentUnavailable => {
                "This file's content is not available yet. Let AIR finish syncing and try again."
            }
            Self::IdExists => "This item already exists.",
            Self::InvalidSnapshot => {
                "This folder snapshot is invalid or exceeds the transfer limits."
            }
            Self::Changed => "This item changed while sharing. Try sharing it again.",
            Self::SharedMount => "Open the shared item to change its contents. Nested shared items cannot be shared as a new folder.",
        })
    }
}
impl std::error::Error for StorageError {}

impl Contract for Vault {
    type Init = ();
    type Message = VaultAction;
    type Result = Result<EntryId, StorageError>;

    fn id() -> Id {
        Id::hash("org.ramp.fig-files.Vault.v1")
    }
    fn init(_: (), metadata: Metadata) -> Self {
        Self {
            owner: metadata.signer,
            entries: BTreeMap::new(),
        }
    }
    fn apply(&mut self, message: VaultAction, metadata: Metadata) -> Self::Result {
        if metadata.signer != self.owner {
            return Err(StorageError::Unauthorized);
        }
        let now = metadata.timestamp / 1_000_000_000;
        match message {
            VaultAction::Create {
                id,
                parent,
                name,
                kind,
            } => {
                if self.entries.contains_key(&id) {
                    return Err(StorageError::IdExists);
                }
                if matches!(kind, EntryKind::Shared { .. }) {
                    return Err(StorageError::SharedMount);
                }
                self.validate_destination(parent, &name, None)?;
                if let EntryKind::File { size, .. } = kind {
                    if size > MAX_FILE_SIZE as u64 {
                        return Err(StorageError::FileTooLarge);
                    }
                }
                self.entries.insert(
                    id,
                    Entry {
                        id,
                        parent,
                        name,
                        kind,
                        favorite: false,
                        created_at: now,
                        modified_at: now,
                        trashed_at: None,
                    },
                );
                Ok(id)
            }
            VaultAction::Rename { id, name } => {
                let entry = self.active(id)?;
                self.validate_destination(entry.parent, &name, Some(id))?;
                let entry = self.entries.get_mut(&id).unwrap();
                entry.name = name;
                entry.modified_at = now;
                Ok(id)
            }
            VaultAction::Move { id, parent } => {
                let entry = self.active(id)?;
                if parent == Some(id) || parent.is_some_and(|parent| self.is_descendant(parent, id))
                {
                    return Err(StorageError::FolderCycle);
                }
                self.validate_destination(parent, &entry.name, Some(id))?;
                let entry = self.entries.get_mut(&id).unwrap();
                entry.parent = parent;
                entry.modified_at = now;
                Ok(id)
            }
            VaultAction::Favorite { id, favorite } => {
                self.active(id)?;
                self.entries.get_mut(&id).unwrap().favorite = favorite;
                Ok(id)
            }
            VaultAction::Trash(id) => {
                self.active(id)?;
                let entry = self.entries.get_mut(&id).unwrap();
                entry.trashed_at = Some(now);
                entry.modified_at = now;
                Ok(id)
            }
            VaultAction::Restore(id) => {
                let entry = self.entries.get(&id).ok_or(StorageError::MissingEntry)?;
                if !self.is_trashed(id) {
                    return Err(StorageError::NotInTrash);
                }
                // A child restored separately from its trashed ancestor goes to
                // the library root; restoring a folder preserves its subtree.
                let parent = entry.parent.filter(|parent| {
                    self.entries.get(parent).is_some_and(Entry::is_folder)
                        && !self.is_trashed(*parent)
                });
                self.validate_destination(parent, &entry.name, Some(id))?;
                let entry = self.entries.get_mut(&id).unwrap();
                entry.parent = parent;
                entry.trashed_at = None;
                entry.modified_at = now;
                Ok(id)
            }
            VaultAction::DeletePermanently(id) => {
                if !self.entries.contains_key(&id) {
                    return Err(StorageError::MissingEntry);
                }
                if !self.is_trashed(id) {
                    return Err(StorageError::NotInTrash);
                }
                let remove: BTreeSet<_> = self
                    .entries
                    .keys()
                    .copied()
                    .filter(|child| *child == id || self.is_descendant(*child, id))
                    .collect();
                self.entries.retain(|key, _| !remove.contains(key));
                Ok(id)
            }
            VaultAction::ImportSnapshot { transfer, items } => {
                validate_import_items(&items)?;
                let root = imported_id(transfer, 0);
                if self.entries.contains_key(&root) {
                    return Ok(root);
                }
                let mut candidate = self.clone();
                for (index, item) in items.into_iter().enumerate() {
                    let id = imported_id(transfer, index as u32);
                    let parent = item.parent.map(|parent| imported_id(transfer, parent));
                    let name = if index == 0 {
                        candidate.available_root_name(
                            &item.name,
                            matches!(item.kind, EntryKind::Folder),
                        )?
                    } else {
                        item.name
                    };
                    candidate.apply(
                        VaultAction::Create {
                            id,
                            parent,
                            name,
                            kind: item.kind,
                        },
                        Metadata {
                            signer: metadata.signer,
                            timestamp: metadata.timestamp,
                            confirmed: metadata.confirmed,
                        },
                    )?;
                }
                *self = candidate;
                Ok(root)
            }
            VaultAction::PromoteShared {
                id,
                space,
                expected,
            } => {
                if self.subtree_fingerprint(id)? != expected
                    && self.legacy_subtree_fingerprint(id)? != expected
                {
                    return Err(StorageError::Changed);
                }
                let folder = self.active(id)?.is_folder();
                let descendants: BTreeSet<_> = self
                    .entries
                    .keys()
                    .copied()
                    .filter(|child| self.is_descendant(*child, id))
                    .collect();
                if std::iter::once(id)
                    .chain(descendants.iter().copied())
                    .any(|child| {
                        matches!(self.entries[&child].kind, EntryKind::Shared { .. })
                            && !self.is_trashed(child)
                    })
                {
                    return Err(StorageError::SharedMount);
                }
                // Existing private Trash is excluded from invitations and stays private.
                let retained: BTreeSet<_> = descendants
                    .iter()
                    .copied()
                    .filter(|child| self.is_trashed(*child))
                    .collect();
                for child in &retained {
                    let entry = self.entries.get_mut(child).unwrap();
                    if entry
                        .parent
                        .is_some_and(|parent| !retained.contains(&parent))
                    {
                        entry.parent = None;
                    }
                }
                self.entries
                    .retain(|child, _| !descendants.contains(child) || retained.contains(child));
                let entry = self.entries.get_mut(&id).unwrap();
                entry.kind = EntryKind::Shared { space, folder };
                entry.modified_at = now;
                Ok(id)
            }
            VaultAction::MountShared {
                space,
                name,
                folder,
            } => {
                if let Some(id) = self.entries.values().find(|entry| matches!(entry.kind, EntryKind::Shared { space: existing, .. } if existing == space)).map(|entry| entry.id) {
                    if self.is_trashed(id) {
                        let name = self.available_root_name(&self.entries[&id].name, folder)?;
                        let entry = self.entries.get_mut(&id).unwrap();
                        entry.name = name;
                        entry.parent = None;
                        entry.trashed_at = None;
                        entry.modified_at = now;
                    }
                    return Ok(id);
                }
                let id = shared_mount_id(space);
                if self.entries.contains_key(&id) {
                    return Err(StorageError::IdExists);
                }
                let name = self.available_root_name(&name, folder)?;
                self.entries.insert(
                    id,
                    Entry {
                        id,
                        parent: None,
                        name,
                        kind: EntryKind::Shared { space, folder },
                        favorite: false,
                        created_at: now,
                        modified_at: now,
                        trashed_at: None,
                    },
                );
                Ok(id)
            }
        }
    }
}

impl Vault {
    pub fn subtree_fingerprint(&self, id: EntryId) -> Result<Id, StorageError> {
        let entries: Vec<_> = self
            .subtree_entries(id)?
            .into_iter()
            .map(|entry| {
                (
                    entry.id,
                    entry.parent,
                    &entry.name,
                    &entry.kind,
                    entry.favorite,
                    entry.trashed_at.is_some(),
                )
            })
            .collect();
        // AIR replaces speculative timestamps during confirmation and replay.
        // Only content, hierarchy, and explicit Trash state guard promotion.
        Ok(Id::hash(&("org.ramp.fig-files.subtree.v2", entries)))
    }

    fn legacy_subtree_fingerprint(&self, id: EntryId) -> Result<Id, StorageError> {
        // Preserve replay of PromoteShared messages written before timestamps
        // were excluded from the fingerprint. New messages use the stable form.
        Ok(Id::hash(&self.subtree_entries(id)?))
    }

    fn subtree_entries(&self, id: EntryId) -> Result<Vec<&Entry>, StorageError> {
        self.active(id)?;
        Ok(self
            .entries
            .values()
            .filter(|entry| entry.id == id || self.is_descendant(entry.id, id))
            .collect())
    }
    fn available_root_name(&self, name: &str, folder: bool) -> Result<String, StorageError> {
        match self.validate_destination(None, name, None) {
            Ok(()) => return Ok(name.into()),
            Err(StorageError::NameExists) => {}
            Err(error) => return Err(error),
        }
        let (stem, extension) = if folder {
            (name, String::new())
        } else {
            name.rsplit_once('.')
                .filter(|(stem, _)| !stem.is_empty())
                .map(|(stem, extension)| (stem, format!(".{extension}")))
                .unwrap_or((name, String::new()))
        };
        // Each suffix is distinct. At most entries.len() names can be occupied,
        // so this finite search always includes an unused valid candidate.
        for index in 2..=self.entries.len().saturating_add(2) {
            let suffix = format!(" ({index})");
            // An extremely long extension must not consume the whole filename.
            // Truncation may expose whitespace that was internal in the name.
            let extension = truncate_utf8(&extension, 100)
                .trim_end_matches(|character: char| character == '.' || character.is_whitespace());
            let stem = truncate_utf8(stem, 255 - suffix.len() - extension.len())
                .trim_end_matches(|character: char| character == '.' || character.is_whitespace());
            let stem = if stem.is_empty() { "Received" } else { stem };
            let candidate = format!("{stem}{suffix}{extension}");
            match self.validate_destination(None, &candidate, None) {
                Ok(()) => return Ok(candidate),
                Err(StorageError::NameExists) => {}
                Err(error) => return Err(error),
            }
        }
        Err(StorageError::NameExists)
    }
    pub fn get(&self, id: EntryId) -> Option<&Entry> {
        self.entries.get(&id)
    }

    fn active(&self, id: EntryId) -> Result<&Entry, StorageError> {
        let entry = self.entries.get(&id).ok_or(StorageError::MissingEntry)?;
        if self.is_trashed(id) {
            return Err(StorageError::InTrash);
        }
        Ok(entry)
    }

    pub fn validate_destination(
        &self,
        parent: Option<EntryId>,
        name: &str,
        except: Option<EntryId>,
    ) -> Result<(), StorageError> {
        validate_name(name)?;
        if let Some(parent) = parent {
            if !self
                .entries
                .get(&parent)
                .is_some_and(|entry| matches!(entry.kind, EntryKind::Folder))
                || self.is_trashed(parent)
            {
                return Err(StorageError::InvalidFolder);
            }
        }
        if self.entries.values().any(|entry| {
            Some(entry.id) != except
                && entry.parent == parent
                && !self.is_trashed(entry.id)
                && entry.name.to_lowercase() == name.to_lowercase()
        }) {
            return Err(StorageError::NameExists);
        }
        Ok(())
    }

    pub fn is_trashed(&self, id: EntryId) -> bool {
        let mut cursor = Some(id);
        let mut visited = BTreeSet::new();
        while let Some(id) = cursor {
            if !visited.insert(id) {
                return true;
            }
            let Some(entry) = self.entries.get(&id) else {
                return true;
            };
            if entry.trashed_at.is_some() {
                return true;
            }
            cursor = entry.parent;
        }
        false
    }

    pub fn is_descendant(&self, child: EntryId, ancestor: EntryId) -> bool {
        let mut cursor = self.entries.get(&child).and_then(|entry| entry.parent);
        let mut visited = BTreeSet::new();
        while let Some(id) = cursor {
            if id == ancestor {
                return true;
            }
            if !visited.insert(id) {
                break;
            }
            cursor = self.entries.get(&id).and_then(|entry| entry.parent);
        }
        false
    }

    pub fn children(&self, parent: Option<EntryId>) -> Vec<Entry> {
        sorted(
            self.entries
                .values()
                .filter(|entry| entry.parent == parent && !self.is_trashed(entry.id))
                .cloned()
                .collect(),
        )
    }
    pub fn favorites(&self) -> Vec<Entry> {
        sorted(
            self.entries
                .values()
                .filter(|entry| entry.favorite && !self.is_trashed(entry.id))
                .cloned()
                .collect(),
        )
    }
    pub fn trash(&self) -> Vec<Entry> {
        // Nested explicitly trashed items are still covered by their trashed
        // ancestor until that ancestor is restored.
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                entry.trashed_at.is_some()
                    && entry.parent.is_none_or(|parent| !self.is_trashed(parent))
            })
            .cloned()
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.trashed_at));
        entries
    }
    pub fn search(&self, query: &str) -> Vec<Entry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        sorted(
            self.entries
                .values()
                .filter(|entry| {
                    !self.is_trashed(entry.id) && entry.name.to_lowercase().contains(&query)
                })
                .cloned()
                .collect(),
        )
    }
    pub fn recent(&self) -> Vec<Entry> {
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|entry| !entry.is_folder() && !self.is_trashed(entry.id))
            .cloned()
            .collect();
        entries.sort_by(|a, b| {
            b.modified_at
                .cmp(&a.modified_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        entries
    }
    pub fn folders(&self) -> Vec<Entry> {
        sorted(
            self.entries
                .values()
                .filter(|entry| entry.is_folder() && !self.is_trashed(entry.id))
                .cloned()
                .collect(),
        )
    }
    /// Root-to-leaf folder chain, excluding the implicit library root.
    pub fn breadcrumbs(&self, parent: Option<EntryId>) -> Vec<Entry> {
        let mut result = Vec::new();
        let mut cursor = parent;
        let mut visited = BTreeSet::new();
        while let Some(id) = cursor {
            if !visited.insert(id) {
                break;
            }
            let Some(entry) = self.entries.get(&id) else {
                break;
            };
            result.push(entry.clone());
            cursor = entry.parent;
        }
        result.reverse();
        result
    }
    pub fn total_size(&self) -> u64 {
        self.entries
            .values()
            .filter(|entry| !self.is_trashed(entry.id))
            .map(Entry::size)
            .sum()
    }
    pub fn item_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| !self.is_trashed(entry.id))
            .count()
    }
}

fn truncate_utf8(value: &str, max: usize) -> &str {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub fn imported_id(transfer: Id, index: u32) -> EntryId {
    Id::hash(&("org.ramp.fig-files.Import.v1", transfer, index))
}
pub fn shared_mount_id(space: Id) -> EntryId {
    Id::hash(&("org.ramp.fig-files.SharedMount.v1", space))
}

pub fn validate_import_items(items: &[ImportItem]) -> Result<(), StorageError> {
    if items.is_empty() || items.len() > MAX_TREE_ITEMS {
        return Err(StorageError::InvalidSnapshot);
    }
    let mut depths = Vec::with_capacity(items.len());
    let mut siblings = BTreeSet::new();
    let mut bytes = 0u64;
    for (index, item) in items.iter().enumerate() {
        if matches!(item.kind, EntryKind::Shared { .. }) {
            return Err(StorageError::SharedMount);
        }
        validate_name(&item.name)?;
        let depth = match item.parent {
            None if index == 0 => 1,
            Some(parent)
                if (parent as usize) < index
                    && matches!(items[parent as usize].kind, EntryKind::Folder) =>
            {
                depths[parent as usize] + 1
            }
            _ => return Err(StorageError::InvalidSnapshot),
        };
        if depth > MAX_TREE_DEPTH || !siblings.insert((item.parent, item.name.to_lowercase())) {
            return Err(StorageError::InvalidSnapshot);
        }
        depths.push(depth);
        if let EntryKind::File { size, ref mime, .. } = item.kind {
            if size > MAX_FILE_SIZE as u64 || mime.len() > 255 || mime.chars().any(char::is_control)
            {
                return Err(StorageError::InvalidSnapshot);
            }
            bytes = bytes
                .checked_add(size)
                .ok_or(StorageError::InvalidSnapshot)?;
            if bytes > MAX_TREE_BYTES {
                return Err(StorageError::InvalidSnapshot);
            }
        }
    }
    Ok(())
}

fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| {
        b.is_folder()
            .cmp(&a.is_folder())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    entries
}

pub fn validate_name(name: &str) -> Result<(), StorageError> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if name.is_empty()
        || name.len() > 255
        || name != name.trim()
        || matches!(name, "." | "..")
        || name.ends_with('.')
        || reserved
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        Err(StorageError::InvalidName)
    } else {
        Ok(())
    }
}

/// Immutable binary data. Its deterministic AIR initialization de-duplicates
/// identical bytes within this identity. No access is shared with other users.
#[derive(Clone, Hash, Serialize, Deserialize)]
pub struct FileBlob {
    pub owner: Name,
    pub bytes: Vec<u8>,
}
impl fmt::Debug for FileBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileBlob")
            .field("owner", &self.owner)
            .field("size", &self.bytes.len())
            .finish()
    }
}
impl Contract for FileBlob {
    type Init = Vec<u8>;
    type Message = ();
    type Result = ();
    fn id() -> Id {
        Id::hash("org.ramp.fig-files.FileBlob.v1")
    }
    fn init(bytes: Vec<u8>, metadata: Metadata) -> Self {
        Self {
            owner: metadata.signer,
            bytes,
        }
    }
    fn apply(&mut self, _: (), _: Metadata) {}
}

pub fn open(air: &Air) -> Instance<Vault> {
    // Starting this collection restores its cached instances and subscribes to
    // the private AIR catalog, including blobs received after the index. File
    // content can sync before the user first asks to preview or export it.
    let _ = air.instances::<FileBlob>();
    air.instances::<Vault>().create(())
}

fn send(vault: &mut Instance<Vault>, action: VaultAction) -> Result<EntryId, StorageError> {
    // Validate before enqueueing so rejected actions do not accumulate in AIR.
    let mut preview = vault.pending().clone();
    preview.apply(
        action.clone(),
        Metadata {
            signer: preview.owner,
            timestamp: 0,
            confirmed: false,
        },
    )?;
    let result = vault.send(action);
    result.pending().clone()
}

pub fn create_folder(
    vault: &mut Instance<Vault>,
    parent: Option<EntryId>,
    name: &str,
) -> Result<EntryId, StorageError> {
    send(
        vault,
        VaultAction::Create {
            id: Id::random(),
            parent,
            name: name.trim().to_owned(),
            kind: EntryKind::Folder,
        },
    )
}

pub fn import_file(
    air: &Air,
    vault: &mut Instance<Vault>,
    parent: Option<EntryId>,
    name: &str,
    bytes: Vec<u8>,
) -> Result<EntryId, StorageError> {
    if bytes.len() > MAX_FILE_SIZE {
        return Err(StorageError::FileTooLarge);
    }
    let name = name.trim();
    vault.pending().validate_destination(parent, name, None)?;
    let size = bytes.len() as u64;
    let blob = air.instances::<FileBlob>().create(bytes);
    send(
        vault,
        VaultAction::Create {
            id: Id::random(),
            parent,
            name: name.to_owned(),
            kind: EntryKind::File {
                blob: blob.id(),
                size,
                mime: mime_for_name(name).into(),
            },
        },
    )
}

pub fn read_file(air: &Air, entry: &Entry) -> Result<Vec<u8>, StorageError> {
    let EntryKind::File { blob, size, .. } = entry.kind else {
        return Err(StorageError::NotAFile);
    };
    let blobs = air.instances::<FileBlob>().list();
    let blob = blobs.get(&blob).ok_or(StorageError::ContentUnavailable)?;
    let content = blob.pending();
    if content.owner != air.me() || content.bytes.len() as u64 != size {
        return Err(StorageError::ContentUnavailable);
    }
    Ok(content.bytes.clone())
}

pub fn rename(
    vault: &mut Instance<Vault>,
    id: EntryId,
    name: &str,
) -> Result<EntryId, StorageError> {
    send(
        vault,
        VaultAction::Rename {
            id,
            name: name.trim().into(),
        },
    )
}
pub fn move_entry(
    vault: &mut Instance<Vault>,
    id: EntryId,
    parent: Option<EntryId>,
) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::Move { id, parent })
}
pub fn set_favorite(
    vault: &mut Instance<Vault>,
    id: EntryId,
    favorite: bool,
) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::Favorite { id, favorite })
}
pub fn trash(vault: &mut Instance<Vault>, id: EntryId) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::Trash(id))
}
pub fn restore(vault: &mut Instance<Vault>, id: EntryId) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::Restore(id))
}
pub fn delete_permanently(
    vault: &mut Instance<Vault>,
    id: EntryId,
) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::DeletePermanently(id))
}

pub fn import_snapshot(
    vault: &mut Instance<Vault>,
    transfer: Id,
    items: Vec<ImportItem>,
) -> Result<EntryId, StorageError> {
    send(vault, VaultAction::ImportSnapshot { transfer, items })
}

pub fn promote_shared(
    vault: &mut Instance<Vault>,
    id: EntryId,
    space: Id,
    expected: Id,
) -> Result<EntryId, StorageError> {
    send(
        vault,
        VaultAction::PromoteShared {
            id,
            space,
            expected,
        },
    )
}
pub fn mount_shared(
    vault: &mut Instance<Vault>,
    space: Id,
    name: &str,
    folder: bool,
) -> Result<EntryId, StorageError> {
    send(
        vault,
        VaultAction::MountShared {
            space,
            name: name.into(),
            folder,
        },
    )
}

pub fn mime_for_name(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "toml" | "yaml" | "yml" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn metadata() -> Metadata {
        Metadata {
            signer: Name::orange_me(),
            timestamp: 12_000_000_000,
            confirmed: true,
        }
    }
    fn vault() -> Vault {
        Vault::init((), metadata())
    }
    fn folder(vault: &mut Vault, id: u64, parent: Option<u64>, name: &str) -> EntryId {
        let id = Id::from(id);
        assert_eq!(
            vault.apply(
                VaultAction::Create {
                    id,
                    parent: parent.map(Id::from),
                    name: name.into(),
                    kind: EntryKind::Folder,
                },
                metadata()
            ),
            Ok(id)
        );
        id
    }

    fn promotion_fixture(timestamp: u64, confirmed: bool) -> (Vault, EntryId, EntryId, EntryId) {
        let at = || Metadata {
            timestamp,
            confirmed,
            ..metadata()
        };
        let mut vault = Vault::init((), at());
        let root = Id::from(100);
        let file = Id::from(101);
        let trashed = Id::from(102);
        for (id, parent, name, kind) in [
            (root, None, "Project", EntryKind::Folder),
            (
                file,
                Some(root),
                "notes.txt",
                EntryKind::File {
                    blob: Id::from(103),
                    size: 8,
                    mime: "text/plain".into(),
                },
            ),
            (trashed, Some(root), "Old drafts", EntryKind::Folder),
        ] {
            vault
                .apply(
                    VaultAction::Create {
                        id,
                        parent,
                        name: name.into(),
                        kind,
                    },
                    at(),
                )
                .unwrap();
        }
        vault.apply(VaultAction::Trash(trashed), at()).unwrap();
        (vault, root, file, trashed)
    }

    #[test]
    fn promotion_survives_pending_to_confirmed_timestamp_changes() {
        let (pending, root, file, trashed) = promotion_fixture(12_000_000_000, false);
        let (mut confirmed, _, _, _) = promotion_fixture(82_000_000_000, true);
        let expected = pending.subtree_fingerprint(root).unwrap();
        assert_eq!(expected, confirmed.subtree_fingerprint(root).unwrap());
        assert_ne!(
            pending.legacy_subtree_fingerprint(root).unwrap(),
            confirmed.legacy_subtree_fingerprint(root).unwrap()
        );
        let space = Id::from(104);
        assert_eq!(
            confirmed.apply(
                VaultAction::PromoteShared {
                    id: root,
                    space,
                    expected,
                },
                Metadata {
                    timestamp: 83_000_000_000,
                    ..metadata()
                },
            ),
            Ok(root)
        );
        assert_eq!(
            confirmed.entries[&root].kind,
            EntryKind::Shared {
                space,
                folder: true
            }
        );
        assert!(confirmed.get(file).is_none());
        assert!(confirmed.is_trashed(trashed));
        assert_eq!(confirmed.entries[&trashed].parent, None);
    }

    #[test]
    fn promotion_accepts_legacy_raw_entry_fingerprints() {
        let (mut vault, root, file, _) = promotion_fixture(12_000_000_000, true);
        // This is the exact original hash representation, including timestamps.
        let entries: Vec<_> = vault.entries.values().collect();
        let expected = Id::hash(&entries);
        assert_ne!(expected, vault.subtree_fingerprint(root).unwrap());
        assert_eq!(
            vault.apply(
                VaultAction::PromoteShared {
                    id: root,
                    space: Id::from(104),
                    expected,
                },
                metadata(),
            ),
            Ok(root)
        );
        assert!(vault.get(file).is_none());
    }

    #[test]
    fn promotion_still_rejects_semantic_changes_without_mutating() {
        let (original, root, file, trashed) = promotion_fixture(12_000_000_000, true);
        let expected = original.subtree_fingerprint(root).unwrap();
        for change in ["rename", "blob", "move", "favorite", "trash", "restore"] {
            let mut changed = original.clone();
            match change {
                "rename" => changed.entries.get_mut(&file).unwrap().name = "changed.txt".into(),
                "blob" => {
                    let EntryKind::File { blob, .. } =
                        &mut changed.entries.get_mut(&file).unwrap().kind
                    else {
                        unreachable!()
                    };
                    *blob = Id::from(105);
                }
                "move" => changed.entries.get_mut(&file).unwrap().parent = None,
                "favorite" => changed.entries.get_mut(&file).unwrap().favorite = true,
                "trash" => changed.entries.get_mut(&file).unwrap().trashed_at = Some(13),
                "restore" => changed.entries.get_mut(&trashed).unwrap().trashed_at = None,
                _ => unreachable!(),
            }
            let before = changed.clone();
            assert_eq!(
                changed.apply(
                    VaultAction::PromoteShared {
                        id: root,
                        space: Id::from(104),
                        expected,
                    },
                    metadata(),
                ),
                Err(StorageError::Changed),
                "{change}"
            );
            assert_eq!(changed, before, "{change}");
        }
    }

    #[test]
    fn folder_moves_reject_cycles_and_duplicate_names_without_mutating() {
        let mut vault = vault();
        let a = folder(&mut vault, 1, None, "Projects");
        let b = folder(&mut vault, 2, Some(1), "Design");
        folder(&mut vault, 3, None, "Design");
        let before = vault.clone();
        assert_eq!(
            vault.apply(
                VaultAction::Move {
                    id: a,
                    parent: Some(b)
                },
                metadata()
            ),
            Err(StorageError::FolderCycle)
        );
        assert_eq!(
            vault.apply(
                VaultAction::Move {
                    id: b,
                    parent: None
                },
                metadata()
            ),
            Err(StorageError::NameExists)
        );
        assert_eq!(
            vault.apply(
                VaultAction::Rename {
                    id: a,
                    name: "design".into()
                },
                metadata()
            ),
            Err(StorageError::NameExists)
        );
        assert_eq!(vault, before);
    }

    #[test]
    fn trash_restore_and_permanent_removal_preserve_subtree_semantics() {
        let mut vault = vault();
        let a = folder(&mut vault, 1, None, "Projects");
        let b = folder(&mut vault, 2, Some(1), "Design");
        let c = folder(&mut vault, 3, Some(2), "Archive");
        vault
            .apply(
                VaultAction::Favorite {
                    id: b,
                    favorite: true,
                },
                metadata(),
            )
            .unwrap();
        vault.apply(VaultAction::Trash(a), metadata()).unwrap();
        assert!(vault.is_trashed(c));
        assert!(vault.favorites().is_empty());
        assert!(vault.search("design").is_empty());
        assert_eq!(
            vault
                .trash()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![a]
        );
        vault.apply(VaultAction::Restore(a), metadata()).unwrap();
        assert!(!vault.is_trashed(c));
        assert_eq!(vault.favorites()[0].id, b);
        assert_eq!(vault.get(b).unwrap().parent, Some(a));
        assert_eq!(
            vault.apply(VaultAction::DeletePermanently(a), metadata()),
            Err(StorageError::NotInTrash)
        );
        vault.apply(VaultAction::Trash(a), metadata()).unwrap();
        vault
            .apply(VaultAction::DeletePermanently(a), metadata())
            .unwrap();
        assert!(vault.entries.is_empty());
    }

    #[test]
    fn restore_conflicts_do_not_overwrite_and_detached_child_goes_to_root() {
        let mut vault = vault();
        let a = folder(&mut vault, 1, None, "Projects");
        let b = folder(&mut vault, 2, Some(1), "Design");
        vault.apply(VaultAction::Trash(a), metadata()).unwrap();
        folder(&mut vault, 3, None, "Projects");
        assert_eq!(
            vault.apply(VaultAction::Restore(a), metadata()),
            Err(StorageError::NameExists)
        );
        assert!(vault.is_trashed(a));
        vault.apply(VaultAction::Restore(b), metadata()).unwrap();
        assert_eq!(vault.get(b).unwrap().parent, None);
        assert!(!vault.is_trashed(b));
    }

    #[test]
    fn a_previously_trashed_child_stays_trashed_when_parent_is_restored() {
        let mut vault = vault();
        let a = folder(&mut vault, 1, None, "Projects");
        let b = folder(&mut vault, 2, Some(1), "Design");
        vault.apply(VaultAction::Trash(b), metadata()).unwrap();
        vault.apply(VaultAction::Trash(a), metadata()).unwrap();
        assert_eq!(vault.trash().len(), 1);
        vault.apply(VaultAction::Restore(a), metadata()).unwrap();
        assert!(vault.is_trashed(b));
        assert_eq!(vault.trash()[0].id, b);
    }

    #[test]
    fn only_owner_may_mutate_and_invalid_names_are_rejected() {
        let mut vault = vault();
        let id = folder(&mut vault, 1, None, "Projects");
        let other = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
            .parse()
            .unwrap();
        assert_eq!(
            vault.apply(
                VaultAction::Trash(id),
                Metadata {
                    signer: other,
                    timestamp: 0,
                    confirmed: true
                }
            ),
            Err(StorageError::Unauthorized)
        );
        assert!(!vault.is_trashed(id));
        for name in [
            "",
            "  ",
            ".",
            "..",
            "../file",
            "file\\name",
            "line\nbreak",
            "bad:",
            "NUL.txt",
            "COM1",
            "trailing.",
        ] {
            assert_eq!(
                validate_name(name),
                Err(StorageError::InvalidName),
                "{name:?}"
            );
        }
        assert!(validate_name("Figs & notes 🟣.txt").is_ok());
    }

    #[test]
    fn file_bytes_and_metadata_survive_independent_air_contract_replay() {
        let mut vault = vault();
        let bytes = vec![0, 1, 2, 255, 0, 120];
        let blob = FileBlob::init(bytes.clone(), metadata());
        let id = Id::from(1);
        vault
            .apply(
                VaultAction::Create {
                    id,
                    parent: None,
                    name: "original.bin".into(),
                    kind: EntryKind::File {
                        blob: Id::from(20),
                        size: bytes.len() as u64,
                        mime: "application/octet-stream".into(),
                    },
                },
                metadata(),
            )
            .unwrap();
        vault
            .apply(
                VaultAction::Rename {
                    id,
                    name: "renamed.bin".into(),
                },
                metadata(),
            )
            .unwrap();
        assert_eq!(blob.bytes, bytes);
        assert_eq!(vault.total_size(), 6);
        assert_eq!(vault.get(id).unwrap().created_at, 12);
        assert_eq!(vault.search("RENAMED")[0].id, id);
        let before = vault.clone();
        assert_eq!(
            vault.apply(
                VaultAction::Create {
                    id: Id::from(2),
                    parent: None,
                    name: "too-large.bin".into(),
                    kind: EntryKind::File {
                        blob: Id::from(21),
                        size: MAX_FILE_SIZE as u64 + 1,
                        mime: "application/octet-stream".into()
                    },
                },
                metadata()
            ),
            Err(StorageError::FileTooLarge)
        );
        assert_eq!(vault, before);
    }

    /// Exercise the published AIR signing, encryption, channel, and contract
    /// implementation against its real Storage API with an in-memory database.
    /// This does not require a live server or make network requests.
    pub(crate) struct TestServer {
        storage: air::Storage,
        database: std::collections::HashMap<air::PublicKey, (u64, air::KeySignature, Vec<u8>)>,
        request_index: u64,
    }

    impl TestServer {
        pub(crate) fn new() -> Self {
            Self {
                storage: air::Storage::new(air::Secret::new()),
                database: Default::default(),
                request_index: 0,
            }
        }

        pub(crate) fn request(&mut self, request: air::Request) -> air::Response {
            self.request_index += 1;
            let index = self.request_index;
            let database = &mut self.database;
            self.storage
                .handle(1, index, request, |request| match request {
                    air::DBRequest::Read(key) => database.get(&key).cloned(),
                    air::DBRequest::Write(timestamp, signed) => match database.entry(signed.key) {
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            Some(entry.get().clone())
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert((timestamp, signed.signature, signed.payload));
                            None
                        }
                    },
                })
                .remove(&(1, index))
                .expect("a request has a response")
                .payload
                .1
        }

        pub(crate) fn sync<C: Contract>(&mut self, instance: &mut air::Instance<C>) {
            let mut request = instance.start();
            for _ in 0..128 {
                let response = self.request(request);
                let mut resolver = air::DefaultResolver::default();
                let _ = ready(instance.response(&mut resolver, response));
                match instance.request() {
                    Some(next) => request = next,
                    None => return,
                }
            }
            panic!("AIR did not settle within the test's bounded request loop");
        }

        pub(crate) fn sync_catalog(&mut self, catalog: &mut air::Air) {
            let mut requests: std::collections::VecDeque<_> = catalog.start().into_iter().collect();
            let mut resolver = air::DefaultResolver::default();
            for _ in 0..256 {
                let Some((kind, request)) = requests.pop_front() else {
                    return;
                };
                let response = self.request(request);
                let _ = ready(catalog.response(&mut resolver, kind, response));
                requests.extend(catalog.requests());
            }
            panic!("AIR catalog did not settle");
        }
    }

    pub(crate) fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => {
                panic!("AIR's local resolver unexpectedly required external work")
            }
        }
    }

    fn cache_roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let bytes = postcard::to_allocvec(value).unwrap();
        postcard::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn unconfirmed_upload_survives_cache_restart_and_cold_server_replay() {
        let secret = air::Secret::new();
        let mut catalog = air::Air::new(secret.clone()).unwrap();
        let bytes: Vec<u8> = (0..4096).map(|index| (index % 256) as u8).collect();
        let mut blob = catalog.create::<FileBlob>(bytes.clone()).unwrap();
        let mut library = catalog.create::<Vault>(()).unwrap();
        let folder_id = Id::random();
        let file_id = Id::random();
        library
            .send(VaultAction::Create {
                id: folder_id,
                parent: None,
                name: "Documents".into(),
                kind: EntryKind::Folder,
            })
            .unwrap();
        library
            .send(VaultAction::Create {
                id: file_id,
                parent: Some(folder_id),
                name: "upload.bin".into(),
                kind: EntryKind::File {
                    blob: blob.id(),
                    size: bytes.len() as u64,
                    mime: "application/octet-stream".into(),
                },
            })
            .unwrap();
        library
            .send(VaultAction::Rename {
                id: file_id,
                name: "kept.bin".into(),
            })
            .unwrap();
        library
            .send(VaultAction::Favorite {
                id: file_id,
                favorite: true,
            })
            .unwrap();

        // Simulate quitting with an in-flight initial upload. AIR caches both
        // the outgoing encrypted request and still-pending contract messages.
        let _ = blob.start();
        let _ = library.start();
        let mut blob: air::Instance<FileBlob> = cache_roundtrip(&blob);
        let mut library: air::Instance<Vault> = cache_roundtrip(&library);
        let catalog: air::Air = cache_roundtrip(&catalog);
        assert!(catalog.list(&FileBlob::id()).contains(blob.location()));
        assert!(catalog.list(&Vault::id()).contains(library.location()));
        assert_eq!(blob.pending().unwrap().bytes, bytes);
        assert_eq!(
            library.pending().unwrap().get(file_id).unwrap().name,
            "kept.bin"
        );

        let mut server = TestServer::new();
        server.sync(&mut blob);
        server.sync(&mut library);
        assert_eq!(blob.confirmed().unwrap().bytes, bytes);
        assert_eq!(
            library.confirmed().unwrap().get(file_id).unwrap().name,
            "kept.bin"
        );

        // A fresh receiver has no cached snapshots or original Init payload.
        // Its bytes and folder tree must come back from encrypted server data.
        let mut restored_blob =
            air::Instance::<FileBlob>::receive(secret.clone(), *blob.location()).unwrap();
        let mut restored_library =
            air::Instance::<Vault>::receive(secret.clone(), *library.location()).unwrap();
        assert!(restored_blob.pending().is_none());
        assert!(restored_library.pending().is_none());
        server.sync(&mut restored_blob);
        server.sync(&mut restored_library);
        assert_eq!(restored_blob.confirmed().unwrap().bytes, bytes);
        assert_eq!(restored_blob.confirmed().unwrap().owner, secret.name());
        assert_eq!(restored_library.confirmed(), library.confirmed());
        assert_eq!(
            restored_library.confirmed().unwrap().favorites()[0].id,
            file_id
        );
        assert_eq!(
            restored_library
                .confirmed()
                .unwrap()
                .get(file_id)
                .unwrap()
                .parent,
            Some(folder_id)
        );
    }

    #[test]
    fn stable_library_initialization_reopens_server_history_without_erasing_it() {
        let secret = air::Secret::new();
        let mut original = air::Instance::<Vault>::new(secret.clone(), ()).unwrap();
        let id = Id::random();
        original
            .send(VaultAction::Create {
                id,
                parent: None,
                name: "Saved folder".into(),
                kind: EntryKind::Folder,
            })
            .unwrap();
        let mut server = TestServer::new();
        server.sync(&mut original);

        // This is the cold-cache fallback behind Instances::create(()).
        let mut reopened = air::Instance::<Vault>::new(secret.clone(), ()).unwrap();
        assert_eq!(reopened.id(), original.id());
        assert_eq!(reopened.location(), original.location());
        server.sync(&mut reopened);
        assert_eq!(reopened.confirmed(), original.confirmed());
        assert_eq!(
            reopened.pending().unwrap().get(id).unwrap().name,
            "Saved folder"
        );
        let another_owner = air::Instance::<Vault>::new(air::Secret::new(), ()).unwrap();
        assert_ne!(another_owner.id(), original.id());
    }

    #[test]
    fn a_signed_foreign_update_cannot_modify_the_restored_library() {
        let owner = air::Secret::new();
        let mut original = air::Instance::<Vault>::new(owner.clone(), ()).unwrap();
        let id = Id::random();
        original
            .send(VaultAction::Create {
                id,
                parent: None,
                name: "Private".into(),
                kind: EntryKind::Folder,
            })
            .unwrap();
        let mut server = TestServer::new();
        server.sync(&mut original);

        // Even possession of the channel location does not grant write
        // authority in the application contract.
        let mut foreign =
            air::Instance::<Vault>::receive(air::Secret::new(), *original.location()).unwrap();
        server.sync(&mut foreign);
        assert_eq!(
            foreign.send(VaultAction::Rename {
                id,
                name: "Changed".into()
            }),
            Err(StorageError::Unauthorized)
        );
        server.sync(&mut foreign);
        let mut restored = air::Instance::<Vault>::receive(owner, *original.location()).unwrap();
        server.sync(&mut restored);
        assert_eq!(
            restored.confirmed().unwrap().get(id).unwrap().name,
            "Private"
        );
    }

    #[test]
    fn snapshot_import_is_atomic_collision_safe_and_idempotent() {
        let mut vault = vault();
        folder(&mut vault, 1, None, "Project");
        let transfer = Id::random();
        let items = vec![
            ImportItem {
                parent: None,
                name: "Project".into(),
                kind: EntryKind::Folder,
            },
            ImportItem {
                parent: Some(0),
                name: "Empty".into(),
                kind: EntryKind::Folder,
            },
            ImportItem {
                parent: Some(0),
                name: "notes.txt".into(),
                kind: EntryKind::File {
                    blob: Id::random(),
                    size: 3,
                    mime: "text/plain".into(),
                },
            },
        ];
        let root = vault
            .apply(
                VaultAction::ImportSnapshot {
                    transfer,
                    items: items.clone(),
                },
                metadata(),
            )
            .unwrap();
        assert_eq!(vault.get(root).unwrap().name, "Project (2)");
        assert_eq!(vault.children(Some(root)).len(), 2);
        assert_eq!(vault.total_size(), 3);
        let expected = vault.clone();
        assert_eq!(
            vault.apply(
                VaultAction::ImportSnapshot {
                    transfer,
                    items: items.clone()
                },
                metadata()
            ),
            Ok(root)
        );
        assert_eq!(vault, expected);
        let mut bad = items;
        bad[2].parent = Some(99);
        assert!(
            vault
                .apply(
                    VaultAction::ImportSnapshot {
                        transfer: Id::random(),
                        items: bad
                    },
                    metadata()
                )
                .is_err()
        );
        assert_eq!(vault, expected);
    }

    #[test]
    fn snapshot_import_avoids_file_overwrite_and_rejects_partial_id_collision() {
        let mut vault = vault();
        let file = ImportItem {
            parent: None,
            name: "report.txt".into(),
            kind: EntryKind::File {
                blob: Id::random(),
                size: 0,
                mime: "text/plain".into(),
            },
        };
        vault
            .apply(
                VaultAction::ImportSnapshot {
                    transfer: Id::random(),
                    items: vec![file.clone()],
                },
                metadata(),
            )
            .unwrap();
        let second = vault
            .apply(
                VaultAction::ImportSnapshot {
                    transfer: Id::random(),
                    items: vec![file],
                },
                metadata(),
            )
            .unwrap();
        assert_eq!(vault.get(second).unwrap().name, "report (2).txt");
        let transfer = Id::random();
        let collision = imported_id(transfer, 1);
        vault
            .apply(
                VaultAction::Create {
                    id: collision,
                    parent: None,
                    name: "Existing".into(),
                    kind: EntryKind::Folder,
                },
                metadata(),
            )
            .unwrap();
        let before = vault.clone();
        assert_eq!(
            vault.apply(
                VaultAction::ImportSnapshot {
                    transfer,
                    items: vec![
                        ImportItem {
                            parent: None,
                            name: "New".into(),
                            kind: EntryKind::Folder
                        },
                        ImportItem {
                            parent: Some(0),
                            name: "Child".into(),
                            kind: EntryKind::Folder
                        },
                    ]
                },
                metadata()
            ),
            Err(StorageError::IdExists)
        );
        assert_eq!(vault, before);
    }

    #[test]
    fn collision_renaming_handles_empty_stems_and_truncated_whitespace() {
        let names = [
            "... .txt".to_owned(),
            "... \u{2003}.txt".to_owned(),
            format!("{}x.txt", ".".repeat(250)),
            format!("f.{} z", "a".repeat(98)),
            format!("f.{}\u{2003}z", "a".repeat(96)),
        ];
        for name in names {
            validate_name(&name).unwrap();
            let mut vault = vault();
            let item = ImportItem {
                parent: None,
                name: name.clone(),
                kind: EntryKind::File {
                    blob: Id::random(),
                    size: 0,
                    mime: "application/octet-stream".into(),
                },
            };
            let mut imported_names = BTreeSet::new();
            for _ in 0..3 {
                let root = vault
                    .apply(
                        VaultAction::ImportSnapshot {
                            transfer: Id::random(),
                            items: vec![item.clone()],
                        },
                        metadata(),
                    )
                    .unwrap();
                let actual = &vault.get(root).unwrap().name;
                validate_name(actual).unwrap();
                assert!(actual.len() <= 255);
                assert!(imported_names.insert(actual.to_lowercase()));
            }
            if name == "... .txt" {
                assert!(imported_names.contains("received (2).txt"));
                assert!(imported_names.contains("received (3).txt"));
            }
            assert_eq!(vault.entries.len(), 3);
        }
    }
}

#[cfg(test)]
mod mount_wire_tests {
    use super::*;
    #[derive(Serialize)]
    enum OriginalKind {
        Folder,
        File { blob: Id, size: u64, mime: String },
    }
    #[derive(Serialize)]
    struct OriginalEntry {
        id: EntryId,
        parent: Option<EntryId>,
        name: String,
        kind: OriginalKind,
        favorite: bool,
        created_at: u64,
        modified_at: u64,
        trashed_at: Option<u64>,
    }
    #[derive(Serialize)]
    struct OriginalVault {
        owner: Name,
        entries: BTreeMap<EntryId, OriginalEntry>,
    }
    #[test]
    fn adding_mounts_preserves_original_vault_snapshots_and_action_indices() {
        let owner = air::Secret::new().name();
        let folder = Id::random();
        let file = Id::random();
        let blob = Id::random();
        let original = OriginalVault {
            owner,
            entries: BTreeMap::from([
                (
                    folder,
                    OriginalEntry {
                        id: folder,
                        parent: None,
                        name: "Original folder".into(),
                        kind: OriginalKind::Folder,
                        favorite: true,
                        created_at: 3,
                        modified_at: 4,
                        trashed_at: None,
                    },
                ),
                (
                    file,
                    OriginalEntry {
                        id: file,
                        parent: Some(folder),
                        name: "original.txt".into(),
                        kind: OriginalKind::File {
                            blob,
                            size: 5,
                            mime: "text/plain".into(),
                        },
                        favorite: false,
                        created_at: 5,
                        modified_at: 6,
                        trashed_at: None,
                    },
                ),
            ]),
        };
        let encoded = postcard::to_allocvec(&original).unwrap();
        let decoded: Vault = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded.owner, owner);
        assert_eq!(decoded.entries.len(), 2);
        assert!(decoded.entries[&folder].favorite);
        assert_eq!(
            decoded.entries[&file].kind,
            EntryKind::File {
                blob,
                size: 5,
                mime: "text/plain".into()
            }
        );
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), encoded);
        let actions = [
            VaultAction::Create {
                id: file,
                parent: None,
                name: "name".into(),
                kind: EntryKind::Folder,
            },
            VaultAction::Rename {
                id: file,
                name: "name".into(),
            },
            VaultAction::Move {
                id: file,
                parent: None,
            },
            VaultAction::Favorite {
                id: file,
                favorite: true,
            },
            VaultAction::Trash(file),
            VaultAction::Restore(file),
            VaultAction::DeletePermanently(file),
            VaultAction::ImportSnapshot {
                transfer: file,
                items: Vec::new(),
            },
        ];
        for (index, action) in actions.into_iter().enumerate() {
            assert_eq!(postcard::to_allocvec(&action).unwrap()[0], index as u8);
        }
        assert_eq!(
            postcard::to_allocvec(&EntryKind::Shared {
                space: file,
                folder: false
            })
            .unwrap()[0],
            2
        );
    }
}
