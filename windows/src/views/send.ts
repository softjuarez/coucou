// "Send to…" — hands the dropped file to one of the user's webhooks (an n8n
// workflow, typically), with who it is for and a note. The Windows counterpart
// of MailView in IslandViewContent.swift: nothing leaves without a click on Send.

import { h, clear } from "./dom";
import { Bridge } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { ViewActions, ViewHost } from "./views";

export function buildSend(actions: ViewActions): ViewHost {
  const title = h("div", { class: "title" });
  const chips = h("div", { class: "send-chips" });
  const to = h("input", {
    type: "text", class: "send-field", placeholder: "To — number, name or chat (optional)", spellcheck: "false",
  }) as HTMLInputElement;
  const note = h("input", {
    type: "text", class: "send-field", placeholder: "Message (optional)", spellcheck: "false",
  }) as HTMLInputElement;
  const problem = h("div", { class: "send-problem" });
  const cancel = h("button", { class: "btn secondary", text: "Cancel", onclick: () => leave() });
  const send = h("button", { class: "btn primary", text: "Send", onclick: () => void submit() });
  const openSettings = h("button", {
    class: "btn primary", text: "Open settings", onclick: () => actions.openSettingsWindow(),
  });
  const row = h("div", { class: "actions" });
  const form = h("div", { class: "send-form" }, chips, to, note, problem);
  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card" }, h("div", { class: "stack", style: "padding:4px 18px 4px 98px" }, title, form, row)),
  );

  let selected = "";
  let sending = false;
  let chipsKey = "";
  let rowKey = "";

  function leave() {
    problem.textContent = "";
    actions.setView(State.defaultView());
  }

  async function submit() {
    const destination = State.settings.destinations.find((d) => d.id === selected);
    if (!destination || sending) return;
    sending = true;
    problem.textContent = "";
    State.notify();
    try {
      await Bridge.sendToDestination(destination.id, State.droppedFile, to.value, note.value);
      Sound.play("send");
      to.value = "";
      note.value = "";
      State.noteMessage = `Sent to ${destination.name}.`;
      actions.setView("note");
    } catch (err) {
      Sound.play("error");
      problem.textContent = String(err).replace(/^Error:\s*/, "");
    } finally {
      sending = false;
      State.notify();
    }
  }

  for (const field of [to, note]) {
    field.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") {
        e.preventDefault();
        void submit();
      }
      e.stopPropagation(); // Escape closes the island, not the form
    });
  }

  return {
    el,
    sync() {
      const destinations = State.settings.destinations;
      if (!destinations.some((d) => d.id === selected)) selected = destinations[0]?.id ?? "";

      clear(title);
      if (destinations.length === 0) {
        title.textContent = "No destination yet.";
      } else {
        title.append(
          document.createTextNode("Send "),
          h("b", { text: State.droppedFile?.name ?? "the file" }),
          document.createTextNode(" to"),
        );
      }
      form.style.display = destinations.length === 0 ? "none" : "";

      // Rebuilt only when the list or the choice changes: a button rebuilt
      // under the pointer would swallow the click.
      const key = destinations.map((d) => `${d.id}:${d.name}`).join("|") + "#" + selected;
      if (key !== chipsKey) {
        chipsKey = key;
        clear(chips);
        for (const d of destinations) {
          chips.append(
            h("button", {
              class: d.id === selected ? "send-chip on" : "send-chip",
              text: d.name,
              onclick: () => {
                selected = d.id;
                actions.blip();
                State.notify();
              },
            }),
          );
        }
      }

      const wantRow = destinations.length === 0 ? "empty" : "form";
      if (wantRow !== rowKey) {
        rowKey = wantRow;
        clear(row);
        if (wantRow === "empty") row.append(cancel, openSettings);
        else row.append(cancel, send);
      }
      send.textContent = sending ? "Sending…" : "Send";
      (send as HTMLButtonElement).disabled = sending;
    },
    focus() {
      to.focus();
    },
  };
}
