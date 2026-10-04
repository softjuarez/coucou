// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, onEvent, type ChatContext, type ChatProgress } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewActions, ViewHost } from "./views";

let nextId = 1;

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

interface PendingAction {
  requestId: string;
  label: string;
  detail: string;
}

export function buildPrompt(onHeightChange: () => void, actions: ViewActions): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let rendered = "";

  // A turn through Claude Code reports as it goes: the answer as it is written,
  // the tool it is using, and any action waiting for a click.
  let streamText = "";
  let toolLabel = "";
  let pending: PendingAction | null = null;
  let pendingEl: HTMLElement | null = null;

  function clearProgress() {
    streamText = "";
    toolLabel = "";
    if (pending) actions.holdForChat(false);
    pending = null;
    pendingEl = null;
  }

  function decide(allow: boolean) {
    if (!pending) return;
    Sound.play(allow ? "approve" : "blip");
    void Bridge.chatApproval(pending.requestId, allow);
    pending = null;
    pendingEl = null;
    actions.holdForChat(false);
    State.notify();
  }

  /** Built once per request: rebuilding it under the pointer would swallow the click. */
  function approvalCard(action: PendingAction): HTMLElement {
    return h(
      "div",
      { class: "chat-row" },
      h(
        "div",
        { class: "chat-approval" },
        h("div", { class: "what", text: action.label }),
        action.detail ? h("div", { class: "detail", text: action.detail }) : null,
        h(
          "div",
          { class: "actions" },
          h("button", { class: "btn secondary", text: "Deny", onclick: () => decide(false) }),
          h("button", { class: "btn primary", text: "Allow", onclick: () => decide(true) }),
        ),
      ),
    );
  }

  void onEvent<ChatProgress>("chat-progress", (p) => {
    if (!sending) return;
    if (p.kind === "delta") {
      streamText += p.text;
    } else if (p.kind === "tool") {
      // Whatever was written before a tool call was a preamble, not the answer.
      streamText = "";
      toolLabel = p.label;
    } else {
      streamText = "";
      toolLabel = p.label;
      pending = { requestId: p.requestId, label: p.label, detail: p.detail };
      pendingEl = approvalCard(pending);
      Sound.play("approval");
      actions.holdForChat(true);
    }
    State.notify();
  });

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    Sound.play("send");

    // A dropped file starts a new conversation by replacing this array; an
    // answer that comes back after that belongs to nobody.
    const history = State.chatHistory;
    history.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null =
      State.chatHistory.length === 1 && file ? { kind: "file", name: file.name, path: file.path } : null;

    try {
      const reply = await Bridge.chatSend(query, context);
      if (State.chatHistory !== history) {
        if (State.stateOverride === "thinking") State.stateOverride = null;
        return;
      }
      history.push({ id: nextId++, role: "assistant", content: reply.text });
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      if (State.chatHistory !== history) {
        if (State.stateOverride === "thinking") State.stateOverride = null;
        return;
      }
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      clearProgress();
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const thinking = State.stateOverride === "thinking";
      const key = [
        State.chatHistory.length, thinking, streamText.length, toolLabel, pending?.requestId ?? "",
      ].join("|");
      if (key !== rendered) {
        rendered = key;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) {
          // The card names the tool itself.
          if (toolLabel && !pendingEl) {
            log.append(h("div", { class: "chat-row" }, h("div", { class: "chat-tool", text: toolLabel })));
          }
          if (pendingEl) log.append(pendingEl);
          else if (streamText) log.append(h("div", { class: "chat-row" }, h("div", { class: "reply", text: streamText })));
          else log.append(typingDots());
        }
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
