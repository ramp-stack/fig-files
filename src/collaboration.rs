//! Mutable, explicitly joined AIR spaces. Private Vault/FileBlob channels are
//! never shared. AIR retains encrypted history; member ACLs govern writes.
use crate::storage::{self, Entry, EntryId, EntryKind, StorageError, Vault, VaultAction};
use prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberStatus {
    Invited,
    Active,
    Declined,
}
#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub struct SharedInit {
    pub nonce: Id,
    pub root: EntryId,
    pub items: Vec<Entry>,
}
#[derive(Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictVersion {
    pub id: Id,
    pub bytes: Vec<u8>,
    pub author: Name,
    pub timestamp: u64,
}
impl fmt::Debug for ConflictVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConflictVersion")
            .field("id", &self.id)
            .field("size", &self.bytes.len())
            .field("author", &self.author)
            .finish()
    }
}
#[derive(Clone, Hash, Serialize, Deserialize)]
pub struct SharedSpace {
    pub owner: Name,
    pub created_at: u64,
    pub root: EntryId,
    pub tree: Vault,
    pub members: BTreeMap<Name, MemberStatus>,
    ready: bool,
    contents: BTreeMap<EntryId, Vec<u8>>,
    authors: BTreeMap<EntryId, Name>,
    last_edit: BTreeMap<EntryId, Id>,
    versions: BTreeMap<EntryId, Vec<ConflictVersion>>,
}
impl fmt::Debug for SharedSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedSpace")
            .field("owner", &self.owner)
            .field("root", &self.root)
            .field("tree", &self.tree)
            .field("members", &self.members)
            .field("ready", &self.ready)
            .finish()
    }
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplaceOutcome {
    Updated,
    SavedConflict(Id),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionResult {
    Unit,
    Entry(EntryId),
    Replace(ReplaceOutcome),
}
#[derive(Clone, Hash, Serialize, Deserialize)]
pub enum SharedAction {
    Upload {
        id: EntryId,
        bytes: Vec<u8>,
    },
    Activate,
    Grant(Name),
    Join,
    Decline,
    Edit(VaultAction),
    Import {
        id: EntryId,
        parent: EntryId,
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    Replace {
        id: EntryId,
        expected: Id,
        version: Id,
        bytes: Vec<u8>,
    },
    Resolve {
        id: EntryId,
        conflict: Id,
        expected: Id,
    },
    DiscardConflict {
        id: EntryId,
        conflict: Id,
    },
}
impl fmt::Debug for SharedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upload { id, bytes } => f
                .debug_struct("Upload")
                .field("id", id)
                .field("size", &bytes.len())
                .finish(),
            Self::Import {
                id, name, bytes, ..
            } => f
                .debug_struct("Import")
                .field("id", id)
                .field("name", name)
                .field("size", &bytes.len())
                .finish(),
            Self::Replace {
                id,
                expected,
                version,
                bytes,
            } => f
                .debug_struct("Replace")
                .field("id", id)
                .field("expected", expected)
                .field("version", version)
                .field("size", &bytes.len())
                .finish(),
            Self::Activate => f.write_str("Activate"),
            Self::Grant(name) => f.debug_tuple("Grant").field(name).finish(),
            Self::Join => f.write_str("Join"),
            Self::Decline => f.write_str("Decline"),
            Self::Edit(action) => action.fmt(f),
            Self::Resolve {
                id,
                conflict,
                expected,
            } => f
                .debug_struct("Resolve")
                .field("id", id)
                .field("conflict", conflict)
                .field("expected", expected)
                .finish(),
            Self::DiscardConflict { id, conflict } => f
                .debug_struct("DiscardConflict")
                .field("id", id)
                .field("conflict", conflict)
                .finish(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollaborationError {
    Storage(StorageError),
    Unauthorized,
    OwnName,
    NotReady,
    InvalidSpace,
    TooLarge,
    RootOperation,
    MissingConflict,
    ContentMismatch,
    Draft(crate::drafts::DraftError),
}
impl From<StorageError> for CollaborationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl fmt::Display for CollaborationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::Draft(error) => error.fmt(f),
            Self::Unauthorized => f.write_str("Join this shared item before making changes. Only invited members can join."),
            Self::OwnName => f.write_str("Enter someone else's Fig Files receiving name."),
            Self::NotReady => f.write_str("This shared item is still syncing. Try again shortly."),
            Self::InvalidSpace => f.write_str("This shared item has an invalid file tree."),
            Self::TooLarge => f.write_str("Shared items support 128 entries and saved conflicts, 32 MiB total, files up to 8 MiB, and 32 folder levels. Discard saved edits or remove unused files first."),
            Self::RootOperation => f.write_str("The shared root cannot be moved or deleted here. Remove its shortcut from your own Files instead."),
            Self::MissingConflict => f.write_str("This saved conflict has already been resolved or removed."),
            Self::ContentMismatch => f.write_str("The file contents do not match this shared item."),
        }
    }
}
impl std::error::Error for CollaborationError {}

