// Mirrors of the Rust views in crates/desktop/src/views.rs and the domain types they embed.
// Domain types (recipe, media properties, permissions) keep their snake_case field names.

export type Platform = "youtube_shorts" | "instagram_reels" | "snapchat_spotlight" | "linkedin";

export const PLATFORMS: Platform[] = [
  "youtube_shorts",
  "instagram_reels",
  "snapchat_spotlight",
  "linkedin",
];

export type PublicationState =
  | "draft"
  | "awaiting_approval"
  | "scheduled"
  | "preparing"
  | "uploading"
  | "processing"
  | "publishing"
  | "published"
  | "retry_scheduled"
  | "needs_authentication"
  | "needs_user_action"
  | "outcome_unknown"
  | "failed"
  | "cancelled";

export type Action =
  | "edit"
  | "submitForReview"
  | "review"
  | "approve"
  | "revokeApproval"
  | "reschedule"
  | "publishNow"
  | "cancel"
  | "retry"
  | "resolveUserAction"
  | "confirmPosted"
  | "resolveOutcome"
  | "exportPackage";

export type PermissionStatus = "unknown" | "granted" | "denied";

export interface Permission {
  status: PermissionStatus;
  note: string | null;
  recorded_at: string | null;
}

export interface MediaProperties {
  duration_ms: number;
  width: number;
  height: number;
  rotation: number;
  frame_rate: number | null;
  video_codec: string | null;
  audio_codec: string | null;
  audio_streams: number;
  container: string | null;
  bit_rate: number | null;
}

export interface TrimRange {
  start_ms: number;
  end_ms: number;
}

export interface CropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type FitMode = "pad" | "fill";
export type SubtitleMode = "burn_in" | "soft";

export interface SubtitleTrack {
  path: string;
  mode: SubtitleMode;
  language: string | null;
}

export type AudioChoice =
  | { kind: "original"; track: number }
  | { kind: "mute" }
  | { kind: "replace"; path: string };

export interface EncodingSettings {
  width: number;
  height: number;
  fps: number;
  crf: number;
  preset: string;
  audio_bitrate_kbps: number;
  audio_sample_rate: number;
}

export interface RenderRecipe {
  schema: number;
  trim: TrimRange | null;
  crop: CropRect | null;
  fit: FitMode;
  subtitles: SubtitleTrack | null;
  audio: AudioChoice;
  encoding: EncodingSettings;
}

export interface MediaView {
  id: string;
  path: string;
  originalName: string;
  checksum: string;
  sizeBytes: number;
  properties: MediaProperties;
}

export interface AssetSummary {
  id: string;
  title: string;
  creator: string | null;
  sourceUrl: string | null;
  notes: string;
  tags: string[];
  rating: number | null;
  status: "awaiting_media" | "ready";
  videoPermission: Permission;
  audioPermission: Permission;
  createdAt: string;
  media: MediaView | null;
  publicationCount: number;
  /** The TikTok post this item came from (new-post detection). */
  sourcePost: SourcePostView | null;
}

export interface SourcePostView {
  videoId: string;
  title: string;
  caption: string;
  postedAt: string;
  durationMs: number | null;
  shareUrl: string | null;
  coverPath: string | null;
  matched: boolean;
}

export interface MatchCandidateView {
  assetId: string;
  title: string;
  originalName: string;
  importedAt: string;
  durationMs: number;
}

export interface MatchSuggestion {
  postAssetId: string;
  post: SourcePostView;
  candidate: MatchCandidateView;
  durationDiffMs: number;
  hoursApart: number;
}

export type TikTokStatus =
  | { kind: "unavailable"; reason: string; nextStep: string }
  | { kind: "off" }
  | { kind: "notConnected" }
  | { kind: "connecting" }
  | {
      kind: "connected";
      displayName: string;
      connectedAt: string;
      lastCheckedAt: string | null;
      lastError: string | null;
      pendingPosts: number;
    }
  | { kind: "reconnect"; displayName: string; message: string };

export type ConnectProgress =
  | { kind: "pending" }
  | { kind: "connected"; status: TikTokStatus }
  | { kind: "failed"; message: string };

export interface CheckReport {
  newPosts: number;
  suggestions: number;
}

export interface RenderedView {
  path: string;
  sizeBytes: number;
  properties: MediaProperties;
  renderedAt: string;
}

export interface VariantView {
  id: string;
  assetId: string;
  name: string;
  revision: number;
  recipe: RenderRecipe;
  rendered: RenderedView | null;
  rendering: boolean;
  outputDurationMs: number;
}

export type NextAction = "reconnect" | "change_account_type" | "request_application_access" | "export_package";

export interface AccountView {
  id: string;
  platform: Platform;
  platformName: string;
  displayName: string;
  automatic: boolean;
  missingReason: string | null;
  nextAction: NextAction | null;
  manualHandoff: boolean;
}

export interface PublicationView {
  id: string;
  assetId: string;
  assetTitle: string;
  platform: Platform;
  platformName: string;
  accountId: string;
  accountName: string;
  state: PublicationState;
  revision: number;
  variantId: string;
  variantName: string;
  caption: string;
  hashtags: string[];
  scheduledAt: string | null;
  timezone: string | null;
  scheduledLocal: string | null;
  approvedRevision: number | null;
  approved: boolean;
  retryAt: string | null;
  lastError: string | null;
  packageDir: string | null;
  publishedUrl: string | null;
  statusText: string;
  actions: Action[];
  updatedAt: string;
}

