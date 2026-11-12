import { useEffect, useState } from "react";
import type { ChatMessage, ChatResult, Command, Snapshot } from "../types";

type Props = {
  snapshot: Snapshot;
  sessionId: string;
  messages: ChatMessage[];
  result: ChatResult | null;
  enabled: boolean;
  send: (command: Command) => boolean;
  speaking: number[];
  pttBinding: string | null;
};

export function ChannelView({ snapshot, speaking, pttBinding, messages, result, enabled, send, sessionId }: Props) {
  const own = snapshot.clients.find((client) => client.is_self);
  const channel = snapshot.channels.find((item) => item.id === own?.channel_id);
  const clients = snapshot.clients.filter((client) => client.channel_id === channel?.id);

  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (pending && result?.request_id === pending) {
      if (!result.error) setDraft("");
      setError(result.error);
      setPending(null);
    }
  }, [pending, result]);
  useEffect(() => {
    if (!pending) return;
    const timer = window.setTimeout(() => {
      setPending(null);
      setError("Send not confirmed. Check the chat before retrying.");
    }, 20000);
    return () => window.clearTimeout(timer);
  }, [pending]);
  const valid = enabled && !pending && !!channel && draft.trim().length > 0 && new TextEncoder().encode(draft).length <= 1024;
  const submit = () => {
    if (!valid || !channel) return;
    const request_id = crypto.randomUUID();
    if (send({ op: "chat", session_id: sessionId, channel: String(channel.id), text: draft, request_id })) {
      setPending(request_id);
      setError(null);
    } else setError("Bridge disconnected. Your draft has been preserved.");
  };
  return (
    <section className="panel panel--center">
      <header className="channel-head">
        <div>
          <h1 className="channel-head__name">{channel?.name ?? "No channel"}</h1>
          <p className="channel-head__sub">
            {channel ? `${channel.codec} · 48 kHz` : "waiting for server state"}
            {channel?.topic ? ` · ${channel.topic}` : ""}
          </p>
        </div>
        <div className="chips">
          <span className="chip">
            {clients.length} {clients.length === 1 ? "client" : "clients"}
          </span>
          <span className="chip">{snapshot.server.max_clients} slots</span>
          <span className="chip">PTT {pttBinding || "—"}</span>
        </div>
      </header>

      <div className="grid">
        {clients.map((client) => {
          const isSpeaking = speaking.includes(client.id);
          return (
            <article key={client.id} className={`client ${isSpeaking ? "client--speaking" : ""}`}>
              <span className={`avatar ${isSpeaking ? "avatar--speaking" : ""}`} aria-hidden>
                {initials(client.name)}
              </span>
              <div className="client__meta">
                <span className="client__name">{client.name}</span>
                <span className="client__state">
                  {isSpeaking ? (
                    <span className="speaking">speaking</span>
                  ) : client.is_self ? (
                    "you"
                  ) : (
                    "connected"
                  )}
                </span>
              </div>
              {(client.input_muted || client.output_muted) && (
                <span className="tag tag--muted">{client.input_muted ? "muted" : "deafened"}</span>
              )}
            </article>
          );
        })}

        {clients.length === 0 && (
          <p className="empty">No one is in this channel. Select a channel on the left to join.</p>
        )}
      </div>

      <div className="chat-history" role="log" aria-label="Channel messages" aria-live="polite">
        {messages.filter((message) => message.channel === String(channel?.id)).map((message, index) => (
          <div className="chat-message" key={index}>
            <small>{message.ts} · {message.sender}{message.from_self ? " (you)" : ""}</small>
            <div>{message.text}</div>
          </div>
        ))}
      </div>
      {new TextEncoder().encode(draft).length > 1024 && <p role="alert">Messages are limited to 1024 UTF-8 bytes.</p>}
      {error && <p role="alert">{error}</p>}
      <form className="composer" onSubmit={(event) => { event.preventDefault(); submit(); }}>
        <input className="composer__input" aria-label="Channel message" type="text"
          placeholder={enabled ? "Message this channel" : "Channel chat unavailable"}
          disabled={!enabled || !!pending} value={draft} onChange={(event) => setDraft(event.target.value)} />
        <button type="submit" className="btn btn--ghost" disabled={!valid}>{pending ? "Sending..." : "Send"}</button>
      </form>
    </section>
  );
}

function initials(name: string): string {
  const trimmed = name.trim();
  return trimmed ? trimmed.slice(0, 2).toUpperCase() : "?";
}
