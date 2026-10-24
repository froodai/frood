// Demo knowledge for the console: two projects, deadlines around today, decisions, owners,
// an accepted change with history, and changes that wait in the Inbox.
// FROOD_URL and FROOD_TOKEN (a personal token with every permission) come from serve.sh.

const SERVER = process.env.FROOD_URL ?? "http://127.0.0.1:18080";
const TOKEN = process.env.FROOD_TOKEN ?? "";

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(SERVER + path, {
    method,
    headers: { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text}`);
  return (text ? JSON.parse(text) : null) as T;
}

const day = (offset: number) => new Date(Date.now() + offset * 86_400_000).toISOString().slice(0, 10);
const ago = (days: number) => new Date(Date.now() - days * 86_400_000).toISOString();

/** A note the way a person sends it; returns its evidence. */
async function note(text: string, daysAgo = 1): Promise<string> {
  const r = await call<{ source: { evidence: { id: string }[] } }>("POST", "/v1/sources", {
    kind: "note",
    origin: "capture",
    content: text,
    said_at: ago(daysAgo),
  });
  return r.source.evidence[0].id;
}

type Content = { kind: string; title?: string; text: string; subjects?: string[]; slots?: Record<string, unknown>; modality?: string };

function assertion(c: Content, evidence: string, daysAgo: number) {
  return { modality: "agreed", subjects: [], slots: {}, ...c, said_at: ago(daysAgo), evidence: [evidence] };
}

type Proposal = { id: string; state: string };

/** A person's own new knowledge: accepted by the own-facts policy. Returns the assertion id. */
async function fact(c: Content, quote: string, daysAgo = 3): Promise<string> {
  const evidence = await note(quote, daysAgo);
  const p = await call<Proposal>("POST", "/v1/proposals", {
    risk: "new",
    operations: [{ op: "create", ref: "it", assertion: assertion(c, evidence, daysAgo) }],
    evidence: [evidence],
  });
  if (p.state !== "accepted") throw new Error(`not accepted: ${c.text}`);
  const found = await call<{ assertion_id: string; text: string }[]>("GET", `/v1/assertions?limit=200&text=${encodeURIComponent(c.text)}`);
  return found.find((a) => a.text === c.text)!.assertion_id;
}

/** A change that waits for a person. */
async function change(id: string, c: Content, quote: string, rationale: string, risk = "change"): Promise<Proposal> {
  const current = await call<{ version: number }>("GET", `/v1/assertions/${id}`);
  const evidence = await note(quote, 0);
  return call<Proposal>("POST", "/v1/proposals", {
    risk,
    rationale,
    base: [{ assertion_id: id, version: current.version }],
    operations: [{ op: "supersede", assertion_id: id, assertion: assertion(c, evidence, 0) }],
    evidence: [evidence],
  });
}

async function entity(type: string, name: string): Promise<string> {
  const p = await call<Proposal>("POST", "/v1/proposals", { risk: "new", operations: [{ op: "create_entity", ref: "e", type, name }] });
  if (p.state !== "accepted") throw new Error(`entity ${name} not accepted`);
  const found = await call<{ id: string; name: string }[]>("GET", `/v1/entities?name=${encodeURIComponent(name)}`);
  return found.find((e) => e.name === name)!.id;
}

const atlas = await entity("project", "Atlas");
const website = await entity("project", "Website");

await fact({ kind: "commitment", text: "Payments demo for the team", subjects: [atlas], slots: { due: day(1), owner: "Igor" } }, "Igor will demo payments to the team on " + day(1));
await fact({ kind: "commitment", text: "Copy for the landing page", subjects: [website], slots: { due: day(5) } }, "I write the landing copy by " + day(5));
await fact({ kind: "commitment", text: "Security review of Atlas", subjects: [atlas], slots: { due: day(12), owner: "Oleg" } }, "Oleg finishes the security review by " + day(12));
await fact({ kind: "commitment", text: "Expense report for October", slots: { due: day(-1) } }, "Expense report due " + day(-1));
await fact({ kind: "decision", text: "Payments go through one provider; no billing of our own", subjects: [atlas], modality: "decided" }, "Decided: one payment provider, we do not build billing", 20);
await fact({ kind: "decision", text: "The landing page lives on the main domain under /launch", subjects: [website], modality: "decided" }, "Landing goes to /launch on the main domain", 9);
await fact({ kind: "responsibility", text: "Oleg is responsible for security", subjects: [atlas] }, "Oleg owns security for Atlas", 30);
await fact({ kind: "open_question", text: "Does the first release of Atlas support single sign-on?", subjects: [atlas] }, "Open: SSO in the first release?", 4);

const release = await fact({ kind: "commitment", text: "Atlas release on " + day(20), subjects: [atlas], slots: { due: day(20) } }, "Atlas ships on " + day(20), 15);
const owner = await fact({ kind: "responsibility", text: "Igor is responsible for the payments module", subjects: [atlas] }, "Igor takes the payments module", 40);
const migration = await fact({ kind: "status", text: "Migration to PostgreSQL 17: in progress", subjects: [atlas], slots: { state: "in progress" } }, "Started the PostgreSQL 17 migration", 8);
const cadence = await fact({ kind: "decision", text: "Atlas releases every Tuesday, weekly", subjects: [atlas], modality: "decided" }, "We release every Tuesday", 50);
const staging = await fact({ kind: "fact", text: "Staging of Atlas is at stage.atlas.example", subjects: [atlas] }, "staging: stage.atlas.example", 60);

// An accepted change: history with two versions.
const moved = await change(staging, { kind: "fact", text: "Staging of Atlas is at staging.atlas.example", subjects: [atlas] }, "Staging moved to staging.atlas.example", "The address changed in the deploy notes");
await call("POST", `/v1/proposals/${moved.id}/verdicts`, { decision: "accept" });

// Waiting in the Inbox.
await change(release, { kind: "commitment", text: "Atlas release on " + day(27), subjects: [atlas], slots: { due: day(27) } }, `Moving the release to ${day(27)}: waiting for the security review`, "The tracker moved the release date by a week");
await change(owner, { kind: "responsibility", text: "Maria is responsible for the payments module", subjects: [atlas] }, "Maria takes over payments, Igor moves to search", "Ownership changed in the tracker");
await change(migration, { kind: "status", text: "Migration to PostgreSQL 17: done", subjects: [atlas], slots: { state: "done" } }, "Migration to PostgreSQL 17 rolled out everywhere", "The migration task was closed");
await change(cadence, { kind: "decision", text: "Atlas releases every second Tuesday", subjects: [atlas], modality: "decided" }, "Team chat: let's release every two weeks", "The team chat agreed on a release every two weeks", "conflict");

// New knowledge an agent found in the tracker, with a link to the issue. A base keeps the
// own-facts policy off, so it waits for a person the way an agent's proposal does.
async function found(c: Content, quote: string, uri: string, title: string, rationale: string): Promise<Proposal> {
  const r = await call<{ source: { evidence: { id: string }[] } }>("POST", "/v1/sources", {
    kind: "conversation",
    origin: "capture",
    content: quote,
    uri,
    title,
    said_at: ago(0),
  });
  const evidence = r.source.evidence[0].id;
  const base = await call<{ version: number }>("GET", `/v1/assertions/${staging}`);
  return call<Proposal>("POST", "/v1/proposals", {
    risk: "new",
    rationale,
    base: [{ assertion_id: staging, version: base.version }],
    operations: [{ op: "create", ref: "it", assertion: assertion(c, evidence, 0) }],
    evidence: [evidence],
  });
}
await found(
  {
    kind: "fact",
    title: "ATL-142: a finished session loads for 22 minutes",
    text: "ATL-142 (Bug, Chat): a finished session with a long history (1987 records) shows only the last part and keeps loading for about 22 minutes; the proposed fix is to attach with session/resume instead of session/load. Assigned to Maria.",
    subjects: [atlas],
  },
  "Task view: a finished session with a long history shows only the projected tail and keeps loading (ATL-142, created by Oleg)",
  "https://tracker.example/issue/ATL-142",
  "ATL-142: Task view keeps loading",
  "Tracker (ATL): ATL-142 was created today by Oleg, state Submitted, priority Normal, assignee Maria.",
);
await found(
  { kind: "decision", title: "Search moves to OpenSearch", text: "Atlas search moves from PostgreSQL full-text to OpenSearch in the next quarter.", subjects: [atlas], modality: "decided" },
  "ADR-12: Status accepted. Search moves to OpenSearch next quarter.",
  "https://git.example/atlas/adr/012-search.md",
  "ADR-12: Search engine",
  "ADR-12 was accepted today.",
);

console.log("seeded the demo knowledge");

export {};
