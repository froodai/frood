// Shapes of the API responses the console reads (core/crates/frood-*).

export type Change = { kind: string; title?: string | null; before: string | null; after: string | null };

export type ProposalContent = {
  proposal_id: string;
  risk: string;
  author: string;
  rationale: string | null;
  state: string;
  alternatives: { key: string; label: string | null; changes: Change[] }[];
  quote: string | null;
  /** Where a person opens the original, when the source has an address. */
  origin?: { uri: string; title: string | null } | null;
};

export type QuestionContent = {
  text: string;
  options: string[] | null;
  context: string | null;
  job: string | null;
  status: string;
};

export type InboxItem = {
  id: string;
  kind: "proposal" | "question" | "reminder" | "alert" | "digest" | string;
  subject: string;
  state: string;
  urgency: string;
  data: Record<string, unknown>;
  created_at: string;
  updated_at: string;
  resolved_at: string | null;
  snoozed_until: string | null;
  resolution: { action?: string; by?: string; state?: string } | null;
};

export type InboxEntry = {
  item: InboxItem;
  content: Record<string, unknown>;
  actions: { action: string; label?: string | null; alternative?: string; answer?: string }[];
  for_me: boolean;
  seen: boolean;
};

export type Assertion = {
  assertion_id: string;
  version: number;
  kind: string;
  title?: string | null;
  text: string;
  subjects: string[];
  slots: Record<string, unknown>;
  modality: string;
  authority: unknown;
  said_at: string | null;
  valid_from: string;
  valid_to: string | null;
  recorded_from: string;
  recorded_to: string | null;
  status: string;
  evidence: string[];
  proposal_id: string;
  verdict_id: string;
};

export type SearchHit = Assertion & { score: number; matched: string };

export type Run = {
  id: string;
  job: string;
  subject: string | null;
  status: string;
  attempts: number;
  error: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  account: string | null;
  cost_usd: number;
};

export type Finished = { status: string; finished_at: string | null; error: string | null };

export type Health = {
  version: string;
  modules: { name: string; up: boolean; provides: string[] }[];
  dispatcher_lag_seconds: number;
  undispatched_events: number;
  runs: { queued: number; running: number; waiting_for_input: number; failed_last_day: number; succeeded_last_day: number };
  inbox_open: number;
  alerts_open: number;
  accounts: { account: string; spent_usd: number; monthly_usd: number | null }[];
  sources: { job: string; source: string; seconds_since_read: number | null }[];
  backups: { targets: string[]; seconds_since_backup: number | null; last_backup: Finished | null; last_verify: Finished | null } | null;
};

export type Me = { user_id: string | null; email: string | null; actor: string; scopes: string[] };