impl Contract for SharedSpace {
    type Init = SharedInit;
    type Message = SharedAction;
    type Result = Result<ActionResult, CollaborationError>;
    fn id() -> Id {
        Id::hash("org.ramp.fig-files.SharedSpace.v1")
    }
    fn init(init: SharedInit, metadata: Metadata) -> Self {
        let mut entries = BTreeMap::new();
        // Duplicate manifest IDs make the root invalid instead of silently
        // accepting a different tree than the sender declared.
        let duplicate = init
            .items
            .iter()
            .any(|entry| entries.insert(entry.id, entry.clone()).is_some());
        if duplicate {
            entries.clear();
        }
        Self {
            owner: metadata.signer,
            created_at: metadata.timestamp / 1_000_000_000,
            root: init.root,
            tree: Vault {
                owner: metadata.signer,
                entries,
            },
            members: BTreeMap::from([(metadata.signer, MemberStatus::Active)]),
            ready: false,
            contents: BTreeMap::new(),
            authors: BTreeMap::new(),
            last_edit: BTreeMap::new(),
            versions: BTreeMap::new(),
        }
    }
    fn apply(&mut self, action: SharedAction, metadata: Metadata) -> Self::Result {
        self.validate()?;
        // All tree/content/limit validation is transactional, including deletes
        // and resolving saved concurrent writes.
        let mut next = self.clone();
        let result = next.apply_inner(action, metadata)?;
        next.validate()?;
        *self = next;
        Ok(result)
    }
}
impl SharedSpace {
    pub fn title(&self) -> &str {
        self.tree
            .get(self.root)
            .map(|entry| entry.name.as_str())
            .unwrap_or("Invalid shared item")
    }
    pub fn is_folder(&self) -> bool {
        self.tree.get(self.root).is_some_and(Entry::is_folder)
    }
    pub fn item_count(&self) -> usize {
        self.tree.item_count()
    }
    pub fn total_size(&self) -> u64 {
        self.tree.total_size()
    }
    pub fn is_ready(&self) -> bool {
        self.ready && self.validate().is_ok()
    }
    pub fn can_write(&self, name: Name) -> bool {
        self.ready && self.members.get(&name) == Some(&MemberStatus::Active)
    }
    pub fn conflicts(&self, id: EntryId) -> Vec<ConflictVersion> {
        self.versions.get(&id).cloned().unwrap_or_default()
    }
    /// Bounded confirmation evidence. Superseded edits may conservatively
    /// remain private recovery copies until the user discards them.
    pub fn has_applied_edit(&self, token: Id) -> bool {
        self.last_edit.values().any(|id| *id == token)
            || self
                .versions
                .values()
                .flatten()
                .any(|version| version.id == token)
    }
    pub fn content_hash(&self, id: EntryId) -> Result<Id, CollaborationError> {
        let entry = self.tree.get(id).ok_or(StorageError::MissingEntry)?;
        if self.tree.is_trashed(id) {
            return Err(StorageError::InTrash.into());
        }
        match entry.kind {
            EntryKind::File { blob, .. } => Ok(blob),
            _ => Err(StorageError::NotAFile.into()),
        }
    }
    pub fn read_file(&self, id: EntryId) -> Result<Vec<u8>, CollaborationError> {
        if !self.is_ready() {
            return Err(CollaborationError::NotReady);
        }
        self.content_hash(id)?;
        self.contents
            .get(&id)
            .cloned()
            .ok_or(CollaborationError::NotReady)
    }
    pub fn validate(&self) -> Result<(), CollaborationError> {
        if self.tree.owner != self.owner
            || self.members.get(&self.owner) != Some(&MemberStatus::Active)
            || self.members.len() > 128
        {
            return Err(CollaborationError::InvalidSpace);
        }
        let root = self
            .tree
            .get(self.root)
            .ok_or(CollaborationError::InvalidSpace)?;
        if root.parent.is_some() || root.trashed_at.is_some() {
            return Err(CollaborationError::InvalidSpace);
        }
        let conflicts: usize = self.versions.values().map(Vec::len).sum();
        if self.tree.entries.len() + conflicts > storage::MAX_TREE_ITEMS {
            return Err(CollaborationError::TooLarge);
        }
        let mut total = 0u64;
        let mut names = BTreeSet::new();
        for (key, entry) in &self.tree.entries {
            storage::validate_name(&entry.name)?;
            if *key != entry.id {
                return Err(CollaborationError::InvalidSpace);
            }
            let mut cursor = entry.id;
            let mut depth = 1;
            let mut seen = BTreeSet::new();
            while cursor != self.root {
                if !seen.insert(cursor) {
                    return Err(CollaborationError::InvalidSpace);
                }
                let current = self
                    .tree
                    .get(cursor)
                    .ok_or(CollaborationError::InvalidSpace)?;
                cursor = current.parent.ok_or(CollaborationError::InvalidSpace)?;
                if !matches!(
                    self.tree.get(cursor).map(|parent| &parent.kind),
                    Some(EntryKind::Folder)
                ) {
                    return Err(CollaborationError::InvalidSpace);
                }
                depth += 1;
                if depth > storage::MAX_TREE_DEPTH {
                    return Err(CollaborationError::TooLarge);
                }
            }
            if !self.tree.is_trashed(entry.id)
                && !names.insert((entry.parent, entry.name.to_lowercase()))
            {
                return Err(StorageError::NameExists.into());
            }
            match &entry.kind {
                EntryKind::Folder => {
                    if self.contents.contains_key(&entry.id) {
                        return Err(CollaborationError::InvalidSpace);
                    }
                }
                EntryKind::Shared { .. } => return Err(StorageError::SharedMount.into()),
                EntryKind::File { blob, size, mime } => {
                    if *size > storage::MAX_FILE_SIZE as u64
                        || mime.len() > 255
                        || mime.chars().any(char::is_control)
                    {
                        return Err(CollaborationError::TooLarge);
                    }
                    total = total.saturating_add(*size);
                    match self.contents.get(&entry.id) {
                        Some(bytes) if bytes.len() as u64 == *size && Id::hash(bytes) == *blob => {}
                        None if !self.ready => {}
                        _ => return Err(CollaborationError::ContentMismatch),
                    }
                }
            }
        }
        if self
            .contents
            .keys()
            .any(|id| !self.tree.entries.contains_key(id))
        {
            return Err(CollaborationError::InvalidSpace);
        }
        let mut version_ids = BTreeSet::new();
        for (id, versions) in &self.versions {
            if !matches!(
                self.tree.get(*id).map(|entry| &entry.kind),
                Some(EntryKind::File { .. })
            ) {
                return Err(CollaborationError::InvalidSpace);
            }
            for version in versions {
                if !version_ids.insert(version.id) || version.bytes.len() > storage::MAX_FILE_SIZE {
                    return Err(CollaborationError::InvalidSpace);
                }
                total = total.saturating_add(version.bytes.len() as u64);
            }
        }
        if total > storage::MAX_TREE_BYTES {
            return Err(CollaborationError::TooLarge);
        }
        Ok(())
    }
    fn apply_inner(
        &mut self,
        action: SharedAction,
        metadata: Metadata,
    ) -> Result<ActionResult, CollaborationError> {
        let signer = metadata.signer;
        let timestamp = metadata.timestamp / 1_000_000_000;
        match action {
            SharedAction::Upload { id, bytes } => {
                if signer != self.owner || self.ready {
                    return Err(CollaborationError::Unauthorized);
                }
                let Some(Entry {
                    kind: EntryKind::File { blob, size, .. },
                    ..
                }) = self.tree.get(id)
                else {
                    return Err(StorageError::NotAFile.into());
                };
                if bytes.len() > storage::MAX_FILE_SIZE
                    || bytes.len() as u64 != *size
                    || Id::hash(&bytes) != *blob
                {
                    return Err(CollaborationError::ContentMismatch);
                }
                self.contents.insert(id, bytes);
                self.authors.insert(id, signer);
                Ok(ActionResult::Unit)
            }
            SharedAction::Activate => {
                if signer != self.owner {
                    return Err(CollaborationError::Unauthorized);
                }
                self.ready = true; // Transactional validation requires every file's bytes.
                Ok(ActionResult::Unit)
            }
            SharedAction::Grant(recipient) => {
                self.authorize(signer)?;
                if recipient == signer {
                    return Err(CollaborationError::OwnName);
                }
                if self.members.get(&recipient) != Some(&MemberStatus::Active) {
                    self.members.insert(recipient, MemberStatus::Invited);
                }
                Ok(ActionResult::Unit)
            }
            SharedAction::Join => {
                if !self.ready {
                    return Err(CollaborationError::NotReady);
                }
                match self.members.get(&signer) {
                    Some(MemberStatus::Invited | MemberStatus::Active) => {}
                    _ => return Err(CollaborationError::Unauthorized),
                }
                self.members.insert(signer, MemberStatus::Active);
                Ok(ActionResult::Unit)
            }
            SharedAction::Decline => {
                if self.members.get(&signer) == Some(&MemberStatus::Declined) {
                    return Ok(ActionResult::Unit);
                }
                if self.members.get(&signer) != Some(&MemberStatus::Invited) {
                    return Err(CollaborationError::Unauthorized);
                }
                self.members.insert(signer, MemberStatus::Declined);
                Ok(ActionResult::Unit)
            }
            SharedAction::Edit(action) => {
                self.authorize(signer)?;
                match &action {
                    VaultAction::Create {
                        kind: EntryKind::Folder,
                        parent: Some(_),
                        ..
                    } => {}
                    VaultAction::Rename { .. } | VaultAction::Favorite { .. } => {}
                    VaultAction::Move {
                        id,
                        parent: Some(_),
                    }
                    | VaultAction::Trash(id)
                    | VaultAction::Restore(id)
                    | VaultAction::DeletePermanently(id)
                        if *id != self.root => {}
                    VaultAction::Move { .. }
                    | VaultAction::Trash(_)
                    | VaultAction::Restore(_)
                    | VaultAction::DeletePermanently(_) => {
                        return Err(CollaborationError::RootOperation);
                    }
                    _ => return Err(CollaborationError::InvalidSpace),
                }
                let result = if let VaultAction::Restore(id) = action {
                    let entry = self.tree.get(id).ok_or(StorageError::MissingEntry)?;
                    if !self.tree.is_trashed(id) {
                        return Err(StorageError::NotInTrash.into());
                    }
                    // The shared folder, rather than the private library, is
                    // the fallback parent when restoring a detached child.
                    let parent = entry
                        .parent
                        .filter(|parent| !self.tree.is_trashed(*parent))
                        .unwrap_or(self.root);
                    self.tree
                        .validate_destination(Some(parent), &entry.name, Some(id))?;
                    let entry = self.tree.entries.get_mut(&id).unwrap();
                    entry.parent = Some(parent);
                    entry.trashed_at = None;
                    entry.modified_at = timestamp;
                    id
                } else {
                    self.tree.apply(
                        action,
                        Metadata {
                            signer: self.owner,
                            timestamp: metadata.timestamp,
                            confirmed: metadata.confirmed,
                        },
                    )?
                };
                self.contents
                    .retain(|id, _| self.tree.entries.contains_key(id));
                self.authors
                    .retain(|id, _| self.tree.entries.contains_key(id));
                self.last_edit
                    .retain(|id, _| self.tree.entries.contains_key(id));
                self.versions
                    .retain(|id, _| self.tree.entries.contains_key(id));
                Ok(ActionResult::Entry(result))
            }
            SharedAction::Import {
                id,
                parent,
                name,
                mime,
                bytes,
            } => {
                self.authorize(signer)?;
                if bytes.len() > storage::MAX_FILE_SIZE {
                    return Err(StorageError::FileTooLarge.into());
                }
                self.tree.apply(
                    VaultAction::Create {
                        id,
                        parent: Some(parent),
                        name,
                        kind: EntryKind::File {
                            blob: Id::hash(&bytes),
                            size: bytes.len() as u64,
                            mime,
                        },
                    },
                    Metadata {
                        signer: self.owner,
                        timestamp: metadata.timestamp,
                        confirmed: metadata.confirmed,
                    },
                )?;
                self.contents.insert(id, bytes);
                self.authors.insert(id, signer);
                Ok(ActionResult::Entry(id))
            }
            SharedAction::Replace {
                id,
                expected,
                version,
                bytes,
            } => {
                self.authorize(signer)?;
                if bytes.len() > storage::MAX_FILE_SIZE {
                    return Err(StorageError::FileTooLarge.into());
                }
                let current = self.content_hash(id)?;
                if current == expected || current == Id::hash(&bytes) {
                    self.update_file(id, bytes, timestamp, signer)?;
                    self.last_edit.insert(id, version);
                    Ok(ActionResult::Replace(ReplaceOutcome::Updated))
                } else {
                    let versions = self.versions.entry(id).or_default();
                    if let Some(existing) = versions.iter().find(|value| value.id == version) {
                        if existing.bytes != bytes || existing.author != signer {
                            return Err(CollaborationError::ContentMismatch);
                        }
                    } else {
                        versions.push(ConflictVersion {
                            id: version,
                            bytes,
                            author: signer,
                            timestamp,
                        });
                    }
                    Ok(ActionResult::Replace(ReplaceOutcome::SavedConflict(
                        version,
                    )))
                }
            }
            SharedAction::Resolve {
                id,
                conflict,
                expected,
            } => {
                self.authorize(signer)?;
                let version = self
                    .versions
                    .get(&id)
                    .and_then(|versions| versions.iter().find(|version| version.id == conflict))
                    .cloned()
                    .ok_or(CollaborationError::MissingConflict)?;
                if self.content_hash(id)? != expected {
                    return Ok(ActionResult::Replace(ReplaceOutcome::SavedConflict(
                        conflict,
                    )));
                }
                let previous = ConflictVersion {
                    id: Id::hash(&(
                        "displaced shared edit",
                        id,
                        conflict,
                        expected,
                        signer,
                        metadata.timestamp,
                    )),
                    bytes: self
                        .contents
                        .get(&id)
                        .cloned()
                        .ok_or(CollaborationError::NotReady)?,
                    author: self.authors.get(&id).copied().unwrap_or(self.owner),
                    timestamp: self.tree.entries[&id].modified_at,
                };
                let redundant = previous.bytes == version.bytes;
                self.update_file(id, version.bytes, timestamp, signer)?;
                self.last_edit.insert(id, conflict);
                self.versions
                    .get_mut(&id)
                    .unwrap()
                    .retain(|version| version.id != conflict);
                if !redundant {
                    self.versions.get_mut(&id).unwrap().push(previous);
                }
                Ok(ActionResult::Replace(ReplaceOutcome::Updated))
            }
            SharedAction::DiscardConflict { id, conflict } => {
                self.authorize(signer)?;
                let versions = self
                    .versions
                    .get_mut(&id)
                    .ok_or(CollaborationError::MissingConflict)?;
                if !versions.iter().any(|version| version.id == conflict) {
                    return Err(CollaborationError::MissingConflict);
                }
                versions.retain(|version| version.id != conflict);
                Ok(ActionResult::Unit)
            }
        }
    }
    fn authorize(&self, signer: Name) -> Result<(), CollaborationError> {
        if !self.ready {
            return Err(CollaborationError::NotReady);
        }
        if !self.can_write(signer) {
            return Err(CollaborationError::Unauthorized);
        }
        Ok(())
    }
    fn update_file(
        &mut self,
        id: EntryId,
        bytes: Vec<u8>,
        timestamp: u64,
        author: Name,
    ) -> Result<(), CollaborationError> {
        self.content_hash(id)?;
        let entry = self.tree.entries.get_mut(&id).unwrap();
        let EntryKind::File { blob, size, .. } = &mut entry.kind else {
            unreachable!()
        };
        *blob = Id::hash(&bytes);
        *size = bytes.len() as u64;
        entry.modified_at = timestamp;
        self.contents.insert(id, bytes);
        self.authors.insert(id, author);
        Ok(())
    }
}

