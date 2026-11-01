# Fig Files

A native, AIR-backed file manager built by adapting Orange's Rust / Cargo Piston application skeleton. All application UI is declared through CHK's public app, page, form, display, and theme APIs.

## Run

Clone CHK beside Fig Files: the app currently uses the local `../chk` dependency. With Rust and the native prerequisites for your platform installed:

```sh
git clone https://github.com/ramp-stack/chk.git
git clone https://github.com/ramp-stack/fig-files.git
cd fig-files
```

Then run:

```sh
cargo run --locked
```

For platform packaging and native development:

```sh
cargo piston dev macos
cargo piston dev ios
```

For a local macOS development bundle with the app icon, run `bash scripts/package-macos.sh`. It creates `target/macos/Fig Files.app` with an ad-hoc signature for local use. Distribution signing remains a Cargo Piston workflow.

iOS development requires Xcode, a configured Cargo Piston developer identity, and access to the signing team in `Cargo.toml`. The iOS bundle identifier is `org.ramp.figfiles`; the macOS bundle identifier is `org.ramp.fig-files`. Platform and public notification registration settings are configured in the manifest; signing credentials are managed separately by Cargo Piston. Running the app does not deploy or upload a build.

## Files and folders

- Browse Files, Shared, Contacts, and Trash. Recent and Favorites are no longer navigation pages.
- Use the **Add files** bumper in Files or a folder to import files or create a folder. Imports are limited to **8 MiB each**. Search results are a browsing view without import controls.
- Search file and folder names across the library with the header search button. Library item and byte totals appear below the listing.
- Preview raster images, SVGs, PDFs (macOS/iOS), and UTF-8 text. Text previews are limited to 16,000 characters. Multi-page PDFs have a page button in the preview header for Previous, Next, or jumping to a page number.
- Rename, move, trash, and restore files or complete folder trees.
- File details use **Preview** as the primary bumper. **Manage file** holds rename, move, and trash controls; folder details use the header manage button.
- Use the header share button on a file, then **Export file**, to hand the original bytes to the system share sheet. Available destinations depend on the platform; iOS includes Save to Files. This is not a native Save As dialog.
- Use **Send** on the Shared page to choose a file or folder, or use **Share access** from its details. Everyone who joins gets read and write access to the same item. Shared items remain accessible through shortcuts in Files.
- Permanent deletion removes an item and its descendants from the library. AIR may retain earlier encrypted history; this is not secure erasure.

The app starts with an empty library; it does not contain seeded demo files.

SVG previews support vector paths, text, gradients, and supported embedded raster images. Linked files/URLs, nested SVG images, and unusually complex documents are rejected with an export option. PDF previews use the native Apple renderer, preserving page orientation and crop boxes; password-protected PDFs require export to a PDF app. Other platforms can export PDFs. Preview images are bounded to 1600 pixels on their longest side and never replace the stored originals.

## Shared access

1. The recipient opens **Shared → Receive** and copies their receiving name or shows its QR code.
2. The sender opens **Shared → Send**, chooses a file or folder, pastes or scans the name, and confirms **Share access**.
3. The recipient opens the invitation in **Shared** and chooses **Join shared file/folder**. A shortcut appears in Files. Both people now use the same shared contents.

Shared presents one list of files and folders, whether you shared them or received access. **Send** and **Receive** are the main buttons. Private saved edits and older copy-based transfers are available from the information button when present.

**Contacts → Add contact** lets you paste or scan someone’s Fig Files receiving name and save it before sharing anything. Contacts also fills automatically from people in your sharing history, including older copies. Adding the same name again opens the existing contact. Select a person, tap **Share files**, choose a file or folder, and confirm **Share access**. Their receiving name is already selected. Saved contacts persist privately across restarts and appear alongside sharing-history contacts without duplicates. Contacts show the full receiving name and any shared items that identify the relationship; merely opening a contact or choosing a file never grants access. Unaccepted incoming invitations do not add unfamiliar people to Contacts.

Sharing a folder includes its active files, subfolders, and future additions. Everyone who joins can import files, create folders, rename and move items within the shared folder, replace file contents, and use **Shared Trash** to remove or restore items for everyone. **Manage file → Edit text** edits complete UTF-8 files up to 256 KiB; **Replace file** accepts one updated file up to 8 MiB and keeps the existing filename. Previews and exports work for shared items too. This is file-version collaboration, not live co-editing of individual keystrokes, PDF pages, or images.

The owner's original library entry becomes a shortcut to the shared item; it does not remain a separately editable copy. Library search and totals include the current shared contents. **Files shortcut**, in the shared item's information page, controls personal placement and removal of the shortcut. Removing a shortcut does not delete the shared contents or revoke access. The shared root remains accessible from Shared.

