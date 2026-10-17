# Human scope

Work that needs the owner's identity, money, legal sign-off, credentials, or judgment, and so can't
be done by an agent. Platform rules below are as understood in mid-2026; confirm each during
Increment 1.

## Long-lead gates (start early)

| Platform | Action | Why it blocks |
|---|---|---|
| Snapchat | Apply for Public Profile API allowlisting; own a Snapchat Public Profile. | External approval with unknown turnaround. Until granted, Snapchat uses the export package. |
| YouTube | YouTube API compliance audit; Google OAuth verification for the `youtube.upload` scope. | Unaudited projects can only upload private videos. Default quota (~10k units/day, ~1,600 per upload) allows ~6 uploads/day; the audit form is also how quota is raised. |
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
- Re-authentication: Google refresh tokens expire after 7 days while the consent screen is in
  Testing; LinkedIn access tokens last ~60 days and programmatic refresh is partner-only.
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
