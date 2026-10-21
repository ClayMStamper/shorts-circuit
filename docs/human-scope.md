# Human scope

Work that needs the owner's identity, money, legal sign-off, credentials, or judgment, and so can't
be done by an agent. Platform rules below are as understood in mid-2026; confirm each during
Increment 1.

## How the app works for you (since 21 October 2026)

Connect each account once, choose the folder your videos arrive in, and let them flow. The app
opens on **Workflows**: one row per destination ("TikTok → YouTube Shorts", …). With **Auto
Publish** on (the default), every new video in the watch folder is rendered, captioned with its
source caption and hashtags (fitted to each platform's limits) and published, with no clicks.
Turn Auto Publish off, or tick **Require review** in a workflow's Settings, to approve each video
yourself instead. Everything else (timezone, posting slots, export packages, the library,
collections and the job monitor) lives under **Settings**.

The first time you start the app a setup guide walks you through three steps:

1. **Source**: choose the watch folder. Optionally connect TikTok (below) so each video uses its
   TikTok post's caption; otherwise a caption file next to the video (`clip.txt`, or
   `clip.json` with `caption`/`description` and `hashtags`, as editors and schedulers export
   them) is used, else a title from the file name.
2. **Destinations**: one card per platform with the console steps below, numbered, with links;
   paste the keys or click Connect, and the card turns *Connected*.
3. **Workflows**: one is suggested per connected destination, Auto Publish on, starting now
   (nothing you posted earlier is published). Tick **I own or have the rights to content from
   this source**: it records video and audio permission for the videos those workflows handle.
   Leave it off if you use TikTok sounds that are licensed for TikTok only; videos then wait for
   you.

Reopen the guide any time with **Setup guide** on the Workflows page. All connection steps below
happen in **Connections** (one card per platform, plus the source).

### How you test it (first run)

1. Start the app; the setup guide opens. Choose a watch folder (a test folder is fine).
2. Connect YouTube (steps below) on its card; leave **Upload as public** off for now.
3. Create the suggested "TikTok → YouTube Shorts" workflow with Auto Publish on and the rights box
   ticked.
4. Copy a short vertical MP4 into the watch folder, optionally with `same-name.txt` holding a
   caption and a few `#hashtags`.
5. Watch the workflow's page: within a minute the video appears, *Preparing video* →
   *Processing n%* → *Uploaded (private)* (or *Published* once the channel uploads publicly),
   with **Open on YouTube Shorts** and "Approved by workflow TikTok → YouTube Shorts".
6. Check the caption and hashtags on YouTube (title = first caption line).
7. Turn Auto Publish off on the row, drop a second video: it stops at *Waiting for review*;
   **Approve** sends it.
8. Pause the workflow (⋯ → Pause), drop a third video, then **Reactivate**: the third video isn't
   published (pausing never floods the destination afterwards).
9. Record anything surprising in the PR or an issue.

## Long-lead gates (start early)

