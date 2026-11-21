//! Explicit, immutable file/folder copies exchanged with an AIR identity.
//!
//! Only a dedicated transfer channel is shared. The sender's private library
//! and reusable FileBlob channels never leave the sender's identity.
use crate::storage::{self, EntryId, EntryKind, FileBlob, ImportItem, StorageError, Vault};
use prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub const MAX_TRANSFER_BYTES: u64 = storage::MAX_TREE_BYTES;
pub const MAX_TRANSFER_ITEMS: usize = storage::MAX_TREE_ITEMS;
pub const MAX_TRANSFER_DEPTH: usize = storage::MAX_TREE_DEPTH;

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferKind {
    Folder,
    File { size: u64, mime: String },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferItem {
    /// Index of an earlier folder. Only item zero has no parent.
    pub parent: Option<u32>,
    pub name: String,
    pub kind: TransferKind,
}

#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub struct TransferInit {
    pub recipient: Name,
    pub nonce: Id,
    pub items: Vec<TransferItem>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferStatus {
    Pending,
    Accepted,
    Declined,
}

#[derive(Clone, Hash, Serialize, Deserialize)]
pub struct Transfer {
    pub sender: Name,
    pub recipient: Name,
    pub created_at: u64,
    pub items: Vec<TransferItem>,
    pub status: TransferStatus,
    sealed: bool,
    contents: BTreeMap<u32, Vec<u8>>,
}
impl fmt::Debug for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transfer")
            .field("sender", &self.sender)
            .field("recipient", &self.recipient)
            .field("items", &self.items)
            .field("status", &self.status)
            .field("sealed", &self.sealed)
            .finish()
    }
}

#[derive(Clone, Hash, Serialize, Deserialize)]
pub enum TransferAction {
    Upload { index: u32, bytes: Vec<u8> },
    Seal,
    Accept,
    Decline,
}
impl fmt::Debug for TransferAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upload { index, bytes } => f
                .debug_struct("Upload")
                .field("index", index)
                .field("size", &bytes.len())
                .finish(),
            Self::Seal => f.write_str("Seal"),
            Self::Accept => f.write_str("Accept"),
            Self::Decline => f.write_str("Decline"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SharingError {
    Storage(StorageError),
    OwnName,
    Unauthorized,
    InvalidSnapshot,
    TooLarge,
    NotReady,
    Closed,
    ContentMismatch,
}
impl From<StorageError> for SharingError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl fmt::Display for SharingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::OwnName => f.write_str("Enter someone else's AirName."),
            Self::Unauthorized => f.write_str("Only the intended recipient can accept or decline this transfer."),
            Self::InvalidSnapshot => f.write_str("This transfer contains an invalid folder tree or filename."),
            Self::TooLarge => f.write_str("Send up to 128 items and 32 MiB total, with files up to 8 MiB and folders up to 32 levels deep."),
            Self::NotReady => f.write_str("This transfer is still arriving. Try again when its files have finished syncing."),
            Self::Closed => f.write_str("This transfer has already been accepted or declined."),
            Self::ContentMismatch => f.write_str("The received file contents do not match this transfer."),
        }
    }
}
impl std::error::Error for SharingError {}

