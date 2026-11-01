//! Private recovery copies of submitted edits. These AIR instances are never
//! shared with collaborators and survive a rejected or still-pending edit.
use crate::{
    collaboration::{self, SharedSpace},
    storage::{self, EntryId, MAX_FILE_SIZE, StorageError},
};
use prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Hash, Serialize, Deserialize)]
pub struct DraftInit {
    pub nonce: Id,
    pub space: Id,
    pub file: EntryId,
    pub name: String,
    pub expected: Id,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for DraftInit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraftInit")
            .field("space", &self.space)
            .field("file", &self.file)
            .field("size", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Hash, Serialize, Deserialize)]
pub struct EditDraft {
    pub owner: Name,
    pub space: Id,
    pub file: EntryId,
    pub name: String,
    pub expected: Id,
    pub bytes: Vec<u8>,
    /// Unix seconds, supplied by AIR rather than by the submitting form.
    pub created_at: u64,
    pub resolved: bool,
}

impl fmt::Debug for EditDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditDraft")
            .field("owner", &self.owner)
            .field("space", &self.space)
            .field("file", &self.file)
            .field("size", &self.bytes.len())
            .field("created_at", &self.created_at)
            .field("resolved", &self.resolved)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Hash, Serialize, Deserialize)]
pub enum DraftAction {
    Resolve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftError {
    Unauthorized,
    Invalid(StorageError),
    Missing,
}

impl fmt::Display for DraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("This saved edit belongs to another identity."),
            Self::Invalid(error) => error.fmt(f),
            Self::Missing => f.write_str("This saved edit is not available yet."),
        }
    }
}
impl std::error::Error for DraftError {}

impl EditDraft {
    pub fn validate(&self) -> Result<(), DraftError> {
        validate_payload(&self.name, &self.bytes)
    }
}

fn validate_payload(name: &str, bytes: &[u8]) -> Result<(), DraftError> {
    storage::validate_name(name).map_err(DraftError::Invalid)?;
    if bytes.len() > MAX_FILE_SIZE {
        return Err(DraftError::Invalid(StorageError::FileTooLarge));
    }
    Ok(())
}

impl Contract for EditDraft {
    type Init = DraftInit;
    type Message = DraftAction;
    type Result = Result<(), DraftError>;

    fn id() -> Id {
        Id::hash("org.ramp.fig-files.Drafts.v1")
    }

    fn init(init: DraftInit, metadata: Metadata) -> Self {
        Self {
            owner: metadata.signer,
            space: init.space,
            file: init.file,
            name: init.name,
            expected: init.expected,
            bytes: init.bytes,
            created_at: metadata.timestamp / 1_000_000_000,
            resolved: false,
        }
    }

    fn apply(&mut self, action: DraftAction, metadata: Metadata) -> Self::Result {
        if metadata.signer != self.owner {
            return Err(DraftError::Unauthorized);
        }
        self.validate()?;
        match action {
            DraftAction::Resolve => self.resolved = true,
        }
        Ok(())
    }
}

/// Register the private contract on startup so AIR restores its cached instances.
pub fn open(air: &Air) {
    let _ = air.instances::<EditDraft>();
}

/// Save before attempting the shared write. Identical submissions remain
/// independent recovery records, and no shared channel or capability is created.
pub fn save(
    air: &Air,
    space: Id,
    file: EntryId,
    name: &str,
    expected: Id,
    bytes: Vec<u8>,
) -> Result<Instance<EditDraft>, DraftError> {
    validate_payload(name, &bytes)?;
    Ok(air.instances::<EditDraft>().create(DraftInit {
        nonce: Id::random(),
        space,
        file,
        name: name.into(),
        expected,
        bytes,
    }))
}

/// Only the local identity's valid recovery records may be displayed or exported.
pub fn list(air: &Air) -> Vec<Instance<EditDraft>> {
    let mut drafts: Vec<_> = air
        .instances::<EditDraft>()
        .list()
        .into_values()
        .filter(|draft| {
            let state = draft.pending();
            state.owner == air.me() && state.validate().is_ok()
        })
        .collect();
    drafts.sort_by_key(|draft| (std::cmp::Reverse(draft.pending().created_at), draft.id()));
    drafts
}

