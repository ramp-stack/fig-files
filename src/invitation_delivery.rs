//! Durable, private work requested explicitly by creating or joining a file link.
//! UI rendering never publishes capabilities or accepts membership.
use crate::{
    collaboration::{self, SharedAction, SharedSpace},
    invitations::{FileInvitation, Invitation},
    storage,
};
use chk::Invitations;
use futures::FutureExt;
use prism::{
    air::{Air, Contract, Id, Instance, Instances, Metadata, Name},
    service::Service,
};
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Purpose {
    Publish,
    Join,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Status {
    Pending,
    Ready,
    Joined,
    Failed(String),
}

#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub(crate) struct LinkJob {
    pub(crate) owner: Name,
    pub(crate) invitation: FileInvitation,
    pub(crate) purpose: Purpose,
    pub(crate) status: Status,
}

#[derive(Clone, Debug, Hash, Serialize, Deserialize)]
pub(crate) enum JobAction {
    Retry,
    Complete(Result<(), String>),
}

impl Contract for LinkJob {
    type Init = (FileInvitation, Purpose);
    type Message = JobAction;
    type Result = Result<(), &'static str>;

    fn id() -> Id {
        Id::hash("org.ramp.fig-files.InvitationDelivery.v1")
    }

    fn init((invitation, purpose): Self::Init, metadata: Metadata) -> Self {
        Self {
            owner: metadata.signer,
            invitation,
            purpose,
            status: Status::Pending,
        }
    }

    fn apply(&mut self, action: JobAction, metadata: Metadata) -> Self::Result {
        if metadata.signer != self.owner {
            return Err("This invitation request belongs to another AirName.");
        }
        match action {
            JobAction::Retry if matches!(self.status, Status::Failed(_)) => {
                self.status = Status::Pending;
            }
            JobAction::Retry => {}
            JobAction::Complete(result) if self.status == Status::Pending => {
                self.status = match result {
                    Ok(()) if self.purpose == Purpose::Publish => Status::Ready,
                    Ok(()) => Status::Joined,
                    Err(error) => Status::Failed(error),
                };
            }
            JobAction::Complete(_) => {}
        }
        Ok(())
    }
}

/// Stable initialization preserves the same job through restarts and repeated
/// taps. Only an explicit request retries a recorded failure.
pub(crate) fn request(
    air: &Air,
    invitation: FileInvitation,
    purpose: Purpose,
) -> Instance<LinkJob> {
    let mut job = air.instances::<LinkJob>().create((invitation, purpose));
    let retry = matches!(job.pending().status, Status::Failed(_));
    if retry {
        job.send(JobAction::Retry);
    }
    air.start::<DeliverLink>(job.clone());
    job
}

/// Register cached jobs and resume requests that the user already authorized.
/// An empty catalog does not connect to the invitation transport.
pub(crate) fn start(air: &Air) {
    air.start::<LinkJobs>(());
}

/// Public within the crate so offline UI fixtures can allow this service key.
pub(crate) struct LinkJobs {
    jobs: Instances<LinkJob>,
    air: Air,
}

impl Service for LinkJobs {
    type Init = ();
    type Predicate = Instance<LinkJob>;

    async fn init(air: Air, _: ()) -> Self {
        let jobs = air.instances::<LinkJob>();
        for job in jobs.list().into_values() {
            if job.pending().owner == air.me() {
                air.start::<DeliverLink>(job);
            }
        }
        Self { jobs, air }
    }

    async fn predicate(&mut self) -> Self::Predicate {
        self.jobs.next().await
    }

    async fn handle(&mut self, job: Self::Predicate) {
        if job.pending().owner == self.air.me() {
            self.air.start::<DeliverLink>(job);
        }
    }
}

/// One keyed worker per persisted job prevents duplicate in-flight publication
/// or joins while leaving independent invitations free to complete separately.
pub(crate) struct DeliverLink {
    air: Air,
    job: Instance<LinkJob>,
}

impl Service for DeliverLink {
    type Init = Instance<LinkJob>;
    type Predicate = (FileInvitation, Purpose);

    async fn init(air: Air, job: Self::Init) -> Self {
        Self { air, job }
    }

    async fn predicate(&mut self) -> Self::Predicate {
        loop {
            {
                let state = self.job.pending();
                if state.owner == self.air.me() && state.status == Status::Pending {
                    return (state.invitation, state.purpose);
                }
            }
            self.job.next().await;
        }
    }

    async fn handle(&mut self, (invitation, purpose): Self::Predicate) {
        let result = AssertUnwindSafe(deliver(self.air.clone(), invitation, purpose))
            .catch_unwind()
            .await
            .unwrap_or_else(
                |_| Err("Could not complete this invitation. Please try again.".into()),
            );
        self.job.send(JobAction::Complete(result));
    }
}