| Platform | Action | Why it blocks |
|---|---|---|
| Snapchat | Create a Snap OAuth app in Ads Manager, apply for Public Profile API allowlisting with its client id; own a Snapchat Public Profile (details below). | External approval with unknown turnaround. Until granted, Snapchat uses the export package. |
| YouTube | YouTube API compliance audit; Google OAuth verification for the `youtube.upload` scope (only needed to leave Testing mode). | Since 2020, unaudited projects have had uploads locked to private (Google's current `videos.insert` page now says they aren't; the app uploads private until you say otherwise, see below). Default quota: 100 uploads a day in their own bucket, plus 10,000 units for everything else; the audit / quota extension form is how either is raised. |
| Instagram | Meta App Review for `instagram_business_content_publish`, possibly Business Verification (only to publish for accounts that aren't on your app). | Not a gate for you: your own professional account, added to the app as an Instagram tester, publishes without review (see below). |
| LinkedIn | Developer app linked to a verified Company Page you administer. | Member posting is self-serve; organization posting needs the Community Management API on its own dedicated app. |
| TikTok (optional) | Register a TikTok developer app for new-post detection (captions from your posts) and get it through TikTok's app review (details below). | Only needed to use TikTok captions; without it captions come from caption files or file names, and the watch folder still imports originals. Review turnaround is outside our control. |

### TikTok developer app (new-post detection and captions)

TikTok offers no API that returns a creator's original video file, so the app never downloads
videos from TikTok. The connector only reads the profile name and the public post list to notice
new posts and their captions; originals arrive through the watch folder. When a file arrives
that is clearly the original of a new post (same length within 1.5 s, within 72 hours, and no
other candidate), workflows use that post's caption and hashtags. A file without a caption file
waits up to two hours for its post before it goes out with a title from the file name. To enable
it:

1. Create an app at TikTok for Developers (developers.tiktok.com) under your developer account.
2. Add **Login Kit** for **Desktop** and the **Display API** with the scopes
   `user.info.basic` and `video.list` only. Nothing that can post.
3. Add the redirect URI `http://127.0.0.1:47616/callback/` exactly (the app listens there during
   sign-in, like the Snapchat one).
4. Fill in the app details review asks for: name, icon, description, category, privacy policy and
   terms of service URLs (see Legal below), and a demo video of the connect flow and how post
   metadata is used. Submit for review.
5. Until review passes, use sandbox mode and add your own TikTok account as a target / test user.
6. In the app: **Connections → Source → TikTok captions**: paste the app's **client key** and
   **client secret** → Save (they go to Windows Credential Manager, entry `tiktok_app`, never the
   repo or the database) → tick **Detect new TikTok posts** → **Connect TikTok…** → sign in and
   allow in the browser; the tab says you can close it.

No broker or hosting is needed: TikTok's desktop Login Kit accepts a loopback redirect with PKCE,
and the secret only ever goes from your PC to TikTok's token endpoint. (Running the optional
OAuth broker with `SHORTS_BROKER_URL` still works when no app is pasted.) If the sign-in fails
at the last step with a PKCE or `code_verifier` error, start the app with
`SHORTS_TIKTOK_PKCE=base64url` and tell us (TikTok's desktop page describes a hex challenge,
which is unusual).

Confirm the product names, scopes and review requirements in the portal when you do this; they
were as understood in October 2026.

### YouTube Shorts (first publishing connector)

The app uploads straight from your PC through the YouTube Data API, using a Google OAuth
client you create. Google treats a desktop app's client secret as non-confidential, so no
broker or hosting is needed. What you do once:

