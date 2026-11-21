//! Mutable, explicitly joined AIR spaces. Private Vault/FileBlob channels are
//! never shared. AIR retains encrypted history; member ACLs govern writes.
use crate::storage::{self, Entry, EntryId, EntryKind, StorageError, Vault, VaultAction};
use prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};

pub const MAX_SPACE_MEMBERS: usize = 128;
const MAX_SPACE_LINKS: usize = 16;

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
#[derive(Clone)]
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
    link_tokens: BTreeSet<Id>,
}

// Keep the original ten-field cache boundary. The old timestamp cannot reach
// this sentinel because it is derived from nanoseconds divided by 1e9. Only
// spaces with explicit link access use the extended third field.
impl Serialize for SharedSpace {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.link_tokens.is_empty() {
            (
                &self.owner,
                self.created_at,
                self.root,
                &self.tree,
                &self.members,
                self.ready,
                &self.contents,
                &self.authors,
                &self.last_edit,
                &self.versions,
            )
                .serialize(serializer)
        } else {
            (
                &self.owner,
                u64::MAX,
                (self.created_at, self.root, &self.link_tokens),
                &self.tree,
                &self.members,
                self.ready,
                &self.contents,
                &self.authors,
                &self.last_edit,
                &self.versions,
            )
                .serialize(serializer)
        }
    }
}
impl<'de> Deserialize<'de> for SharedSpace {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SharedSpaceVisitor;
        impl<'de> serde::de::Visitor<'de> for SharedSpaceVisitor {
            type Value = SharedSpace;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a shared file or folder")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                macro_rules! field {
                    ($index:expr) => {
                        seq.next_element()?
                            .ok_or_else(|| serde::de::Error::invalid_length($index, &self))?
                    };
                }
                let owner = field!(0);
                let timestamp: u64 = field!(1);
                let (created_at, root, link_tokens) = if timestamp == u64::MAX {
                    field!(2)
                } else {
                    (timestamp, field!(2), BTreeSet::new())
                };
                Ok(SharedSpace {
                    owner,
                    created_at,
                    root,
                    tree: field!(3),
                    members: field!(4),
                    ready: field!(5),
                    contents: field!(6),
                    authors: field!(7),
                    last_edit: field!(8),
                    versions: field!(9),
                    link_tokens,
                })
            }
        }
        deserializer.deserialize_tuple(10, SharedSpaceVisitor)
    }
}
impl Hash for SharedSpace {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
        self.created_at.hash(state);
        self.root.hash(state);
        self.tree.hash(state);
        self.members.hash(state);
        self.ready.hash(state);
        self.contents.hash(state);
        self.authors.hash(state);
        self.last_edit.hash(state);
        self.versions.hash(state);
        if !self.link_tokens.is_empty() {
            self.link_tokens.hash(state);
        }
    }
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
    EnableLink(Id),
    JoinLink(Id),
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
            Self::EnableLink(_) => f.write_str("EnableLink([redacted])"),
            Self::JoinLink(_) => f.write_str("JoinLink([redacted])"),
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
    NoRecipients,
    TooManyMembers,
    InvalidLink,
    TooManyLinks,
    PartialInvite {
        sent: usize,
        total: usize,
        reason: String,
    },
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
            Self::OwnName => f.write_str("Enter someone else's AirName."),
            Self::NotReady => f.write_str("This shared item is still syncing. Try again shortly."),
            Self::InvalidSpace => f.write_str("This shared item has an invalid file tree."),
            Self::TooLarge => f.write_str("Shared items support 128 entries and saved conflicts, 32 MiB total, files up to 8 MiB, and 32 folder levels. Discard saved edits or remove unused files first."),
            Self::RootOperation => f.write_str("The shared root cannot be moved or deleted here. Remove its shortcut from your own Files instead."),
            Self::MissingConflict => f.write_str("This saved conflict has already been resolved or removed."),
            Self::ContentMismatch => f.write_str("The file contents do not match this shared item."),
            Self::NoRecipients => f.write_str("Choose at least one contact."),
            Self::TooManyMembers => f.write_str("A shared item can include up to 128 people, including you. Select fewer new contacts."),
            Self::InvalidLink => f.write_str("This sharing link is invalid. Ask for a new link."),
            Self::TooManyLinks => f.write_str("This item already has sharing links. Use an existing link."),
            Self::PartialInvite { sent, total, reason } => write!(f, "Queued invitations for {sent} of {total} selected contacts. The remaining invitations were not queued. {reason}"),
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
            link_tokens: BTreeSet::new(),
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
    pub fn permits_link(&self, token: Id) -> bool {
        self.is_ready() && self.link_tokens.contains(&token)
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
            || self.members.len() > MAX_SPACE_MEMBERS
            || self.link_tokens.len() > MAX_SPACE_LINKS
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
            SharedAction::EnableLink(token) => {
                self.authorize(signer)?;
                if !self.link_tokens.contains(&token) && self.link_tokens.len() >= MAX_SPACE_LINKS {
                    return Err(CollaborationError::TooManyLinks);
                }
                self.link_tokens.insert(token);
                Ok(ActionResult::Unit)
            }
            SharedAction::JoinLink(token) => {
                if !self.ready {
                    return Err(CollaborationError::NotReady);
                }
                if !self.link_tokens.contains(&token) {
                    return Err(CollaborationError::InvalidLink);
                }
                if !self.members.contains_key(&signer) && self.members.len() >= MAX_SPACE_MEMBERS {
                    return Err(CollaborationError::TooManyMembers);
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

fn normalize_recipients(
    recipients: Vec<Name>,
    signer: Name,
) -> Result<Vec<Name>, CollaborationError> {
    if recipients.is_empty() {
        return Err(CollaborationError::NoRecipients);
    }
    if recipients.contains(&signer) {
        return Err(CollaborationError::OwnName);
    }
    let recipients: BTreeSet<_> = recipients.into_iter().collect();
    if recipients.len() >= MAX_SPACE_MEMBERS {
        return Err(CollaborationError::TooManyMembers);
    }
    Ok(recipients.into_iter().collect())
}

fn prepare_recipients(
    space: &SharedSpace,
    signer: Name,
    recipients: Vec<Name>,
) -> Result<Vec<Name>, CollaborationError> {
    space.validate()?;
    space.authorize(signer)?;
    let recipients = normalize_recipients(recipients, signer)?;
    let additional = recipients
        .iter()
        .filter(|name| !space.members.contains_key(name))
        .count();
    if space.members.len().saturating_add(additional) > MAX_SPACE_MEMBERS {
        return Err(CollaborationError::TooManyMembers);
    }
    Ok(recipients)
}

/// Preflight the whole selection before changing membership or forwarding this
/// channel. Use the existing Grant wire format for each distinct recipient.
/// These remain separate AIR actions; this is not a server-side transaction.
pub fn invite_many(
    air: &Air,
    space: &mut Instance<SharedSpace>,
    recipients: Vec<Name>,
) -> Result<(), CollaborationError> {
    let recipients = prepare_recipients(&space.pending(), air.me(), recipients)?;
    let total = recipients.len();
    for (sent, recipient) in recipients.into_iter().enumerate() {
        if let Err(error) = invite(air, space, recipient) {
            return Err(if sent == 0 {
                error
            } else {
                CollaborationError::PartialInvite {
                    sent,
                    total,
                    reason: error.to_string(),
                }
            });
        }
    }
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
    share_with(air, vault, id, vec![recipient])
}

/// Promote a private item once, then grant the complete recipient batch. All
/// recipient validation happens before creating a space or changing the Vault.
pub fn share_with(
    air: &Air,
    vault: &mut Instance<Vault>,
    id: EntryId,
    recipients: Vec<Name>,
) -> Result<Instance<SharedSpace>, CollaborationError> {
    let recipients = normalize_recipients(recipients, air.me())?;
    let mut space = promote(air, vault, id)?;
    invite_many(air, &mut space, recipients)?;
    Ok(space)
}

/// Explicitly enable bearer-link access. Merely viewing a file or sharing it
/// with named contacts never calls this path. Existing links are reused.
pub fn enable_link(air: &Air, space: &mut Instance<SharedSpace>) -> Result<Id, CollaborationError> {
    let snapshot = space.pending();
    snapshot.validate()?;
    snapshot.authorize(air.me())?;
    if let Some(token) = snapshot.link_tokens.first() {
        return Ok(*token);
    }
    drop(snapshot);
    let token = Id::random();
    dispatch(air, space, SharedAction::EnableLink(token))?;
    Ok(token)
}

/// Promote one private item to the same canonical shared space used by named
/// invitations, then enable a reusable link for read/write access.
pub fn prepare_link(
    air: &Air,
    vault: &mut Instance<Vault>,
    id: EntryId,
) -> Result<(Instance<SharedSpace>, Id), CollaborationError> {
    let mut space = promote(air, vault, id)?;
    let token = enable_link(air, &mut space)?;
    Ok((space, token))
}

pub fn join_link(
    air: &Air,
    vault: &mut Instance<Vault>,
    space: &mut Instance<SharedSpace>,
    token: Id,
) -> Result<EntryId, CollaborationError> {
    if vault.pending().owner != air.me() {
        return Err(CollaborationError::Unauthorized);
    }
    dispatch(air, space, SharedAction::JoinLink(token))?;
    let snapshot = space.pending();
    Ok(storage::mount_shared(
        vault,
        space.id(),
        snapshot.title(),
        snapshot.is_folder(),
    )?)
}

fn promote(
    air: &Air,
    vault: &mut Instance<Vault>,
    id: EntryId,
) -> Result<Instance<SharedSpace>, CollaborationError> {
    let snapshot = vault.pending();
    if snapshot.owner != air.me() {
        return Err(CollaborationError::Unauthorized);
    }
    let expected = snapshot.subtree_fingerprint(id)?;
    let entry = snapshot.get(id).ok_or(StorageError::MissingEntry)?;
    if let EntryKind::Shared { space, .. } = entry.kind {
        return get(air, space);
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
    fn link_access_requires_explicit_authorization_and_a_valid_token() {
        let (alice, bob, mut space, _, file) = fixture();
        let token = Id::random();
        assert!(!space.permits_link(token));
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::EnableLink(token)),
            Err(CollaborationError::Unauthorized)
        );
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::JoinLink(token)),
            Err(CollaborationError::InvalidLink)
        );
        apply(&mut space, alice.name(), SharedAction::EnableLink(token)).unwrap();
        apply(&mut space, alice.name(), SharedAction::EnableLink(token)).unwrap();
        assert_eq!(space.link_tokens.len(), 1);
        assert!(space.permits_link(token));
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::Join),
            Err(CollaborationError::Unauthorized),
            "Named-contact Join remains restricted"
        );
        let before = postcard::to_allocvec(&space).unwrap();
        assert_eq!(
            apply(&mut space, bob.name(), SharedAction::JoinLink(Id::random())),
            Err(CollaborationError::InvalidLink)
        );
        assert_eq!(postcard::to_allocvec(&space).unwrap(), before);
        apply(&mut space, bob.name(), SharedAction::JoinLink(token)).unwrap();
        apply(&mut space, bob.name(), SharedAction::JoinLink(token)).unwrap();
        assert_eq!(space.members.len(), 2);
        assert!(space.can_write(bob.name()));
        apply(
            &mut space,
            bob.name(),
            SharedAction::Edit(VaultAction::Rename {
                id: file,
                name: "edited by link recipient.txt".into(),
            }),
        )
        .unwrap();
        assert_eq!(
            space.tree.get(file).unwrap().name,
            "edited by link recipient.txt"
        );
        assert_eq!(space.read_file(file).unwrap(), b"original");
        assert!(!format!("{space:?}").contains(&token.to_string()));
        for action in [
            SharedAction::EnableLink(token),
            SharedAction::JoinLink(token),
        ] {
            assert!(!format!("{action:?}").contains(&token.to_string()));
        }
    }

    #[test]
    fn link_join_and_publication_limits_are_transactional_and_allow_retries() {
        let (alice, bob, mut space, _, _) = fixture();
        let token = Id::random();
        apply(&mut space, alice.name(), SharedAction::EnableLink(token)).unwrap();
        apply(&mut space, alice.name(), SharedAction::Grant(bob.name())).unwrap();
        apply(&mut space, bob.name(), SharedAction::Decline).unwrap();
        while space.members.len() < MAX_SPACE_MEMBERS {
            space
                .members
                .insert(air::Secret::new().name(), MemberStatus::Invited);
        }
        let before = postcard::to_allocvec(&space).unwrap();
        assert_eq!(
            apply(
                &mut space,
                air::Secret::new().name(),
                SharedAction::JoinLink(token)
            ),
            Err(CollaborationError::TooManyMembers)
        );
        assert_eq!(postcard::to_allocvec(&space).unwrap(), before);
        apply(&mut space, bob.name(), SharedAction::JoinLink(token)).unwrap();
        apply(&mut space, bob.name(), SharedAction::JoinLink(token)).unwrap();
        assert_eq!(space.members.len(), MAX_SPACE_MEMBERS);
        while space.link_tokens.len() < MAX_SPACE_LINKS {
            apply(
                &mut space,
                bob.name(),
                SharedAction::EnableLink(Id::random()),
            )
            .unwrap();
        }
        let before = postcard::to_allocvec(&space).unwrap();
        assert_eq!(
            apply(
                &mut space,
                bob.name(),
                SharedAction::EnableLink(Id::random())
            ),
            Err(CollaborationError::TooManyLinks)
        );
        assert_eq!(postcard::to_allocvec(&space).unwrap(), before);
        apply(&mut space, bob.name(), SharedAction::EnableLink(token)).unwrap();
        let mut unfinished = space.clone();
        unfinished.ready = false;
        assert!(!unfinished.permits_link(token));
        assert_eq!(
            apply(&mut unfinished, bob.name(), SharedAction::JoinLink(token)),
            Err(CollaborationError::NotReady)
        );
    }

    #[test]
    fn link_state_preserves_v1_cache_bytes_hashes_and_nested_boundaries() {
        #[derive(Serialize, Hash)]
        struct V1Space<'a> {
            owner: Name,
            created_at: u64,
            root: EntryId,
            tree: &'a Vault,
            members: &'a BTreeMap<Name, MemberStatus>,
            ready: bool,
            contents: &'a BTreeMap<EntryId, Vec<u8>>,
            authors: &'a BTreeMap<EntryId, Name>,
            last_edit: &'a BTreeMap<EntryId, Id>,
            versions: &'a BTreeMap<EntryId, Vec<ConflictVersion>>,
        }
        let (alice, _, mut space, _, _) = fixture();
        let legacy = V1Space {
            owner: space.owner,
            created_at: space.created_at,
            root: space.root,
            tree: &space.tree,
            members: &space.members,
            ready: space.ready,
            contents: &space.contents,
            authors: &space.authors,
            last_edit: &space.last_edit,
            versions: &space.versions,
        };
        assert_eq!(
            postcard::to_allocvec(&legacy).unwrap(),
            postcard::to_allocvec(&space).unwrap()
        );
        assert_eq!(Id::hash(&legacy), Id::hash(&space));
        let legacy_envelope = postcard::to_allocvec(&(vec![&legacy, &legacy], 42u64)).unwrap();
        let (decoded, trailing): (Vec<SharedSpace>, u64) =
            postcard::from_bytes(&legacy_envelope).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(trailing, 42);
        assert!(decoded.iter().all(|item| item.link_tokens.is_empty()));
        assert!(
            decoded
                .iter()
                .all(|item| Id::hash(item) == Id::hash(&space))
        );
        let token = Id::random();
        apply(&mut space, alice.name(), SharedAction::EnableLink(token)).unwrap();
        let envelope =
            postcard::to_allocvec(&(vec![decoded[0].clone(), space.clone()], 73u64)).unwrap();
        let (restored, trailing): (Vec<SharedSpace>, u64) =
            postcard::from_bytes(&envelope).unwrap();
        assert_eq!(trailing, 73);
        assert!(restored[0].link_tokens.is_empty());
        assert!(restored[1].permits_link(token));
        assert_eq!(Id::hash(&restored[1]), Id::hash(&space));
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
    fn recipient_preflight_checks_the_entire_batch_without_changing_members() {
        let (alice, bob, mut space, _, _) = fixture();
        let charlie = air::Secret::new().name();
        join_member(&mut space, alice.name(), bob.name());
        apply(&mut space, alice.name(), SharedAction::Grant(charlie)).unwrap();
        apply(&mut space, charlie, SharedAction::Decline).unwrap();
        let before = space.members.clone();
        for (signer, recipients, error) in [
            (alice.name(), vec![], CollaborationError::NoRecipients),
            (
                alice.name(),
                vec![charlie, alice.name()],
                CollaborationError::OwnName,
            ),
            (charlie, vec![bob.name()], CollaborationError::Unauthorized),
        ] {
            assert_eq!(prepare_recipients(&space, signer, recipients), Err(error));
            assert_eq!(space.members, before);
        }
        while space.members.len() < MAX_SPACE_MEMBERS - 1 {
            space
                .members
                .insert(air::Secret::new().name(), MemberStatus::Invited);
        }
        let first = air::Secret::new().name();
        let second = air::Secret::new().name();
        let before = space.members.clone();
        assert_eq!(
            prepare_recipients(&space, bob.name(), vec![charlie, first, second, first]),
            Err(CollaborationError::TooManyMembers)
        );
        assert_eq!(
            space.members, before,
            "Rejected preflight must not reinvite declined members or add a subset"
        );
        let recipients =
            prepare_recipients(&space, bob.name(), vec![charlie, first, first]).unwrap();
        assert_eq!(recipients.len(), 2);
        assert_eq!(
            space.members, before,
            "Successful preflight is also read-only"
        );
        for recipient in recipients {
            apply(&mut space, bob.name(), SharedAction::Grant(recipient)).unwrap();
        }
        assert_eq!(space.members.len(), MAX_SPACE_MEMBERS);
        assert_eq!(space.members[&charlie], MemberStatus::Invited);
        assert_eq!(space.members[&first], MemberStatus::Invited);
        assert_eq!(space.members[&bob.name()], MemberStatus::Active);
        // Existing recipients and duplicates consume no additional slots.
        for recipient in
            prepare_recipients(&space, alice.name(), vec![bob.name(), first, first]).unwrap()
        {
            apply(&mut space, alice.name(), SharedAction::Grant(recipient)).unwrap();
        }
        assert_eq!(space.members[&bob.name()], MemberStatus::Active);
        let before = space.members.clone();
        assert_eq!(
            prepare_recipients(&space, alice.name(), vec![second]),
            Err(CollaborationError::TooManyMembers)
        );
        assert_eq!(
            apply(&mut space, alice.name(), SharedAction::Grant(second)),
            Err(CollaborationError::InvalidSpace)
        );
        assert_eq!(
            space.members, before,
            "The original Grant behavior remains unchanged"
        );
    }

    #[test]
    fn recipient_normalization_rejects_invalid_batches_and_preserves_action_wire_tags() {
        let owner = air::Secret::new().name();
        let peer = air::Secret::new().name();
        assert_eq!(
            normalize_recipients(vec![], owner),
            Err(CollaborationError::NoRecipients)
        );
        assert_eq!(
            normalize_recipients(vec![peer, owner], owner),
            Err(CollaborationError::OwnName)
        );
        assert_eq!(
            normalize_recipients(vec![peer; MAX_SPACE_MEMBERS * 2], owner).unwrap(),
            vec![peer]
        );
        let too_many: Vec<_> = (0..MAX_SPACE_MEMBERS)
            .map(|_| air::Secret::new().name())
            .collect();
        assert_eq!(
            normalize_recipients(too_many, owner),
            Err(CollaborationError::TooManyMembers)
        );
        let id = Id::random();
        let previous = [
            SharedAction::Upload {
                id,
                bytes: Vec::new(),
            },
            SharedAction::Activate,
            SharedAction::Grant(peer),
            SharedAction::Join,
            SharedAction::Decline,
            SharedAction::Edit(VaultAction::Trash(id)),
            SharedAction::Import {
                id,
                parent: id,
                name: "name".into(),
                mime: "text/plain".into(),
                bytes: Vec::new(),
            },
            SharedAction::Replace {
                id,
                expected: id,
                version: id,
                bytes: Vec::new(),
            },
            SharedAction::Resolve {
                id,
                conflict: id,
                expected: id,
            },
            SharedAction::DiscardConflict { id, conflict: id },
        ];
        for (tag, action) in previous.into_iter().enumerate() {
            assert_eq!(postcard::to_allocvec(&action).unwrap()[0], tag as u8);
        }
        assert_eq!(
            postcard::to_allocvec(&SharedAction::EnableLink(id)).unwrap()[0],
            10
        );
        assert_eq!(
            postcard::to_allocvec(&SharedAction::JoinLink(id)).unwrap()[0],
            11
        );
    }

    #[test]
    fn link_recipients_replay_authorization_and_edit_the_same_encrypted_space_after_restart() {
        let (alice, bob, state, root, file) = fixture();
        let token = Id::random();
        let mut sender = air::Instance::<SharedSpace>::new(
            alice.clone(),
            SharedInit {
                nonce: Id::random(),
                root,
                items: state.tree.entries.values().cloned().collect(),
            },
        )
        .unwrap();
        sender
            .send(SharedAction::Upload {
                id: file,
                bytes: b"original".to_vec(),
            })
            .unwrap();
        sender.send(SharedAction::Activate).unwrap();
        sender.send(SharedAction::EnableLink(token)).unwrap();
        let mut server = TestServer::new();
        server.sync(&mut sender);
        // Receiving the channel capability alone does not bypass its write ACL.
        let mut receiver =
            air::Instance::<SharedSpace>::receive(bob.clone(), *sender.location()).unwrap();
        server.sync(&mut receiver);
        assert!(receiver.confirmed().unwrap().permits_link(token));
        assert!(!receiver.confirmed().unwrap().can_write(bob.name()));
        receiver.send(SharedAction::JoinLink(token)).unwrap();
        let mut receiver: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&receiver).unwrap()).unwrap();
        server.sync(&mut receiver);
        server.sync(&mut sender);
        assert!(receiver.confirmed().unwrap().can_write(bob.name()));
        assert_eq!(receiver.id(), sender.id());
        let expected = receiver.pending().unwrap().content_hash(file).unwrap();
        receiver
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"updated through the invitation".to_vec(),
            })
            .unwrap();
        server.sync(&mut receiver);
        let mut sender: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&sender).unwrap()).unwrap();
        server.sync(&mut sender);
        assert_eq!(
            sender.confirmed().unwrap().read_file(file).unwrap(),
            b"updated through the invitation"
        );
        assert!(sender.confirmed().unwrap().permits_link(token));
    }

    #[test]
    fn original_grants_deliver_the_same_encrypted_space_to_multiple_people() {
        let (alice, bob, state, root, file) = fixture();
        let charlie = air::Secret::new();
        let mut catalog = air::Air::new(alice.clone()).unwrap();
        let mut sender = catalog
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
        let recipients = prepare_recipients(
            sender.pending().unwrap(),
            alice.name(),
            vec![bob.name(), charlie.name(), bob.name()],
        )
        .unwrap();
        for recipient in recipients {
            sender.send(SharedAction::Grant(recipient)).unwrap();
        }
        let mut resolver = air::DefaultResolver::default();
        for name in [bob.name(), charlie.name()] {
            ready(catalog.share(&mut resolver, name, *sender.location()));
        }
        let mut sender: air::Instance<SharedSpace> =
            postcard::from_bytes(&postcard::to_allocvec(&sender).unwrap()).unwrap();
        let mut catalog: air::Air =
            postcard::from_bytes(&postcard::to_allocvec(&catalog).unwrap()).unwrap();
        let mut server = TestServer::new();
        server.sync_catalog(&mut catalog);
        server.sync(&mut sender);
        let mut receivers = Vec::new();
        for secret in [bob, charlie] {
            let mut catalog = air::Air::new(secret.clone()).unwrap();
            server.sync_catalog(&mut catalog);
            assert!(catalog.list(&Vault::id()).is_empty());
            assert!(catalog.list(&storage::FileBlob::id()).is_empty());
            let locations = catalog.list(&SharedSpace::id());
            assert_eq!(locations.len(), 1);
            assert_eq!(locations.iter().next(), Some(sender.location()));
            let mut receiver =
                air::Instance::<SharedSpace>::receive(secret.clone(), *sender.location()).unwrap();
            server.sync(&mut receiver);
            assert_eq!(
                receiver.pending().unwrap().members[&secret.name()],
                MemberStatus::Invited
            );
            receiver.send(SharedAction::Join).unwrap();
            server.sync(&mut receiver);
            receivers.push(receiver);
        }
        server.sync(&mut sender);
        assert_eq!(sender.pending().unwrap().members.len(), 3);
        assert!(
            sender
                .pending()
                .unwrap()
                .members
                .values()
                .all(|status| *status == MemberStatus::Active)
        );
        let expected = sender.pending().unwrap().content_hash(file).unwrap();
        sender
            .send(SharedAction::Replace {
                id: file,
                expected,
                version: Id::random(),
                bytes: b"same shared contents".to_vec(),
            })
            .unwrap();
        server.sync(&mut sender);
        for receiver in &mut receivers {
            server.sync(receiver);
            assert_eq!(receiver.id(), sender.id());
            assert_eq!(
                receiver.pending().unwrap().read_file(file).unwrap(),
                b"same shared contents"
            );
        }
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