impl Contract for Transfer {
    type Init = TransferInit;
    type Message = TransferAction;
    type Result = Result<(), SharingError>;
    fn id() -> Id {
        Id::hash("org.ramp.fig-files.Transfer.v1")
    }
    fn init(init: TransferInit, metadata: Metadata) -> Self {
        Self {
            sender: metadata.signer,
            recipient: init.recipient,
            created_at: metadata.timestamp / 1_000_000_000,
            items: init.items,
            status: TransferStatus::Pending,
            sealed: false,
            contents: BTreeMap::new(),
        }
    }
    fn apply(&mut self, action: TransferAction, metadata: Metadata) -> Self::Result {
        self.validate()?;
        match action {
            TransferAction::Upload { index, bytes } => {
                if metadata.signer != self.sender {
                    return Err(SharingError::Unauthorized);
                }
                if self.sealed || self.status != TransferStatus::Pending {
                    return Err(SharingError::Closed);
                }
                let Some(TransferItem {
                    kind: TransferKind::File { size, .. },
                    ..
                }) = self.items.get(index as usize)
                else {
                    return Err(SharingError::ContentMismatch);
                };
                if bytes.len() as u64 != *size || bytes.len() > storage::MAX_FILE_SIZE {
                    return Err(SharingError::ContentMismatch);
                }
                if let Some(existing) = self.contents.get(&index) {
                    return if existing == &bytes {
                        Ok(())
                    } else {
                        Err(SharingError::ContentMismatch)
                    };
                }
                self.contents.insert(index, bytes);
                Ok(())
            }
            TransferAction::Seal => {
                if metadata.signer != self.sender {
                    return Err(SharingError::Unauthorized);
                }
                if self.status != TransferStatus::Pending {
                    return Err(SharingError::Closed);
                }
                if !self.complete() {
                    return Err(SharingError::NotReady);
                }
                self.sealed = true;
                Ok(())
            }
            TransferAction::Accept => {
                if metadata.signer != self.recipient {
                    return Err(SharingError::Unauthorized);
                }
                if self.status == TransferStatus::Accepted {
                    return Ok(());
                }
                if self.status != TransferStatus::Pending {
                    return Err(SharingError::Closed);
                }
                if !self.is_ready() {
                    return Err(SharingError::NotReady);
                }
                self.status = TransferStatus::Accepted;
                Ok(())
            }
            TransferAction::Decline => {
                if metadata.signer != self.recipient {
                    return Err(SharingError::Unauthorized);
                }
                if self.status == TransferStatus::Declined {
                    return Ok(());
                }
                if self.status != TransferStatus::Pending {
                    return Err(SharingError::Closed);
                }
                self.status = TransferStatus::Declined;
                Ok(())
            }
        }
    }
}

impl Transfer {
    pub fn title(&self) -> &str {
        self.items
            .first()
            .map(|item| item.name.as_str())
            .unwrap_or("Invalid transfer")
    }
    pub fn total_size(&self) -> u64 {
        self.items.iter().fold(0u64, |total, item| {
            total.saturating_add(match item.kind {
                TransferKind::Folder => 0,
                TransferKind::File { size, .. } => size,
            })
        })
    }
    pub fn item_count(&self) -> usize {
        self.items.len()
    }
    pub fn is_folder(&self) -> bool {
        self.items
            .first()
            .is_some_and(|item| matches!(item.kind, TransferKind::Folder))
    }
    pub fn is_ready(&self) -> bool {
        self.sealed && self.validate().is_ok() && self.complete()
    }
    fn complete(&self) -> bool {
        self.items
            .iter()
            .enumerate()
            .all(|(index, item)| match item.kind {
                TransferKind::Folder => !self.contents.contains_key(&(index as u32)),
                TransferKind::File { size, .. } => self
                    .contents
                    .get(&(index as u32))
                    .is_some_and(|bytes| bytes.len() as u64 == size),
            })
            && self
                .contents
                .keys()
                .all(|index| (*index as usize) < self.items.len())
    }
    pub fn validate(&self) -> Result<(), SharingError> {
        if self.sender == self.recipient {
            return Err(SharingError::OwnName);
        }
        if self.items.len() > MAX_TRANSFER_ITEMS || self.total_size() > MAX_TRANSFER_BYTES {
            return Err(SharingError::TooLarge);
        }
        let items = self
            .items
            .iter()
            .map(|item| ImportItem {
                parent: item.parent,
                name: item.name.clone(),
                kind: match &item.kind {
                    TransferKind::Folder => EntryKind::Folder,
                    TransferKind::File { size, mime } => EntryKind::File {
                        blob: Id::default(),
                        size: *size,
                        mime: mime.clone(),
                    },
                },
            })
            .collect::<Vec<_>>();
        storage::validate_import_items(&items).map_err(|_| SharingError::InvalidSnapshot)
    }
}