pub fn get(air: &Air, id: Id) -> Result<Instance<EditDraft>, DraftError> {
    let draft = air
        .instances::<EditDraft>()
        .list()
        .remove(&id)
        .ok_or(DraftError::Missing)?;
    {
        let state = draft.pending();
        if state.owner != air.me() {
            return Err(DraftError::Unauthorized);
        }
        state.validate()?;
    }
    Ok(draft)
}

/// Explicit dismissal is an owner-signed action; pending shared success alone
/// must never call this helper, because AIR may reject that edit on confirmation.
pub fn resolve(draft: &mut Instance<EditDraft>) -> Result<(), DraftError> {
    draft.send(DraftAction::Resolve).pending().clone()
}

/// A pending shared write is insufficient evidence of durability. Keep the
/// recovery copy until explicit dismissal or the same bytes appear in confirmed
/// canonical content or a confirmed preserved conflict. No writes occur here.
pub fn recoverable(air: &Air, draft: &Instance<EditDraft>) -> bool {
    let token = draft.id();
    let draft = draft.pending();
    if draft.owner != air.me() || draft.validate().is_err() || draft.resolved {
        return false;
    }
    let Ok(space) = collaboration::get(air, draft.space) else {
        return true;
    };
    let confirmed = space.confirmed();
    needs_recovery(token, &draft, confirmed.as_deref())
}

