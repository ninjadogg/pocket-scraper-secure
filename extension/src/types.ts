// Shapes shared by the extension's pages and service worker. Recipes and planner
// messages mirror the Rust core's serde types; the core validates them.

export type FieldKind = 'text' | 'href' | 'html' | 'value';
export interface Field { name: string; selector: string; kind: FieldKind }
export interface ClickAction { selector: string; waitMs: number }
export type Step =
  | { type: 'click'; selector: string; waitMs: number }
  | { type: 'wait'; waitMs: number }
  | { type: 'extract' };
export interface Workflow { steps: Step[]; repeatCount: number }

export interface Recipe {
  id: string;
  name: string;
  url: string;
  fields: Field[];
  actions: ClickAction[];
  rowSelector: string;
  nextSelector: string;
  maxPages: number;
  scheduleMinutes: number;
  workflow: Workflow | null;
  useNativeHost: boolean;
}

/** An unsaved recipe being built in the popup. Validated only when saved. */
export interface Draft extends Partial<Omit<Recipe, 'fields' | 'actions'>> {
  fields: Field[];
  actions: ClickAction[];
  lastPick?: string;
}

export interface RecipeEntry {
  recipe: Recipe;
  createdAt: string;
  updatedAt: string;
  /** The site needs the person (sign-in, verification); schedules pause. */
  needsAttention: boolean;
  /** Claude created it, or changed where it goes or what it clicks: it doesn't run
   * (or get signed for runners) until the person approves it in the dashboard. */
  review?: boolean;
}

export type RunStatus = 'running' | 'success' | 'error' | 'needs_attention';
export interface RunRecord {
  id: string;
  recipeId: string;
  via: 'tab' | 'host' | 'runner';
  /** For runner batches: the enrolled runner's name. */
  runner?: string;
  trigger: 'manual' | 'schedule';
  startedAt: string;
  finishedAt?: string;
  status: RunStatus;
  rowCount?: number;
  warnings: string[];
  error?: string;
}

export interface SealedRecord { key: string; sealed: string }
/** A runner enrolled to send result batches (its Ed25519 public key, base64). */
/** `pending`: enrolled by Claude; its batches are refused until the person approves it. */
export interface Runner { name: string; key: string; fingerprint: string; addedAt: string; pending?: boolean }
/** core::bundle::Batch as JSON. */
export interface Batch {
  batchId: string; runnerKey: string; vaultPublicKey: string; recipeId: string; runId: string;
  startedAt: string; finishedAt: string; status: 'success' | 'error' | 'needs_attention';
  rowCount: number; warnings: string[]; error?: string | null; records: SealedRecord[];
}
/** Stored as-is: a random key and ciphertext. When it was observed is inside the ciphertext. */
export interface StoredRecord extends SealedRecord { recipeId: string; runId: string }
export interface PlainRecord { observedAt: string; runId: string; pageUrl: string; fields: Record<string, string> }
export interface Row { pageUrl: string; fields: Record<string, string> }

/** Planner actions (core::plan::Action). */
export type PlanAction =
  | { type: 'navigate'; url: string }
  | { type: 'click'; selector: string; timeoutMs: number }
  | { type: 'clickNext'; selector: string }
  | { type: 'wait'; ms: number }
  | { type: 'read' }
  | { type: 'finish'; rows: Row[]; warnings: string[] }
  | { type: 'fail'; message: string; attention: boolean };

/** Planner events (core::plan::Event). */
export type PlanEvent =
  | { type: 'navigated'; status: number }
  | { type: 'clicked' }
  | { type: 'clickFailed'; message: string }
  | { type: 'nextClicked'; urlBefore: string }
  | { type: 'nextUnavailable' }
  | { type: 'waited' }
  | { type: 'read'; url: string; status: number; output?: unknown; error?: string; challenge?: boolean };

export type RunOutcome =
  | { ok: true; records: SealedRecord[]; rowCount: number; warnings: string[] }
  | { ok: false; message: string; attention: boolean };

/** Messages extension pages send to the service worker. */
export type PageRequest =
  | { type: 'startPicker'; tabId: number; mode: PickMode; fieldName: string; kind: FieldKind }
  | { type: 'saveRecipe'; recipe: unknown }
  | { type: 'deleteRecipe'; id: string; purgeRecords: boolean }
  | { type: 'runNow'; id: string }
  | { type: 'hostStatus' }
  | { type: 'storageRestricted' }
  | { type: 'session'; id: string; action: 'open' | 'close' }
  | { type: 'clearAttention'; id: string }
  | { type: 'approveRecipe'; id: string };

export type PickMode = 'row' | 'field' | 'action' | 'next';

/** The only message a content script (the picker) may send. */
export interface PickMessage { type: 'pick'; nonce: string; selector: string; matches: number }
