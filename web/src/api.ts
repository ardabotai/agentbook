/** REST client for tmax-web API. */

export interface SessionInfo {
  id: string;
  label: string | null;
  exec: string;
  args: string[];
  exited: boolean;
  exit_code: number | null;
}

export interface CreateSessionOpts {
  exec?: string;
  label?: string;
  cols?: number;
  rows?: number;
}

const BASE = "";

export async function listSessions(): Promise<SessionInfo[]> {
  const res = await fetch(`${BASE}/api/sessions`);
  if (!res.ok) throw new Error(`list sessions failed: ${res.status}`);
  return res.json();
}

interface CreateResponse {
  session_id: string;
}

export async function createSession(
  opts: CreateSessionOpts = {},
): Promise<SessionInfo> {
  const res = await fetch(`${BASE}/api/sessions`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      exec: opts.exec ?? "/bin/bash",
      label: opts.label,
      cols: opts.cols ?? 80,
      rows: opts.rows ?? 24,
    }),
  });
  if (!res.ok) throw new Error(`create session failed: ${res.status}`);
  const created: CreateResponse = await res.json();

  // Fetch full session info
  const infoRes = await fetch(`${BASE}/api/sessions/${created.session_id}`);
  if (!infoRes.ok) throw new Error(`get session info failed: ${infoRes.status}`);
  return infoRes.json();
}