pub(crate) fn needs_recovery(
    token: Id,
    draft: &EditDraft,
    confirmed: Option<&SharedSpace>,
) -> bool {
    if draft.resolved {
        return false;
    }
    let Some(space) = confirmed.filter(|space| space.is_ready()) else {
        return true;
    };
    if space.has_applied_edit(token) {
        return false;
    }
    if space
        .read_file(draft.file)
        .is_ok_and(|bytes| bytes == draft.bytes)
    {
        return false;
    }
    !space
        .conflicts(draft.file)
        .iter()
        .any(|version| version.bytes == draft.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        collaboration::{SharedAction, SharedInit},
        storage::{Entry, EntryKind},
    };

    fn owner() -> Name {
        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
            .parse()
            .unwrap()
    }
    fn other() -> Name {
        "0379be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
            .parse()
            .unwrap()
    }
    fn metadata(signer: Name) -> Metadata {
        Metadata {
            signer,
            timestamp: 7_000_000_000,
            confirmed: true,
        }
    }
    fn draft(bytes: &[u8]) -> EditDraft {
        EditDraft::init(
            DraftInit {
                nonce: Id::random(),
                space: Id::from(20),
                file: Id::from(21),
                name: "draft.txt".into(),
                expected: Id::hash(&b"original".to_vec()),
                bytes: bytes.into(),
            },
            metadata(owner()),
        )
    }
    fn shared(bytes: &[u8]) -> SharedSpace {
        let id = Id::from(21);
        let mut space = SharedSpace::init(
            SharedInit {
                nonce: Id::random(),
                root: id,
                items: vec![Entry {
                    id,
                    parent: None,
                    name: "draft.txt".into(),
                    kind: EntryKind::File {
                        blob: Id::hash(&bytes.to_vec()),
                        size: bytes.len() as u64,
                        mime: "text/plain".into(),
                    },
                    favorite: false,
                    created_at: 1,
                    modified_at: 1,
                    trashed_at: None,
                }],
            },
            metadata(owner()),
        );
        space
            .apply(
                SharedAction::Upload {
                    id,
                    bytes: bytes.into(),
                },
                metadata(owner()),
            )
            .unwrap();
        space
            .apply(SharedAction::Activate, metadata(owner()))
            .unwrap();
        space
    }

    #[test]
    fn draft_survives_serialization_with_binary_bytes_and_original_revision() {
        let original = draft(b"private\0draft\xff");
        let saved = postcard::to_allocvec(&original).unwrap();
        let restored: EditDraft = postcard::from_bytes(&saved).unwrap();
        assert_eq!(restored.owner, owner());
        assert_eq!(restored.created_at, 7);
        assert_eq!(restored.expected, original.expected);
        assert_eq!(restored.name, "draft.txt");
        assert_eq!(restored.bytes, original.bytes);
        assert!(needs_recovery(Id::from(90), &restored, None));
        assert!(!format!("{restored:?}").contains("private"));
    }

    #[test]
    fn only_owner_can_dismiss_and_replay_is_idempotent() {
        let mut draft = draft(b"text");
        assert_eq!(
            draft.apply(DraftAction::Resolve, metadata(other())),
            Err(DraftError::Unauthorized)
        );
        assert!(!draft.resolved);
        draft
            .apply(DraftAction::Resolve, metadata(owner()))
            .unwrap();
        draft
            .apply(DraftAction::Resolve, metadata(owner()))
            .unwrap();
        assert!(draft.resolved);
        assert!(!needs_recovery(Id::from(90), &draft, None));
    }

    #[test]
    fn invalid_names_and_oversized_drafts_are_rejected() {
        assert!(validate_payload("../escape.txt", b"x").is_err());
        assert!(validate_payload("", b"x").is_err());
        assert_eq!(
            validate_payload("large.bin", &vec![0; MAX_FILE_SIZE + 1]),
            Err(DraftError::Invalid(StorageError::FileTooLarge))
        );
        let mut invalid = draft(b"bytes");
        invalid.name = "bad\nname".into();
        assert!(
            invalid
                .apply(DraftAction::Resolve, metadata(owner()))
                .is_err()
        );
        assert!(!invalid.resolved);
    }

    #[test]
    fn missing_or_different_confirmed_content_keeps_recovery() {
        let draft = draft(b"edited");
        assert!(needs_recovery(Id::from(90), &draft, None));
        assert!(needs_recovery(
            Id::from(90),
            &draft,
            Some(&shared(b"original"))
        ));
        assert!(!needs_recovery(
            Id::from(90),
            &draft,
            Some(&shared(b"edited"))
        ));
    }

    #[test]
    fn confirmed_conflict_is_durable_but_a_failed_write_keeps_recovery() {
        let draft = draft(b"edited");
        let mut space = shared(b"remote version");
        assert!(needs_recovery(Id::from(90), &draft, Some(&space)));
        space
            .apply(
                SharedAction::Replace {
                    id: draft.file,
                    expected: draft.expected,
                    version: Id::random(),
                    bytes: draft.bytes.clone(),
                },
                metadata(owner()),
            )
            .unwrap();
        assert_eq!(space.read_file(draft.file).unwrap(), b"remote version");
        assert!(!needs_recovery(Id::from(90), &draft, Some(&space)));
        let mut rejected = shared(b"remote version");
        assert!(
            rejected
                .apply(
                    SharedAction::Replace {
                        id: draft.file,
                        expected: draft.expected,
                        version: Id::random(),
                        bytes: draft.bytes.clone(),
                    },
                    metadata(other())
                )
                .is_err()
        );
        assert!(needs_recovery(Id::from(90), &draft, Some(&rejected)));
    }

    #[test]
    fn confirmed_receipt_acknowledges_a_specific_submitted_edit() {
        let draft = draft(b"edited");
        let token = Id::from(90);
        let mut space = shared(b"original");
        assert!(!space.has_applied_edit(token));
        assert!(needs_recovery(token, &draft, Some(&space)));
        space
            .apply(
                SharedAction::Replace {
                    id: draft.file,
                    expected: draft.expected,
                    version: token,
                    bytes: draft.bytes.clone(),
                },
                metadata(owner()),
            )
            .unwrap();
        assert!(space.has_applied_edit(token));
        assert!(!space.has_applied_edit(Id::from(91)));
        assert!(!needs_recovery(token, &draft, Some(&space)));
        // The same optimistic success is never sufficient without a confirmed
        // snapshot, and filtering the recovery list does not dismiss the record.
        assert!(needs_recovery(token, &draft, None));
        assert!(!draft.resolved);
    }
}
