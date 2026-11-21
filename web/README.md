# Fig Files invitation landing page

These static files belong on `https://therampstack.com`. They do not add a separate Fig Files website.

Deploy only:

- `fig-files/invite.html`, `fig-files/invite.js`, and `fig-files/invite-icon.svg` under `/fig-files/`.
- `.well-known/apple-app-site-association` at the site root, with `Content-Type: application/json` and no redirect.

Keep the existing `/fig-files/` page. If the site already has an Apple association file, merge the two Fig Files entries into its `applinks.details` array instead of overwriting other apps. The association must be served directly on both `therampstack.com` and `www.therampstack.com` to support both associated domains; the `www` association path must be exempt from any apex redirect. Canonical invitation links use the apex host.

The web page converts a validated invitation fragment to an explicit **Open Fig Files** action:

```text
https://therampstack.com/fig-files/invite.html#contact=<AirName>
fig-files://contact/<AirName>

https://therampstack.com/fig-files/invite.html#file=<sender>.<shared-space-id>.<token>
fig-files://file/<sender>/<shared-space-id>/<token>
```

AirNames are canonical, lowercase compressed secp256k1 public keys (66 hex characters). IDs and tokens are 64 lowercase hex characters. File invitations grant read/write access to anyone holding the link. Treat the entire file link as a secret.

The payload stays in the URL fragment and is not sent to the web server. This page has no analytics, external resources, or network calls; its `no-referrer` policy also keeps referral data off outgoing links. Do not add analytics or logging that reads the fragment. Do not rewrite the fragment into query parameters.

Native universal links require the corresponding associated-domain entitlement in a provisioned Fig Files build and the live Apple association file. Installing the static files alone does not update an installed app. If a browser shows the landing page, **Open Fig Files** uses the registered custom scheme; the app must already be installed. No automatic redirect or app-install detection is performed.

Run the parser and association checks with:

```sh
node --test web/tests/invite.test.cjs
```

The invitation page and apex Apple association were published and verified on 2026-11-21. Do not publish `tests/`, `publish.py`, or this README as website content.

`publish.py` is the narrow CloudShell deployment helper. Package it with the four static files and a SHA-256 `manifest.json`, then run it in the existing authenticated AWS environment. It checks account `173652135802` and the `therampstack.com` distribution, preserves other Apple association entries, uploads only the invitation assets, and invalidates only those paths. The current CloudFront directory rewrite requires the association at S3 key `.well-known/apple-app-site-association/index.html`; the helper also uploads the conventional key for future routing. The canonical HTTPS endpoint returns JSON directly. The existing `www` redirect is unchanged; canonical links use the apex domain.
