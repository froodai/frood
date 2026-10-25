// Talking to the Frood server: where it lives, signing in (OAuth with PKCE as the client
// `frood-admin`) and calling the API with the token.

/** The server's path behind a proxy (`/frood`), or empty at the root; the server puts it in. */
export const BASE = document.querySelector<HTMLMetaElement>('meta[name="frood-base"]')?.content ?? "";

const CLIENT = "frood-admin";
const SCOPES = "knowledge:read knowledge:capture knowledge:propose knowledge:decide journal:read admin";
const store = window.sessionStorage;
/** Development only: a personal token instead of signing in (`VITE_FROOD_TOKEN`). */
const DEV_TOKEN: string | undefined = import.meta.env.VITE_FROOD_TOKEN;

type Tokens = { access: string; refresh?: string; scope?: string };
type Metadata = { issuer: string; authorization_endpoint: string; token_endpoint: string };

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

let metadataPromise: Promise<Metadata> | undefined;
function metadata(): Promise<Metadata> {
  metadataPromise ??= fetch(`${BASE}/.well-known/oauth-authorization-server`).then((r) => {
    if (!r.ok) throw new Error("no authorization server metadata");
    return r.json() as Promise<Metadata>;
  });
  return metadataPromise;
}

const issuerBase = (meta: Metadata) => meta.issuer.replace(/\/$/, "");
const b64url = (bytes: ArrayBuffer | Uint8Array) =>
  btoa(String.fromCharCode(...new Uint8Array(bytes)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");

function tokens(): Tokens | null {
  if (DEV_TOKEN) return { access: DEV_TOKEN };
  return JSON.parse(store.getItem("tokens") ?? "null") as Tokens | null;
}

/** Leaves for the sign-in page; comes back to the console's root, then to `returnTo`. */
export async function signIn(): Promise<never> {
  const meta = await metadata();
  // The sign-in returns only to the issuer's own /admin/ (the client's one redirect address).
  const home = `${issuerBase(meta)}/admin/`;
  store.setItem("return_to", location.href);
  const verifier = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const challenge = b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
  const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
  store.setItem("pkce", JSON.stringify({ verifier, state }));
  const url = new URL(meta.authorization_endpoint);
  url.search = new URLSearchParams({
    response_type: "code",
    client_id: CLIENT,
    redirect_uri: home,
    resource: `${issuerBase(meta)}/v1`,
    scope: SCOPES,
    state,
    code_challenge: challenge,
    code_challenge_method: "S256",
  }).toString();
  location.assign(url.toString());
  return new Promise<never>(() => {});
}

async function tokenRequest(params: Record<string, string>): Promise<Tokens> {
  const meta = await metadata();
  const r = await fetch(meta.token_endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: CLIENT, resource: `${issuerBase(meta)}/v1`, ...params }),
  });
  if (!r.ok) throw new Error(`token: ${r.status}`);
  const t = (await r.json()) as { access_token: string; refresh_token?: string; scope?: string };
  const saved = { access: t.access_token, refresh: t.refresh_token, scope: t.scope };
  store.setItem("tokens", JSON.stringify(saved));
  return saved;
}

/** After the sign-in page: trades the code for tokens. Returns where to go back to. */
export async function finishSignIn(): Promise<string | null> {
  const q = new URLSearchParams(location.search);
  const code = q.get("code");
  if (!code) return null;
  const saved = JSON.parse(store.getItem("pkce") ?? "{}") as { verifier?: string; state?: string };
  history.replaceState(null, "", location.pathname);
  if (q.get("state") !== saved.state || !saved.verifier) throw new Error("sign-in state mismatch");
  await tokenRequest({
    grant_type: "authorization_code",
    code,
    redirect_uri: location.origin + location.pathname,
    code_verifier: saved.verifier,
  });
  store.removeItem("pkce");
  const back = store.getItem("return_to");
  store.removeItem("return_to");
  return back;
}

export function signOut() {
  store.removeItem("tokens");
  location.assign(`${BASE}/admin/`);
}

export function hasScope(scope: string): boolean {
  const t = tokens();
  return !!t && (DEV_TOKEN !== undefined || (t.scope ?? "").split(" ").includes(scope));
}

/** `json` or `form` (a file upload) as the body; `blob` reads the answer as bytes. */
type Init = Omit<RequestInit, "body"> & { json?: unknown; form?: FormData; blob?: boolean };

/** Calls the API; signs in again when the token is gone, refreshes it when it expired. */
export async function api<T>(path: string, init: Init = {}): Promise<T> {
  let t = tokens();
  if (!t) return signIn();
  const { json, form, blob, ...rest } = init;
  const call = (access: string) =>
    fetch(BASE + path, {
      ...rest,
      headers: {
        ...(rest.headers ?? {}),
        Authorization: `Bearer ${access}`,
        ...(json !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: json !== undefined ? JSON.stringify(json) : form,
    });
  let r = await call(t.access);
  if (r.status === 401 && t.refresh) {
    try {
      t = await tokenRequest({ grant_type: "refresh_token", refresh_token: t.refresh });
      r = await call(t.access);
    } catch {
      store.removeItem("tokens");
      return signIn();
    }
  }
  if (r.status === 401 && !DEV_TOKEN) {
    store.removeItem("tokens");
    return signIn();
  }
  if (blob && r.ok) return (await r.blob()) as T;
  const body = r.status === 204 ? null : await r.json().catch(() => null);
  if (!r.ok) {
    const problem = (body ?? {}) as { detail?: string; title?: string; type?: string };
    throw new ApiError(r.status, problem.type ?? "", problem.detail ?? problem.title ?? `HTTP ${r.status}`);
  }
  return body as T;
}
