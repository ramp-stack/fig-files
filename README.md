# Fig Files

A native, AIR-backed file manager built by adapting Orange's Rust / Cargo Piston application skeleton. All application UI is declared through CHK's public app, page, form, display, and theme APIs.

## Run

Clone a compatible CHK 2 checkout beside Fig Files: the app uses the local `../chk` dependency, including CHK’s contact/profile pages and keyed-form APIs. The current API baseline is commit `1b05d0990d0c212f5b9f8f4f1948c7e2a1fa7163` on `master`. With Rust and the native prerequisites for your platform installed:

```sh
git clone --branch master https://github.com/ramp-stack/chk.git
git -C chk checkout 1b05d0990d0c212f5b9f8f4f1948c7e2a1fa7163
git clone https://github.com/ramp-stack/fig-files.git
cd fig-files
```

Resolve dependencies once for the checkout, then run with the lockfile:

```sh
cargo build
cargo run --locked
```

The RAMP development workspace may override Ramp, Prism, and native/UI dependencies with sibling paths through a parent Cargo configuration. Current workspace checks also include local CHK changes; a standalone build from the pinned commit has not been verified. Switching between workspace overrides and standalone dependencies requires refreshing `Cargo.lock` with `cargo build` before using `--locked` again.

For platform packaging and native development:

```sh
cargo piston dev macos
cargo piston dev ios
```

For a local macOS development bundle with the app icon, run `bash scripts/package-macos.sh`. It creates `target/macos/Fig Files.app` with an ad-hoc signature for local use. Distribution signing remains a Cargo Piston workflow.

iOS development requires Xcode, a configured Cargo Piston developer identity, and access to the signing team in `Cargo.toml`. The iOS bundle identifier is `org.ramp.figfiles`; the macOS bundle identifier is `org.ramp.fig-files`. Platform and public notification registration settings are configured in the manifest; signing credentials are managed separately by Cargo Piston. Running the app does not deploy or upload a build.

## Files and folders

- Browse Files, Shared, Contacts, Trash, and Profile. Recent and Favorites are no longer navigation pages.
- Use the **Add files** bumper in Files or a folder to import files or create a folder. Imports are limited to **8 MiB each**. Search results are a browsing view without import controls.
- Search file and folder names with the header search button. From Files, search covers the library; from a folder, it includes only that folder’s contents and descendants, including shared folders inside it. Results update as you type, and tapping a result opens it directly, matching CHK’s Contacts and Messages search. Contacts also search by name or AirName without a Continue step. Library item and byte totals appear below the listing.
- Opening a file automatically previews raster images, SVGs, PDFs (macOS/iOS), and UTF-8 text, with file artwork while the preview loads. Text previews are limited to 16,000 characters. Use **Info → PDF pages** for Previous, Next, or jumping to a page number.
- Rename, move, trash, and restore files or complete folder trees. Renaming uses CHK’s edit page with **Save**, matching group editing in Orange, including validation and unsaved-change handling.
- The preview header’s **Info** button opens file details. Files and folders both use CHK `Display::cta()` actions for **Share**, **Move**, and **Delete**, with no bumper and an **Edit** header button that opens the rename editor. File details also offer **Export file**; shared files add **File contents**. Shared-item pages do not display participant lists or counts. Delete confirms before moving items to Trash. Moving or deleting a shared root affects only your Files shortcut; moving or deleting a nested shared item applies to everyone.
- Use **Info → Export file** on a file, to hand the original bytes to the system share sheet. Available destinations depend on the platform; iOS includes Save to Files. This is not a native Save As dialog.
- Use **Send** on the Shared page to choose a file or folder, or use **Share** from a file’s Info page. Everyone who joins gets read and write access to the same item. Shared items remain accessible through shortcuts in Files.
- Permanent deletion removes an item and its descendants from the library. AIR may retain earlier encrypted history; this is not secure erasure.

The app starts with an empty library; it does not contain seeded demo files.

SVG previews support vector paths, text, gradients, and supported embedded raster images. Linked files/URLs, nested SVG images, and unusually complex documents are rejected with an export option. PDF previews use the native Apple renderer, preserving page orientation and crop boxes; password-protected PDFs require export to a PDF app. Other platforms can export PDFs. Preview images are bounded to 1600 pixels on their longest side and never replace the stored originals.

## Shared access

1. Open **Shared → Receive** to paste or scan an invitation link, or choose **Show my QR code** to display and share your AirName invitation. To copy the literal AirName, open **Profile → AirName → Copy**.
2. The sender opens **Shared → Send**, chooses a file or folder, searches and selects one or more contacts, and presses **Continue** to share read/write access. **New contact** opens the paste/QR page, then returns with that person selected.
3. The recipient opens the invitation in **Shared** and chooses **Join shared file/folder**. A shortcut appears in Files. Both people now use the same shared contents.

