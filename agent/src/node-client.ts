import { createConnection, Socket } from "net";
import { createInterface, Interface } from "readline";

/**
 * Client for communicating with the agentbook-node daemon
 * over the Unix socket JSON-lines protocol.
 */
export class NodeClient {
  private socket: Socket | null = null;
  private readline: Interface | null = null;
  private responseQueue: Array<(value: NodeResponse) => void> = [];
  private eventHandlers: Array<(event: NodeEvent) => void> = [];
  public nodeId: string = "";
  public version: string = "";

  async connect(socketPath: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.socket = createConnection(socketPath, () => {
        this.readline = createInterface({ input: this.socket! });
        this.readline.on("line", (line) => this.handleLine(line));
        this.socket!.on("error", (err) => {
          if (this.responseQueue.length > 0) {
            const handler = this.responseQueue.shift()!;
            handler({ type: "error", code: "connection_error", message: err.message });
          }
        });
      });

      this.socket.on("error", reject);

      // Wait for Hello message
      const onFirstLine = (line: string) => {
        try {
          const msg = JSON.parse(line);
          if (msg.type === "hello") {
            this.nodeId = msg.node_id;
            this.version = msg.version;
            resolve();
          } else {
            reject(new Error(`Expected hello, got: ${msg.type}`));
          }
        } catch (e) {
          reject(e);
        }
      };

      // Temporarily handle first line for Hello
      const tempHandler = (data: Buffer) => {
        const line = data.toString().trim();
        if (line) {
          this.socket!.removeListener("data", tempHandler);
          onFirstLine(line);
        }
      };
      this.socket.on("data", tempHandler);
    });
  }

  private handleLine(line: string): void {
    try {
      const msg: NodeResponse = JSON.parse(line);
      if (msg.type === "event") {
        for (const handler of this.eventHandlers) {
          handler((msg as EventResponse).event);
        }
      } else if (this.responseQueue.length > 0) {
        const handler = this.responseQueue.shift()!;
        handler(msg);
      }
    } catch {
      // ignore malformed lines
    }
  }

  onEvent(handler: (event: NodeEvent) => void): void {
    this.eventHandlers.push(handler);
  }

  async request(req: NodeRequest): Promise<NodeResponse> {
    if (!this.socket) throw new Error("Not connected");

    const line = JSON.stringify(req);
    this.socket.write(line + "\n");

    return new Promise((resolve) => {
      this.responseQueue.push(resolve);
    });
  }

  async getIdentity(): Promise<IdentityInfo | null> {
    const resp = await this.request({ type: "identity" });
    if (resp.type === "ok" && resp.data) return resp.data as IdentityInfo;
    return null;
  }

  async getInbox(unreadOnly: boolean = false, limit?: number): Promise<InboxEntry[]> {
    const resp = await this.request({ type: "inbox", unread_only: unreadOnly, limit });
    if (resp.type === "ok" && resp.data) return resp.data as InboxEntry[];
    return [];
  }

  async getFollowing(): Promise<FollowInfo[]> {
    const resp = await this.request({ type: "following" });
    if (resp.type === "ok" && resp.data) return resp.data as FollowInfo[];
    return [];
  }

  async getFollowers(): Promise<FollowInfo[]> {
    const resp = await this.request({ type: "followers" });
    if (resp.type === "ok" && resp.data) return resp.data as FollowInfo[];
    return [];
  }

  async sendDm(to: string, body: string): Promise<NodeResponse> {
    return this.request({ type: "send_dm", to, body });
  }

  async postFeed(body: string): Promise<NodeResponse> {
    return this.request({ type: "post_feed", body });
  }

  async lookupUsername(username: string): Promise<UsernameLookup | null> {
    const resp = await this.request({ type: "lookup_username", username });
    if (resp.type === "ok" && resp.data) return resp.data as UsernameLookup;
    return null;
  }

  async ackMessage(messageId: string): Promise<NodeResponse> {
    return this.request({ type: "inbox_ack", message_id: messageId });
  }

  async getHealth(): Promise<HealthStatus | null> {
    const resp = await this.request({ type: "health" });
    if (resp.type === "ok" && resp.data) return resp.data as HealthStatus;
    return null;
  }

  close(): void {
    this.socket?.destroy();
    this.readline?.close();
    this.socket = null;
    this.readline = null;
  }
}

// -- Protocol types (mirror Rust protocol.rs) --

export type NodeRequest =
  | { type: "identity" }
  | { type: "health" }
  | { type: "follow"; target: string }
  | { type: "unfollow"; target: string }
  | { type: "block"; target: string }
  | { type: "following" }
  | { type: "followers" }
  | { type: "register_username"; username: string }
  | { type: "lookup_username"; username: string }
  | { type: "send_dm"; to: string; body: string }
  | { type: "post_feed"; body: string }
  | { type: "inbox"; unread_only?: boolean; limit?: number }
  | { type: "inbox_ack"; message_id: string }
  | { type: "shutdown" };

export type NodeResponse = OkResponse | ErrorResponse | EventResponse | HelloResponse;

interface HelloResponse {
  type: "hello";
  node_id: string;
  version: string;
}

interface OkResponse {
  type: "ok";
  data?: unknown;
}

interface ErrorResponse {
  type: "error";
  code: string;
  message: string;
}

interface EventResponse {
  type: "event";
  event: NodeEvent;
}

export interface NodeEvent {
  kind: string;
  message_id?: string;
  from?: string;
  message_type?: string;
  preview?: string;
  node_id?: string;
}

export interface IdentityInfo {
  node_id: string;
  public_key_b64: string;
  username: string | null;
}

export interface FollowInfo {
  node_id: string;
  username: string | null;
  followed_at_ms: number;
}

export interface InboxEntry {
  message_id: string;
  from_node_id: string;
  from_username: string | null;
  message_type: string;
  body: string;
  timestamp_ms: number;
  acked: boolean;
}

export interface UsernameLookup {
  username: string;
  node_id: string;
  public_key_b64: string;
}

export interface HealthStatus {
  healthy: boolean;
  relay_connected: boolean;
  following_count: number;
  unread_count: number;
}
