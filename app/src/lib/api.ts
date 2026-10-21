// Typed wrappers around the Tauri commands in app/src-tauri/src/commands.rs.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AccountView,
  ActivityView,
  AppError,
  AssetDetail,
  AssetSummary,
  AssetUpdate,
  CheckReport,
  CloudStatus,
  CollectionView,
  ConnectProgress,
  EditPublication,
  GetOriginal,
  ImportResult,
  InstagramLogin,
  InstagramPages,
  InstagramStatus,
  LinkedInOrganization,
  LinkedInStatus,
  LibraryQuery,
  MatchSuggestion,
  MediaStatus,
  MonitorView,
  NewPublication,
  OutcomeResolution,
  Platform,
  Preferences,
  PreferencesUpdate,
  PublicationView,
  RenderRecipe,
  ReviewView,
  RuleActivityView,
  RuleInput,
  ScheduleInput,
  SnapConnectProgress,
  SnapStatus,
  TagCount,
  TikTokAppInfo,
  TikTokStatus,
  VariantView,
  NewWorkflow,
  WorkflowDetailView,
  WorkflowUpdate,
  WorkflowView,
  WorkflowsView,
  YouTubeConnectProgress,
  YouTubeStatus,
} from "./types";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

let invokeImpl: Invoke = tauriInvoke;

/** Replaces the transport (tests use an in-memory fake). */
export function setInvoke(fn: Invoke): void {
  invokeImpl = fn;
}

/** Normalizes whatever a command rejected with into an `AppError`. */
export function toAppError(e: unknown): AppError {
  if (e && typeof e === "object" && "message" in e) {
    const obj = e as { kind?: unknown; message: unknown };
    return { kind: typeof obj.kind === "string" ? obj.kind : "error", message: String(obj.message) };
  }
  return { kind: "error", message: String(e) };
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invokeImpl<T>(cmd, args);
  } catch (e) {
    throw toAppError(e);
  }
}

