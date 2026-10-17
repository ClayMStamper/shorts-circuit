# Human scope

Work that needs the owner's identity, money, legal sign-off, credentials, or judgment, and so can't
be done by an agent. Platform rules below are as understood in mid-2026; confirm each during
Increment 1.

## Long-lead gates (start early)

| Platform | Action | Why it blocks |
|---|---|---|
| Snapchat | Apply for Public Profile API allowlisting; own a Snapchat Public Profile. | External approval with unknown turnaround. Until granted, Snapchat uses the export package. |
| YouTube | YouTube API compliance audit; Google OAuth verification for the `youtube.upload` scope (only needed to leave Testing mode). | Since 2020, unaudited projects have had uploads locked to private (Google's current `videos.insert` page now says they aren't; the app uploads private until you say otherwise, see below). Default quota: 100 uploads a day in their own bucket, plus 10,000 units for everything else; the audit / quota extension form is how either is raised. |
| Instagram | Meta App Review for `instagram_business_content_publish`, possibly Business Verification. | Testing works without review by adding your account as an app tester; review is needed for anyone else. |
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

## Watch folder

Pick a folder your phone or editor syncs finished videos into (e.g. a OneDrive, Google Drive,
Dropbox or iCloud folder, or the editor's export folder) and choose it in Settings → Folders.
Optionally route its imports into a collection whose rules create publications for review.
Prefer the original export over a video saved back from TikTok, which carries a watermark.

## Accounts and sign-ins

- Developer accounts: Google Cloud, Meta for Developers, Snap Kit, LinkedIn Developers, and
  (optional) TikTok for Developers.
- Throwaway test destination accounts: YouTube channel, Instagram Professional account, LinkedIn
  member + Company Page, Snapchat Public Profile.
- Passwords and 2FA during every OAuth consent.
- Pasting client secrets into backend config / credential storage. Secrets never go in this repo.

## Paid infrastructure

- Domain: OAuth redirects, Google domain verification, privacy-policy hosting.
- OAuth backend hosting (Axum + Postgres): needed from Increment 1, since Meta and LinkedIn token
  exchange requires a client secret.
- Object storage for media staging, unless Instagram resumable upload removes the need.
- Code signing: Windows (Azure Trusted Signing or OV cert); Apple Developer Program for macOS.

## Legal and review material

- Privacy policy, terms of service, Meta data-deletion instructions/callback.
- App-review screencasts of the real OAuth flow (scripted by an agent, recorded by the owner).

## Ongoing duties

- Approving publications; resolving `OutcomeUnknown` publishes.
- Switching a YouTube channel to public uploads once the Google Cloud project passes the API
  audit.
- Re-authentication: Google refresh tokens expire after 7 days while the consent screen is in
  Testing (the app asks with a Reconnect button; see YouTube above); LinkedIn access tokens last ~60 days and programmatic refresh is partner-only.
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