export interface AssetDetail {
  asset: AssetSummary;
  collections: CollectionRef[];
  variants: VariantView[];
  publications: PublicationView[];
}

export type ValidationIssue = { kind: string } & Record<string, unknown>;

export interface ReviewView {
  publication: PublicationView;
  account: AccountView;
  variant: VariantView;
  videoPath: string | null;
  fullCaption: string;
  issues: ValidationIssue[];
  issueText: string[];
  warnings: string[];
  canApprove: boolean;
}

export interface JobView {
  id: string;
  kind: "render_variant" | "publish_publication" | "export_package";
  subject: string;
  state: "queued" | "running" | "paused" | "succeeded" | "failed" | "cancelled" | "expired";
  runAt: string;
  claims: number;
  failures: number;
  lastError: string | null;
  pauseReason: string | null;
  packageDir: string | null;
  updatedAt: string;
}

export interface MonitorView {
  publications: PublicationView[];
  jobs: JobView[];
}

export interface TagCount {
  tag: string;
  count: number;
}

export type OverduePolicy = { kind: "catch_up" } | { kind: "expire"; grace_secs: number };

export type Weekday = "Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun";

export const WEEKDAYS: Weekday[] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export interface PostingSlot {
  weekdays: Weekday[];
  /** `HH:MM:SS` local time. */
  time: string;
}

export interface Preferences {
  timezone: string;
  overduePolicy: OverduePolicy;
  watchFolder: string | null;
  exportDir: string | null;
  postingSlots: PostingSlot[];
  /** Watch-folder imports are added to this collection (and its rules run). */
  watchCollection: string | null;
}

export interface PreferencesUpdate {
  timezone?: string;
  overduePolicy?: OverduePolicy;
  watchFolder?: string | null;
  exportDir?: string | null;
  postingSlots?: PostingSlot[];
  watchCollection?: string | null;
}

export type ScheduleInput =
  | { kind: "local"; local: string; timezone?: string | null }
  | { kind: "now" }
  | { kind: "nextSlot" };

export interface LibraryQuery {
  text?: string;
  tags?: string[];
  minRating?: number;
  awaitingMedia?: boolean;
}

export interface PermissionInput {
  status: PermissionStatus;
  note?: string | null;
}

export interface AssetUpdate {
  title?: string;
  creator?: string;
  sourceUrl?: string;
  notes?: string;
  rating?: number;
  addTags?: string[];
  removeTags?: string[];
  videoPermission?: PermissionInput;
  audioPermission?: PermissionInput;
}

export interface NewPublication {
  assetId: string;
  variantId: string;
  accountId: string;
  caption: string;
  hashtags: string[];
  schedule: ScheduleInput | null;
}

export interface EditPublication {
  variantId?: string;
  caption?: string;
  hashtags?: string[];
  accountId?: string;
}

export type OutcomeResolution =
  | { kind: "published"; url: string | null; remote_id: string | null }
  | { kind: "not_published" };

export interface ImportResult {
  path: string;
  outcome: "imported" | "duplicate" | "failed";
  assetId: string | null;
  message: string | null;
}

export interface MediaStatus {
  available: boolean;
  message: string | null;
}

export type AppEvent =
  | { type: "libraryChanged" }
  | { type: "imported"; assetId: string; duplicate: boolean }
  | { type: "importFailed"; path: string; message: string }
  | { type: "publicationChanged"; id: string }
  | { type: "jobsChanged" }
  | { type: "renderProgress"; variantId: string; fraction: number }
  | { type: "watchFolderError"; message: string }
  | { type: "collectionsChanged" }
  | { type: "rulesFailed"; assetId: string; message: string }
  | { type: "tikTokChanged" }
  | { type: "matchSuggested"; assetId: string | null };

export interface CollectionRef {
  id: string;
  name: string;
}

/** What a rule-created publication starts as. */
export type RuleOutput = "draft" | "awaiting_approval";

export interface RuleDestinationView {
  accountId: string;
  accountName: string;
  platformName: string;
  /** The rule approves these publications itself (unattended publishing is on). */
  unattended: boolean;
}

export interface RuleView {
  id: string;
  name: string;
  enabled: boolean;
  destinations: RuleDestinationView[];
  output: RuleOutput;
  captionTemplate: string;
  hashtags: string[];
  includeAssetTags: boolean;
  firedCount: number;
}

export interface UnattendedView {
  accountId: string;
  accountName: string;
  platformName: string;
  enabled: boolean;
  available: boolean;
  unavailableReason: string | null;
  enabledAt: string | null;
}

export interface CollectionView {
  id: string;
  name: string;
  memberCount: number;
  rules: RuleView[];
  unattended: UnattendedView[];
  watchFolderTarget: boolean;
}

export interface RuleInput {
  name: string;
  enabled: boolean;
  destinations: string[];
  output: RuleOutput;
  captionTemplate: string;
  hashtags: string[];
  includeAssetTags: boolean;
}

export interface AppError {
  kind: string;
  message: string;
}
