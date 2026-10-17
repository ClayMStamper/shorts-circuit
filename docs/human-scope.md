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
| TikTok | Skip a TikTok app initially. | oEmbed gives link metadata without auth. |

## Accounts and sign-ins

- Developer accounts: Google Cloud, Meta for Developers, Snap Kit, LinkedIn Developers.
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
- Exporting original videos from TikTok or the editing tool into the watch folder.
- Rights calls, especially audio: TikTok sounds are often licensed for TikTok only.

## Open decisions

- LinkedIn: member, organization, or both.
- Default overdue-job policy: catch up or expire.
- Personal use vs distribution to others (drives signing, legal, and review depth).