pub fn open(air: &Air) {
    let _ = air.instances::<SharedSpace>();
}
pub fn get(air: &Air, id: Id) -> Result<Instance<SharedSpace>, CollaborationError> {
    air.instances::<SharedSpace>()
        .list()
        .get(&id)
        .cloned()
        .ok_or(CollaborationError::NotReady)
}
fn dispatch(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    action: SharedAction,
) -> Result<ActionResult, CollaborationError> {
    space.pending().clone().apply(
        action.clone(),
        Metadata {
            signer: air.me(),
            timestamp: 0,
            confirmed: false,
        },
    )?;
    space.send(action).pending().clone()
}
pub fn invite(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    recipient: Name,
) -> Result<(), CollaborationError> {
    dispatch(air, space, SharedAction::Grant(recipient))?;
    space.share(recipient);
    Ok(())
}
pub fn join(
    air: &Air,
    vault: &mut Instance<Vault>,
    space: &mut Instance<SharedSpace>,
) -> Result<EntryId, CollaborationError> {
    if vault.pending().owner != air.me() {
        return Err(CollaborationError::Unauthorized);
    }
    dispatch(air, space, SharedAction::Join)?;
    let snapshot = space.pending();
    Ok(storage::mount_shared(
        vault,
        space.id(),
        snapshot.title(),
        snapshot.is_folder(),
    )?)
}
pub fn decline(air: &Air, space: &mut Instance<SharedSpace>) -> Result<(), CollaborationError> {
    dispatch(air, space, SharedAction::Decline)?;
    Ok(())
}
pub fn share(
    air: &Air,
    vault: &mut Instance<Vault>,
    id: EntryId,
    recipient: Name,
) -> Result<Instance<SharedSpace>, CollaborationError> {
    if recipient == air.me() {
        return Err(CollaborationError::OwnName);
    }
    let snapshot = vault.pending();
    if snapshot.owner != air.me() {
        return Err(CollaborationError::Unauthorized);
    }
    let expected = snapshot.subtree_fingerprint(id)?;
    let entry = snapshot.get(id).ok_or(StorageError::MissingEntry)?;
    if let EntryKind::Shared { space, .. } = entry.kind {
        let mut space = get(air, space)?;
        drop(snapshot);
        invite(air, &mut space, recipient)?;
        return Ok(space);
    }
    let mut items = Vec::new();
    let mut payloads = Vec::new();
    let mut total_bytes = 0u64;
    let mut queue = vec![id];
    while let Some(child) = queue.pop() {
        let mut entry = snapshot
            .get(child)
            .ok_or(StorageError::MissingEntry)?
            .clone();
        if child == id {
            entry.parent = None;
        }
        entry.favorite = false;
        match &mut entry.kind {
            EntryKind::Folder => queue.extend(
                snapshot
                    .children(Some(child))
                    .into_iter()
                    .rev()
                    .map(|entry| entry.id),
            ),
            EntryKind::File { blob, size, .. } => {
                total_bytes = total_bytes.saturating_add(*size);
                if total_bytes > storage::MAX_TREE_BYTES {
                    return Err(CollaborationError::TooLarge);
                }
                let bytes = storage::read_file(air, snapshot.get(child).unwrap())?;
                *blob = Id::hash(&bytes);
                payloads.push((child, bytes));
            }
            EntryKind::Shared { .. } => return Err(StorageError::SharedMount.into()),
        }
        items.push(entry);
        if items.len() > storage::MAX_TREE_ITEMS {
            return Err(CollaborationError::TooLarge);
        }
    }
    drop(snapshot);
    let init = SharedInit {
        nonce: Id::random(),
        root: id,
        items,
    };
    SharedSpace::init(
        init.clone(),
        Metadata {
            signer: air.me(),
            timestamp: 0,
            confirmed: false,
        },
    )
    .validate()?;
    let mut space = air.instances::<SharedSpace>().create(init);
    for (id, bytes) in payloads {
        dispatch(air, &mut space, SharedAction::Upload { id, bytes })?;
    }
    dispatch(air, &mut space, SharedAction::Activate)?;
    storage::promote_shared(vault, id, space.id(), expected)?;
    invite(air, &mut space, recipient)?;
    Ok(space)
}
fn edit(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    action: VaultAction,
) -> Result<EntryId, CollaborationError> {
    match dispatch(air, space, SharedAction::Edit(action))? {
        ActionResult::Entry(id) => Ok(id),
        _ => Err(CollaborationError::InvalidSpace),
    }
}
pub fn create_folder(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    parent: EntryId,
    name: &str,
) -> Result<EntryId, CollaborationError> {
    edit(
        air,
        space,
        VaultAction::Create {
            id: Id::random(),
            parent: Some(parent),
            name: name.trim().into(),
            kind: EntryKind::Folder,
        },
    )
}
pub fn import_file(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    parent: EntryId,
    name: &str,
    bytes: Vec<u8>,
) -> Result<EntryId, CollaborationError> {
    match dispatch(
        air,
        space,
        SharedAction::Import {
            id: Id::random(),
            parent,
            name: name.trim().into(),
            mime: storage::mime_for_name(name).into(),
            bytes,
        },
    )? {
        ActionResult::Entry(id) => Ok(id),
        _ => Err(CollaborationError::InvalidSpace),
    }
}
pub fn rename(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    name: &str,
) -> Result<EntryId, CollaborationError> {
    edit(
        air,
        space,
        VaultAction::Rename {
            id,
            name: name.trim().into(),
        },
    )
}
pub fn move_entry(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    parent: EntryId,
) -> Result<EntryId, CollaborationError> {
    edit(
        air,
        space,
        VaultAction::Move {
            id,
            parent: Some(parent),
        },
    )
}
pub fn set_favorite(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    favorite: bool,
) -> Result<EntryId, CollaborationError> {
    edit(air, space, VaultAction::Favorite { id, favorite })
}
pub fn trash(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
) -> Result<EntryId, CollaborationError> {
    edit(air, space, VaultAction::Trash(id))
}
pub fn restore(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
) -> Result<EntryId, CollaborationError> {
    edit(air, space, VaultAction::Restore(id))
}
pub fn delete_permanently(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
) -> Result<EntryId, CollaborationError> {
    edit(air, space, VaultAction::DeletePermanently(id))
}
pub fn replace_file(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    expected: Id,
    bytes: Vec<u8>,
) -> Result<ReplaceOutcome, CollaborationError> {
    // Validate before persisting a draft, then preserve it privately before
    // queueing. Optimistic success can become a quota error after remote replay.
    space.pending().clone().apply(
        SharedAction::Replace {
            id,
            expected,
            version: Id::random(),
            bytes: bytes.clone(),
        },
        Metadata {
            signer: air.me(),
            timestamp: 0,
            confirmed: false,
        },
    )?;
    let name = space
        .pending()
        .tree
        .get(id)
        .ok_or(StorageError::MissingEntry)?
        .name
        .clone();
    let draft = crate::drafts::save(air, space.id(), id, &name, expected, bytes.clone())
        .map_err(CollaborationError::Draft)?;
    match dispatch(
        air,
        space,
        SharedAction::Replace {
            id,
            expected,
            version: draft.id(),
            bytes,
        },
    )? {
        ActionResult::Replace(result) => Ok(result),
        _ => Err(CollaborationError::InvalidSpace),
    }
}
pub fn resolve_conflict(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    conflict: Id,
    expected: Id,
) -> Result<ReplaceOutcome, CollaborationError> {
    match dispatch(
        air,
        space,
        SharedAction::Resolve {
            id,
            conflict,
            expected,
        },
    )? {
        ActionResult::Replace(result) => Ok(result),
        _ => Err(CollaborationError::InvalidSpace),
    }
}
pub fn discard_conflict(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    id: EntryId,
    conflict: Id,
) -> Result<(), CollaborationError> {
    dispatch(air, space, SharedAction::DiscardConflict { id, conflict })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::{TestServer, ready};

    fn metadata(signer: Name) -> Metadata {
        Metadata {
            signer,
            timestamp: 10_000_000_000,
            confirmed: true,
        }
    }
    fn fixture() -> (air::Secret, air::Secret, SharedSpace, EntryId, EntryId) {
        let alice = air::Secret::new();
        let bob = air::Secret::new();
        let root = Id::random();
        let file = Id::random();
        let mut space = SharedSpace::init(
            SharedInit {
                nonce: Id::random(),
                root,
                items: vec![
                    Entry {
                        id: root,
                        parent: None,
                        name: "Project".into(),
                        kind: EntryKind::Folder,
                        favorite: false,
                        created_at: 1,
                        modified_at: 1,
                        trashed_at: None,
                    },
                    Entry {
                        id: file,
                        parent: Some(root),
                        name: "notes.txt".into(),
                        kind: EntryKind::File {
                            blob: Id::hash(&b"original".to_vec()),
                            size: 8,
                            mime: "text/plain".into(),
                        },
                        favorite: false,
                        created_at: 1,
                        modified_at: 1,
                        trashed_at: None,
                    },
                ],
            },
            metadata(alice.name()),
        );
        space
            .apply(
                SharedAction::Upload {
                    id: file,
                    bytes: b"original".to_vec(),
                },
                metadata(alice.name()),
            )
            .unwrap();
        space
            .apply(SharedAction::Activate, metadata(alice.name()))
            .unwrap();
        (alice, bob, space, root, file)
    }
    fn apply(
        space: &mut SharedSpace,
        signer: Name,
        action: SharedAction,
    ) -> Result<ActionResult, CollaborationError> {
        space.apply(action, metadata(signer))
    }
    fn join_member(space: &mut SharedSpace, owner: Name, member: Name) {
        apply(space, owner, SharedAction::Grant(member)).unwrap();
        apply(space, member, SharedAction::Join).unwrap();
    }
    #[test]
    fn signed_membership_controls_all_writes_and_members_can_invite() {
        let (alice, bob, mut space, root, file) = fixture();
        let charlie = air::Secret::new();
        let rename = SharedAction::Edit(VaultAction::Rename {
            id: file,
            name: "updated.txt".into(),
        });
        assert_eq!(
            apply(&mut space, bob.name(), rename.clone()),
            Err(CollaborationError::Unauthorized)
        );
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::Grant(charlie.name())),
            Err(CollaborationError::Unauthorized)
        );
        apply(&mut space, alice.name(), SharedAction::Grant(bob.name())).unwrap();
        assert_eq!(
            apply(&mut space, bob.name(), rename.clone()),
            Err(CollaborationError::Unauthorized)
        );
        apply(&mut space, bob.name(), SharedAction::Decline).unwrap();
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::Join),
            Err(CollaborationError::Unauthorized)
        );
        join_member(&mut space, alice.name(), bob.name());
        join_member(&mut space, bob.name(), charlie.name());
        apply(&mut space, charlie.name(), rename).unwrap();
        assert_eq!(space.tree.entries[&file].name, "updated.txt");
        assert_eq!(
            apply(
                &mut space,
                charlie.name(),
                SharedAction::Edit(VaultAction::Trash(root))
            ),
            Err(CollaborationError::RootOperation)
        );
        assert_eq!(
            apply(
                &mut space,
                charlie.name(),
                SharedAction::Edit(VaultAction::MountShared {
                    space: Id::random(),
                    name: "bad".into(),
                    folder: true
                })
            ),
            Err(CollaborationError::InvalidSpace)
        );
        assert_eq!(
            apply(
                &mut space,
                charlie.name(),
                SharedAction::Upload {
                    id: file,
                    bytes: b"overwrite".to_vec()
                }
            ),
            Err(CollaborationError::Unauthorized)
        );
        assert_eq!(
            apply(
                &mut space,
                charlie.name(),
                SharedAction::Grant(charlie.name())
            ),
            Err(CollaborationError::OwnName)
        );
    }
    #[test]
    fn replacements_preserve_concurrent_bytes_and_resolution_is_an_atomic_swap() {
        let (alice, bob, mut space, _, file) = fixture();
        join_member(&mut space, alice.name(), bob.name());
        let expected = space.content_hash(file).unwrap();
        let conflict = Id::random();
        apply(
            &mut space,
            alice.name(),
            SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"Alice edit".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut space,
                bob.name(),
                SharedAction::Replace {
                    id: file,
                    expected,
                    version: conflict,
                    bytes: b"Bob edit".to_vec()
                }
            )
            .unwrap(),
            ActionResult::Replace(ReplaceOutcome::SavedConflict(conflict))
        );
        assert_eq!(space.read_file(file).unwrap(), b"Alice edit");
        assert_eq!(space.conflicts(file)[0].bytes, b"Bob edit");
        assert_eq!(space.conflicts(file)[0].author, bob.name());
        assert_eq!(
            apply(
                &mut space,
                bob.name(),
                SharedAction::Resolve {
                    id: file,
                    conflict,
                    expected
                }
            )
            .unwrap(),
            ActionResult::Replace(ReplaceOutcome::SavedConflict(conflict))
        );
        let expected = space.content_hash(file).unwrap();
        apply(
            &mut space,
            bob.name(),
            SharedAction::Resolve {
                id: file,
                conflict,
                expected,
            },
        )
        .unwrap();
        assert_eq!(space.read_file(file).unwrap(), b"Bob edit");
        let previous = space.conflicts(file).pop().unwrap();
        assert_ne!(previous.id, conflict);
        assert_eq!(previous.bytes, b"Alice edit");
        assert_eq!(previous.author, alice.name());
        assert_eq!(
            apply(
                &mut space,
                bob.name(),
                SharedAction::Resolve {
                    id: file,
                    conflict,
                    expected
                }
            ),
            Err(CollaborationError::MissingConflict)
        );
        apply(
            &mut space,
            alice.name(),
            SharedAction::DiscardConflict {
                id: file,
                conflict: previous.id,
            },
        )
        .unwrap();
        assert!(space.conflicts(file).is_empty());
    }
    #[test]
    fn mutations_are_atomic_bounded_and_keep_one_valid_shared_root() {
        let (alice, _, mut space, root, file) = fixture();
        let name = alice.name();
        let folder = Id::random();
        apply(
            &mut space,
            name,
            SharedAction::Edit(VaultAction::Create {
                id: folder,
                parent: Some(root),
                name: "Folder".into(),
                kind: EntryKind::Folder,
            }),
        )
        .unwrap();
        apply(
            &mut space,
            name,
            SharedAction::Edit(VaultAction::Move {
                id: file,
                parent: Some(folder),
            }),
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut space,
                name,
                SharedAction::Edit(VaultAction::Move {
                    id: folder,
                    parent: Some(file)
                })
            ),
            Err(StorageError::FolderCycle.into())
        );
        apply(
            &mut space,
            name,
            SharedAction::Edit(VaultAction::Trash(folder)),
        )
        .unwrap();
        assert!(space.read_file(file).is_err());
        apply(
            &mut space,
            name,
            SharedAction::Edit(VaultAction::Restore(file)),
        )
        .unwrap();
        assert_eq!(space.tree.entries[&file].parent, Some(root));
        apply(
            &mut space,
            name,
            SharedAction::Edit(VaultAction::DeletePermanently(folder)),
        )
        .unwrap();
        assert!(space.tree.get(folder).is_none());
        while space.tree.entries.len() < storage::MAX_TREE_ITEMS {
            let index = space.tree.entries.len();
            apply(
                &mut space,
                name,
                SharedAction::Edit(VaultAction::Create {
                    id: Id::random(),
                    parent: Some(root),
                    name: format!("Folder {index}"),
                    kind: EntryKind::Folder,
                }),
            )
            .unwrap();
        }
        let before = Id::hash(&space);
        assert_eq!(
            apply(
                &mut space,
                name,
                SharedAction::Import {
                    id: Id::random(),
                    parent: root,
                    name: "overflow.txt".into(),
                    mime: "text/plain".into(),
                    bytes: vec![1]
                }
            ),
            Err(CollaborationError::TooLarge)
        );
        assert_eq!(Id::hash(&space), before);
        let hash = space.content_hash(file).unwrap();
        assert_eq!(
            apply(
                &mut space,
                name,
                SharedAction::Replace {
                    id: file,
                    expected: hash,
                    version: Id::random(),
                    bytes: vec![0; storage::MAX_FILE_SIZE + 1]
                }
            ),
            Err(StorageError::FileTooLarge.into())
        );
        assert_eq!(Id::hash(&space), before);
    }
    #[test]
    fn malformed_initial_manifest_and_uploads_never_activate() {
        let (alice, _, space, root, file) = fixture();
        let mut items: Vec<_> = space.tree.entries.values().cloned().collect();
        items.push(items[0].clone());
        let invalid = SharedSpace::init(
            SharedInit {
                nonce: Id::random(),
                root,
                items,
            },
            metadata(alice.name()),
        );
        assert_eq!(invalid.validate(), Err(CollaborationError::InvalidSpace));
        let mut initial = SharedSpace::init(
            SharedInit {
                nonce: Id::random(),
                root,
                items: space.tree.entries.values().cloned().collect(),
            },
            metadata(alice.name()),
        );
        assert_eq!(
            apply(&mut initial, alice.name(), SharedAction::Activate),
            Err(CollaborationError::ContentMismatch)
        );
        assert!(!initial.ready);
        assert_eq!(
            apply(
                &mut initial,
                alice.name(),
                SharedAction::Upload {
                    id: file,
                    bytes: b"tampered".to_vec()
                }
            ),
            Err(CollaborationError::ContentMismatch)
        );
        assert!(initial.contents.is_empty());
    }
    #[test]
    fn promotion_replaces_original_tree_preserves_private_trash_and_mounts_idempotently() {
        let (alice, _, space, root, file) = fixture();
        let mut vault = space.tree.clone();
        let secret = Id::random();
        vault
            .apply(
                VaultAction::Create {
                    id: secret,
                    parent: Some(root),
                    name: "Private trash".into(),
                    kind: EntryKind::Folder,
                },
                metadata(alice.name()),
            )
            .unwrap();
        let nested_space = Id::random();
        let nested_mount = vault
            .apply(
                VaultAction::MountShared {
                    space: nested_space,
                    name: "Old shared shortcut".into(),
                    folder: true,
                },
                metadata(alice.name()),
            )
            .unwrap();
        vault
            .apply(
                VaultAction::Move {
                    id: nested_mount,
                    parent: Some(secret),
                },
                metadata(alice.name()),
            )
            .unwrap();
        vault
            .apply(VaultAction::Trash(secret), metadata(alice.name()))
            .unwrap();
        vault
            .apply(
                VaultAction::Favorite {
                    id: root,
                    favorite: true,
                },
                metadata(alice.name()),
            )
            .unwrap();
        let expected = vault.subtree_fingerprint(root).unwrap();
        let channel = Id::random();
        assert_eq!(
            vault.apply(
                VaultAction::PromoteShared {
                    id: root,
                    space: channel,
                    expected: Id::random()
                },
                metadata(alice.name())
            ),
            Err(StorageError::Changed)
        );
        vault
            .apply(
                VaultAction::PromoteShared {
                    id: root,
                    space: channel,
                    expected,
                },
                metadata(alice.name()),
            )
            .unwrap();
        assert!(vault.get(file).is_none());
        assert!(vault.get(root).unwrap().favorite);
        assert!(matches!(vault.entries[&root].kind,EntryKind::Shared{space,..} if space==channel));
        assert_eq!(vault.entries[&secret].parent, None);
        assert!(vault.is_trashed(secret));
        assert!(vault.is_trashed(nested_mount));
        assert_eq!(vault.entries[&nested_mount].parent, Some(secret));
        assert_eq!(
            vault.apply(
                VaultAction::Create {
                    id: Id::random(),
                    parent: Some(root),
                    name: "Invisible child".into(),
                    kind: EntryKind::Folder
                },
                metadata(alice.name())
            ),
            Err(StorageError::InvalidFolder)
        );
        let mount = VaultAction::MountShared {
            space: channel,
            name: "Project".into(),
            folder: true,
        };
        assert_eq!(
            vault.apply(mount.clone(), metadata(alice.name())).unwrap(),
            root
        );
        vault
            .apply(VaultAction::Trash(root), metadata(alice.name()))
            .unwrap();
        vault
            .apply(
                VaultAction::Create {
                    id: Id::random(),
                    parent: None,
                    name: "Project".into(),
                    kind: EntryKind::Folder,
                },
                metadata(alice.name()),
            )
            .unwrap();
        assert_eq!(vault.apply(mount, metadata(alice.name())).unwrap(), root);
        assert!(!vault.is_trashed(root));
        assert_eq!(vault.entries[&root].name, "Project (2)");
    }
    #[test]
    fn encrypted_three_identity_channel_survives_restart_and_preserves_racing_edits() {
        let (alice, bob, state, root, file) = fixture();
        let charlie = air::Secret::new();
        let mut alice_catalog = air::Air::new(alice.clone()).unwrap();
        let mut private_vault = alice_catalog.create::<Vault>(()).unwrap();
        let private_blob = alice_catalog
            .create::<storage::FileBlob>(b"original".to_vec())
            .unwrap();
        for mut entry in state
            .tree
            .entries
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|entry| entry.id == root)
        {
            entry.kind = EntryKind::Folder;
            private_vault
                .send(VaultAction::Create {
                    id: entry.id,
                    parent: None,
                    name: entry.name,
                    kind: entry.kind,
                })
                .unwrap();
        }
        private_vault
            .send(VaultAction::Create {
                id: file,
                parent: Some(root),
                name: "notes.txt".into(),
                kind: EntryKind::File {
                    blob: private_blob.id(),
                    size: 8,
                    mime: "text/plain".into(),
                },
            })
            .unwrap();
        let mut sender = alice_catalog
            .create::<SharedSpace>(SharedInit {
                nonce: Id::random(),
                root,
                items: state.tree.entries.values().cloned().collect(),
            })
            .unwrap();
        sender
            .send(SharedAction::Upload {
                id: file,
                bytes: b"original".to_vec(),
            })
            .unwrap();
        sender.send(SharedAction::Activate).unwrap();
        sender.send(SharedAction::Grant(bob.name())).unwrap();
        let fingerprint = private_vault
            .pending()
            .unwrap()
            .subtree_fingerprint(root)
            .unwrap();
        private_vault
            .send(VaultAction::PromoteShared {
                id: root,
                space: sender.id(),
                expected: fingerprint,
            })
            .unwrap();
        let mut resolver = air::DefaultResolver::default();
        ready(alice_catalog.share(&mut resolver, bob.name(), *sender.location()));
        let mut sender: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&sender).unwrap()).unwrap();
        let mut alice_catalog: air::Air =
            postcard::from_bytes(&postcard::to_allocvec(&alice_catalog).unwrap()).unwrap();
        let mut server = TestServer::new();
        server.sync_catalog(&mut alice_catalog);
        server.sync(&mut sender);
        server.sync(&mut private_vault);
        let mut bob_catalog = air::Air::new(bob.clone()).unwrap();
        server.sync_catalog(&mut bob_catalog);
        assert!(bob_catalog.list(&Vault::id()).is_empty());
        assert!(bob_catalog.list(&storage::FileBlob::id()).is_empty());
        let location = *bob_catalog.list(&SharedSpace::id()).iter().next().unwrap();
        let mut receiver = air::Instance::<SharedSpace>::receive(bob.clone(), location).unwrap();
        server.sync(&mut receiver);
        assert_eq!(
            receiver.send(SharedAction::Edit(VaultAction::Rename {
                id: file,
                name: "not yet".into()
            })),
            Err(CollaborationError::Unauthorized)
        );
        receiver.send(SharedAction::Join).unwrap();
        server.sync(&mut receiver);
        server.sync(&mut sender);
        receiver.send(SharedAction::Grant(charlie.name())).unwrap();
        server.sync(&mut receiver);
        ready(bob_catalog.share(&mut resolver, charlie.name(), location));
        server.sync_catalog(&mut bob_catalog);
        let mut charlie_catalog = air::Air::new(charlie.clone()).unwrap();
        server.sync_catalog(&mut charlie_catalog);
        assert!(charlie_catalog.list(&Vault::id()).is_empty());
        assert!(charlie_catalog.list(&storage::FileBlob::id()).is_empty());
        assert_eq!(
            charlie_catalog.list(&SharedSpace::id()).iter().next(),
            Some(&location)
        );
        let mut third = air::Instance::<SharedSpace>::receive(charlie.clone(), location).unwrap();
        server.sync(&mut third);
        third.send(SharedAction::Join).unwrap();
        server.sync(&mut third);
        server.sync(&mut sender);
        server.sync(&mut receiver);
        let expected = sender.pending().unwrap().content_hash(file).unwrap();
        let conflict = Id::random();
        sender
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"Alice online".to_vec(),
            })
            .unwrap();
        receiver
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: conflict,
                bytes: b"Bob offline".to_vec(),
            })
            .unwrap();
        let mut receiver: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&receiver).unwrap()).unwrap();
        server.sync(&mut sender);
        server.sync(&mut receiver);
        server.sync(&mut third);
        server.sync(&mut sender);
        for current in [&sender, &receiver, &third] {
            assert_eq!(
                current.pending().unwrap().read_file(file).unwrap(),
                b"Alice online"
            );
            assert_eq!(
                current.pending().unwrap().conflicts(file)[0].bytes,
                b"Bob offline"
            );
        }
        let new_file = Id::random();
        third
            .send(SharedAction::Import {
                id: new_file,
                parent: root,
                name: "from third.txt".into(),
                mime: "text/plain".into(),
                bytes: b"shared".to_vec(),
            })
            .unwrap();
        server.sync(&mut third);
        let mut cold = air::Instance::<SharedSpace>::receive(alice.clone(), location).unwrap();
        server.sync(&mut cold);
        assert_eq!(
            cold.pending().unwrap().read_file(new_file).unwrap(),
            b"shared"
        );
        assert!(cold.pending().unwrap().can_write(charlie.name()));
        assert!(private_vault.pending().unwrap().get(file).is_none());
        assert!(matches!(
            private_vault.pending().unwrap().entries[&root].kind,
            EntryKind::Shared { .. }
        ));
    }

    #[test]
    fn quota_rejection_after_remote_replay_keeps_the_private_recovery_bytes() {
        use crate::drafts::{DraftInit, EditDraft};
        let (alice, bob, mut state, root, file) = fixture();
        while state.tree.entries.len() < storage::MAX_TREE_ITEMS - 1 {
            let index = state.tree.entries.len();
            apply(
                &mut state,
                alice.name(),
                SharedAction::Edit(VaultAction::Create {
                    id: Id::random(),
                    parent: Some(root),
                    name: format!("Folder {index}"),
                    kind: EntryKind::Folder,
                }),
            )
            .unwrap();
        }
        let mut alice_catalog = air::Air::new(alice.clone()).unwrap();
        let mut sender = alice_catalog
            .create::<SharedSpace>(SharedInit {
                nonce: Id::random(),
                root,
                items: state.tree.entries.values().cloned().collect(),
            })
            .unwrap();
        sender
            .send(SharedAction::Upload {
                id: file,
                bytes: b"original".to_vec(),
            })
            .unwrap();
        sender.send(SharedAction::Activate).unwrap();
        sender.send(SharedAction::Grant(bob.name())).unwrap();
        let mut server = TestServer::new();
        server.sync(&mut sender);
        let location = *sender.location();
        let mut receiver = air::Instance::<SharedSpace>::receive(bob.clone(), location).unwrap();
        server.sync(&mut receiver);
        receiver.send(SharedAction::Join).unwrap();
        server.sync(&mut receiver);
        server.sync(&mut sender);
        let expected = receiver.pending().unwrap().content_hash(file).unwrap();
        let mut bob_catalog = air::Air::new(bob.clone()).unwrap();
        let draft = bob_catalog
            .create::<EditDraft>(DraftInit {
                nonce: Id::random(),
                space: receiver.id(),
                file,
                name: "notes.txt".into(),
                expected,
                bytes: b"offline edit".to_vec(),
            })
            .unwrap();
        let token = draft.id();
        assert_eq!(
            receiver
                .send(SharedAction::Replace {
                    id: file,
                    expected,
                    version: token,
                    bytes: b"offline edit".to_vec()
                })
                .unwrap(),
            ActionResult::Replace(ReplaceOutcome::Updated)
        );
        // While Bob is offline, Alice changes the canonical bytes and consumes
        // the final entry slot. Bob's replacement now needs a conflict slot.
        sender
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"online edit".to_vec(),
            })
            .unwrap();
        sender
            .send(SharedAction::Edit(VaultAction::Create {
                id: Id::random(),
                parent: Some(root),
                name: "Final slot".into(),
                kind: EntryKind::Folder,
            }))
            .unwrap();
        server.sync(&mut sender);
        let mut receiver: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&receiver).unwrap()).unwrap();
        let mut draft: air::Instance<EditDraft> =
            postcard::from_bytes(&postcard::to_allocvec(&draft).unwrap()).unwrap();
        server.sync(&mut receiver);
        server.sync(&mut draft);
        let shared = receiver.confirmed().unwrap();
        assert_eq!(shared.read_file(file).unwrap(), b"online edit");
        assert!(shared.conflicts(file).is_empty());
        assert!(!shared.has_applied_edit(token));
        let mut cold_draft =
            air::Instance::<EditDraft>::receive(bob.clone(), *draft.location()).unwrap();
        server.sync(&mut cold_draft);
        let saved = cold_draft.confirmed().unwrap();
        assert_eq!(saved.bytes, b"offline edit");
        assert_eq!(saved.owner, bob.name());
        assert!(!saved.resolved);
        assert!(crate::drafts::needs_recovery(token, saved, Some(shared)));
        assert!(alice_catalog.list(&EditDraft::id()).is_empty());
    }
}