Shared presents one list of files and folders, whether you shared them or received access. **Send** and **Receive** are the main buttons. **Receive** uses CHK’s invitation text form with **Paste**, **Scan QR Code**, and **Show my QR code** actions. The information button contains private saved edits and older copy-based transfers when present.

Sharing from file or folder details, including inviting someone to an existing shared item, reuses CHK’s new-message contact selector and built-in **New contact** form. Fig Files changes the outer title to **Share access** and the final action to file sharing. The selector includes saved profiles and people from existing sharing history. When there are no contacts, a CHK action page offers **New contact** and **Create link** without an empty-list message; saving a contact opens the usual recipient selector. Selected people appear as removable chips, and adding someone keeps existing selections. Selecting or saving a contact does not grant access; **Continue** sends the invitations to the selected people for the same shared item.

**Create link** in that selector is a separate choice. Before enabling a link, Fig Files explains that **anyone with the link can read and edit the file or entire folder, and forward the link**. Links do not expire and existing access cannot be revoked. Only confirming **Create link** enables this access. The link is made available to copy or share after CHK confirms its publication. Selecting contacts and pressing Continue does not enable link access.

Invitation links use `https://therampstack.com/fig-files/invite.html`. File-link capabilities stay in the URL fragment; the website does not receive file contents or tokens. Opening a file link shows **Join shared item** before receiving the shared channel or changing membership. Confirmed membership is required before adding the Files shortcut. Pending publication/join jobs are saved privately and resume after restart; failed jobs have an explicit retry action.

Sharing an **AirName** from Receive or Profile uses a contact invitation link. Opening it saves the contact privately and shows their Fig Files profile; it does not create a chat or share files. The CHK contact forms accept both plain AirNames and these links. The Receive QR encodes the contact invitation link.

iOS handles invitation links through CHK’s native app-link queue, with the custom `fig-files://` scheme as a website fallback. Universal links require the associated-domain entitlement in the installed signed build and the live Apple association file. The current native macOS runtime does not deliver app links; on Mac, use **Shared → Receive** to paste or scan. No shared runtime crate is modified. Website deployment files are in [web/](web/README.md).

**Contacts** reuses CHK’s Contacts home, with its **Search** button, **New Contact** action, and empty state: “No contacts yet. Get started by adding a friend.” **New Contact** opens CHK’s form to paste or scan an AirName and save a private profile. Contact pages retain CHK’s username, avatar, **About me**, and **AirName** controls, including **Copy** and **Display QR Code**. Fig Files adds **Share files** and the existing **Sharing history**. Choose a file or folder from **Share files**, then confirm **Share access** for that contact. Opening a contact or choosing a file never grants access.

**Profile** opens CHK’s **My profile** editor for your username, avatar, and **About me**. Its **AirName** section provides **Copy**, **Display QR Code**, and **Share**.

New contacts are stored in CHK’s private `Profile` contracts. Existing Fig Files address-book entries and contacts from outgoing or accepted sharing history are imported into that store without replacing saved usernames, notes, or avatars. Reopening Contacts does not duplicate existing profiles. Unaccepted incoming invitations do not add unfamiliar people to Contacts.

Sharing a folder includes its active files, subfolders, and future additions. Everyone who joins can import files, create folders, rename and move items within the shared folder, replace file contents, and use **Shared Trash** to remove or restore items for everyone. **Info → File contents → Edit text** edits complete UTF-8 files up to 256 KiB; **Replace file** accepts one updated file up to 8 MiB and keeps the existing filename. Successful text saves stay in the editor, and failed saves retain the unsaved draft. Previews and exports work for shared items too. This is file-version collaboration, not live co-editing of individual keystrokes, PDF pages, or images.

The owner's original library entry becomes a shortcut to the shared item; it does not remain a separately editable copy. Library search and totals include the current shared contents. **Files shortcut**, in the shared item's information page, controls personal placement and removal of the shortcut. Removing a shortcut does not delete the shared contents or revoke access. The shared root remains accessible from Shared.

Each accepted collaborator can invite another person to the **whole shared item**. A child inside a shared folder cannot be shared separately with narrower permissions. A private folder that already contains a shared shortcut cannot be promoted as another shared folder; share its existing shared item directly. There are no read-only roles or access-revocation controls. AIR shares the channel's decryption capability, so declining an invitation is not secure erasure or revocation of already delivered data.

