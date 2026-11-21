"use strict";

// Invitations stay in the URL fragment. Never send them to a server, analytics,
// or another page; only the explicit app-opening action receives the payload.
function validAirName(value) {
  if (!/^(02|03)[0-9a-f]{64}$/.test(value)) return false;
  const prime = (1n << 256n) - (1n << 32n) - 977n;
  const x = BigInt(`0x${value.slice(2)}`);
  if (x >= prime) return false;
  const square = (x * x % prime * x + 7n) % prime;
  let exponent = (prime + 1n) / 4n;
  let base = square;
  let root = 1n;
  while (exponent > 0n) {
    if (exponent & 1n) root = root * base % prime;
    base = base * base % prime;
    exponent >>= 1n;
  }
  return root * root % prime === square;
}

function parseFragment(fragment) {
  if (typeof fragment !== "string") return null;
  const contact = /^#contact=([0-9a-f]{66})$/.exec(fragment);
  if (contact && validAirName(contact[1])) {
    return { kind: "contact", url: `fig-files://contact/${contact[1]}` };
  }
  const file = /^#file=([0-9a-f]{66})\.([0-9a-f]{64})\.([0-9a-f]{64})$/.exec(fragment);
  if (file && validAirName(file[1])) {
    return { kind: "file", url: `fig-files://file/${file[1]}/${file[2]}/${file[3]}` };
  }
  return null;
}

function showInvitation() {
  const invitation = parseFragment(window.location.hash);
  const title = document.getElementById("invitation-title");
  const description = document.getElementById("invitation-description");
  const detail = document.getElementById("invitation-detail");
  const open = document.getElementById("open-app");
  open.hidden = true;
  open.removeAttribute("href");
  if (!invitation) {
    title.textContent = "Invitation unavailable";
    description.textContent = "This link isn’t valid.";
    detail.textContent = "Ask the sender for a new invitation.";
    return;
  }
  if (invitation.kind === "file") {
    title.textContent = "A file to share";
    description.textContent = "Open Fig Files to join this shared file or folder.";
    detail.textContent = "Anyone with this link can read and edit its contents.";
  } else {
    title.textContent = "Add a contact";
    description.textContent = "Open this AirName in Fig Files.";
    detail.textContent = "Adding a contact doesn’t share your files.";
  }
  open.href = invitation.url;
  open.hidden = false;
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = { parseFragment };
}
if (typeof window !== "undefined" && typeof document !== "undefined") {
  showInvitation();
  window.addEventListener("hashchange", showInvitation);
}
