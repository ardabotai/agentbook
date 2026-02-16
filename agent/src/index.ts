import { getModel, stream, type Context } from "@mariozechner/pi-ai";
import { NodeClient } from "./node-client.js";
import { createTools } from "./tools/index.js";
import { createInterface } from "readline";

const DEFAULT_MODEL_PROVIDER = "anthropic";
const DEFAULT_MODEL_NAME = "claude-sonnet-4-20250514";

/**
 * agentbook-agent: AI assistant for the agentbook messaging network.
 *
 * Modes:
 *   --interactive    Run as standalone REPL (default)
 *   --stdio          Run as a sidecar: reads JSON-lines on stdin, writes on stdout
 *
 * Environment:
 *   AGENTBOOK_SOCKET   Path to node daemon socket
 *   AGENTBOOK_MODEL     Model in "provider:model" format (default: anthropic:claude-sonnet-4-20250514)
 */
async function main() {
  const args = process.argv.slice(2);
  const stdioMode = args.includes("--stdio");
  const socketPath = process.env.AGENTBOOK_SOCKET ?? getDefaultSocketPath();

  // Connect to node daemon
  const client = new NodeClient();
  try {
    await client.connect(socketPath);
  } catch (err) {
    console.error(`Failed to connect to agentbook-node at ${socketPath}: ${err}`);
    process.exit(1);
  }

  const identity = await client.getIdentity();
  const nodeId = identity?.node_id ?? client.nodeId;
  const username = identity?.username ?? null;

  // Parse model
  const modelSpec = process.env.AGENTBOOK_MODEL ?? `${DEFAULT_MODEL_PROVIDER}:${DEFAULT_MODEL_NAME}`;
  const [provider, modelName] = modelSpec.split(":", 2);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const model = getModel(provider as any, modelName as any);

  // Build tools
  const { tools, executeTool } = createTools(client, async (action, details) => {
    if (stdioMode) {
      // In stdio mode, send approval request and wait for response
      const request = JSON.stringify({ type: "approval_request", action, details });
      process.stdout.write(request + "\n");
      return waitForApproval();
    } else {
      // In interactive mode, prompt the user
      return promptApproval(action, details);
    }
  });

  // Listen for inbound events
  client.onEvent((event) => {
    if (stdioMode) {
      process.stdout.write(JSON.stringify({ type: "node_event", event }) + "\n");
    } else {
      const preview =
        event.kind === "new_message"
          ? `New ${event.message_type} from ${event.from}: ${event.preview}`
          : `New follower: ${event.node_id}`;
      console.log(`\n[event] ${preview}`);
    }
  });

  // Build system prompt
  const systemPrompt = buildSystemPrompt(nodeId, username);

  // Create context
  const context: Context = {
    systemPrompt,
    messages: [],
    tools,
  };

  if (stdioMode) {
    await runStdioMode(model, context, executeTool);
  } else {
    await runInteractiveMode(model, context, executeTool);
  }

  client.close();
}

function buildSystemPrompt(nodeId: string, username: string | null): string {
  const id = username ? `@${username} (${nodeId.slice(0, 12)}...)` : nodeId.slice(0, 16);
  return `You are the AI agent for agentbook user ${id}.

Your role:
- Help the user read and understand their messages (inbox, feed, DMs)
- Draft messages and posts when asked — but NEVER send without explicit human approval
- Summarize conversations and threads
- Help manage their social graph (following, followers, blocking)
- Answer questions about the network and their contacts

Key rules:
1. NEVER send a DM or post to feed without human approval. Always use the tool which will prompt for approval.
2. When drafting messages, show the draft to the user first and ask if they want to send it.
3. Be concise and conversational — you're a chat assistant, not a formal butler.
4. When showing inbox messages, highlight unread ones and summarize long threads.
5. Use @usernames when available, fall back to shortened node IDs.
6. NEVER start or restart the node daemon. Only a human should start the node because it requires
   access to the recovery key (passphrase). The recovery key must never be provided to an agent.
   If the node is not running, tell the user to start it themselves with "agentbook up".

Available tools: read_inbox, send_dm, post_feed, list_following, list_followers, lookup_username, ack_message, get_health, get_wallet, yolo_send_eth, yolo_send_usdc, read_contract, write_contract, sign_message

Note: Human wallet send_eth/send_usdc are NOT available to the agent because they require TOTP codes. Only yolo wallet variants are available. If the user asks to send from the human wallet, explain they need to use the CLI or TUI directly.`;
}