1. **Google Cloud project.** At console.cloud.google.com, create a project (e.g. "Shorts
   Circuit") with the Google account that owns the channel, or one that manages it.
2. **Enable the API.** APIs & Services → Library → **YouTube Data API v3** → Enable.
3. **Consent screen.** Google Auth Platform (APIs & Services → OAuth consent screen):
   - User type **External**, publishing status **Testing**.
   - App name, your support email, developer contact email.
   - Data access / scopes: add `https://www.googleapis.com/auth/youtube.upload` and
     `https://www.googleapis.com/auth/youtube.readonly` (the app asks for exactly these two:
     upload, and read your channel's videos to follow processing and find an upload again).
   - Audience / **Test users**: add the Google account you'll sign in with.
4. **OAuth client.** Clients → Create client → Application type **Desktop app** → name it →
   Create. Copy the **client ID** (ends in `.apps.googleusercontent.com`) and the **client
   secret**. You don't add a redirect URI: desktop clients accept the app's
   `http://127.0.0.1:<port>` loopback redirect automatically.
5. **Paste them into the app.** Connections → YouTube Shorts → 1. Your Google OAuth
   client → Client ID and Client secret → Save. They go to Windows Credential Manager (entry
   `io.github.claymstamper.shorts-circuit` / `google_oauth_client`), never into the database,
   logs, or the repo.
6. **Connect the channel.** Connections → YouTube Shorts → Connect a YouTube channel… → your browser opens
   Google's consent page → pick the test-user account and channel → Google warns that the app
   isn't verified (expected in Testing) → Continue → allow both permissions → the tab says you
   can close it. The channel appears under Channels as *connected*.
7. **Every 7 days (while in Testing).** Google ends sign-ins after 7 days for External apps in
   Testing. When that happens, publications for the channel stop at *Needs authentication* with
   a **Reconnect** button, and the workflow and its Connections card show *Needs reconnect*: click **Reconnect …**, sign
   in again, and the waiting uploads continue on their own. Publishing the consent screen to
   Production (which needs Google's verification for these scopes) removes the 7-day limit.

**Private until audited.** New channels upload as **private**. Since July 2020 Google has
locked uploads from API projects that haven't passed the YouTube API compliance audit to
private. (In October 2026 Google's `videos.insert` page says unverified projects are *not*
restricted; we haven't confirmed that with a real upload, so the app stays conservative.) After
your project passes the audit (YouTube API Services → audit and quota extension form), or once
a test upload shows YouTube publishing your uploads publicly, tick **Upload as public** for the
channel. Whatever you choose, the publication tells you if YouTube kept a video private.

**Quota.** By default the project gets 100 uploads a day and 10,000 units for other calls
(each upload uses about 3–30 of those to watch processing and confirm it), resetting at
midnight Pacific time. The YouTube card in Connections shows today's use and warns when it runs low; an upload that
would go over waits until the reset. If Google raises your quota, start the app with
`SHORTS_YOUTUBE_UPLOADS_PER_DAY` / `SHORTS_YOUTUBE_UNITS_PER_DAY` set to the new limits.

**Made for kids.** Uploads are declared *not made for kids*. If any of your videos are, change
that in YouTube Studio (and tell us, so the app can ask per video).

### YouTube smoke test (Windows)

Use a throwaway or test channel the first time. The steps under "How you test it (first run)"
above are the smoke test; in addition:

1. Install FFmpeg (or use the bundled build) and start the app; there is no FFmpeg banner.
2. After the first upload, open the link (signed in as the channel owner) or YouTube Studio →
   Content: the video is there, private, with the first caption line as title and the caption
   and hashtags as description. The YouTube card shows 1 upload in today's quota.
3. Optional resilience checks: quit the app mid-upload (a larger file helps) and start it
   again; the upload resumes rather than starting over. Disconnect the channel and reconnect
   it; waiting videos keep their place.
4. Record the video id, processing states seen and any surprises in the PR or an issue
   (Integration acceptance in `docs/architecture.md`).

### Instagram Reels (second publishing connector)

The app publishes Reels through the Instagram API with Instagram Login, uploading the file
straight from your PC. You connect an account by pasting a token from your Meta app's
dashboard, so there is no redirect address to host and no app secret in the app. What you do
once:

1. **Professional account.** In the Instagram app: Settings → Account type and tools → switch
   to a **Creator** or **Business** account (free; no Facebook Page needed).
2. **Meta app.** At developers.facebook.com → My Apps → Create app. Pick the use case for
   managing messaging and content on Instagram (the "Instagram API" product), app type
   Business if asked. The app can stay in **Development** mode.
3. **Add your account as a tester.** App dashboard → App roles → Roles → Add people →
   **Instagram Tester** → your Instagram username. Accept the invitation in Instagram (Settings
   → Website permissions / Apps and websites → Tester invites), or on instagram.com.
4. **Permissions.** Instagram → API setup with Instagram login: make sure
   `instagram_business_basic` and `instagram_business_content_publish` are included.
5. **Generate the token.** Same page → Generate access tokens → Add account → sign in to the
   professional account → allow → **Generate token**. Copy it (it starts with `IGAA`). It is a
   long-lived token (60 days).
6. **Paste it into the app.** Connections → Instagram Reels → Access token → Connect.
   The token goes to Windows Credential Manager (`instagram:<account id>`), never into the
   database, logs, or the repo.
7. **Keeping it alive.** The app renews the token for another 60 days whenever it has less
   than 20 days left, as long as the app runs at least every few weeks. If it lapses (or you
   remove the app's access in Instagram), the account shows *needs reconnecting*: generate a
   new token (step 5) and paste it with **Reconnect**. Waiting publications then continue.

Confirm the dashboard names and permission names when you do this; Meta renames them often.
App Review is only needed if the app should publish for Instagram accounts that aren't on
your app's roles.

### Instagram smoke test (Windows)

Use a test professional account the first time if you have one; Reels published here are
public.

1. Do the Instagram steps above; its card shows *Connected* (the account's "good until" date is
   under Manage connection, about 60 days out).
2. Workflows → **Create workflow** → Instagram Reels (Auto Publish on, rights ticked). Optionally
   untick **Also share to the main feed** in its Settings.
3. Drop a short MP4 (3 seconds to a few minutes; vertical 1080×1920 looks best) into the watch
   folder. The workflow's page shows *Processing* → *Published* with a link to the reel. Open it
   to check the caption and hashtags.
4. Optional: quit the app during *Processing* with a larger file and start it again; the upload
   repeats into the same container instead of creating a second post.
5. Record the container id, the media id and anything surprising (error codes, how long
   processing took) in the PR or an issue.

### LinkedIn Company Page (third publishing connector)

The app posts videos as your Company Page. LinkedIn's Community Management API is the gate:
you apply for it once, and LinkedIn decides (expect days to weeks). Until it is approved the
page keeps using export packages. What you do:

1. **Developer app.** At linkedin.com/developers → Create app. Name it, link it to your
   Company Page (a page admin must verify the link from the page's admin view), add a logo and
   the privacy policy URL.
2. **Community Management API.** In the app → Products → request **Community Management API**.
   LinkedIn requires it to be the only product on that app, so use a dedicated app. Fill in
   the access form (use case: publishing your own organization's videos). Wait for approval.
3. **Generate a token.** Once approved: app → Auth → OAuth 2.0 tools → **Create token** (the
   token generator). Choose `w_organization_social`, `r_organization_social` and
   `rw_organization_admin`, sign in with the LinkedIn member who administers the page, allow,
   and copy the access token. It lasts 60 days.
4. **Connect the page.** Connections → LinkedIn → paste the token → **Find my pages**
   → choose the page (or enter its id: the number in `linkedin.com/company/<id>/admin`) →
   Connect. The token goes to Windows Credential Manager (`linkedin:<account id>`).
5. **Every 60 days.** The app warns a week before the token runs out; generate a new one
   (step 3) and use **Reconnect**. If it does run out, waiting publications pause until you
   reconnect, then continue.

Confirm the product, scope and portal names when you do this; LinkedIn changes them.

### LinkedIn smoke test (Windows)

LinkedIn posts are public on your page, so post something you're happy to keep (or delete it
afterwards on LinkedIn).

1. Do the LinkedIn steps above; its card shows *Connected*.
2. Create the "TikTok → LinkedIn" workflow (Workflows → Create workflow).
3. Drop a short MP4 (at least 75 KB and 3 seconds) into the watch folder.
4. The workflow's page shows *Processing* → *Published* with a link to the post. Check the
   caption, hashtags and video title on LinkedIn.
5. Record the video URN, post URN and anything surprising in the PR or an issue.

### Snapchat Spotlight (fourth publishing connector)

The app posts to Spotlight from your PC through Snap's Public Profile API, using a Snap OAuth
app you create. Snap only lets allowlisted apps use that API, so the slow part is waiting for
Snap. Until then, Snapchat publications use export packages. What you do once:

1. **Public Profile.** In the Snapchat app, make sure the account you'll post as has a
   **Public Profile** (Profile → Public Profile; create one if needed), and that you are its
   owner or an admin.
2. **OAuth app (Ads Manager, not the Developer Portal).** Sign in at ads.snapchat.com with that
   Snapchat account (create a free Business account if asked). Go to **Business Details** →
   **OAuth apps** → create an app. Name it (e.g. "Shorts Circuit"). Copy the **client id** and
   the **client secret**; Snap shows the secret only once. (Apps from the Snap Kit Developer
   Portal are a different kind and don't work with the Public Profile API.)
3. **Redirect address.** In the same OAuth app, add exactly this redirect URI:
   `http://127.0.0.1:47615/snapchat/callback`. That lets the app catch the sign-in itself. If
   Snap refuses an `http` address, register
   `https://claymstamper.github.io/shorts-circuit/snap-callback.html` instead (published by the
   Policy pages workflow, see Legal below); that page shows a short text you paste back into the
   app. Use the same address in step 5, character for character.
4. **Ask for allowlisting.** Send Snap the **client id** (never the secret) and ask for Public
   Profile API access for it, using the contact or form Snap's Public Profile API "Get Started"
   page names (or your Snap partner contact). Say it posts your own Spotlight videos from a
   desktop app. Wait for their answer; the turnaround is outside our control.
5. **Paste the app into Shorts Circuit.** Connections → Snapchat Spotlight → 1. Your
   Snap OAuth app → Client ID, Client secret and Redirect address (the loopback address is
   filled in; replace it with the https page if you registered that) → Save. They go to Windows
   Credential Manager (entry `io.github.claymstamper.shorts-circuit` / `snap_oauth_client`),
   never into the database, logs, or the repo.
6. **Connect the profile.** Connections → Snapchat Spotlight → **Connect a Snapchat public profile…** → your browser
   opens Snap's sign-in → sign in as the profile's owner → allow. With the loopback address the
   tab says you can close it; with the https page, click **Copy** there and paste the text into
   **Paste the text from the page** → Finish connecting. The profile appears as *connected*.
   Signing in before Snap has allowlisted the app fails with an access error; try again once
   they confirm.
7. **If it says *needs reconnecting*.** Access tokens last an hour and the app renews them on
   its own. If Snap ends the sign-in (you removed the app's access, changed the client secret,
   or the refresh token expired), publications for the profile stop at *Needs authentication*:
   click **Reconnect …**, sign in again, and they continue.

Confirm the Ads Manager menu names, the allowlisting route and whether `http://127.0.0.1`
redirects are accepted when you do this; they are as understood in October 2026.

### Snapchat smoke test (Windows)

Spotlight posts are public once Snap's review approves them, so post something you're happy
to keep (you can delete it from the profile in the Snapchat app).

1. Do the Snapchat steps above; its card shows *Connected*, and **Check connection** (under
   Manage connection) keeps it that way.
2. Create the "TikTok → Snapchat Spotlight" workflow.
3. Drop a vertical MP4 of 6 to 60 seconds, at least 540×960 (1080×1920 is ideal), into the
   watch folder. Long captions are cut to fit Spotlight's 160 characters automatically (hashtags
   are dropped from the end first).
4. The workflow's page shows *Processing* (the file is encrypted and sent in parts) →
   *Published*. Snap reviews Spotlight posts, so it can take a while to appear on the profile.
5. Optional: quit the app during *Processing* with a larger file (over 32 MB) and start it
   again; the upload continues with the next part.
6. Record the media id, the Spotlight id, how long Snap's review took, and anything surprising
   (error codes, rejected fields) in the PR or an issue. In particular, note whether any step
   failed in a way that points at the unverified details listed under "Snapchat Spotlight
   connector" in `docs/architecture.md`.

## Cloud runner (optional: posting while your PC is off)

Nothing here is needed for the desktop app; without a runner everything posts from your PC
while the app runs. The full walkthrough is `docs/cloud-runner.md`. What only you can do:

1. **Rent a server.** A small Linux VPS, e.g. a DigitalOcean droplet (Ubuntu 24.04, Basic,
   1 vCPU / 1–2 GB, about $6–12 a month), with your SSH key. Turn on its backups if you want
   a second copy.
2. **Pick a domain.** Add an `A` record such as `runner.<your domain>` pointing at the server.
   The HTTPS certificate is issued for it automatically (Caddy, Let's Encrypt).
3. **Install and start it** on the server (Docker, the image, `compose.yaml`, `.env`), as in
   `docs/cloud-runner.md`. You generate the database password and the master encryption key
   there; they live only in the server's `.env`, never in this repo.
4. **Keep the master key safe.** Store `SHORTS_RUNNER_MASTER_KEY` in your password manager.
   Without it the runner's stored credentials can't be decrypted (you would reconnect every
   account); with it, anyone holding a database backup could read them.
5. **Create a token for each PC** with `shorts-runner token create "<name>"` and paste it, with
   the runner's `https://` address, into Connections → Cloud runner → Connect.
6. **Backups.** Set up the nightly database dump and copy it off the server (the cron line in
   `docs/cloud-runner.md`). Media needs no backup.
7. **First test.** Approve a post for a test account a few minutes ahead, choose **Post from
   the cloud**, quit the app and check it goes out. Record anything surprising in the PR or
   an issue.

Ongoing: upgrade the image now and then (`git pull`, rebuild, `docker compose up -d`), rotate
the master key if you suspect it leaked (`docs/cloud-runner.md` → Rotating the encryption
key), revoke a token when a PC is lost (`shorts-runner token revoke`), and take publications
back to the PC to resolve anything the runner reports as needing you.

## Watch folder

Pick a folder your phone or editor syncs finished videos into (e.g. a OneDrive, Google Drive,
Dropbox or iCloud folder, or the editor's export folder) and choose it in Connections → Source.
Workflows publish what arrives there. To give a video its own caption without TikTok, save a
caption file next to it with the same name: `.txt` (the caption, hashtags inline) or `.json`
(`caption` or `description`, and `hashtags` as a list).
Prefer the original export over a video saved back from TikTok, which carries a watermark.

## Accounts and sign-ins

- Developer accounts: Google Cloud, Meta for Developers, Snapchat Ads Manager (Business
  account), LinkedIn Developers, and
  (optional) TikTok for Developers.
- Throwaway test destination accounts: YouTube channel, Instagram Professional account, LinkedIn
  member + Company Page, Snapchat Public Profile.
- Passwords and 2FA during every OAuth consent.
- Pasting client secrets into the app (Connections) or backend config. Secrets never go in this
  repo.

## Paid infrastructure

- Domain: OAuth redirects, Google domain verification, privacy-policy hosting.
- OAuth backend hosting (Axum): **not needed for publishing to YouTube, Instagram, LinkedIn
  or Snapchat, nor for TikTok captions** (YouTube uses a desktop OAuth client; Instagram and
  LinkedIn are connected with pasted tokens; Snapchat and TikTok use your own developer apps,
  pasted into the app, with loopback redirects). Needed only for a browser sign-in flow if the
  app is shared with other people.
- Cloud runner hosting (a small VPS with PostgreSQL and a domain): only for cloud mode, i.e.
  posting while the PC is off. See "Cloud runner" above.
- Object storage for media staging: not needed for Instagram (the app uploads the file to the
  container directly); possibly for other providers that only fetch a public URL.
- Code signing: Windows (Azure Trusted Signing or OV cert); Apple Developer Program for macOS.
  Only needed once the installer goes to other people (see Releases).

## Releases

The **Release** workflow builds an unsigned Windows installer with FFmpeg bundled. What only
you can do:

- **Run it.** Actions → Release → Run workflow for a test build (download the artifact), or
  bump `version` in `app/src-tauri/tauri.conf.json` (and `app/package.json`), commit, and push a
  tag like `v0.2.0` for a draft GitHub release you then publish.
- **Pin FFmpeg (recommended).** After the first successful run, copy the `SHA-256 OK: …` value
  from the "Fetch FFmpeg" step into a repository variable `FFMPEG_SHA256` (Settings → Secrets
  and variables → Actions → Variables). Later runs then refuse any other file.
- **SmartScreen.** Unsigned installers show "Windows protected your PC" on first run (More
  info → Run anyway). That's fine for you; for other people, get code signing: Azure Trusted
  Signing (identity validation, monthly fee) or an OV certificate, then add a `signCommand` to
  `tauri.release.conf.json` with the credentials as repository secrets.
- **FFmpeg's license.** The renders use libx264, so the bundled FFmpeg is a GPL build. Shipping
  it as a separate program next to an MIT app is normal, but the GPL asks you to pass on its
  license and point to the corresponding source. The installer includes
  `licenses/FFMPEG-LICENSE.txt` and `licenses/FFMPEG-NOTICE.txt` for that. Decide whether that's
  enough for how you distribute the app (get advice if you sell it), or ask for an LGPL build
  that encodes with Windows' own H.264 encoder instead.
- **Size.** Static FFmpeg builds are large (about 100 MB each for "essentials", over 200 MB
  for "full"); the installer compresses them.
- **Updates.** There is no auto-updater yet; install new versions over the old one (your data
  stays in `%LOCALAPPDATA%`).

## Legal and review material

- **Policy pages (drafted in `site/`).** A privacy policy, terms of service and data-deletion
  instructions written for how the app actually works (local-first, tokens in Windows
  Credential Manager, no server of yours holding user data). Meta, LinkedIn, TikTok and Google
  ask for these as public URLs. To publish them:
  1. Read all three pages and change anything that doesn't match how you'll use or share the
     app. Get legal advice if you'll distribute or sell it.
  2. Repository Settings → Pages → Build and deployment → Source: **GitHub Actions**.
  3. Settings → Secrets and variables → Actions → Variables: add `POLICY_OPERATOR_NAME` (you or
     your studio), `POLICY_CONTACT_EMAIL` (a public contact address) and
     `POLICY_EFFECTIVE_DATE` (e.g. `1 November 2026`). The workflow refuses to publish without
     them, so no placeholder text goes live.
  4. Actions → Policy pages → Run workflow (it also runs when `site/` changes on `main`). The
     pages appear at `https://claymstamper.github.io/shorts-circuit/` (`privacy.html`,
     `terms.html`, `data-deletion.html`, and `snap-callback.html`, the Snapchat sign-in page).
  5. Paste those URLs into each developer app: Meta (Privacy policy URL, Terms of service URL,
     User data deletion → Data deletion instructions URL), LinkedIn (Privacy policy URL),
     TikTok (Privacy policy and Terms of service). Google's OAuth verification needs the pages
     on a domain you own, so it waits for a custom domain (Pages supports one).
- App-review screencasts of the real OAuth flow (scripted by an agent, recorded by the owner).

## Ongoing duties

- Nothing per video while Auto Publish is on. Approving videos only for workflows set to require
  review, or when a video can't go out as is (the workflow page says why: too long for
  Spotlight, a permission recorded as denied). Resolving `OutcomeUnknown` publishes (for one the
  cloud runner reports, take it back to the PC first).
- Keeping the app running at least every few weeks so Instagram tokens get renewed, or pasting
  a new token when an account says *needs reconnecting*.
- Switching a YouTube channel to public uploads once the Google Cloud project passes the API
  audit.
- Re-authentication: Google refresh tokens expire after 7 days while the consent screen is in
  Testing (the app asks with a Reconnect button; see YouTube above); LinkedIn access tokens last
  60 days and programmatic refresh is partner-only, so paste a new token every two months (the
  app warns a week ahead).
- Snapchat reconnects: the app renews hourly access tokens itself; reconnect when a profile says
  *needs reconnecting* (for example after rotating the Snap OAuth app's secret: paste the new
  one in Connections, then reconnect).
- Meta annual data-use checkup; periodic YouTube re-audits.
- Getting original videos into the watch folder (phone or editor sync). Unambiguous TikTok
  post/file matches are confirmed automatically; an ambiguous one waits in Settings → Advanced →
  Library.
- TikTok reconnects: access tokens last about a day and the app refreshes them with your TikTok
  app; the refresh token lasts about a year, or until revoked in TikTok's settings.
- Choosing, per workflow, between Auto Publish (default) and review. Auto Publish is only
  possible for accounts the app can post to.
- Rights calls, especially audio: TikTok sounds are often licensed for TikTok only.

## Open decisions

- ~~Review every post, or publish automatically.~~ **Resolved (21 October 2026): automatic by
  default.** Content flows with no review; captions and hashtags come from the source; review
  and customizing are optional per-workflow settings.
- ~~LinkedIn: member, organization, or both.~~ **Resolved (October 2026): the organization
  (Company) page**, posted through the Community Management API on its own dedicated LinkedIn
  app. Organization is the default LinkedIn destination type; member posting isn't planned. (A
  Repurpose.io screenshot that showed the personal profile was misleading.)
- Default overdue-job policy: catch up or expire.
- Personal use vs distribution to others (drives signing, legal, and review depth).