A shared item supports **128 entries and saved conflicting edits**, **32 MiB total**, **8 MiB per file**, **128 participants including the owner**, and **32 levels** of nesting. Trash and saved conflicts count against those limits. New writes and structural changes are validated as atomic contract actions. A stale file replacement preserves its bytes under **Edits to review** while leaving the current version unchanged. Applying a saved edit keeps the displaced current version for review; explicitly discarding a reviewed edit frees its space. This does not automatically merge text.

Submitted replacement and text-edit bytes are also stored privately under **Shared → information button → Saved edits** before the shared write is queued. These recovery drafts survive restart and can be exported, retried, or explicitly discarded. A draft can remain even after its edit has synced; it is not an error or delivery receipt. If a concurrent change causes a queued write to be rejected at confirmation—for example because the folder is full—the recovery draft remains available. Retrying uses the original base version and preserves conflicts instead of overwriting later edits.

An AirName identifies an AIR public identity. Orange and Fig Files normally have different app-local identities. Use the AirName displayed by the recipient's Fig Files installation. Invitations arrive inside Fig Files; Orange is unchanged.

**Shared → information button → Older copies** keeps transfers created by earlier versions of Fig Files available. Those transfers remain snapshots with the original Save to Files behavior. They cannot retroactively become shared access. New invitations use shared access only.

## Storage and synchronization

The app's AIR contracts are defined within Fig Files:

- `Vault` holds the private library index and shared-item shortcuts. `FileBlob` holds immutable private originals.
- `SharedSpace` holds one canonical shared tree, mutable file bytes, membership, and conflicting versions. Only this channel is shared. The sender's other files, private index, and private Trash remain private.
- `EditDraft` retains private recovery submissions and is never shared.
- `LinkJob` retains private invitation publication/join work and is never shared.
- `AddressBook` retains previously saved AirNames for migration into CHK profiles.
- `Transfer` remains available for reading older copy-based exchanges.

Contacts and Profile use CHK’s private `Profile` contracts. Saving a contact does not send an invitation or grant access to files.

AIR authenticates every change. Only active participants can write or invite others; joining requires either an explicit membership invitation or a valid token from a deliberately enabled file link. Pending local changes appear immediately, and other participants see them after AIR synchronization. Offline updates are persisted for retry. Being queued locally is not confirmation that the server or another person has received a change.

Ramp supplies encrypted transport, local caching, and the persisted app identity. The local cache is not separately encrypted by Fig Files. This version has no account, identity recovery, or cross-device pairing UI; cloning one identity onto concurrent devices is unsupported. Initial promotion of a private subtree to a shared item spans two contracts and assumes one active installation of the private identity. Keep the app's cache and identity data to preserve library access.

Permanent deletion removes entries from the current index; encrypted AIR history can retain earlier data. Discarding a recovery draft hides it from recovery controls and does not securely erase history. Exports create uniquely named private temporary directories retained for the system share sheet and eventually cleaned by the operating system.

## Project scope

The duplicate retains Orange's native entrypoint, compatible dependency versions, and useful rendering build settings. Fig Files reuses CHK’s Contacts, profile, and contact-selection UI. Orange’s messaging screens, wallet draft, GIF configuration, old Xcode project, vendor tree, caches, credentials, and deployment metadata are excluded. The app depends directly on CHK for UI; Pelican UI and PTSD are CHK's internal transitive dependencies. It does not import them or alter their components or drawing instructions. Ramp/Prism provide the application lifecycle, AIR, and native file-picker events.

`icon.png` is the generated native icon master. Cargo Piston derives platform sizes from it. Runtime vector branding, empty states, and semantic icons are in `resources/`; iOS launch resources are in `ios/`. See [the asset inventory and exact generation prompt](design/ASSETS.md).

The bright icon, logo, illustrations, and full-color file artwork remain app-owned assets. `ChkTheme::Dark` supplies the lime accent through CHK's supported theme configuration. Controls and navigation use CHK's standard rendering; there is no custom drawing layer or lower-level theme override.

## Checks

```sh
cargo fmt --check
cargo build --locked --bin fig-files
cargo test --locked --lib
cargo check --locked --target aarch64-apple-ios --lib
node --test web/tests/invite.test.cjs
```

Tests cover folder constraints, signed collaborator permissions, shared mutations, conflict preservation, private draft recovery, shortcut promotion, byte preservation, limits, legacy transfers, and encrypted collaboration among three identities with offline restart and concurrent edits. Sharing pages are rendered with the real CHK interface at phone and desktop sizes. Build output lives inside this project's ignored `target/` directory.
