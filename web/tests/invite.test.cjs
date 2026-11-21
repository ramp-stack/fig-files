"use strict";

const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { parseFragment } = require("../fig-files/invite.js");

const name = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const id = "ab".repeat(32);
const token = "cd".repeat(32);

test("contact fragment becomes only the registered contact scheme", () => {
  assert.deepEqual(parseFragment(`#contact=${name}`), {
    kind: "contact", url: `fig-files://contact/${name}`,
  });
});

test("file fragment preserves the exact sender, space ID and token", () => {
  assert.deepEqual(parseFragment(`#file=${name}.${id}.${token}`), {
    kind: "file", url: `fig-files://file/${name}/${id}/${token}`,
  });
});

test("rejects noncanonical keys, wrong routes, missing fields and added data", () => {
  const invalid = [
    undefined, null, "", `contact=${name}`, `?contact=${name}`,
    `#contact=${name.toUpperCase()}`, `#contact=04${name.slice(2)}`,
    `#contact=02${"00".repeat(32)}`, `#contact=02${"ff".repeat(32)}`,
    `#contact=${name} `, `#contact=${name}\n`, `#contact=${name}&contact=${name}`,
    `#contact=${name}%0a`, `#contact=%30${name.slice(1)}`, `#name=${name}`,
    `#contact=${name}&file=${name}.${id}.${token}`, `#file=${name}.${id}`,
    `#file=${name}.${id}.${token.slice(1)}`, `#file=${name}.${id}.${token}0`,
    `#file=${name}.${id.toUpperCase()}.${token}`, `#file=${name}.${id}.${token}/extra`,
    `#file=${name}.${id}.${token}&redirect=https://example.com`,
    `https://therampstack.com/fig-files/invite.html#contact=${name}`,
    "#contact=javascript:alert(1)",
  ];
  for (const value of invalid) assert.equal(parseFragment(value), null, String(value));
});

test("only a valid fragment enables app opening; changing to invalid clears it", () => {
  const elements = new Map();
  for (const id of ["invitation-title", "invitation-description", "invitation-detail", "open-app"]) {
    elements.set(id, { hidden: true, textContent: "", removeAttribute(key) { delete this[key]; } });
  }
  let hashchange;
  const window = {
    location: { hash: `#file=${name}.${id}.${token}` },
    addEventListener(type, handler) { assert.equal(type, "hashchange"); hashchange = handler; },
  };
  const source = fs.readFileSync(path.join(__dirname, "../fig-files/invite.js"), "utf8");
  vm.runInNewContext(source, { window, document: { getElementById(id) { return elements.get(id); } } });
  assert.equal(elements.get("open-app").hidden, false);
  assert.equal(elements.get("open-app").href, `fig-files://file/${name}/${id}/${token}`);
  assert.match(elements.get("invitation-detail").textContent, /Anyone with this link can read and edit/);
  window.location.hash = "#file=invalid";
  hashchange();
  assert.equal(elements.get("open-app").hidden, true);
  assert.equal(elements.get("open-app").href, undefined);
  assert.equal(elements.get("invitation-title").textContent, "Invitation unavailable");
});

test("Apple association claims only the invitation page for both Fig Files apps", () => {
  const association = JSON.parse(fs.readFileSync(path.join(__dirname, "../.well-known/apple-app-site-association"), "utf8"));
  assert.deepEqual(association.applinks.details, [
    { appID: "RYYG6X8WK7.org.ramp.figfiles", paths: ["/fig-files/invite.html"] },
    { appID: "RYYG6X8WK7.org.ramp.fig-files", paths: ["/fig-files/invite.html"] },
  ]);
});
