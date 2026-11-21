//! Fig Files' private address book. Adding a contact records an AirName;
//! it does not send an invitation or share any AIR channel with that person.
use prism::air::{Air, Contract, Id, Instance, Metadata, Name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressBook {
    pub owner: Name,
    /// First-added Unix seconds, supplied by AIR's signed message metadata.
    pub contacts: BTreeMap<Name, u64>,
}

#[derive(Clone, Copy, Debug, Hash, Serialize, Deserialize)]
pub enum ContactAction {
    Add(Name),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContactError {
    Unauthorized,
    OwnName,
    InvalidName,
}

impl fmt::Display for ContactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unauthorized => "This address book belongs to another identity.",
            Self::OwnName => "You cannot add yourself as a contact.",
            Self::InvalidName => "Enter a valid AirName.",
        })
    }
}
impl std::error::Error for ContactError {}

impl AddressBook {
    pub fn validate(&self) -> Result<(), ContactError> {
        if self.contacts.contains_key(&self.owner) {
            return Err(ContactError::OwnName);
        }
        Ok(())
    }
}

impl Contract for AddressBook {
    type Init = ();
    type Message = ContactAction;
    type Result = Result<Name, ContactError>;

    fn id() -> Id {
        Id::hash("org.ramp.fig-files.AddressBook.v1")
    }

    fn init(_: (), metadata: Metadata) -> Self {
        Self {
            owner: metadata.signer,
            contacts: BTreeMap::new(),
        }
    }

    fn apply(&mut self, message: ContactAction, metadata: Metadata) -> Self::Result {
        if metadata.signer != self.owner {
            return Err(ContactError::Unauthorized);
        }
        self.validate()?;
        match message {
            ContactAction::Add(name) => {
                if name == self.owner {
                    return Err(ContactError::OwnName);
                }
                self.contacts
                    .entry(name)
                    .or_insert(metadata.timestamp / 1_000_000_000);
                Ok(name)
            }
        }
    }
}

pub fn parse(text: &str, me: Name) -> Result<Name, ContactError> {
    let name = text
        .trim()
        .parse::<Name>()
        .map_err(|_| ContactError::InvalidName)?;
    if name == me {
        return Err(ContactError::OwnName);
    }
    Ok(name)
}

/// The unit Init gives this identity a stable address-book instance on relaunch.
/// Call on startup or explicit submission, never while rendering a watched page.
pub fn open(air: &Air) -> Instance<AddressBook> {
    air.instances::<AddressBook>().create(())
}

