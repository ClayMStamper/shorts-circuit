// Typed wrappers around the Tauri commands in app/src-tauri/src/commands.rs.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AccountView,
  AppError,
  AssetDetail,
  AssetSummary,
  AssetUpdate,
  EditPublication,
  ImportResult,
  LibraryQuery,
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
  ScheduleInput,
  TagCount,
  VariantView,
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

  // Monitor
  monitor: () => call<MonitorView>("monitor"),
  retryJob: (id: string) => call<void>("retry_job", { id }),
  cancelJob: (id: string) => call<void>("cancel_job", { id }),

  // Settings and system
  preferences: () => call<Preferences>("preferences"),
  updatePreferences: (update: PreferencesUpdate) => call<Preferences>("update_preferences", { update }),
  mediaStatus: () => call<MediaStatus>("media_status"),
  openFolder: (path: string) => call<void>("open_folder", { path }),
};
