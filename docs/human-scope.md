# Human scope

Work that needs the owner's identity, money, legal sign-off, credentials, or judgment, and so can't
be done by an agent. Platform rules below are as understood in mid-2026; confirm each during
Increment 1.

## Long-lead gates (start early)

| Platform | Action | Why it blocks |
|---|---|---|
| Snapchat | Create a Snap OAuth app in Ads Manager, apply for Public Profile API allowlisting with its client id; own a Snapchat Public Profile (details below). | External approval with unknown turnaround. Until granted, Snapchat uses the export package. |
| YouTube | YouTube API compliance audit; Google OAuth verification for the `youtube.upload` scope (only needed to leave Testing mode). | Since 2020, unaudited projects have had uploads locked to private (Google's current `videos.insert` page now says they aren't; the app uploads private until you say otherwise, see below). Default quota: 100 uploads a day in their own bucket, plus 10,000 units for everything else; the audit / quota extension form is how either is raised. |
| Instagram | Meta App Review for `instagram_business_content_publish`, possibly Business Verification (only to publish for accounts that aren't on your app). | Not a gate for you: your own professional account, added to the app as an Instagram tester, publishes without review (see below). |
| LinkedIn | Developer app linked to a verified Company Page you administer. | Member posting is self-serve; organization posting needs the Community Management API on its own dedicated app. |
| TikTok (optional) | Register a TikTok developer app for new-post detection and get it through TikTok's app review (details below). | Only needed for detecting new posts; without it the feature shows as unavailable and the watch folder still imports originals. Review turnaround is outside our control. |

### TikTok developer app (new-post detection)

TikTok offers no API that returns a creator's original video file, so the app never downloads
videos from TikTok. The connector only reads the profile name and the public post list to notice
new posts; originals arrive through the watch folder. To enable it:

1. Create an app at TikTok for Developers (developers.tiktok.com) under your developer account.
2. Add **Login Kit** (web) and the **Display API** product with the scopes `user.info.basic` and
   `video.list` only. Nothing that can post.
3. Set the redirect URI to `<public broker URL>/v1/tiktok/callback` (HTTPS).
4. Fill in the app details review asks for: name, icon, description, category, privacy policy and
   terms of service URLs (see Legal below), and a demo video of the connect flow and how post
   metadata is used. Submit for review.
5. Until review passes, use sandbox mode and add your own TikTok account as a target / test user.
6. Put the client key and secret in the broker's environment (`TIKTOK_CLIENT_KEY`,
   `TIKTOK_CLIENT_SECRET`, `TIKTOK_REDIRECT_URI`), never in this repo, and start the desktop app
   with `SHORTS_BROKER_URL` pointing at the broker.
7. In the app: Settings → TikTok new-post detection → turn it on → Connect TikTok.

Confirm the product names, scopes and review requirements in the portal when you do this; they
were as understood in mid-2026.

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
5. **Paste them into the app.** Settings → YouTube Shorts publishing → 1. Your Google OAuth
   client → Client ID and Client secret → Save. They go to Windows Credential Manager (entry
   `io.github.claymstamper.shorts-circuit` / `google_oauth_client`), never into the database,
   logs, or the repo.
6. **Connect the channel.** Settings → Connect a YouTube channel… → your browser opens
   Google's consent page → pick the test-user account and channel → Google warns that the app
   isn't verified (expected in Testing) → Continue → allow both permissions → the tab says you
   can close it. The channel appears under Channels as *connected*.
7. **Every 7 days (while in Testing).** Google ends sign-ins after 7 days for External apps in
   Testing. When that happens, publications for the channel stop at *Needs authentication* with
   a **Reconnect** button, and Settings shows *needs reconnecting*: click **Reconnect …**, sign
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
midnight Pacific time. Settings shows today's use and warns when it runs low; an upload that
would go over waits until the reset. If Google raises your quota, start the app with
`SHORTS_YOUTUBE_UPLOADS_PER_DAY` / `SHORTS_YOUTUBE_UNITS_PER_DAY` set to the new limits.

