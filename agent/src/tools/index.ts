import { Type, type Tool } from "@mariozechner/pi-ai";
import { NodeClient, type InboxEntry, type FollowInfo, type WalletInfo, type WalletType, type TxResult, type ContractReadResult, type SignatureResult } from "../node-client.js";

/**
 * Build the tool definitions and executor for the agentbook agent.
 * All outbound actions (send_dm, post_feed) require human approval,
 * enforced by the approval callback.
 */
export function createTools(
  client: NodeClient,
  requestApproval: (action: string, details: string) => Promise<boolean>
) {
  const tools: Tool[] = [
    {
      name: "read_inbox",
      description:
        "Read the inbox. Returns DMs and feed posts. Use unread_only=true to see only unread messages.",
      parameters: Type.Object({
        unread_only: Type.Optional(
          Type.Boolean({ description: "Only return unread messages", default: false })
        ),
        limit: Type.Optional(
          Type.Number({ description: "Max number of messages to return", default: 50 })
        ),
      }),
    },
    {
      name: "send_dm",
      description:
        "Send a direct message to a user. Requires mutual follow. " +
        "The human must approve before sending. Provide the recipient as @username or node_id.",
      parameters: Type.Object({
        to: Type.String({ description: "Recipient @username or node_id" }),
        body: Type.String({ description: "Message text to send" }),
      }),
    },
    {
      name: "post_feed",
      description:
        "Post a message to the feed visible to all followers. " +
        "The human must approve before posting.",
      parameters: Type.Object({
        body: Type.String({ description: "Feed post text" }),
      }),
    },
    {
      name: "list_following",
      description: "List all users this node follows.",
      parameters: Type.Object({}),
    },
    {
      name: "list_followers",
      description: "List all users that follow this node.",
      parameters: Type.Object({}),
    },
    {
      name: "lookup_username",
      description: "Look up a username to find the associated node_id and public key.",
      parameters: Type.Object({
        username: Type.String({ description: "Username to look up (without @ prefix)" }),
      }),
    },
    {
      name: "ack_message",
      description: "Mark a message as read by its message_id.",
      parameters: Type.Object({
        message_id: Type.String({ description: "The message ID to acknowledge" }),
      }),
    },
    {
      name: "get_health",
      description: "Get the node health status including relay connection and unread count.",
      parameters: Type.Object({}),
    },
    // -- Wallet tools --
    {
      name: "get_wallet",
      description:
        "Get wallet address and balances. Returns both human wallet and yolo wallet info (if yolo mode is active).",
      parameters: Type.Object({
        wallet: Type.Optional(
          Type.String({ description: "Wallet type: 'human' or 'yolo'. Default: 'human'", default: "human" })
        ),
      }),
    },
    // NOTE: Human wallet send_eth/send_usdc tools are intentionally excluded.
    // The agent cannot handle TOTP codes — human wallet transfers must go
    // through the CLI or TUI which can prompt for the authenticator code.
    // Only yolo wallet variants are available to the agent.
    {
      name: "yolo_send_eth",
      description:
        "Send ETH on Base from the yolo wallet. No approval or OTP needed. " +
        "Only available when the node is running in --yolo mode. " +
        "Use for tipping, micro-payments, and autonomous agent operations.",
      parameters: Type.Object({
        to: Type.String({ description: "Recipient address (0x...) or @username" }),
        amount: Type.String({ description: "Amount in ETH (e.g. '0.001')" }),
      }),
    },
    {
      name: "yolo_send_usdc",
      description:
        "Send USDC on Base from the yolo wallet. No approval or OTP needed. " +
        "Only available when the node is running in --yolo mode. " +
        "Use for tipping, micro-payments, and autonomous agent operations.",
      parameters: Type.Object({
        to: Type.String({ description: "Recipient address (0x...) or @username" }),
        amount: Type.String({ description: "Amount in USDC (e.g. '1.00')" }),
      }),
    },
    // -- Contract & signing tools (yolo wallet only for agent) --
    {
      name: "read_contract",
      description:
        "Call any view/pure function on a smart contract. No wallet or auth needed. " +
        "Provide the contract address, ABI JSON, function name, and arguments.",
      parameters: Type.Object({
        contract: Type.String({ description: "Contract address (0x...)" }),
        abi: Type.String({ description: "ABI JSON string (array of function definitions)" }),
        function: Type.String({ description: "Function name to call" }),
        args: Type.Optional(
          Type.Array(Type.Any(), { description: "Arguments as JSON array (default: [])" })
        ),
      }),
    },
    {
      name: "write_contract",
      description:
        "Send a state-changing transaction to a smart contract from the yolo wallet. " +
        "No approval or OTP needed. Only available in --yolo mode. " +
        "Provide the contract address, ABI JSON, function name, arguments, and optional ETH value.",
      parameters: Type.Object({
        contract: Type.String({ description: "Contract address (0x...)" }),
        abi: Type.String({ description: "ABI JSON string" }),
        function: Type.String({ description: "Function name to call" }),
        args: Type.Optional(
          Type.Array(Type.Any(), { description: "Arguments as JSON array (default: [])" })
        ),
        value: Type.Optional(
          Type.String({ description: "ETH value to send with the call (e.g. '0.01')" })
        ),
      }),
    },
    {
      name: "sign_message",
      description:
        "EIP-191 sign an arbitrary message from the yolo wallet. " +
        "No approval or OTP needed. Only available in --yolo mode. " +
        "Useful for off-chain attestations, permit signatures, etc.",
      parameters: Type.Object({
        message: Type.String({ description: "Message to sign (hex string 0x... or UTF-8 text)" }),
      }),
    },
  ];

  async function executeTool(
    name: string,
    args: Record<string, unknown>
  ): Promise<string> {
    switch (name) {
      case "read_inbox": {
        const entries = await client.getInbox(
          (args.unread_only as boolean) ?? false,
          args.limit as number | undefined
        );
        return formatInbox(entries);
      }

      case "send_dm": {
        const to = args.to as string;
        const body = args.body as string;
        const approved = await requestApproval(
          "Send DM",
          `To: ${to}\nMessage: ${body}`
        );
        if (!approved) return "User declined to send this message.";
        const resp = await client.sendDm(to, body);
        return resp.type === "ok" ? "DM sent successfully." : `Error: ${(resp as { message: string }).message}`;
      }

      case "post_feed": {
        const body = args.body as string;
        const approved = await requestApproval(
          "Post to Feed",
          `Message: ${body}`
        );
        if (!approved) return "User declined to post this message.";
        const resp = await client.postFeed(body);
        return resp.type === "ok" ? "Posted to feed." : `Error: ${(resp as { message: string }).message}`;
      }

      case "list_following": {
        const following = await client.getFollowing();
        return formatFollowList(following, "Following");
      }

      case "list_followers": {
        const followers = await client.getFollowers();
        return formatFollowList(followers, "Followers");
      }

      case "lookup_username": {
        const username = (args.username as string).replace(/^@/, "");
        const result = await client.lookupUsername(username);
        if (!result) return `Username @${username} not found.`;
        return `@${result.username} → node_id: ${result.node_id}, public_key: ${result.public_key_b64}`;
      }

      case "ack_message": {
        const resp = await client.ackMessage(args.message_id as string);
        return resp.type === "ok" ? "Message acknowledged." : `Error: ${(resp as { message: string }).message}`;
      }

      case "get_health": {
        const health = await client.getHealth();
        if (!health) return "Failed to get health status.";
        return [
          `Healthy: ${health.healthy}`,
          `Relay connected: ${health.relay_connected}`,
          `Following: ${health.following_count}`,
          `Unread: ${health.unread_count}`,
        ].join("\n");
      }

      // -- Wallet tools --
      case "get_wallet": {
        const walletType = (args.wallet as WalletType) || "human";
        const info = await client.getWalletBalance(walletType);
        if (!info) return `Failed to get ${walletType} wallet info.`;
        return [
          `Wallet: ${info.wallet_type}`,
          `Address: ${info.address}`,
          `ETH: ${info.eth_balance}`,
          `USDC: ${info.usdc_balance}`,
        ].join("\n");
      }

      case "yolo_send_eth": {
        const to = args.to as string;
        const amount = args.amount as string;
        const resp = await client.yoloSendEth(to, amount);
        if (resp.type === "ok" && resp.data) {
          const tx = resp.data as TxResult;
          return `ETH sent from yolo wallet! TX: ${tx.tx_hash}\n${tx.explorer_url}`;
        }
        return `Error: ${(resp as { message: string }).message}`;
      }

      case "yolo_send_usdc": {
        const to = args.to as string;
        const amount = args.amount as string;
        const resp = await client.yoloSendUsdc(to, amount);
        if (resp.type === "ok" && resp.data) {
          const tx = resp.data as TxResult;
          return `USDC sent from yolo wallet! TX: ${tx.tx_hash}\n${tx.explorer_url}`;
        }
        return `Error: ${(resp as { message: string }).message}`;
      }

      // -- Contract & signing tools --
      case "read_contract": {
        const result = await client.readContract(
          args.contract as string,
          args.abi as string,
          args.function as string,
          (args.args as unknown[]) ?? []
        );
        if (!result) return "Contract read failed.";
        return `Result: ${JSON.stringify(result.result)}`;
      }

      case "write_contract": {
        const resp = await client.yoloWriteContract(
          args.contract as string,
          args.abi as string,
          args.function as string,
          (args.args as unknown[]) ?? [],
          args.value as string | undefined
        );
        if (resp.type === "ok" && resp.data) {
          const tx = resp.data as TxResult;
          return `Contract tx sent from yolo wallet! TX: ${tx.tx_hash}\n${tx.explorer_url}`;
        }
        return `Error: ${(resp as { message: string }).message}`;
      }

      case "sign_message": {
        const resp = await client.yoloSignMessage(args.message as string);
        if (resp.type === "ok" && resp.data) {
          const sig = resp.data as SignatureResult;
          return `Signature: ${sig.signature}\nSigner: ${sig.address}`;
        }
        return `Error: ${(resp as { message: string }).message}`;
      }

      default:
        return `Unknown tool: ${name}`;
    }
  }

  return { tools, executeTool };
}

function formatInbox(entries: InboxEntry[]): string {
  if (entries.length === 0) return "Inbox is empty.";
  return entries
    .map((e) => {
      const from = e.from_username ? `@${e.from_username}` : e.from_node_id.slice(0, 12);
      const status = e.acked ? "read" : "UNREAD";
      const type = e.message_type === "feed_post" ? "feed" : "dm";
      const time = new Date(e.timestamp_ms).toISOString();
      return `[${status}] [${type}] ${from} (${time}): ${e.body} [id: ${e.message_id}]`;
    })
    .join("\n");
}

function formatFollowList(list: FollowInfo[], label: string): string {
  if (list.length === 0) return `${label}: none.`;
  const items = list
    .map((f) => {
      const name = f.username ? `@${f.username}` : f.node_id.slice(0, 16);
      return `  - ${name} (${f.node_id.slice(0, 12)}...)`;
    })
    .join("\n");
  return `${label} (${list.length}):\n${items}`;
}