Each accepted collaborator can invite another person to the **whole shared item**. A child inside a shared folder cannot be shared separately with narrower permissions. A private folder that already contains a shared shortcut cannot be promoted as another shared folder; share its existing shared item directly. There are no read-only roles or access-revocation controls. AIR shares the channel's decryption capability, so declining an invitation is not secure erasure or revocation of already delivered data.

A shared item supports **128 entries and saved conflicting edits**, **32 MiB total**, **8 MiB per file**, **128 participants including the owner**, and **32 levels** of nesting. Trash and saved conflicts count against those limits. New writes and structural changes are validated as atomic contract actions. A stale file replacement preserves its bytes under **Edits to review** while leaving the current version unchanged. Applying a saved edit keeps the displaced current version for review; explicitly discarding a reviewed edit frees its space. This does not automatically merge text.

Submitted replacement and text-edit bytes are also stored privately under **Shared → information button → Saved edits** before the shared write is queued. These recovery drafts survive restart and can be exported, retried, or explicitly discarded. A draft can remain even after its edit has synced; it is not an error or delivery receipt. If a concurrent change causes a queued write to be rejected at confirmation—for example because the folder is full—the recovery draft remains available. Retrying uses the original base version and preserves conflicts instead of overwriting later edits.

Orange names are AIR public identities, not profile nicknames. Orange and Fig Files normally have different app-local identities. Use the receiving name displayed by the recipient's Fig Files installation. Invitations arrive inside Fig Files; Orange is unchanged.

**Shared → information button → Older copies** keeps transfers created by earlier versions of Fig Files available. Those transfers remain snapshots with the original Save to Files behavior. They cannot retroactively become shared access. New invitations use shared access only.

## Storage and synchronization

The app's AIR contracts are defined within Fig Files:

- `Vault` holds the private library index and shared-item shortcuts. `FileBlob` holds immutable private originals.
- `SharedSpace` holds one canonical shared tree, mutable file bytes, membership, and conflicting versions. Only this channel is shared. The sender's other files, private index, and private Trash remain private.
- `EditDraft` retains private recovery submissions and is never shared.
- `AddressBook` privately stores receiving names added from Contacts; adding a name sends no invitation or shared access.
- `Transfer` remains available for reading older copy-based exchanges.

AIR authenticates every change. Only active, explicitly invited participants can write or invite others. Pending local changes appear immediately, and other participants see them after AIR synchronization. Offline updates are persisted for retry. Being queued locally is not confirmation that the server or another person has received a change.

Ramp supplies encrypted transport, local caching, and the persisted app identity. The local cache is not separately encrypted by Fig Files. This version has no account, identity recovery, or cross-device pairing UI; cloning one identity onto concurrent devices is unsupported. Initial promotion of a private subtree to a shared item spans two contracts and assumes one active installation of the private identity. Keep the app's cache and identity data to preserve library access.

Permanent deletion removes entries from the current index; encrypted AIR history can retain earlier data. Discarding a recovery draft hides it from recovery controls and does not securely erase history. Exports create uniquely named private temporary directories retained for the system share sheet and eventually cleaned by the operating system.

## Project scope

The duplicate retains Orange's native entrypoint, compatible dependency versions, and useful rendering build settings. Orange messaging, contacts, profiles, wallet draft, GIF configuration, old Xcode project, vendor tree, caches, credentials, and deployment metadata are excluded. The app depends directly on CHK for UI; Pelican UI and PTSD are CHK's internal transitive dependencies. It does not import them or alter their components or drawing instructions. Ramp/Prism provide the application lifecycle, AIR, and native file-picker events.

`icon.png` is the generated native icon master. Cargo Piston derives platform sizes from it. Runtime vector branding, empty states, and semantic icons are in `resources/`; iOS launch resources are in `ios/`. See [the asset inventory and exact generation prompt](design/ASSETS.md).

The bright icon, logo, illustrations, and full-color file artwork remain app-owned assets. `ChkTheme::Dark` supplies the lime accent through CHK's supported theme configuration. Controls and navigation use CHK's standard rendering; there is no custom drawing layer or lower-level theme override.

## Checks

```sh
cargo fmt --check
cargo check --locked
cargo test --locked --lib
```

Tests cover folder constraints, signed collaborator permissions, shared mutations, conflict preservation, private draft recovery, shortcut promotion, byte preservation, limits, legacy transfers, and encrypted collaboration among three identities with offline restart and concurrent edits. Sharing pages are rendered with the real CHK interface at phone and desktop sizes. Build output lives inside this project's ignored `target/` directory.