pub fn open(air: &Air) {
    let _ = air.instances::<Transfer>();
}

pub fn send(
    air: &Air,
    vault: &Instance<Vault>,
    id: EntryId,
    recipient: Name,
) -> Result<Instance<Transfer>, SharingError> {
    if recipient == air.me() {
        return Err(SharingError::OwnName);
    }
    let snapshot = vault.pending();
    if snapshot.owner != air.me() {
        return Err(SharingError::Unauthorized);
    }
    if snapshot.get(id).is_none() {
        return Err(StorageError::MissingEntry.into());
    }
    if snapshot.is_trashed(id) {
        return Err(StorageError::InTrash.into());
    }
    let mut items = Vec::new();
    let mut files = Vec::new();
    let mut queue = vec![(id, None, 1usize)];
    let mut total = 0u64;
    while let Some((id, parent, depth)) = queue.pop() {
        if items.len() >= MAX_TRANSFER_ITEMS || depth > MAX_TRANSFER_DEPTH {
            return Err(SharingError::TooLarge);
        }
        let entry = snapshot.get(id).ok_or(StorageError::MissingEntry)?;
        let index = items.len() as u32;
        let kind = match &entry.kind {
            EntryKind::Shared { .. } => return Err(StorageError::SharedMount.into()),
            EntryKind::Folder => {
                let children = snapshot.children(Some(id));
                for child in children.into_iter().rev() {
                    queue.push((child.id, Some(index), depth + 1));
                }
                TransferKind::Folder
            }
            EntryKind::File { size, mime, .. } => {
                total = total.checked_add(*size).ok_or(SharingError::TooLarge)?;
                if total > MAX_TRANSFER_BYTES {
                    return Err(SharingError::TooLarge);
                }
                files.push((index, entry.clone()));
                TransferKind::File {
                    size: *size,
                    mime: mime.clone(),
                }
            }
        };
        items.push(TransferItem {
            parent,
            name: entry.name.clone(),
            kind,
        });
    }
    drop(snapshot);
    // Read all payloads before publishing anything. A missing cached file must
    // not leave an incomplete offer visible to the recipient.
    let payloads = files
        .into_iter()
        .map(|(index, entry)| storage::read_file(air, &entry).map(|bytes| (index, bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    let init = TransferInit {
        recipient,
        nonce: Id::random(),
        items,
    };
    Transfer::init(
        init.clone(),
        Metadata {
            signer: air.me(),
            timestamp: 0,
            confirmed: false,
        },
    )
    .validate()?;
    let mut transfer = air.instances::<Transfer>().create(init);
    // AIR sends one queued contract action per request; air_server writes each
    // request in its own WebSocket message. No upload exceeds the 8 MiB cap.
    for (index, bytes) in payloads {
        dispatch(&mut transfer, TransferAction::Upload { index, bytes })?;
    }
    dispatch(&mut transfer, TransferAction::Seal)?;
    transfer.share(recipient);
    Ok(transfer)
}

pub fn accept(
    air: &Air,
    vault: &mut Instance<Vault>,
    transfer: &mut Instance<Transfer>,
) -> Result<EntryId, SharingError> {
    let snapshot = transfer.pending();
    snapshot.validate()?;
    if snapshot.recipient != air.me() || vault.pending().owner != air.me() {
        return Err(SharingError::Unauthorized);
    }
    let root = storage::imported_id(transfer.id(), 0);
    if snapshot.status == TransferStatus::Accepted {
        return Ok(root);
    }
    if snapshot.status != TransferStatus::Pending {
        return Err(SharingError::Closed);
    }
    if !snapshot.is_ready() {
        return Err(SharingError::NotReady);
    }
    let mut items = Vec::with_capacity(snapshot.items.len());
    for (index, item) in snapshot.items.iter().enumerate() {
        let kind = match &item.kind {
            TransferKind::Folder => EntryKind::Folder,
            TransferKind::File { size, .. } => {
                let bytes = snapshot
                    .contents
                    .get(&(index as u32))
                    .ok_or(SharingError::NotReady)?;
                let owned = air.instances::<FileBlob>().create(bytes.clone());
                EntryKind::File {
                    blob: owned.id(),
                    size: *size,
                    mime: storage::mime_for_name(&item.name).into(),
                }
            }
        };
        items.push(ImportItem {
            parent: item.parent,
            name: item.name.clone(),
            kind,
        });
    }
    drop(snapshot);
    // A single validated action makes the whole tree visible atomically.
    // Deterministic IDs make retry after a crash before Accept idempotent.
    let root = storage::import_snapshot(vault, transfer.id(), items)?;
    dispatch(transfer, TransferAction::Accept)?;
    Ok(root)
}

pub fn decline(air: &Air, transfer: &mut Instance<Transfer>) -> Result<(), SharingError> {
    let snapshot = transfer.pending();
    if snapshot.recipient != air.me() {
        return Err(SharingError::Unauthorized);
    }
    snapshot.validate()?;
    if snapshot.status == TransferStatus::Accepted {
        return Err(SharingError::Closed);
    }
    drop(snapshot);
    dispatch(transfer, TransferAction::Decline)
}

fn dispatch(transfer: &mut Instance<Transfer>, action: TransferAction) -> Result<(), SharingError> {
    let result = transfer.send(action);
    result.pending().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::{TestServer, ready};

    fn meta(signer: Name) -> Metadata {
        Metadata {
            signer,
            timestamp: 1_000_000_000,
            confirmed: true,
        }
    }
    fn tree() -> Vec<TransferItem> {
        vec![
            TransferItem {
                parent: None,
                name: "Project".into(),
                kind: TransferKind::Folder,
            },
            TransferItem {
                parent: Some(0),
                name: "Empty folder".into(),
                kind: TransferKind::Folder,
            },
            TransferItem {
                parent: Some(0),
                name: "hello.txt".into(),
                kind: TransferKind::File {
                    size: 5,
                    mime: "text/plain".into(),
                },
            },
        ]
    }
    fn fixture() -> (Transfer, Name, Name) {
        let sender = air::Secret::new().name();
        let recipient = air::Secret::new().name();
        let transfer = Transfer::init(
            TransferInit {
                recipient,
                nonce: Id::random(),
                items: tree(),
            },
            meta(sender),
        );
        (transfer, sender, recipient)
    }

    #[test]
    fn only_sender_can_upload_and_only_recipient_can_accept_a_sealed_snapshot() {
        let (mut transfer, sender, recipient) = fixture();
        let outsider = air::Secret::new().name();
        assert_eq!(
            transfer.apply(
                TransferAction::Upload {
                    index: 2,
                    bytes: b"hello".to_vec()
                },
                meta(recipient)
            ),
            Err(SharingError::Unauthorized)
        );
        assert_eq!(
            transfer.apply(TransferAction::Seal, meta(sender)),
            Err(SharingError::NotReady)
        );
        assert_eq!(
            transfer.apply(TransferAction::Accept, meta(recipient)),
            Err(SharingError::NotReady)
        );
        transfer
            .apply(
                TransferAction::Upload {
                    index: 2,
                    bytes: b"hello".to_vec(),
                },
                meta(sender),
            )
            .unwrap();
        assert_eq!(
            transfer.apply(
                TransferAction::Upload {
                    index: 2,
                    bytes: b"other".to_vec()
                },
                meta(sender)
            ),
            Err(SharingError::ContentMismatch)
        );
        transfer.apply(TransferAction::Seal, meta(sender)).unwrap();
        assert!(transfer.is_ready());
        assert_eq!(
            transfer.apply(
                TransferAction::Upload {
                    index: 2,
                    bytes: b"other".to_vec()
                },
                meta(sender)
            ),
            Err(SharingError::Closed)
        );
        assert_eq!(
            transfer.apply(TransferAction::Accept, meta(sender)),
            Err(SharingError::Unauthorized)
        );
        assert_eq!(
            transfer.apply(TransferAction::Accept, meta(outsider)),
            Err(SharingError::Unauthorized)
        );
        assert_eq!(
            transfer.apply(TransferAction::Decline, meta(outsider)),
            Err(SharingError::Unauthorized)
        );
        transfer
            .apply(TransferAction::Accept, meta(recipient))
            .unwrap();
        transfer
            .apply(TransferAction::Accept, meta(recipient))
            .unwrap();
        assert_eq!(transfer.status, TransferStatus::Accepted);
        assert_eq!(
            transfer.apply(TransferAction::Decline, meta(recipient)),
            Err(SharingError::Closed)
        );
        assert_eq!(transfer.contents.get(&2).unwrap(), b"hello");
    }

    #[test]
    fn malformed_and_over_limit_snapshots_never_become_importable() {
        let (original, sender, recipient) = fixture();
        let mut mutations = Vec::new();
        let mut items = tree();
        items[1].parent = Some(1);
        mutations.push(items);
        let mut items = tree();
        items[1].parent = None;
        mutations.push(items);
        let mut items = tree();
        items[2].parent = Some(2);
        mutations.push(items);
        let mut items = tree();
        items[2].name = "../escape.txt".into();
        mutations.push(items);
        let mut items = tree();
        items[2].name = "EMPTY FOLDER".into();
        mutations.push(items);
        let mut items = tree();
        items[2].kind = TransferKind::File {
            size: storage::MAX_FILE_SIZE as u64 + 1,
            mime: "text/plain".into(),
        };
        mutations.push(items);
        let mut deep = Vec::new();
        for index in 0..=MAX_TRANSFER_DEPTH {
            deep.push(TransferItem {
                parent: (index > 0).then(|| index as u32 - 1),
                name: format!("folder{index}"),
                kind: TransferKind::Folder,
            });
        }
        mutations.push(deep);
        let mut many = vec![tree()[0].clone()];
        for index in 0..MAX_TRANSFER_ITEMS {
            many.push(TransferItem {
                parent: Some(0),
                name: format!("folder{index}"),
                kind: TransferKind::Folder,
            });
        }
        mutations.push(many);
        for items in mutations {
            let mut invalid = Transfer {
                items,
                ..original.clone()
            };
            assert!(invalid.validate().is_err());
            assert!(invalid.apply(TransferAction::Seal, meta(sender)).is_err());
            assert!(
                invalid
                    .apply(TransferAction::Accept, meta(recipient))
                    .is_err()
            );
        }
        let self_transfer = Transfer {
            recipient: sender,
            ..original
        };
        assert_eq!(self_transfer.validate(), Err(SharingError::OwnName));
    }

    #[test]
    fn empty_folders_transfer_and_decline_are_supported_without_file_payloads() {
        let (mut transfer, sender, recipient) = fixture();
        transfer.items.truncate(2);
        transfer.apply(TransferAction::Seal, meta(sender)).unwrap();
        assert!(transfer.is_ready());
        assert_eq!(transfer.total_size(), 0);
        assert_eq!(transfer.item_count(), 2);
        transfer
            .apply(TransferAction::Decline, meta(recipient))
            .unwrap();
        transfer
            .apply(TransferAction::Decline, meta(recipient))
            .unwrap();
        assert_eq!(
            transfer.apply(TransferAction::Accept, meta(recipient)),
            Err(SharingError::Closed)
        );
    }

    #[test]
    fn encrypted_inbox_transfer_restores_offline_and_imports_recipient_owned_bytes() {
        let alice = air::Secret::new();
        let bob = air::Secret::new();
        let mut sender_catalog = air::Air::new(alice.clone()).unwrap();
        let mut source_vault = sender_catalog.create::<Vault>(()).unwrap();
        let source_blob = sender_catalog
            .create::<FileBlob>(b"hello".to_vec())
            .unwrap();
        source_vault
            .send(storage::VaultAction::Create {
                id: Id::random(),
                parent: None,
                name: "Private original".into(),
                kind: EntryKind::File {
                    blob: source_blob.id(),
                    size: 5,
                    mime: "text/plain".into(),
                },
            })
            .unwrap();
        let mut outgoing = sender_catalog
            .create::<Transfer>(TransferInit {
                recipient: bob.name(),
                nonce: Id::random(),
                items: tree(),
            })
            .unwrap();
        outgoing
            .send(TransferAction::Upload {
                index: 2,
                bytes: b"hello".to_vec(),
            })
            .unwrap();
        outgoing.send(TransferAction::Seal).unwrap();
        let mut resolver = air::DefaultResolver::default();
        ready(sender_catalog.share(&mut resolver, bob.name(), *outgoing.location()));

        // Persist while fully offline: both the inbox invitation and all file
        // bytes must survive restart before any request reaches the server.
        let mut sender_catalog: air::Air =
            postcard::from_bytes(&postcard::to_allocvec(&sender_catalog).unwrap()).unwrap();
        let mut outgoing: air::Instance<Transfer> =
            postcard::from_bytes(&postcard::to_allocvec(&outgoing).unwrap()).unwrap();
        let mut server = TestServer::new();
        server.sync_catalog(&mut sender_catalog);
        server.sync(&mut outgoing);

        let mut receiver_catalog = air::Air::new(bob.clone()).unwrap();
        server.sync_catalog(&mut receiver_catalog);
        assert!(receiver_catalog.list(&Vault::id()).is_empty());
        assert!(receiver_catalog.list(&FileBlob::id()).is_empty());
        let locations = receiver_catalog.list(&Transfer::id());
        assert_eq!(locations.len(), 1);
        let location = *locations.iter().next().unwrap();
        assert_eq!(location, *outgoing.location());
        let mut incoming = air::Instance::<Transfer>::receive(bob.clone(), location).unwrap();
        server.sync(&mut incoming);
        assert!(incoming.pending().unwrap().is_ready());
        assert_eq!(incoming.pending().unwrap().sender, alice.name());
        assert_eq!(
            incoming.pending().unwrap().contents.get(&2).unwrap(),
            b"hello"
        );

        // Accept creates fresh blobs under Bob's identity and one atomic tree.
        let mut owned_blob = receiver_catalog
            .create::<FileBlob>(incoming.pending().unwrap().contents[&2].clone())
            .unwrap();
        assert_ne!(owned_blob.id(), source_blob.id());
        assert_eq!(owned_blob.pending().unwrap().owner, bob.name());
        let mut recipient_vault = receiver_catalog.create::<Vault>(()).unwrap();
        let items = vec![
            ImportItem {
                parent: None,
                name: "Project".into(),
                kind: EntryKind::Folder,
            },
            ImportItem {
                parent: Some(0),
                name: "Empty folder".into(),
                kind: EntryKind::Folder,
            },
            ImportItem {
                parent: Some(0),
                name: "hello.txt".into(),
                kind: EntryKind::File {
                    blob: owned_blob.id(),
                    size: 5,
                    mime: "text/plain".into(),
                },
            },
        ];
        let action = storage::VaultAction::ImportSnapshot {
            transfer: incoming.id(),
            items,
        };
        let root = recipient_vault.send(action.clone()).unwrap();
        assert_eq!(recipient_vault.send(action).unwrap(), root);
        incoming.send(TransferAction::Accept).unwrap();
        server.sync(&mut owned_blob);
        server.sync(&mut recipient_vault);
        server.sync(&mut incoming);
        server.sync(&mut outgoing);
        assert_eq!(outgoing.pending().unwrap().status, TransferStatus::Accepted);
        assert_eq!(recipient_vault.pending().unwrap().entries.len(), 3);
        assert_eq!(
            recipient_vault
                .pending()
                .unwrap()
                .children(Some(root))
                .len(),
            2
        );
        assert_eq!(owned_blob.confirmed().unwrap().bytes, b"hello");
        assert_eq!(source_vault.pending().unwrap().entries.len(), 1);
    }
}