async function runInteractiveMode(
  model: ReturnType<typeof getModel>,
  context: Context,
  executeTool: (name: string, args: Record<string, unknown>) => Promise<string>
): Promise<void> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const prompt = () =>
    new Promise<string>((resolve) => {
      rl.question("you> ", (answer) => resolve(answer));
    });

  console.log("agentbook agent ready. Type your message or 'quit' to exit.\n");

  while (true) {
    const input = await prompt();
    if (input.trim().toLowerCase() === "quit") break;
    if (!input.trim()) continue;

    context.messages.push({ role: "user", content: input, timestamp: Date.now() });

    // Agent loop: keep going while there are tool calls
    let continueLoop = true;
    while (continueLoop) {
      const s = stream(model, context);

      for await (const event of s) {
        if (event.type === "text_delta") {
          process.stdout.write(event.delta);
        }
      }

      const result = await s.result();
      context.messages.push(result);

      // Check for tool calls
      const toolCalls = result.content.filter(
        (b: { type: string }) => b.type === "toolCall"
      );

      if (toolCalls.length > 0) {
        for (const call of toolCalls as Array<{
          type: "toolCall";
          id: string;
          name: string;
          arguments: Record<string, unknown>;
        }>) {
          const toolResult = await executeTool(call.name, call.arguments);
          context.messages.push({
            role: "toolResult",
            toolCallId: call.id,
            toolName: call.name,
            content: [{ type: "text", text: toolResult }],
            isError: false,
            timestamp: Date.now(),
          });
        }
      }

      continueLoop = toolCalls.length > 0;
    }

    process.stdout.write("\n");
  }
}

async function runStdioMode(
  model: ReturnType<typeof getModel>,
  context: Context,
  executeTool: (name: string, args: Record<string, unknown>) => Promise<string>
): Promise<void> {
  const rl = createInterface({ input: process.stdin });

  for await (const line of rl) {
    let msg: StdioMessage;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }

    if (msg.type === "approval_response") {
      resolveApproval(msg.approved);
      continue;
    }

    if (msg.type === "user_message") {
      context.messages.push({ role: "user", content: msg.content, timestamp: Date.now() });

      let continueLoop = true;
      while (continueLoop) {
        const s = stream(model, context);
        let textBuffer = "";

        for await (const event of s) {
          if (event.type === "text_delta") {
            textBuffer += event.delta;
            // Stream text deltas to the TUI
            process.stdout.write(
              JSON.stringify({ type: "text_delta", delta: event.delta }) + "\n"
            );
          }
        }

        const result = await s.result();
        context.messages.push(result);

        const toolCalls = result.content.filter(
          (b: { type: string }) => b.type === "toolCall"
        );

        if (toolCalls.length > 0) {
          for (const call of toolCalls as Array<{
            type: "toolCall";
            id: string;
            name: string;
            arguments: Record<string, unknown>;
          }>) {
            process.stdout.write(
              JSON.stringify({
                type: "tool_call",
                name: call.name,
                arguments: call.arguments,
              }) + "\n"
            );
            const toolResult = await executeTool(call.name, call.arguments);
            process.stdout.write(
              JSON.stringify({ type: "tool_result", name: call.name, result: toolResult }) + "\n"
            );
            context.messages.push({
              role: "toolResult",
              toolCallId: call.id,
              toolName: call.name,
              content: [{ type: "text", text: toolResult }],
              isError: false,
              timestamp: Date.now(),
            });
          }
          continueLoop = true;
        } else {
          continueLoop = false;
        }
      }

      // Signal message complete
      process.stdout.write(JSON.stringify({ type: "done" }) + "\n");
    }
  }
}

// -- Helpers --

function getDefaultSocketPath(): string {
  if (process.env.AGENTBOOK_SOCKET) return process.env.AGENTBOOK_SOCKET;
  const xdg = process.env.XDG_RUNTIME_DIR;
  if (xdg) return `${xdg}/agentbook/agentbook.sock`;
  const uid = process.getuid?.() ?? 0;
  return `/tmp/agentbook-${uid}/agentbook.sock`;
}

let approvalResolve: ((approved: boolean) => void) | null = null;

function waitForApproval(): Promise<boolean> {
  return new Promise((resolve) => {
    approvalResolve = resolve;
    // The stdio reader will call resolveApproval when it gets the response
  });
}

// This would be called from the stdio message handler
export function resolveApproval(approved: boolean): void {
  if (approvalResolve) {
    approvalResolve(approved);
    approvalResolve = null;
  }
}

async function promptApproval(action: string, details: string): Promise<boolean> {
  return new Promise((resolve) => {
    const rl = createInterface({ input: process.stdin, output: process.stdout });
    console.log(`\n--- Approval Required ---`);
    console.log(`Action: ${action}`);
    console.log(details);
    rl.question("Approve? (y/n): ", (answer) => {
      rl.close();
      resolve(answer.trim().toLowerCase().startsWith("y"));
    });
  });
}

type StdioMessage =
  | { type: "user_message"; content: string }
  | { type: "approval_response"; approved: boolean };

main().catch((err) => {
  console.error("Agent error:", err);
  process.exit(1);
});
