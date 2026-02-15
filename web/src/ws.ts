/**
 * WebSocket client for tmax-web.
 *
 * Multiplexes multiple session subscriptions over a single WebSocket.
 * Binary frames: [sid_len: u8][sid: bytes][pty_data: bytes]
 * Text frames: JSON control messages.
 */

type OutputCallback = (data: Uint8Array) => void;
type EventCallback = (event: WsEvent) => void;

export interface WsEvent {
  type: string;
  session_id?: string;
  [key: string]: unknown;
}

interface SessionCallbacks {
  onOutput: OutputCallback;
  onEvent?: EventCallback;
}

export class TmaxWsClient {
  private ws: WebSocket | null = null;
  private subs = new Map<string, SessionCallbacks>();
  private pendingMessages: string[] = [];
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  constructor(private url: string) {
    this.connect();
  }

  private connect(): void {
    if (this.disposed) return;

    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      for (const msg of this.pendingMessages) {
        ws.send(msg);
      }
      this.pendingMessages = [];
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (ev.data instanceof ArrayBuffer) {
        this.handleBinaryFrame(new Uint8Array(ev.data));
      } else {
        this.handleTextMessage(ev.data as string);
      }
    };

    ws.onclose = () => {
      this.ws = null;
      if (!this.disposed) {
        this.reconnectTimer = setTimeout(() => this.connect(), 1000);
      }
    };

    ws.onerror = () => {
      ws.close();
    };

    this.ws = ws;
  }

  private handleBinaryFrame(frame: Uint8Array): void {
    if (frame.length === 0) return;
    const sidLen = frame[0]!;
    if (frame.length < 1 + sidLen) return;
    const sid = new TextDecoder().decode(frame.slice(1, 1 + sidLen));
    const data = frame.slice(1 + sidLen);
    this.subs.get(sid)?.onOutput(data);
  }

  private handleTextMessage(raw: string): void {
    try {
      const msg = JSON.parse(raw) as WsEvent;
      const sid = msg.session_id;
      if (sid) {
        this.subs.get(sid)?.onEvent?.(msg);
      }
    } catch {
      // ignore malformed messages
    }
  }

  private send(msg: object): void {
    const json = JSON.stringify(msg);
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(json);
    } else {
      this.pendingMessages.push(json);
    }
  }

  subscribe(
    sessionId: string,
    onOutput: OutputCallback,
    onEvent?: EventCallback,
  ): void {
    this.subs.set(sessionId, { onOutput, onEvent });
    this.send({ action: "subscribe", session_id: sessionId, mode: "edit" });
  }

  unsubscribe(sessionId: string): void {
    this.subs.delete(sessionId);
    this.send({ action: "unsubscribe", session_id: sessionId });
  }

  sendInput(sessionId: string, data: string): void {
    this.send({
      action: "input",
      session_id: sessionId,
      data: btoa(data),
    });
  }

  sendResize(sessionId: string, cols: number, rows: number): void {
    this.send({
      action: "resize",
      session_id: sessionId,
      cols,
      rows,
    });
  }

  dispose(): void {
    this.disposed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.ws?.close();
    this.subs.clear();
  }
}

/** Singleton WS client — the URL uses the Vite proxy so it's relative. */
let instance: TmaxWsClient | null = null;

export function getWsClient(): TmaxWsClient {
  if (!instance) {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    instance = new TmaxWsClient(`${proto}//${location.host}/ws`);
  }
  return instance;
}