async fn deliver(air: Air, invitation: FileInvitation, purpose: Purpose) -> Result<(), String> {
    match purpose {
        Purpose::Publish => {
            if invitation.publisher != air.me() {
                return Err("Create this invitation using your own AirName.".into());
            }
            let mut space =
                collaboration::get(&air, invitation.space).map_err(|error| error.to_string())?;
            wait_until_publishable(&mut space, invitation).await?;
            Invitations::<Invitation>::publish(space, invitation.publisher, invitation.token).await;
        }
        Purpose::Join => {
            let mut space = Invitations::<Invitation>::receive_contract::<SharedSpace>(
                air.clone(),
                invitation.publisher,
                invitation.token,
                invitation.space,
            )
            .await;
            // A cached or partially replayed ready state may predate EnableLink.
            // Queue the signed action even when its optimistic result rejects
            // the token; AIR replays intervening records before confirming it.
            while space.confirmed().is_none() {
                space.next().await;
            }
            space
                .send(SharedAction::JoinLink(invitation.token))
                .confirmed()
                .await
                .map_err(|error| error.to_string())?;
            let (title, folder) = {
                let confirmed = space
                    .confirmed()
                    .ok_or("The shared item is still syncing.")?;
                mount_details(&confirmed, invitation, air.me())?
            };
            let mut vault = storage::open(&air);
            storage::mount_shared(&mut vault, space.id(), &title, folder)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

async fn wait_until_publishable(
    space: &mut Instance<SharedSpace>,
    invitation: FileInvitation,
) -> Result<(), String> {
    loop {
        let ready = {
            let confirmed = space.confirmed();
            let pending = space.pending();
            publication_ready(confirmed.as_deref(), &pending, invitation)?
        };
        if ready {
            return Ok(());
        }
        space.next().await;
    }
}

/// Pending publication waits for its queued EnableLink/Activate actions.
fn publication_ready(
    confirmed: Option<&SharedSpace>,
    pending: &SharedSpace,
    invitation: FileInvitation,
) -> Result<bool, String> {
    pending.validate().map_err(|error| error.to_string())?;
    let Some(confirmed) = confirmed else {
        return Ok(false);
    };
    confirmed.validate().map_err(|error| error.to_string())?;
    if confirmed.permits_link(invitation.token) && confirmed.can_write(invitation.publisher) {
        return Ok(true);
    }
    if pending.permits_link(invitation.token) && pending.can_write(invitation.publisher) {
        return Ok(false);
    }
    if !confirmed.is_ready() {
        return Ok(false);
    }
    Err("This invitation is no longer available.".into())
}

/// Called only after the join action's authoritative confirmed success. Neither
/// optimistic membership nor possession of a channel alone may create a mount.
fn mount_details(
    confirmed: &SharedSpace,
    invitation: FileInvitation,
    me: Name,
) -> Result<(String, bool), String> {
    confirmed.validate().map_err(|error| error.to_string())?;
    if !confirmed.permits_link(invitation.token)
        || !confirmed.can_write(me)
        || !confirmed.can_write(invitation.publisher)
    {
        return Err("Joining this shared item was not confirmed.".into());
    }
    Ok((confirmed.title().to_owned(), confirmed.is_folder()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        collaboration::{MemberStatus, SharedInit},
        storage::{Entry, EntryKind, tests::TestServer},
    };

    fn metadata(signer: Name) -> Metadata {
        Metadata {
            signer,
            timestamp: 1_000_000_000,
            confirmed: true,
        }
    }

    fn invitation(publisher: Name) -> FileInvitation {
        FileInvitation {
            publisher,
            space: Id::random(),
            token: Id::random(),
        }
    }

    fn shared_folder(owner: Name) -> SharedSpace {
        let root = Id::random();
        let mut space = SharedSpace::init(
            SharedInit {
                nonce: Id::random(),
                root,
                items: vec![Entry {
                    id: root,
                    parent: None,
                    name: "Documents".into(),
                    kind: EntryKind::Folder,
                    favorite: false,
                    created_at: 1,
                    modified_at: 1,
                    trashed_at: None,
                }],
            },
            metadata(owner),
        );
        space
            .apply(SharedAction::Activate, metadata(owner))
            .unwrap();
        space
    }

    #[test]
    fn capability_publication_requires_confirmed_authority() {
        let owner = air::Secret::new().name();
        let invitation = invitation(owner);
        let confirmed = shared_folder(owner);
        let mut pending = confirmed.clone();
        pending
            .apply(SharedAction::EnableLink(invitation.token), metadata(owner))
            .unwrap();
        assert_eq!(publication_ready(None, &pending, invitation), Ok(false));
        assert_eq!(
            publication_ready(Some(&confirmed), &pending, invitation),
            Ok(false),
            "A queued EnableLink must wait for confirmation before mailbox publication"
        );
        assert!(
            publication_ready(Some(&confirmed), &confirmed, invitation).is_err(),
            "An absent or rejected token must fail instead of waiting forever"
        );
        assert_eq!(
            publication_ready(Some(&pending), &pending, invitation),
            Ok(true)
        );
        let mut inactive = pending.clone();
        inactive.members.insert(owner, MemberStatus::Declined);
        assert!(publication_ready(Some(&inactive), &inactive, invitation).is_err());
    }

    #[test]
    fn stale_ready_receivers_queue_join_and_mount_only_after_authoritative_acceptance() {
        let owner = air::Secret::new();
        let recipient = air::Secret::new();
        let state = shared_folder(owner.name());
        let mut sender = air::Instance::<SharedSpace>::new(
            owner.clone(),
            SharedInit {
                nonce: Id::random(),
                root: state.root,
                items: state.tree.entries.values().cloned().collect(),
            },
        )
        .unwrap();
        sender.send(SharedAction::Activate).unwrap();
        let mut server = TestServer::new();
        server.sync(&mut sender);
        let mut receiver =
            air::Instance::<SharedSpace>::receive(recipient.clone(), *sender.location()).unwrap();
        server.sync(&mut receiver);
        let invitation = FileInvitation {
            publisher: owner.name(),
            space: sender.id(),
            token: Id::random(),
        };
        sender
            .send(SharedAction::EnableLink(invitation.token))
            .unwrap();
        server.sync(&mut sender);
        assert!(receiver.confirmed().unwrap().is_ready());
        assert!(!receiver.confirmed().unwrap().permits_link(invitation.token));
        assert!(
            mount_details(receiver.confirmed().unwrap(), invitation, recipient.name()).is_err()
        );

        // The local snapshot rejects this optimistic action, but AIR retains
        // the signed request and re-evaluates it after the missing link event.
        assert!(
            receiver
                .send(SharedAction::JoinLink(invitation.token))
                .is_err()
        );
        assert!(
            mount_details(receiver.confirmed().unwrap(), invitation, recipient.name()).is_err()
        );
        server.sync(&mut receiver);
        assert_eq!(
            mount_details(receiver.confirmed().unwrap(), invitation, recipient.name()),
            Ok(("Documents".into(), true)),
        );
        let invalid = FileInvitation {
            token: Id::random(),
            ..invitation
        };
        assert!(mount_details(receiver.confirmed().unwrap(), invalid, recipient.name()).is_err());
        let spoofed = FileInvitation {
            publisher: air::Secret::new().name(),
            ..invitation
        };
        assert!(mount_details(receiver.confirmed().unwrap(), spoofed, recipient.name()).is_err());
    }

    #[test]
    fn only_the_owner_can_complete_or_retry_a_private_link_job() {
        let owner = air::Secret::new().name();
        let other = air::Secret::new().name();
        let mut job = LinkJob::init((invitation(owner), Purpose::Publish), metadata(owner));
        assert!(
            job.apply(JobAction::Complete(Ok(())), metadata(other))
                .is_err()
        );
        assert_eq!(job.status, Status::Pending);
        job.apply(JobAction::Complete(Err("Offline".into())), metadata(owner))
            .unwrap();
        assert!(job.apply(JobAction::Retry, metadata(other)).is_err());
        assert_eq!(job.status, Status::Failed("Offline".into()));
        job.apply(JobAction::Retry, metadata(owner)).unwrap();
        assert_eq!(job.status, Status::Pending);
    }

    #[test]
    fn completed_jobs_are_reused_and_failed_jobs_require_explicit_retry() {
        let owner = air::Secret::new().name();
        for (purpose, completed) in [
            (Purpose::Publish, Status::Ready),
            (Purpose::Join, Status::Joined),
        ] {
            let mut job = LinkJob::init((invitation(owner), purpose), metadata(owner));
            job.apply(JobAction::Complete(Err("Offline".into())), metadata(owner))
                .unwrap();
            job.apply(JobAction::Complete(Ok(())), metadata(owner))
                .unwrap();
            assert_eq!(job.status, Status::Failed("Offline".into()));
            job.apply(JobAction::Retry, metadata(owner)).unwrap();
            job.apply(JobAction::Complete(Ok(())), metadata(owner))
                .unwrap();
            assert_eq!(job.status, completed);
            job.apply(JobAction::Retry, metadata(owner)).unwrap();
            assert_eq!(job.status, completed);
        }
    }

    #[test]
    fn retry_history_survives_serialization_and_debug_does_not_expose_the_link_token() {
        let owner = air::Secret::new().name();
        let invitation = invitation(owner);
        let mut job = LinkJob::init((invitation, Purpose::Join), metadata(owner));
        job.apply(JobAction::Complete(Err("Offline".into())), metadata(owner))
            .unwrap();
        let bytes = postcard::to_allocvec(&job).unwrap();
        let restored: LinkJob = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.owner, owner);
        assert_eq!(restored.invitation, invitation);
        assert_eq!(restored.status, Status::Failed("Offline".into()));
        assert!(!format!("{restored:?}").contains(&invitation.token.to_string()));
        assert!(
            !format!("{:?}", (invitation, Purpose::Join)).contains(&invitation.token.to_string())
        );
    }
}
