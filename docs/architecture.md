# Architecture

Short-form video curation and publishing: a local-first Rust desktop app that distributes
TikTok-origin content to YouTube Shorts, Instagram Reels, Snapchat Spotlight, and LinkedIn.

Status: proposed design (16 October 2026). Platform access and format requirements are
implementation-time verification points, not settled facts.

## Core design

One source video produces several independently managed publications. Each destination has its
own media variant, caption, schedule, approval, upload state, and published URL.

Components:

| Component | Role |
|---|---|
| Desktop interface | Tauri 2 + Svelte / TypeScript |
| Rust application core | Library, plans, durable scheduler |
| SQLite | Metadata and job checkpoints |
| Media worker | FFprobe / FFmpeg / file cache |
| Platform adapters | Upload / process / publish |
| Manual handoff | Video + copy + publishing steps (export package) |
| OAuth backend | Holds confidential client credentials |
| Media staging | Temporary HTTPS video delivery for providers that fetch a URL |
| Optional cloud runner | Single execution owner per post |

Design commitments:

- **Local-first.** Curation, originals, editing, and job history live on the creator's computer.
- **Durable automation.** Interrupted work recovers from persisted checkpoints; uncertain publish
  outcomes are reconciled, not retried blindly.
- **Capability-aware.** Automatic publishing is exposed only when the connected account and the
  application have the required access.

Scope assumes creator-owned or authorized content, initially for individual creators and small teams.

## Platform integration boundaries

Use official publishing integrations, with a prepared export package when an account or
application lacks API access.

| Platform | Integration and design consequence |
|---|---|
| TikTok (source) | The Display API provides metadata, share links, and embeds, but no source-video download field. Import original files or authorized exports; a saved TikTok URL can remain a library item awaiting media. oEmbed covers link metadata without auth. The optional new-post detector (below) uses only `user.info.basic` and `video.list`. |
| YouTube Shorts | Upload through the YouTube Data API using resumable uploads. Validate the asset against Shorts format rules; uploading a video and being classified as a Short are distinct concerns. Implemented first (Increment 3, below). |
| Instagram Reels | Professional accounts linked to a Facebook Page publish through a media-container workflow (Instagram API with Facebook Login, which is what allows uploading the file itself). Model container creation, processing, and publication as separate steps. |
| Snapchat Spotlight | The Public Profile API supports Spotlight publication but requires application allowlisting. Verify access before enabling automation; otherwise offer export and manual handoff. Implemented as the fourth connector (below); unverified against a real account. |
| LinkedIn | Upload media through the Videos API, then reference it in a Posts API request. Check member vs organization permissions. Feed placement is outside the app's control. |

### Account capabilities

Maintain a capability record for every connected account: publishing access, accepted media
formats, native scheduling, analytics availability, and manual-handoff support. Check both granted
permissions and account eligibility. A missing capability is explained with a next action:
reconnect, change account type, request application access, or export a publishing package.

### Integration acceptance

Before committing to a connector, authenticate a test account and complete one end-to-end
publication. Record the media identifier, post identifier, processing states, and required
permissions.

## Curation and media workflow

| Stage | User experience |
|---|---|
| 1. Collect | Drag in videos, watch an export folder, or save TikTok links. Store creator, source URL, tags, notes, and permission information. |
| 2. Curate | Preview, rate, search, group into campaigns, detect duplicates. Original media is immutable source material. |
| 3. Adapt | Platform-specific trims, crops, subtitle tracks, covers, captions, hashtags, audio choices. |
| 4. Review | Final video, destination account, copy, and posting time shown together before approval. |
| 5. Schedule | An independent publication per destination, placed in a selected posting slot. |
| 6. Monitor | Processing status, published links, recoverable failures, and the action needed to unblock a job. |

### Rules and approvals

A rule can say: "When an approved video enters this collection, create YouTube and Instagram
publications in the next available slots." Unattended publishing is enabled explicitly per
collection and account.

As implemented (Increment 2):

- A collection rule fires when an asset enters the collection: added by hand, routed there from
  the watch folder (a preference), or getting its media while already a member. It creates one
  publication per destination in that account's next free posting slot (unscheduled when no
  slot is free) and leaves it `Draft` or `AwaitingApproval`.
- A rule fires at most once per asset and destination (`rule_firings`), so re-imports,
  duplicates, and leaving and re-entering the collection create nothing new.
- Every rule has an `effective_from` instant (default: when it was created or turned back on).
  It only acts on assets that entered the collection at or after it, and whose source post (if
  any) went up at or after it, so turning a rule on never floods destinations with the back
  catalog. An earlier date is an explicit backfill choice. The UI states it in plain words
  ("Videos that enter … on or after … get publications for …").
- The default caption reuses the source caption (`{caption}`: the TikTok caption, hashtags
  included, else the title); each publication's copy can still be edited before approval.
- Each rule has an activity view, like a workflow's content list: every asset it handled with
  each destination's state and next action, plus members still waiting for their file.
- Unattended publishing is off by default. It needs an explicit grant for the collection and
  account *and* an account that can publish automatically; export-package accounts always
  require approval. Under a grant the rule approves only what it created, once, after the
  variant is rendered and the review shows no issues; the approval is recorded as
  `rule:<name>`. Anything else waits for the user.