**Made for kids.** Uploads are declared *not made for kids*. If any of your videos are, change
that in YouTube Studio (and tell us, so the app can ask per video).

### MVP smoke test (Windows)

Use a throwaway or test channel the first time.

1. Install FFmpeg (or use the bundled build) and start the app; Settings shows no FFmpeg
   warning.
2. Do the YouTube steps above (client saved, channel connected, *Uploads stay private* note
   shown, quota at 0 of 100 uploads).
3. Collections → New collection "YouTube MVP" → add a rule: destination your channel (it says
   *uploads automatically, as private*), leave publications *submitted for my approval*.
4. Settings → Folders → choose a watch folder and set **Put watch-folder imports in** to
   "YouTube MVP". Optionally add a posting slot a few minutes from now.
5. Copy a short vertical video (under 3 minutes, e.g. 1080×1920 MP4) into the watch folder.
   Within a minute it appears in the Library, and the Monitor shows a YouTube publication
   *Waiting for your approval*.
6. Open it (Review): the private-upload note is shown, the video and caption look right. Set
   the posting time to now (or keep the slot) and **Approve**.
7. Watch the Monitor: *Uploading* with a percentage → *YouTube is processing the video* →
   *Published* with **Open on YouTube Shorts** and the note *Uploaded as private*.
8. Open the link (signed in as the channel owner) or YouTube Studio → Content: the video is
   there, private, with the caption as title/description. Settings → quota shows 1 upload.
9. Optional resilience checks: quit the app mid-upload (a larger file helps) and start it
   again; the upload resumes rather than starting over. Disconnect the channel and reconnect
   it; scheduled publications keep their place.
10. Record the video id, processing states seen and any surprises in the PR or an issue
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
6. **Paste it into the app.** Settings → Instagram Reels publishing → Access token → Connect.
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

1. Do the Instagram steps above; Settings shows the account as *connected* with a "good
   until" date about 60 days out.
2. Collections → New collection "Instagram MVP" → add a rule with destination the account,
   publications *submitted for my approval*. Route the watch folder into it (Settings →
   Folders), or add an asset to the collection by hand.
3. Drop a short MP4 (3 seconds to a few minutes; vertical 1080×1920 looks best) into the
   watch folder. The Monitor shows an Instagram publication *Waiting for your approval*.
4. Review it, set the time to now and **Approve**.
5. Watch the Monitor: *Uploading* → *Instagram is processing the reel* → *Published* with a
   link to the reel. Open it to check the caption and hashtags.
6. Optional: quit the app during *Uploading* with a larger file and start it again; the
   upload repeats into the same container instead of creating a second post.
7. Record the container id, the media id and anything surprising (error codes, how long
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
4. **Connect the page.** Settings → LinkedIn page posts → paste the token → **Find my pages**
   → choose the page (or enter its id: the number in `linkedin.com/company/<id>/admin`) →
   Connect. The token goes to Windows Credential Manager (`linkedin:<account id>`).
5. **Every 60 days.** The app warns a week before the token runs out; generate a new one
   (step 3) and use **Reconnect**. If it does run out, waiting publications pause until you
   reconnect, then continue.

Confirm the product, scope and portal names when you do this; LinkedIn changes them.

### LinkedIn smoke test (Windows)

LinkedIn posts are public on your page, so post something you're happy to keep (or delete it
afterwards on LinkedIn).

1. Do the LinkedIn steps above; Settings shows the page as *connected*.
2. Collections → New collection "LinkedIn MVP" → rule with the page as destination,
   publications *submitted for my approval*; route the watch folder into it.
3. Drop a short MP4 (at least 75 KB and 3 seconds) into the watch folder, review and
   **Approve** it with the time set to now.
