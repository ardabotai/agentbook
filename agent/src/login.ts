/**
 * OAuth login flow handler for agentbook agent.
 *
 * Usage: node agent/src/index.ts --login <provider>
 *
 * Communicates via JSON-lines on stdin/stdout:
 *   OUT: {"type":"auth_url","url":"..."}         — display this URL to the user
 *   OUT: {"type":"prompt","message":"..."}        — prompt user for input
 *   IN:  {"type":"auth_code","code":"..."}        — user's pasted code
 *   OUT: {"type":"auth_result","credentials":{…}} — success
 *   OUT: {"type":"auth_error","error":"..."}      — failure
 */

import {
  loginAnthropic,
  loginOpenAICodex,
  type OAuthCredentials,
} from "@mariozechner/pi-ai";
import { createInterface } from "readline";

type LoginMessage =
  | { type: "auth_url"; url: string; instructions?: string }
  | { type: "prompt"; message: string }
  | { type: "auth_result"; credentials: OAuthCredentials }
  | { type: "auth_error"; error: string };

type InboundMessage = { type: "auth_code"; code: string };

function emit(msg: LoginMessage): void {
  process.stdout.write(JSON.stringify(msg) + "\n");
}

/** Wait for the TUI to send an auth_code message on stdin. */
function waitForCode(): Promise<string> {
  return new Promise((resolve, reject) => {
    const rl = createInterface({ input: process.stdin });
    const onLine = (line: string) => {
      try {
        const msg: InboundMessage = JSON.parse(line);
        if (msg.type === "auth_code") {
          rl.close();
          resolve(msg.code);
        }
      } catch {
        // ignore malformed lines
      }
    };
    rl.on("line", onLine);
    rl.on("close", () => reject(new Error("stdin closed before auth code received")));
  });
}

async function loginProvider(provider: string): Promise<OAuthCredentials> {
  switch (provider) {
    case "anthropic": {
      return loginAnthropic(
        (url: string) => {
          emit({ type: "auth_url", url });
        },
        async () => {
          emit({ type: "prompt", message: "Paste the authorization code:" });
          return waitForCode();
        }
      );
    }

    case "openai-codex": {
      return loginOpenAICodex({
        onAuth: (info: { url: string; instructions?: string }) => {
          emit({
            type: "auth_url",
            url: info.url,
            instructions: info.instructions,
          });
        },
        onPrompt: async () => {
          emit({ type: "prompt", message: "Paste the authorization code:" });
          return waitForCode();
        },
        onManualCodeInput: async () => {
          emit({ type: "prompt", message: "Paste the authorization code:" });
          return waitForCode();
        },
      });
    }

    default:
      throw new Error(`OAuth login not supported for provider: ${provider}`);
  }
}

export async function runLogin(provider: string): Promise<void> {
  try {
    const credentials = await loginProvider(provider);
    emit({ type: "auth_result", credentials });
  } catch (err) {
    emit({
      type: "auth_error",
      error: err instanceof Error ? err.message : String(err),
    });
    process.exit(1);
  }
}