Approval binds to an **immutable publication revision**. Any change to approved media, copy, or
destination invalidates that approval and returns the publication to review.

### Workflows UX (owner decision, 21 October 2026)

After the first real upload the owner found the collection/rule/review flow had too many manual
steps (render, permissions, approving each post, re-typing captions) spread over too many
screens. **Decision: content flows automatically by default.** Connect accounts once, set up the
source, and every new video is published to every connected destination with its source
caption and hashtags. Reviewing and customizing are optional advanced settings. The reference is
Repurpose.io's Workflows page.

- **A workflow is "source → one destination account"**, e.g. "TikTok → YouTube Shorts". The
  source is the owner's own videos: the watch folder, plus TikTok new-post detection for
  captions when connected. A workflow is a thin record (`workflows`, migration 0007) over the
  existing model, not a replacement: one `CollectionRule` with a single destination in the
  *source collection* (the collection the watch folder routes into; setup creates "Source
  videos" when there is none). At most one workflow per source collection and account.
  - *Auto Publish* on = an `UnattendedGrant` for that collection and account. Off = no grant,
    so publications wait at `AwaitingApproval` ("require review" in the settings drawer is the
    same switch, worded from the other side). This restores the earlier behaviour exactly.
  - *Active / Inactive* = the rule's `enabled`. Reactivating restarts it from now
    (`effective_from`), so pausing never floods destinations with what arrived meanwhile.
  - *Needs reconnect* = the account's capability record asks for a reconnect, or a publication
    for it waits at `NeedsAuthentication`. The fix is one click (Reconnect).
  - The workflow keeps its *settings*: posting (`as soon as possible`, the default, or the
    posting slots), caption template (default `{caption}`), extra hashtags, destination options
    (Instagram share to feed; YouTube public/private is the channel's setting) and the
    effective-from date (the rule's).
- **Rights.** Creating a workflow asks once: "I own or have the rights to content from this
  source". The confirmation is stored on the workflow; when the workflow's rule fires for an
  item whose video or audio permission is still *unknown*, both are recorded as granted with the
  note "Rights confirmed in workflow <name>". A recorded *denied* is never overwritten, so such
  an item still waits for review.
- **Zero-touch pipeline.** When a workflow's rule fires: the default variant is rendered (for
  every rule-created publication, not only unattended ones, so review is instant too); copy
  comes from the source (TikTok post caption when matched, else a sidecar `.txt`/`.json` next
  to the file, else a title from the file name); each destination's copy is fitted to its limits
  (`shorts_domain::copy::fit`: YouTube refuses `<`/`>` and uses the first line as title, Snapchat
  is cut to 160 characters, Instagram keeps ≤ 30 hashtags, LinkedIn escaping stays in the
  adapter) by dropping trailing hashtags first, then shortening the text with "…"; the posting
  time is now (or the next free slot; no free slot falls back to now). Under Auto Publish the
  workflow approves the publication's current revision once it is rendered and the review shows
  no issues, recorded as approved by `workflow:<name>`; an "as soon as possible" time that
  passed while rendering is moved to now first, so the overdue policy doesn't expire it.
  Anything the review flags (too long for Spotlight, a denied permission) waits with the reason
  and an Approve / Skip choice.
- **The invariant still holds.** Every approval, by the owner or a workflow, binds to one
  immutable revision; editing media, copy or destination makes a new revision and drops it. With
  Auto Publish on, the workflow approves the new revision again only if it created the
  publication and hasn't approved it before (`rule_firings.auto_approved_at`), so an edit made by
  hand goes back to the owner.
- **Navigation.** Workflows (home), Activity (every item, newest first, with each destination's
  status), Connections (one card per platform plus the source), Settings (rarely used options).
  Library, Collections, Review and Monitor stay reachable under Settings → Advanced. A first-run
  wizard (source → destinations → suggested workflows) shows until a workflow exists and can be
  reopened from Workflows.
- **Turning Auto Publish on** for a workflow also approves the items it is holding for review
  (the revisions it created), since that is what the owner just asked for.
- **Small rules.** A YouTube upload that stays private shows "Uploaded (private)" with a hint to
  make it public in YouTube Studio instead of "Published". Each connection problem shows a single
  next action.

### Automatic intake

Originals come in through the watch folder, typically a folder the phone or editor syncs to.
TikTok is used at most to notice that a post exists:

- **New-post detection** (optional, off by default, behind a setting). A poller in the desktop
  app calls the Display API. Tokens come from one of two sign-ins, both kept in the OS
  credential store:
  - **Direct** (default since 21 October 2026, `shorts_adapters::tiktok_oauth`). Researched in
    TikTok's "Login Kit for Desktop" and "Manage user access tokens" pages (21 October 2026):
    desktop apps register a redirect on `localhost`/`127.0.0.1` with an explicit (or `*`) port
    and must use PKCE (`code_challenge_method=S256`), but the token endpoint
    (`open.tiktokapis.com/v2/oauth/token/`) still takes `client_secret` alongside the
    `code_verifier`. So there is no secret-free public-client flow; instead the owner pastes
    their own TikTok app's client key and secret (OS credential store, `tiktok_app`), exactly
    like the Snap OAuth app, and the desktop listens on `http://127.0.0.1:47616/callback/`,
    exchanges the code and refreshes tokens itself. No broker or paste page is needed.
    *Assumption to confirm:* TikTok's desktop page describes the challenge as the hex SHA-256
    of the verifier (not RFC 7636's base64url); hex is the default and
    `SHORTS_TIKTOK_PKCE=base64url` switches. A fixed port is used rather than `*` in case
    wildcard ports aren't honoured.
  - **Broker** (fallback when `SHORTS_BROKER_URL` is set and no app is pasted). The OAuth
    broker holds the client secret and does code exchange, refresh and revoke; the desktop
    starts sign-in at `<broker>/v1/tiktok/connect?state&challenge` and collects tokens from
    `/v1/tiktok/claim` with the verifier behind the challenge. Sign-ins made through it keep
    refreshing through it.
- Each new post becomes an asset awaiting media with title, caption, share URL, post time,
  duration and a cached copy of the cover image (`tiktok_posts`). Nothing else is downloaded.
- When a file is imported, files whose duration is within 1.5 s of a pending post and that
  arrived within 72 hours of it are suggested as its original. Confirming merges them: the
  file's asset takes the post's details (its caption wins over a sidecar) and collections (so
  rules fire) and the placeholder is removed. Dismissed pairs aren't suggested again. Once a
  workflow exists, unambiguous pairs (the post's only candidate, which is no other post's
  candidate) are confirmed automatically after each import and TikTok check; ambiguous ones
  still wait for the user.
- **Caption source for workflows.** The matched TikTok post's caption and hashtags; else a
  sidecar next to the file (`<name>.json` with `caption`/`description`/`text`/`title` and
  `hashtags`/`tags`, or `<name>.txt`), stored as the asset's notes; else a title from the file
  name. While TikTok detection is connected, a workflow holds a new file that has no caption of
  its own for up to two hours (`CAPTION_WAIT_MINUTES`) so its post can be matched first; each
  watch-folder rescan and TikTok check re-runs rules for recent items, so it goes out once the
  wait ends either way.
- Without a TikTok developer app (pasted, or on a configured broker) the feature reports itself
  unavailable with the next step (see `docs/human-scope.md`).
- The watch folder and the TikTok poller both show when they last checked and offer "Check
  now". The watch folder also relists itself every 10 minutes, since sync clients don't always
  raise file events.
- No media fetcher exists: originals never come from TikTok. If official access to original
  files ever appears, a fetcher would attach the file to the awaiting item through the normal
  import path (`import_file` with `into`), leaving matching and rules unchanged.

#### Originals downloaded from TikTok

- The downloads folder can be watched in a second mode (`WatchKind::Downloads`): only files
  modified after watching starts are considered, and a video is imported only when
  `pick_post_for_download` attributes it to a pending TikTok post. It is then attached to that
  post's waiting item (`import_file(path, into)`), keeping its caption, link and collections,
  and the post is marked matched. Anything else is left in place, unimported.
- "Get original" (`App::expect_original`) opens the post and remembers it for 30 minutes; a
  requested post whose duration fits wins, even if it is older than the 72-hour window that
  unrequested downloads (and the watch folder's matching) use. Without a request, the file must
  fit exactly one pending post.
- The owner clicks TikTok's own "Download video"; the app never automates TikTok's site.

### Media pipeline

- Inspect originals with FFprobe.
- Transformations are versioned **recipes**: trim range, crop, subtitles, audio selection, encoding
  settings.
- Render with bounded FFmpeg subprocesses; cache by source checksum + recipe + encoder version.
- Initial preset: vertical 1080x1920 H.264/AAC MP4. Validate each destination's current
  requirements. Reuse a render across destinations only when it satisfies every relevant adapter.
- Preserve clean masters. Track audio permission separately from video permission. Renders and
  thumbnails are regenerable; originals and publication history are durable.

## Persistence and reliable execution

| Entity | Responsibility |
|---|---|
| Asset | Logical source video, provenance, tags, permission records |
| MediaFile | Original bytes, checksum, technical properties, storage location |
| Variant | Edit recipe and rendered output derived from an original |
| ConnectedAccount | Provider identity, permissions, capabilities, credential reference |
| Publication | Destination account, immutable revision, caption, schedule, approval |
| Job | Durable unit of work with a lease, retry time, and checkpoint |
| PublishAttempt | Upload sessions, provider requests, remote identifiers, outcomes |
| MetricSnapshot | Timestamped provider metrics with their original definitions preserved |

### Publication state machine

```
Draft > AwaitingApproval > Scheduled > Preparing > Uploading > Processing > Publishing > Published
```

Additional states: `RetryScheduled`, `NeedsAuthentication`, `NeedsUserAction`, `OutcomeUnknown`,
`Failed`, `Cancelled`.

### Worker guarantees

- Claim jobs with transactional leases, renewed by heartbeats.
- Persist checkpoints after meaningful provider operations.
- Retry transient failures with jitter; honor rate-limit responses; pause on invalid submissions or
  expired credentials for corrective action.
- On restart, recover interrupted jobs from checkpoints.
- Track each destination independently so one failure doesn't hide other successes in a campaign.

### Duplicate prevention

A lost publish response can mean the platform accepted the post. Mark the outcome
`OutcomeUnknown`, reconcile using available remote identifiers, and require user resolution when
reconciliation is unavailable. Exactly-once publishing cannot be guaranteed across all providers.

## Implementation and operating modes

| Responsibility | Implementation |
|---|---|
| Desktop and UI | Tauri 2 with Svelte / TypeScript. Windows first, then macOS. |
| Application core | Rust crates with explicit domain, media, scheduler, and provider boundaries. |
| I/O and persistence | Tokio; Reqwest with typed payloads; SQLx + SQLite with migrations and WAL mode. |
| Media and diagnostics | Packaged FFprobe / FFmpeg subprocesses; structured `tracing` with credential redaction. |
| Credentials and backend | OS credential storage locally. Rust / Axum backend with encrypted credential storage; PostgreSQL for cloud execution. |

### Adapter contract

```
capabilities(account)
validate(publication, media)
begin_upload(publication)
upload_step(checkpoint)
check_processing(remote_media)
publish(publication, remote_media)
reconcile(attempt)
fetch_metrics(remote_publication)
```

Provider-specific checkpoints and metadata stay typed inside each adapter. Upload, processing, and
publish are separate operations so interrupted work resumes at the correct step.

### Desktop and cloud scheduling

- Desktop mode executes while the app or tray process is active. Overdue jobs follow a user-selected
  catch-up or expiry policy.
- Cloud mode executes approved publications while the computer is off. Use native platform
  scheduling where available.
- The cloud runner reuses the Rust domain logic and adapters. Each publication has exactly one
  execution owner so desktop and cloud workers never both publish it.
- Execution instants are stored in UTC; the user's timezone is preserved for recurring schedules.

### OAuth and temporary hosting

- Browser-based authorization, state validation, and PKCE where supported.
- The backend protects confidential OAuth client secrets. No shared secrets in the desktop binary;
  no tokens in the webview.
- Exception: Google issues installed-app ("Desktop app") clients whose secret is explicitly not
  confidential. The owner creates their own and pastes it into the app; the desktop does the
  loopback sign-in and token refresh with Google directly, and keeps the client and refresh
  tokens in the OS credential store.
- Snapchat follows the same owner-supplied pattern with a confidential client: the owner
  creates their own Snap OAuth app and pastes its id and secret, which stay in the OS credential
  store on the owner's computer and are only sent to Snap's token endpoint. The browser sign-in
  returns either to a loopback port or to a static page that shows the code for pasting
  (below). Nothing is shipped in the binary, so this is not a shared secret.
- For providers that fetch a video URL, stage only the required variant at an unguessable HTTPS URL,
  keep it through processing and retries, then delete it under a defined retention policy.

### YouTube Shorts connector (Increment 3, first slice)

- **Sign-in.** Google's installed-app flow: the desktop listens on `http://127.0.0.1:<random
  port>`, opens the consent page with PKCE (S256), a random `state`, `access_type=offline` and
  `prompt=consent`, ignores loopback requests with any other state, and exchanges the code for
  tokens. Scopes: `youtube.upload` and `youtube.readonly`. The owner's OAuth client and each
  channel's refresh token and channel id live in the OS credential store
  (`google_oauth_client`, `youtube:<account id>`); SQLite holds only the account, its capability
  record and a non-secret status record. Access tokens stay in memory.
- **Capabilities.** A connected channel publishes automatically with `private_only` set until
  the owner says the project passed YouTube's audit; the record carries plain-language notes
  (visibility, quota, 7-day sign-in in Testing). A refused refresh (`invalid_grant`) moves the
  publication to `NeedsAuthentication` with a Reconnect action; reconnecting resumes it.
  Disconnecting revokes the grant and leaves the destination on export packages.
- **Upload.** Resumable `videos.insert` with title, description, tags (including a marker tag
  derived from the publication id and revision), privacy (private while `private_only`, else
  public) and `selfDeclaredMadeForKids: false`. Chunks are 8 MiB. The checkpoint holds the
  session URL and confirmed offset; a new process, or any failed chunk, asks the session
  (`Content-Range: bytes */N`) before sending more, so a restart resumes mid-file and a lost
  final response is read back. An expired session is searched for by the marker among recent
  uploads before a new session is opened.
- **Processing and publish.** `videos.list` (`status`, `processingDetails`) until processed or
  failed. Privacy is fixed at upload time (the narrow scope can't change it later), so publish
  confirms the video and records its URL (`youtube.com/shorts/<id>`) with a note if it is
  private. Reconcile looks the video up by id (or by session and marker when no id was
  recorded) and otherwise leaves the outcome to the user.
- **Quota.** Spend is recorded per bucket (`youtube:uploads`, `youtube:units`) and Pacific-time
  day. A call that would exceed the limits (or an upload without enough units left to watch it
  process) waits until midnight Pacific; so does a `quotaExceeded` answer, which also marks the
  day as used up. `rateLimitExceeded` waits a minute or `Retry-After`. Rate-limit waits don't
  count against the retry budget.
- **Limits** (`shorts_domain::youtube`): Shorts up to 3 minutes, square or vertical; files up to
  256 GB in a supported container; title ≤ 100 characters and description ≤ 5,000 bytes, without
  `<` or `>`.

### Instagram Reels connector (Increment 3, second slice)

Switched to Facebook Login on 21 October 2026, after the first real test: with Instagram Login
(`graph.instagram.com`) Instagram refused the resumable container ("The parameter video_url is
required"). Meta's resumable-upload docs offer uploading the file itself only to apps with
Facebook Login for Business; the alternative, a public `video_url`, would need hosting.

- **Sign-in.** The Instagram professional account is linked to a Facebook Page; the owner's Meta
  app uses "API setup with Facebook login". The owner generates a user token for that app in
  Graph API Explorer (`instagram_basic`, `instagram_content_publish`, `pages_show_list`,
  `pages_read_engagement`, `business_management`), extends it to a long-lived token in the
  Access Token Debugger and pastes it. No redirect address, hosting or app secret is needed;
  the app stays in Development mode with the owner as its admin, so no App Review.
- **Checks, one next action each.** `GET /debug_token?input_token=<token>` with the token
  itself as caller (allowed when its user is a developer of the app) gives the granted scopes
  and expiry; if Meta refuses that, `GET /me/permissions` gives the scopes and the owner is told
  how to read the expiry in the Access Token Debugger. Missing scopes are named (the four
  above; `business_management` is only mentioned when no Page is found, since only Pages in a
  business portfolio need it). A token with under a day left is short-lived: extend it. An
  `IGAA…` token is an Instagram Login token. `GET /me/accounts?fields=name,access_token,
  instagram_business_account{id,username}` lists the Pages: none → choose the Page when
  generating the token; none with `instagram_business_account` → link it in the Page's
  settings → Linked accounts. One linked Page is picked automatically; with several the owner
  picks.
- **Stored.** The chosen Page's access token (it doesn't expire when derived from a long-lived
  user token, so there is no renewal), the `instagram_business_account` id, the username and
  the Page's name live in the OS credential store (`instagram:<account id>`, with
  `login: facebook_login`); the user token is not kept. A token Meta ends anyway (190) moves
  publications to `NeedsAuthentication` with Reconnect; pasting a new token (matched to the
  same account by Instagram id or username) resumes them.
- **Instagram Login fallback.** Still selectable as the connection type (`GET
  graph.instagram.com/me` identifies the account), but such accounts can't upload files, so
  their capability record is `ManualOnly` with `Reconnect` ("reconnect with Facebook login")
  and publications use export packages. Accounts connected that way before the switch (grants
  without `login`) get that record on start; their workflows and Auto Publish grants stay and
  publish again once reconnected. No schema migration: grants and the per-account status live
  in the credential store and settings JSON, read with defaults.
- **Upload.** `POST graph.facebook.com/<version>/{ig-user-id}/media` with
  `media_type=REELS`, `upload_type=resumable`, the caption and `share_to_feed` (Bearer Page
  token) creates a container; the file goes to
  `rupload.facebook.com/ig-api-upload/<version>/<container>` (`Authorization: OAuth <token>`,
  `offset`, `file_size`). Before each send the adapter reads the container's
  `video_status.uploading_phase.bytes_transferred` and sends from that offset, so an
  interrupted upload continues and one that fully arrived isn't sent again; if the field can't
  be read the whole file is sent from 0 (re-sending replaces the bytes). A checkpoint older than
  23 hours opens a new container (containers expire after 24).
- **Processing and publish.** The container's `status_code` is polled until `FINISHED`
  (`ERROR`/`EXPIRED` fail the attempt). Publish checks `content_publishing_limit` (100 API
  posts per 24 hours), calls `media_publish`, then reads the reel's permalink. Reconcile reads
  the container: `PUBLISHED` → the reel is found among the account's recent media by caption
  digest; `FINISHED` → nothing was published, so publishing again is safe. All on
  `graph.facebook.com`.
- **Errors.** Graph codes 190/102/463/467 ask for a reconnect, with code 190's subcode named
  (458 app removed, 459 checkpoint, 460 password changed, 463 expired, 464 unconfirmed user,
  467 invalidated, 492 no longer a Page admin); 10/200–299 ask for a reconnect with the
  permissions named; 4/17/32/613/80002 (and 429) pause 15 minutes; subcode 2207042 (posting
  limit) pauses an hour. Transient or 5xx errors retry; other 4xx answers stop at
  `NeedsUserAction` (Retry there continues once fixed).
- **Cloud runner.** The credential carries `login`; the runner uses Page tokens as they are and
  asks for a reconnect on the desktop for Instagram Login credentials. It no longer renews
  Instagram tokens.
- **Verified vs assumed** (no network access to Meta from the build; checked against the docs
  as quoted by the owner and Meta's public pages): container creation, the rupload address and
  its headers, Facebook Login being required for resumable uploads, `/me/accounts` fields and
  Page tokens not expiring are from Meta's docs. Assumed until the smoke test: the exact shape
  of `video_status.uploading_phase.bytes_transferred` on an Instagram container (only seen in a
  forum report), `/debug_token` accepting the user token as caller for an app admin, Bearer
  headers on `graph.facebook.com`, and the Instagram Login `user_id` equalling the
  `instagram_business_account` id (the username is matched as well).
- **Limits** (`shorts_domain::instagram`): MP4/MOV, 3 s to 15 min, 300 MB, any aspect ratio
  between 0.01:1 and 10:1, caption ≤ 2,200 characters, ≤ 30 hashtags and ≤ 20 @mentions. The
  Graph API version defaults to `v24.0` (`SHORTS_INSTAGRAM_API_VERSION` overrides it).
- **Later.** The same Page connection could publish Facebook Reels (`/{page-id}/video_reels`);
  not in scope.

### LinkedIn Company Page connector (Increment 3, third slice)

- **Sign-in.** The owner pastes an access token generated with the LinkedIn developer portal's
  token generator for their app (which has the Community Management API), with
  `w_organization_social`, `r_organization_social` and `rw_organization_admin`, and picks the
  page: the app lists the pages the token's member administers (`organizationAcls`,
  role `ADMINISTRATOR`), or takes the page's numeric id / admin address when listing isn't
  allowed. No redirect, hosting or client secret is needed. The token and the page URN live in
  the OS credential store (`linkedin:<account id>`).
- **Expiry.** Tokens last 60 days and programmatic refresh is limited to some partner programs,
  so the app shows a warning in the last week and moves publications to `NeedsAuthentication`
  once the token has run out or is refused; pasting a new token reconnects the page and
  resumes them.
- **Upload.** `POST /rest/videos?action=initializeUpload` (owner = the page) returns the video
  URN and one pre-signed address per part; each upload step `PUT`s one part and keeps its
  ETag, and the last calls `finalizeUpload` with the ETags. The checkpoint holds the parts and
  ETags, so a restart resumes at the next part; a part address LinkedIn refuses (expired)
  starts a new upload.
- **Processing and publish.** `GET /rest/videos/{urn}` until `AVAILABLE`
  (`PROCESSING_FAILED` fails the attempt). The post is `POST /rest/posts` authored by the page,
  `visibility: PUBLIC`, `feedDistribution: MAIN_FEED`, the video as `content.media` with a
  title from the first caption line, and the caption as "little text" commentary (reserved
  characters escaped, hashtags as `{hashtag|\#|name}`). The post URN comes from `x-restli-id`;
  the link is `linkedin.com/feed/update/<urn>/`. Reconcile lists the page's recent posts
  (`q=author`) and looks for the uploaded video; if the posts can't be read, the user decides.
- **Errors.** 401 and 403 ask for a reconnect (403 names the scopes), 429 waits until midnight
  UTC (or `Retry-After`), 426 says the API version is retired (`SHORTS_LINKEDIN_API_VERSION`,
  default `202609`), 5xx retries.
- **Limits** (`shorts_domain::linkedin`): MP4, 3 s to 30 min, 75 KB to 500 MB, commentary ≤ 3,000
  characters after escaping. Metrics aren't collected yet (they need the organization share
  statistics API).

### Snapchat Spotlight connector (Increment 3, fourth slice)

Built from Snap's public Public Profile API docs (Get Started, authentication, Profile Asset
Management, Spotlight) as read on 17 October 2026, without an allowlisted app to test against.
The list at the end says which details are assumptions to confirm in the first smoke test.

- **Sign-in.** OAuth 2 authorization code with the owner's Snap OAuth app, created in Ads
  Manager (Business Details → OAuth apps), scope `snapchat-profile-api`, a random `state` and
  the client secret at the token endpoint (no PKCE; it is a confidential client). The redirect
  address is whatever the owner registered with Snap and saved in Settings, and it decides how
  the code comes back:
  - `http://127.0.0.1:<port>/…` (default `http://127.0.0.1:47615/snapchat/callback`): the app
    listens on that fixed port (Snap needs an exact match, so it can't be random) and ignores
    requests with any other state, like the YouTube sign-in;
  - any `https://` page, intended for `site/snap-callback.html` on GitHub Pages if Snap refuses
    `http`: the static page reads `state` and `code` from its query string in the browser and
    shows `<state>.<code>`; the owner pastes it into Settings, which checks the state against
    the sign-in it started and exchanges the code. The page makes no requests (CSP
    `default-src 'none'`), suppresses the referrer and removes the code from the address bar.
    The code alone is useless without the client secret, which never leaves the owner's PC.
  After the exchange, `GET /v1/public_profiles/my_profile` identifies the profile. The OAuth
  app (`snap_oauth_client`: client id, secret, redirect address) and each profile's refresh
  token and profile id (`snapchat:<account id>`) live in the OS credential store; SQLite holds
  only the account, its capability record and a non-secret status record. The redirect address
  is shown in Settings; the secret is never returned to the webview.
- **Tokens.** Access tokens last an hour and are kept in memory; the connector refreshes them
  with the refresh token and client secret when one is needed (two minutes before expiry or on
  a 401, once), storing a rotated refresh token if Snap returns one. `invalid_grant` or a
  refused client marks the profile *needs reconnecting* and moves publications to
  `NeedsAuthentication`; reconnecting the same profile resumes them. Disconnecting deletes the
  grant and leaves the destination on export packages.
- **Upload.** A fresh AES-256-CBC key and IV per upload (PKCS#7 padding). `POST
  /v1/public_profiles/{id}/media` with `type: VIDEO`, a name, and the base64 key and IV creates
  the media and returns `media_id`, `add_path` and `finalize_path`. The ciphertext goes up in
  parts of up to 32 MB (at most 35) as multipart `POST`s to `add_path` (`action=ADD`,
  `part_number`, `file`), then `finalize_path` (`action=FINALIZE`). The checkpoint holds the
  key, IV, part count and parts sent, so a restart re-encrypts the same file to the same bytes
  and continues with the next part; re-sending a part is treated as harmless. Media lasts 24
  hours, so a checkpoint older than 23 hours creates new media.
- **Processing and publish.** Snap has no media status call, so processing is reported ready
  and problems surface when posting. Publish is `POST /v1/public_profiles/{id}/spotlights`
  with `media_id`, `locale` (`en_US`, `SHORTS_SNAPCHAT_LOCALE` overrides it) and the caption
  plus hashtags on one line, cut to 160 characters. The id comes from `spotlight_id` (or
  `id`); a success without one is `OutcomeUnknown`. Snap reviews Spotlight posts, so there is
  no public link to record. Reconcile lists the profile's Spotlight posts and accepts one that
  names this media id; otherwise the user decides, since "not listed" can't prove "not posted".
- **Errors.** 401 retries once with a fresh token, then asks for a reconnect; 403 asks for a
  reconnect and names allowlisting; 429 waits `Retry-After` or a minute; 5xx retries;
  `MEDIA_EXPIRED` asks to publish again; other 4xx are invalid submissions.
- **Limits** (`shorts_domain::snapchat`): MP4, 6 to 60 s, at least 540×960
  (`ValidationIssue::ResolutionTooLow`), up to 1 GB, description ≤ 160 characters. Metrics
  aren't collected.

**From Snap's docs:** the Ads Manager OAuth app, the authorize URL and the
`snapchat-profile-api` scope, hourly access tokens refreshed with the client secret, the
`businessapi.snapchat.com` base, `my_profile`, AES-256-CBC with a per-upload key and IV, the
media → `add_path` / `finalize_path` flow with 32 MB parts, the 24-hour media lifetime, the
Spotlight endpoint and its review, and the format limits above.

**Assumptions to confirm** (the first smoke test settles them; each is a small change in
`crates/adapters/src/snapchat.rs`):

1. **Media creation body.** The adapter sends JSON `{type, name, key, iv}` with base64 key and
   IV, and reads `media_id`, `add_path` and `finalize_path` from anywhere in the answer. The
   docs' exact field names, nesting (e.g. a `media: [...]` wrapper) and encoding are unverified.
2. **Encrypted or plain parts.** The adapter uploads the ciphertext in parts (and sizes the part
   count from the padded length). If Snap expects the plain file in the parts and only uses the
   key and IV for storage, uploads will fail or the video won't play; switch `encrypted()` to
   the plain bytes.
3. **Token URL.** `https://accounts.snapchat.com/login/oauth2/access_token` is used for both the
   code exchange and refresh. If the exchange returns 404 and Snap's current page names
   `…/oauth2/token` instead, change `TOKEN_URL`.
4. **Spotlight listing.** Reconcile assumes `GET /v1/public_profiles/{id}/spotlights` lists
   posts with the media id they were made from. If it doesn't, reconcile always leaves the
   outcome to the user (safe, but manual).
5. Also unconfirmed: whether Snap accepts an `http://127.0.0.1` redirect (else the paste page),
   whether refresh tokens rotate, the `my_profile` field names (`id`, `display_name`,
   `snap_user_name`), and the error body fields (`error_code`, `display_message`).

### Cloud runner (Increment 4, first slice)

Optional. With no runner configured (the default) the desktop behaves exactly as before: every
publication is desktop-owned and nothing talks to a server. Hosting and operations:
`docs/cloud-runner.md`.

- **Crates.** `shorts-runner` is a new crate (library plus the `shorts-runner` binary) rather
  than a mode of `shorts-backend`: the broker is public-facing, stateless and holds only OAuth
  client secrets, while the runner holds per-account credentials, media and PostgreSQL, and the
  owner may host either without the other. Shared pieces move out of the desktop:
  `shorts-publish` holds the publish stage machine (prepare → upload → process → publish →
  reconcile, the checkpoint and the never-re-publish rule) behind a `PublishHost` trait that the
  desktop (SQLite, renders, export packages) and the runner (PostgreSQL, uploaded media)
  implement; `shorts-scheduler`'s `Worker` drives a `JobQueue` trait, implemented for the SQLite
  store and the runner's PostgreSQL store; `shorts-cloud` holds the sync protocol types and the
  HTTPS client the desktop (and the runner's tests) use.
- **Execution owner.** Each publication carries `owner` (`desktop` or `cloud`) and, while the
  cloud owns it, a `handover` id. Ownership only changes through domain events:
  `HandOver { to: Cloud, handover }` is allowed from `Scheduled` with a valid approval;
  `HandOver { to: Desktop }` from any state that isn't active. Starting execution
  (`BeginPreparing`, `Resume`) names the executor and is refused unless it is the owner, and
  user events (edit, approve, reschedule, cancel, retry, resolve…) are refused while the cloud
  owns the publication. The desktop shows the runner's progress through `Mirror { report }`,
  accepted only while the cloud owns it under the same handover and revision.
- **No double execution.** The desktop hands over in one SQLite `BEGIN IMMEDIATE`
  transaction: it applies `HandOver`, and cancels the queued publish job, refusing if that job
  is running or its checkpoint shows the upload already reached the platform. The desktop's
  claim query skips cloud-owned publications, and the publish handler re-checks the owner, so a
  queued job that slipped through abandons. On the runner, claiming takes the job row with
  `FOR UPDATE SKIP LOCKED` and requires `owner = cloud`; releasing locks the same row, so a
  claim and a release can't both win. Release (taking it back) is refused while the runner's
  job is running, the publication is active, or a `RetryScheduled` / `NeedsAuthentication`
  attempt already reached the platform (the runner finishes those itself, after a reconnect if
  needed); `OutcomeUnknown`, `NeedsUserAction`, `Failed`, `Published` and untouched `Scheduled`
  publications can come back. Only then does the desktop mirror the final report and apply
  `HandOver { to: Desktop }`.
- **Sync protocol** (HTTPS, `Authorization: Bearer <installation token>`; the desktop refuses
  plain `http://` except to a loopback address). All writes are idempotent:
  - `POST /v1/uploads {sha256, size}`, `PUT /v1/uploads/{sha256}?offset=N` (chunks up to
    8 MiB), `GET /v1/uploads/{sha256}`: content-addressed, resumable media upload. A wrong
    offset answers `409` with the server's offset; the last chunk is verified against the
    SHA-256 before the file counts as received.
  - `PUT /v1/accounts/{id} {account, credential, base_revision}`: the account record and its
    credential. Credentials have a revision; if the runner rotated the credential since
    `base_revision` (Snap and sometimes Google rotate refresh tokens) it answers `409` and the
    desktop first pulls `GET /v1/accounts/{id}/credential` into its OS credential store. A new
    credential resumes the runner's `NeedsAuthentication` publications for that account.
  - `PUT /v1/publications/{id} {handover, publication, media}`: the approved,
    immutable revision. Same handover → same answer; a different revision under a live
    handover → `409`; a handover the desktop already took back → `410`.
  - `GET /v1/publications` / `GET /v1/publications/{id}`: owner reports (state, revision,
    retry time, error, user action, published URL, whether the attempt reached the platform).
    The desktop polls every 30 s while cloud mode is on, mirrors reports, and re-delivers any
    cloud-owned publication the runner doesn't list (so a lost delivery heals itself).
  - `POST /v1/publications/{id}/release {handover}`: take back; `409` with the reason while in
    flight, else the final report.
- **Runner storage.** PostgreSQL through `sqlx` migrations: installations (token SHA-256
  only), accounts, encrypted credentials, publications (the domain record as JSONB with
  queryable state/owner/version columns, optimistic concurrency), immutable revisions, events,
  jobs (same lease columns as SQLite), attempts, uploads and YouTube quota. Instants are
  `timestamptz` (UTC). Media lives on disk under the data directory, named by SHA-256, and is
  deleted once its publications are published or taken back.
- **Credentials at rest.** Envelope encryption: each credential write gets a fresh 256-bit data
  key; the JSON is sealed with AES-256-GCM under it (bound to the account id), and the data key
  is wrapped with AES-256-GCM under the master key from `SHORTS_RUNNER_MASTER_KEY` (base64, 32
  bytes; never stored, never logged). `SHORTS_RUNNER_OLD_MASTER_KEYS` keeps retired keys
  readable while `shorts-runner rotate-keys` re-wraps every data key under the current one.
  Secrets are only decrypted to build adapter sessions; token types redact themselves in
  `Debug`.
- **Tokens.** The runner refreshes as the desktop connectors do, with the same adapter
  clients: Google and Snap access tokens from refresh tokens (storing rotated refresh tokens),
  Instagram renewal when under 20 days are left (on use and every 6 hours), LinkedIn tokens
  used until they expire. A refused refresh pauses the publication at `NeedsAuthentication`;
  reconnecting on the desktop pushes the new credential.
- **Out of scope here.** Analytics, shared multi-user approvals, native platform scheduling,
  the broker's TikTok flow in the runner, and multi-runner high availability (one runner
  process per database is assumed; leases make a second one safe but not useful).

## Delivery plan

| Increment | Reviewable outcome |
|---|---|
| 1. Prove access | Authenticate test accounts and publish a sample through every available API. Start Snapchat allowlisting. |
| 2. Desktop workflow | File import, library curation, media variants, review, export packages, durable jobs. |
| 3. Publishing | YouTube and Instagram, then LinkedIn; Snapchat when access is confirmed. Restart recovery and duplicate prevention. |
| 4. Unattended use | Cloud execution, shared approvals, analytics where permissions allow. |

### Release acceptance

Exercise interrupted uploads, revoked credentials, rate limiting, lost publish responses,
daylight-saving transitions, and partial campaign success. Every failure must leave a clear status
and a safe recovery action.

## Primary sources (checked 16 October 2026; YouTube and Google OAuth rechecked 17 October 2026)

1. TikTok — Video Object
2. YouTube Data API — Videos: insert; Resumable uploads; Errors; Quota costs; Revision history
   (granular quota buckets, June 2026)
3. YouTube Help — Understand three-minute Shorts
4. Meta — Instagram API documentation (official Postman collection)
5. Snap — Public Profile API: Get Started; authentication (OAuth apps in Ads Manager)
6. Snap — Profile Asset Management (encrypted media, add / finalize parts) / Spotlight
   publishing (read 17 October 2026)
7. LinkedIn — Videos API
8. LinkedIn — Posts API
9. Tauri — Process Model
10. Google Identity — OAuth 2.0 for iOS & Desktop Apps (loopback redirect, PKCE; 7-day refresh
    tokens for External apps in Testing)