4. Watch the Monitor: *Uploading* → *LinkedIn is processing the video* → *Published* with a
   link to the post. Check the caption, hashtags and video title on LinkedIn.
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
5. **Paste the app into Shorts Circuit.** Settings → Snapchat Spotlight publishing → 1. Your
   Snap OAuth app → Client ID, Client secret and Redirect address (the loopback address is
   filled in; replace it with the https page if you registered that) → Save. They go to Windows
   Credential Manager (entry `io.github.claymstamper.shorts-circuit` / `snap_oauth_client`),
   never into the database, logs, or the repo.
6. **Connect the profile.** Settings → **Connect a Snapchat public profile…** → your browser
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

1. Do the Snapchat steps above; Settings shows the profile as *connected*, and **Check
   connection** keeps it that way.
2. Collections → New collection "Snapchat MVP" → rule with the profile as destination,
   publications *submitted for my approval*; route the watch folder into it.
3. Drop a vertical MP4 of 6 to 60 seconds, at least 540×960 (1080×1920 is ideal), into the
   watch folder. Keep the caption short: Spotlight descriptions are cut at 160 characters.
4. Review it, set the time to now and **Approve**.
5. Watch the Monitor: *Uploading* (the file is encrypted and sent in parts) → *Published*.
   Snap reviews Spotlight posts, so it can take a while to appear on the profile.
6. Optional: quit the app during *Uploading* with a larger file (over 32 MB) and start it
   again; the upload continues with the next part.
7. Record the media id, the Spotlight id, how long Snap's review took, and anything surprising
   (error codes, rejected fields) in the PR or an issue. In particular, note whether any step
   failed in a way that points at the unverified details listed under "Snapchat Spotlight
   connector" in `docs/architecture.md`.

## Watch folder

Pick a folder your phone or editor syncs finished videos into (e.g. a OneDrive, Google Drive,
Dropbox or iCloud folder, or the editor's export folder) and choose it in Settings → Folders.
Optionally route its imports into a collection whose rules create publications for review.
Prefer the original export over a video saved back from TikTok, which carries a watermark.

## Accounts and sign-ins

- Developer accounts: Google Cloud, Meta for Developers, Snapchat Ads Manager (Business
  account), LinkedIn Developers, and
  (optional) TikTok for Developers.
- Throwaway test destination accounts: YouTube channel, Instagram Professional account, LinkedIn
  member + Company Page, Snapchat Public Profile.
- Passwords and 2FA during every OAuth consent.
- Pasting client secrets into backend config / credential storage. Secrets never go in this repo.

## Paid infrastructure

- Domain: OAuth redirects, Google domain verification, privacy-policy hosting.
- OAuth backend hosting (Axum + Postgres): **not needed for publishing to YouTube, Instagram,
  LinkedIn or Snapchat** (YouTube uses a desktop OAuth client; Instagram and LinkedIn are
  connected with pasted tokens; Snapchat uses your own Snap OAuth app, pasted into the app, with
  a loopback redirect or the free GitHub Pages sign-in page). Needed for TikTok new-post detection, for a browser sign-in flow if the app is
  shared with other people, and for cloud mode (posting while the PC is off).
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

- Approving publications; resolving `OutcomeUnknown` publishes.
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
  one in Settings, then reconnect).
- Meta annual data-use checkup; periodic YouTube re-audits.
- Getting original videos into the watch folder (phone or editor sync), and confirming the
  suggested match when a detected TikTok post and an arriving file belong together.
- TikTok reconnects: access tokens last about a day and are refreshed through the broker; the
  refresh token lasts about a year, or until revoked in TikTok's settings.
- Turning on unattended publishing, if ever: it is per collection and account, off by default,
  and only possible for accounts that publish automatically.
- Rights calls, especially audio: TikTok sounds are often licensed for TikTok only.

## Open decisions

- ~~LinkedIn: member, organization, or both.~~ **Resolved (October 2026): the organization
  (Company) page**, posted through the Community Management API on its own dedicated LinkedIn
  app. Organization is the default LinkedIn destination type; member posting isn't planned. (A
  Repurpose.io screenshot that showed the personal profile was misleading.)
- Default overdue-job policy: catch up or expire.
- Personal use vs distribution to others (drives signing, legal, and review depth).