export const api = {
  // Workflows (the home screen) and activity
  listWorkflows: () => call<WorkflowsView>("list_workflows"),
  workflowDetail: (id: string) => call<WorkflowDetailView>("workflow_detail", { id }),
  createWorkflow: (workflow: NewWorkflow) => call<WorkflowView>("create_workflow", { workflow }),
  updateWorkflow: (id: string, update: WorkflowUpdate) => call<WorkflowView>("update_workflow", { id, update }),
  deleteWorkflow: (id: string) => call<void>("delete_workflow", { id }),
  checkForNewContent: () => call<WorkflowsView>("check_for_new_content"),
  activity: (limit?: number) => call<ActivityView>("activity", { limit: limit ?? null }),
  publishNow: (id: string) => call<PublicationView>("publish_now", { id }),
  republish: (id: string) => call<PublicationView>("republish", { id }),

  // Library
  listAssets: (query: LibraryQuery) => call<AssetSummary[]>("list_assets", { query }),
  assetDetail: (id: string) => call<AssetDetail>("asset_detail", { id }),
  updateAsset: (id: string, update: AssetUpdate) => call<AssetSummary>("update_asset", { id, update }),
  tagCounts: () => call<TagCount[]>("tag_counts"),
  thumbnail: (id: string) => call<string | null>("thumbnail", { id }),
  importFiles: (paths: string[], into?: string) =>
    call<ImportResult[]>("import_files", { paths, into: into ?? null }),
  saveLink: (url: string, title: string | null) => call<string>("save_link", { url, title }),

  // Variants
  createVariant: (assetId: string, name: string, recipe: RenderRecipe) =>
    call<VariantView>("create_variant", { assetId, name, recipe }),
  updateVariant: (id: string, name: string, recipe: RenderRecipe) =>
    call<VariantView>("update_variant", { id, name, recipe }),
  renderVariant: (id: string) => call<void>("render_variant", { id }),

  // Accounts
  listAccounts: () => call<AccountView[]>("list_accounts"),
  addAccount: (platform: Platform, displayName: string) =>
    call<AccountView>("add_account", { platform, displayName }),
  renameAccount: (id: string, displayName: string) =>
    call<AccountView>("rename_account", { id, displayName }),

  // Publications
  createPublication: (publication: NewPublication) =>
    call<PublicationView>("create_publication", { publication }),
  editPublication: (id: string, edit: EditPublication) =>
    call<PublicationView>("edit_publication", { id, edit }),
  submitForReview: (id: string) => call<PublicationView>("submit_for_review", { id }),
  review: (id: string) => call<ReviewView>("review", { id }),
  approve: (id: string, revision: number) => call<PublicationView>("approve", { id, revision }),
  revokeApproval: (id: string) => call<PublicationView>("revoke_approval", { id }),
  schedule: (id: string, input: ScheduleInput) => call<PublicationView>("schedule", { id, input }),
  cancelPublication: (id: string) => call<PublicationView>("cancel_publication", { id }),
  retryPublication: (id: string) => call<PublicationView>("retry_publication", { id }),
  resolveUserAction: (id: string) => call<PublicationView>("resolve_user_action", { id }),
  confirmPosted: (id: string, url: string | null) => call<PublicationView>("confirm_posted", { id, url }),
  resolveOutcome: (id: string, resolution: OutcomeResolution) =>
    call<PublicationView>("resolve_outcome", { id, resolution }),
  exportPackage: (id: string) => call<void>("export_package", { id }),
  nextSlot: (accountId: string) => call<string | null>("next_slot", { accountId }),

  // Collections and rules
  listCollections: () => call<CollectionView[]>("list_collections"),
  createCollection: (name: string) => call<CollectionView>("create_collection", { name }),
  renameCollection: (id: string, name: string) => call<CollectionView>("rename_collection", { id, name }),
  deleteCollection: (id: string) => call<void>("delete_collection", { id }),
  addToCollection: (collectionId: string, assetId: string) =>
    call<string[]>("add_to_collection", { collectionId, assetId }),
  removeFromCollection: (collectionId: string, assetId: string) =>
    call<void>("remove_from_collection", { collectionId, assetId }),
  createRule: (collectionId: string, rule: RuleInput) => call<CollectionView>("create_rule", { collectionId, rule }),
  updateRule: (id: string, rule: RuleInput) => call<CollectionView>("update_rule", { id, rule }),
  deleteRule: (id: string) => call<CollectionView>("delete_rule", { id }),
  ruleActivity: (id: string) => call<RuleActivityView>("rule_activity", { id }),
  setUnattended: (collectionId: string, accountId: string, enabled: boolean) =>
    call<CollectionView>("set_unattended", { collectionId, accountId, enabled }),

  // TikTok new-post detection and matching
  tiktokStatus: () => call<TikTokStatus>("tiktok_status"),
  tiktokAppInfo: () => call<TikTokAppInfo>("tiktok_app_info"),
  tiktokSetApp: (clientKey: string, clientSecret: string) =>
    call<TikTokStatus>("tiktok_set_app", { clientKey, clientSecret }),
  tiktokClearApp: () => call<TikTokStatus>("tiktok_clear_app"),
  tiktokSetEnabled: (enabled: boolean) => call<TikTokStatus>("tiktok_set_enabled", { enabled }),
  tiktokConnect: () => call<void>("tiktok_connect"),
  tiktokFinishConnect: () => call<ConnectProgress>("tiktok_finish_connect"),
  tiktokDisconnect: () => call<TikTokStatus>("tiktok_disconnect"),
  tiktokCheckNow: () => call<CheckReport>("tiktok_check_now"),
  matchSuggestions: () => call<MatchSuggestion[]>("match_suggestions"),
  confirmMatch: (videoId: string, assetId: string) => call<AssetSummary>("confirm_match", { videoId, assetId }),
  dismissMatch: (videoId: string, assetId: string) => call<void>("dismiss_match", { videoId, assetId }),

  // YouTube publishing
  youtubeStatus: () => call<YouTubeStatus>("youtube_status"),
  youtubeSetClient: (clientId: string, clientSecret: string) =>
    call<YouTubeStatus>("youtube_set_client", { clientId, clientSecret }),
  youtubeClearClient: () => call<YouTubeStatus>("youtube_clear_client"),
  youtubeConnect: (accountId: string | null) => call<void>("youtube_connect", { accountId }),
  youtubeConnectProgress: () => call<YouTubeConnectProgress>("youtube_connect_progress"),
  youtubeCancelConnect: () => call<void>("youtube_cancel_connect"),
  youtubeDisconnect: (accountId: string) => call<YouTubeStatus>("youtube_disconnect", { accountId }),
  youtubeSetPublicUploads: (accountId: string, isPublic: boolean) =>
    call<YouTubeStatus>("youtube_set_public_uploads", { accountId, public: isPublic }),

  // Instagram publishing
  instagramStatus: () => call<InstagramStatus>("instagram_status"),
  instagramFindPages: (accessToken: string) => call<InstagramPages>("instagram_find_pages", { accessToken }),
  instagramConnect: (accessToken: string, login: InstagramLogin, pageId: string | null, accountId: string | null) =>
    call<InstagramStatus>("instagram_connect", { accessToken, login, pageId, accountId }),
  instagramDisconnect: (accountId: string) => call<InstagramStatus>("instagram_disconnect", { accountId }),

  // LinkedIn publishing
  linkedinStatus: () => call<LinkedInStatus>("linkedin_status"),
  linkedinFindPages: (accessToken: string) => call<LinkedInOrganization[]>("linkedin_find_pages", { accessToken }),
  linkedinConnect: (accessToken: string, page: string, accountId: string | null) =>
    call<LinkedInStatus>("linkedin_connect", { accessToken, page, accountId }),
  linkedinDisconnect: (accountId: string) => call<LinkedInStatus>("linkedin_disconnect", { accountId }),

  // Snapchat Spotlight publishing
  snapchatStatus: () => call<SnapStatus>("snapchat_status"),
  snapchatSetClient: (clientId: string, clientSecret: string, redirectUri: string) =>
    call<SnapStatus>("snapchat_set_client", { clientId, clientSecret, redirectUri }),
  snapchatClearClient: () => call<SnapStatus>("snapchat_clear_client"),
  snapchatConnect: (accountId: string | null) => call<void>("snapchat_connect", { accountId }),
  snapchatConnectProgress: () => call<SnapConnectProgress>("snapchat_connect_progress"),
  snapchatCancelConnect: () => call<void>("snapchat_cancel_connect"),
  snapchatFinishWithCode: (pasted: string) => call<SnapStatus>("snapchat_finish_with_code", { pasted }),
  snapchatCheckConnection: (accountId: string) => call<SnapStatus>("snapchat_check_connection", { accountId }),
  snapchatDisconnect: (accountId: string) => call<SnapStatus>("snapchat_disconnect", { accountId }),

  openLink: (url: string) => call<void>("open_link", { url }),

  // Cloud runner (optional)
  cloudStatus: () => call<CloudStatus>("cloud_status"),
  cloudConfigure: (url: string, token: string) => call<CloudStatus>("cloud_configure", { url, token }),
  cloudSetEnabled: (enabled: boolean) => call<CloudStatus>("cloud_set_enabled", { enabled }),
  cloudForget: () => call<CloudStatus>("cloud_forget"),
  cloudSyncNow: () => call<CloudStatus>("cloud_sync_now"),
  sendToCloud: (id: string) => call<PublicationView>("send_to_cloud", { id }),
  takeBackFromCloud: (id: string) => call<PublicationView>("take_back_from_cloud", { id }),

  // Monitor
  monitor: () => call<MonitorView>("monitor"),
  retryJob: (id: string) => call<void>("retry_job", { id }),
  cancelJob: (id: string) => call<void>("cancel_job", { id }),

  // Settings and system
  preferences: () => call<Preferences>("preferences"),
  updatePreferences: (update: PreferencesUpdate) => call<Preferences>("update_preferences", { update }),
  checkWatchFolder: () => call<Preferences>("check_watch_folder"),
  /** Opens a waiting TikTok post and expects its downloaded video. */
  getOriginal: (assetId: string) => call<GetOriginal>("get_original", { assetId }),
  cancelGetOriginal: (assetId: string) => call<void>("cancel_get_original", { assetId }),
  /** "Pick file…": attaches a chosen video to a waiting item. */
  attachOriginal: (assetId: string, path: string) => call<void>("attach_original", { assetId, path }),
  defaultDownloadsFolder: () => call<string | null>("default_downloads_folder"),
  mediaStatus: () => call<MediaStatus>("media_status"),
  openFolder: (path: string) => call<void>("open_folder", { path }),
};