pub fn add(air: &Air, name: Name) -> Result<Name, ContactError> {
    if name == air.me() {
        return Err(ContactError::OwnName);
    }
    let mut book = open(air);
    {
        let state = book.pending();
        if state.owner != air.me() {
            return Err(ContactError::Unauthorized);
        }
        state.validate()?;
        if state.contacts.contains_key(&name) {
            return Ok(name);
        }
    }
    book.send(ContactAction::Add(name)).pending().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::TestServer;

    fn metadata(signer: Name, seconds: u64) -> Metadata {
        Metadata {
            signer,
            timestamp: seconds * 1_000_000_000,
            confirmed: true,
        }
    }

    #[test]
    fn only_the_owner_can_add_contacts_and_repeats_preserve_the_first_timestamp() {
        let owner = air::Secret::new().name();
        let peer = air::Secret::new().name();
        let stranger = air::Secret::new().name();
        let mut book = AddressBook::init((), metadata(owner, 1));
        assert_eq!(book.owner, owner);
        assert!(book.contacts.is_empty());
        assert_eq!(
            book.apply(ContactAction::Add(peer), metadata(stranger, 2)),
            Err(ContactError::Unauthorized)
        );
        assert_eq!(
            book.apply(ContactAction::Add(owner), metadata(owner, 3)),
            Err(ContactError::OwnName)
        );
        assert!(book.contacts.is_empty());
        assert_eq!(
            book.apply(ContactAction::Add(peer), metadata(owner, 4)),
            Ok(peer)
        );
        assert_eq!(
            book.apply(ContactAction::Add(peer), metadata(owner, 50)),
            Ok(peer)
        );
        assert_eq!(book.contacts, BTreeMap::from([(peer, 4)]));
        let unchanged = book.clone();
        assert_eq!(
            book.apply(ContactAction::Add(stranger), metadata(peer, 60)),
            Err(ContactError::Unauthorized)
        );
        assert_eq!(book, unchanged);
    }

    #[test]
    fn pasted_names_are_trimmed_and_invalid_or_own_names_are_rejected() {
        let owner = air::Secret::new().name();
        let peer = air::Secret::new().name();
        assert_eq!(parse(&format!(" \n{peer}\t "), owner), Ok(peer));
        assert_eq!(parse(&owner.to_string(), owner), Err(ContactError::OwnName));
        for invalid in [
            "",
            " ",
            "not a name",
            "https://example.com/name",
            "03",
            "00000000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(
                parse(invalid, owner),
                Err(ContactError::InvalidName),
                "{invalid}"
            );
        }
        let mut corrupted = AddressBook::init((), metadata(owner, 0));
        corrupted.contacts.insert(owner, 1);
        assert_eq!(corrupted.validate(), Err(ContactError::OwnName));
    }

    #[test]
    fn offline_contacts_survive_cache_restart_and_encrypted_server_replay_without_sharing() {
        let owner = air::Secret::new();
        let peer = air::Secret::new();
        let another = air::Secret::new();
        let mut catalog = air::Air::new(owner.clone()).unwrap();
        let mut book = catalog.create::<AddressBook>(()).unwrap();
        book.send(ContactAction::Add(peer.name())).unwrap();
        book.send(ContactAction::Add(another.name())).unwrap();
        let provisional_first_added = book.pending().unwrap().contacts[&peer.name()];
        book.send(ContactAction::Add(peer.name())).unwrap();
        // Include an in-flight encrypted request in the restart fixture.
        let _ = book.start();
        let mut book: air::Instance<AddressBook> =
            postcard::from_bytes(&postcard::to_allocvec(&book).unwrap()).unwrap();
        let mut catalog: air::Air =
            postcard::from_bytes(&postcard::to_allocvec(&catalog).unwrap()).unwrap();
        assert_eq!(book.pending().unwrap().contacts.len(), 2);
        assert_eq!(
            book.pending().unwrap().contacts[&peer.name()],
            provisional_first_added
        );
        assert!(catalog.list(&AddressBook::id()).contains(book.location()));

        let mut server = TestServer::new();
        server.sync_catalog(&mut catalog);
        server.sync(&mut book);
        assert_eq!(book.confirmed().unwrap().owner, owner.name());
        assert_eq!(book.confirmed().unwrap().contacts.len(), 2);
        // AIR's confirmed metadata replaces the provisional client timestamp.
        // Once confirmed, repeats and cold replay must preserve that first time.
        let confirmed_first_added = book.confirmed().unwrap().contacts[&peer.name()];
        book.send(ContactAction::Add(peer.name())).unwrap();
        server.sync(&mut book);
        assert_eq!(
            book.confirmed().unwrap().contacts[&peer.name()],
            confirmed_first_added
        );
        let mut restored =
            air::Instance::<AddressBook>::receive(owner.clone(), *book.location()).unwrap();
        assert!(restored.pending().is_none());
        server.sync(&mut restored);
        assert_eq!(restored.confirmed(), book.confirmed());
        assert_eq!(
            restored.confirmed().unwrap().contacts[&peer.name()],
            confirmed_first_added
        );

        // Saving names creates no inbox invitations for either contact.
        for secret in [peer.clone(), another] {
            let mut recipient = air::Air::new(secret).unwrap();
            server.sync_catalog(&mut recipient);
            assert!(recipient.list(&AddressBook::id()).is_empty());
        }
        // Even possession of the location cannot authorize another signer.
        let mut foreign = air::Instance::<AddressBook>::receive(peer, *book.location()).unwrap();
        server.sync(&mut foreign);
        assert_eq!(
            foreign.send(ContactAction::Add(owner.name())),
            Err(ContactError::Unauthorized)
        );
        server.sync(&mut foreign);
        server.sync(&mut restored);
        assert_eq!(restored.confirmed(), book.confirmed());
    }
}
