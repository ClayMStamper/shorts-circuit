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
| TikTok (source) | The Display API provides metadata, share links, and embeds, but no source-video download field. Import original files or authorized exports; a saved TikTok URL can remain a library item awaiting media. oEmbed covers link metadata without auth. |
| YouTube Shorts | Upload through the YouTube Data API using resumable uploads. Validate the asset against Shorts format rules; uploading a video and being classified as a Short are distinct concerns. |
| Instagram Reels | Professional accounts publish through a media-container workflow (Instagram Login, no linked Facebook Page needed). Model container creation, processing, and publication as separate steps. |
| Snapchat Spotlight | The Public Profile API supports Spotlight publication but requires application allowlisting. Verify access before enabling automation; otherwise offer export and manual handoff. |
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

Approval binds to an **immutable publication revision**. Any change to approved media, copy, or
destination invalidates that approval and returns the publication to review.

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
- For providers that fetch a video URL, stage only the required variant at an unguessable HTTPS URL,
  keep it through processing and retries, then delete it under a defined retention policy.

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

## Primary sources (checked 16 October 2026)

1. TikTok — Video Object
2. YouTube Data API — Videos: insert
3. YouTube Help — Understand three-minute Shorts
4. Meta — Instagram API documentation (official Postman collection)
5. Snap — Public Profile API: Get Started
6. Snap — Profile Asset Management / Spotlight publishing
7. LinkedIn — Videos API
8. LinkedIn — Posts API
9. Tauri — Process Model
