// tmux-prefix, adapted from herdrdev/herdr-layout-examples (Apache-2.0),
// tmux-prefix/mods/tmux-prefix at ed4eda7a02e255191203df0ff04b9dc7ae56e1ad:
// https://github.com/herdrdev/herdr-layout-examples/tree/main/tmux-prefix
//
// Changes from upstream, to match modules/binarin/tmux.nix (prefix C-o,
// pain-control, base-index 1):
// - The prefix is Ctrl+O. Ctrl+O Ctrl+O goes to the last tab (tmux
//   last-window, through the last-tab mod's history); Ctrl+O o sends a
//   literal ^O (tmux send-prefix).
// - Bindings may carry Ctrl or Alt (pain-control's C-h/j/k/l).
// - Nearly every app key lives here, so the terminal gets everything else
//   (keys.json keeps only the prefix and clipboard keys): C-1..9 spaces,
//   M-h/j/k/l swap panes, [ ] agents, a A next/previous done agent, u
//   unread (host-agents), N new space, / search, C-x + - 0 zoom.
// - More tmux keys: | \ - _ splits, & close tab, , $ rename, ( ) spaces,
//   : palette, b sidebar.
// - s / w open host-picker (fuzzy spaces+tabs / agents, with host tags);
//   e toggles host-spaces' overview (every space's tabs);
//   ? opens keys-help (keys.json and this table in one list). The table is
//   published in shared "tmux-prefix.bindings" for keys-help to read.
// - Keymaps nest, Emacs style: a binding can open another keymap (C-o C-x
//   for zoom), shown as you go.
// - Repeat maps, like Emacs's repeat-mode: a binding with a repeat group
//   leaves the popup in repeat mode, where the bare keys of that group (in the
//   same keymap) keep working: C-o C-x + + + - . No timeout. Any other key
//   ends it and is handled as usual: a keys.json binding runs (C-o starts a
//   fresh prefix), anything else goes to the terminal through `hc keys`.
//   hc keys can't type shifted punctuation (+ | < ? ...) or ctrl+shift keys:
//   those end repeat mode and are dropped. Esc just ends it.
// - While open, sets shared "tmux-prefix.open": host-spaces and host-tabs
//   show the space / tab numbers the digit keys select.
// - Opens the last-tab tracker when it isn't running.
import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { closeOverlay, commands, focused, hc, keys, nextCommand, openOverlay, props, run, setShared, shared, spaces, theme } from "herdr";

const OPEN = "tmux-prefix.open";
// Repeat mode ends after this many ms without a repeat key; null: only another key ends it (Emacs).
const REPEAT_EXIT_MS = null;
// [key, label, command, args, repeat group]. Keys are what a plain press types
// (key_char, so "%", "J", "|"), or "ctrl-<key>" / "alt-<key>" with Ctrl / Alt
// held. Command "overlay" opens args as an overlay (mod.open can't pass
// props); "keymap" opens args.bindings as the next keymap. After a binding
// with a repeat group, the bare keys of that group in its keymap repeat it.
const PICKER = (mode) => ({ mod: "host-picker", kind: "dialog", width: 760, height: 0.7, props: { mode } });
const C_X = [
  ["+", "Zoom in", "app.zoom_in", {}, "zoom"],
  ["=", "Zoom in", "app.zoom_in", {}, "zoom"],
  ["-", "Zoom out", "app.zoom_out", {}, "zoom"],
  ["0", "Reset zoom", "app.zoom_reset", {}, "zoom"],
];
const BINDINGS = [
  ["ctrl-o", "Last tab", "last-tab"],
  ["c", "New tab", "tab.new"],
  ["n", "Next tab", "tab.next"],
  ["p", "Previous tab", "tab.prev"],
  [",", "Rename tab", "tab.rename"],
  ["&", "Close tab", "tab.close"],
  ["|", "Split right", "pane.split", { direction: "right" }],
  ["\\", "Split right", "pane.split", { direction: "right" }],
  ["%", "Split right", "pane.split", { direction: "right" }],
  ["-", "Split down", "pane.split", { direction: "down" }],
  ["_", "Split down", "pane.split", { direction: "down" }],
  ['"', "Split down", "pane.split", { direction: "down" }],
  ["x", "Close pane", "pane.close"],
  ["z", "Zoom pane", "pane.zoom"],
  ["h", "Focus left", "pane.go", { direction: "left" }, "focus"],
  ["j", "Focus down", "pane.go", { direction: "down" }, "focus"],
  ["k", "Focus up", "pane.go", { direction: "up" }, "focus"],
  ["l", "Focus right", "pane.go", { direction: "right" }, "focus"],
  ["ctrl-h", "Focus left", "pane.go", { direction: "left" }, "focus"],
  ["ctrl-j", "Focus down", "pane.go", { direction: "down" }, "focus"],
  ["ctrl-k", "Focus up", "pane.go", { direction: "up" }, "focus"],
  ["ctrl-l", "Focus right", "pane.go", { direction: "right" }, "focus"],
  ["left", "Focus left", "pane.go", { direction: "left" }, "focus"],
  ["down", "Focus down", "pane.go", { direction: "down" }, "focus"],
  ["up", "Focus up", "pane.go", { direction: "up" }, "focus"],
  ["right", "Focus right", "pane.go", { direction: "right" }, "focus"],
  ["alt-h", "Swap pane left", "pane.swap", { direction: "left" }, "swap"],
  ["alt-j", "Swap pane down", "pane.swap", { direction: "down" }, "swap"],
  ["alt-k", "Swap pane up", "pane.swap", { direction: "up" }, "swap"],
  ["alt-l", "Swap pane right", "pane.swap", { direction: "right" }, "swap"],
  ["/", "Search the terminal", "terminal.search"],
  ["N", "New space", "space.new"],
  ["J", "Next space", "space.next", {}, "spaces"],
  ["K", "Previous space", "space.prev", {}, "spaces"],
  [")", "Next space", "space.next"],
  ["(", "Previous space", "space.prev"],
  ["$", "Rename space", "space.rename"],
  ["s", "Go to space / tab (fuzzy)", "overlay", PICKER("spaces")],
  ["w", "Go to agent (fuzzy)", "overlay", PICKER("agents")],
  ["e", "Spaces overview (all tabs)", "toggle-overview"],
  ["]", "Next agent", "agent.next", {}, "agents"],
  ["[", "Previous agent", "agent.prev", {}, "agents"],
  ["a", "Next done agent", "host-agents.next-done", {}, "done"],
  ["A", "Previous done agent", "host-agents.prev-done", {}, "done"],
  ["u", "Mark agent unread / read", "host-agents.toggle-unread"],
  ["?", "All keys", "overlay", { mod: "keys-help", kind: "dialog", width: 680, height: 0.8 }],
  [":", "Command palette", "app.palette"],
  ["b", "Toggle sidebar", "layout.toggle", { id: "sidebar" }],
  ["d", "Minimize window", "window.minimize"],
  ["ctrl-x", "Zoom…", "keymap", { bindings: C_X }],
  ...Array.from({ length: 10 }, (_, i) => [String(i), `Tab ${i === 0 ? 10 : i}`, "tab.select", { index: i === 0 ? 10 : i }]),
  ...Array.from({ length: 9 }, (_, i) => [`ctrl-${i + 1}`, `Space ${i + 1}`, "space.select", { index: i + 1 }]),
];
// Left out of the root popup (keys-help lists them all).
const DETAIL = (key) => /^(left|right|up|down|ctrl-[hjkl]|alt-[hjkl]|\d|ctrl-\d)$/.test(key) || ["\\", "%", "_", '"'].includes(key);
const pretty = (k) => k.replace(/^ctrl-/, "C-").replace(/^alt-/, "M-");

// Every binding with its whole key sequence, for keys-help.
function flatten(map, path = []) {
  return map.flatMap(([key, label, command, args, group]) => command === "keymap"
    ? flatten(args.bindings, [...path, key])
    : [[[...path, key].join(" "), group ? `${label} (repeats)` : label]]);
}

// The last-tab mod's tracker and history (see mods/last-tab/main.js).
const LAST_TAB_HISTORY = "last-tab.history";
const LAST_TAB_TRACKER = {
  mod: "last-tab", kind: "window", key: "last-tab-tracker", keep: true, frame: "none",
  width: 2, height: 2, at: { x: 0, y: 0 }, props: { role: "tracker" },
};

function ensureLastTabTracker() {
  // The tracker beats every 500 ms while it runs; reopening replaces it in place.
  if (Date.now() - (globalThis.__herdrLastTabBeat ?? 0) > 2000) openOverlay(LAST_TAB_TRACKER);
}

function lastTab() {
  const current = focused()?.tab;
  const live = new Set(spaces().flatMap((s) => s.tabs.map((t) => t.id)));
  const history = Array.isArray(shared(LAST_TAB_HISTORY)) ? shared(LAST_TAB_HISTORY) : [];
  const target = history.find((id) => id !== current && live.has(id));
  if (target) run("tab.select", { id: target });
}

// keys.json's way of writing a key event ("ctrl-shift-c"), to look it up in keys().
function keysName(e) {
  const m = e.modifiers;
  return [m.control && "ctrl", m.alt && "alt", m.shift && "shift", m.platform && "cmd", e.key].filter(Boolean).join("-");
}

const NAMED = ["enter", "tab", "backspace", "left", "right", "up", "down", "home", "end", "pageup", "pagedown", "delete", "insert", "space",
  ...Array.from({ length: 12 }, (_, i) => `f${i + 1}`)];
const UNSHIFTED = "abcdefghijklmnopqrstuvwxyz0123456789`-=[]\\;',./";
// The key event as `hc keys` names it, or null when hc keys can't type it
// (shifted punctuation, ctrl+shift letters: they come out without the shift).
function hcKey(e) {
  const m = e.modifiers;
  if (m.platform) return null;
  const mods = `${m.control ? "ctrl+" : ""}${m.alt ? "alt+" : ""}`;
  if (NAMED.includes(e.key)) return `${mods}${m.shift ? "shift+" : ""}${e.key}`;
  if (m.control || m.alt) return !m.shift && e.key.length === 1 && UNSHIFTED.includes(e.key) ? `${mods}${e.key}` : null;
  const ch = e.key_char ?? e.key;
  if (ch === " ") return "space";
  return ch.length === 1 && (/[A-Z]/.test(ch) || UNSHIFTED.includes(ch)) ? ch : null;
}

// "send:<text>" as hc keys: a control character, or Esc before a character (Alt).
function sendAsKey(text) {
  if (text.length === 1 && text.charCodeAt(0) < 32) return `ctrl+${String.fromCharCode(text.charCodeAt(0) + 96)}`;
  if (text.length === 2 && text[0] === "\u001b") return `alt+${text[1]}`;
  return null;
}

export default class TmuxPrefix extends View {
  init(_props, cx) {
    this.handle = cx.focus_handle();
    this.handle.focus();
    // Lists show the numbers C-o 1..9 / C-o C-1..9 target while this is set.
    if (!shared(OPEN)) setShared(OPEN, true);
    this.reset();
    this.generation = 0;
    ensureLastTabTracker();
    const table = [...flatten(BINDINGS), ["o", "Send a literal ^O"], ["Esc", "Cancel"]];
    if (JSON.stringify(shared("tmux-prefix.bindings")) !== JSON.stringify(table)) setShared("tmux-prefix.bindings", table);
    // keys.json's mod:tmux-prefix section beats the global opener while we're
    // open, so a second Ctrl+O arrives here as "last-tab": right after the
    // prefix it's C-o C-o; deeper in, or repeating, it starts a fresh prefix.
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command === "send-prefix") this.sendPrefix(cx);
        else if (command === "last-tab" && this.map === BINDINGS && !this.repeat) this.act(BINDINGS[0], BINDINGS, cx);
        else if (command === "last-tab") this.reset();
        cx.notify();
      }
    });
  }

  // However the popup goes (a key, Esc, a click beside it, quitting).
  async close() {
    setShared(OPEN, false);
  }

  // Back to "waiting for a key" at the top keymap.
  reset() {
    this.map = BINDINGS;
    this.path = ["ctrl-o"];
    this.repeat = null; // { map, group } while repeating
    this.generation = (this.generation ?? 0) + 1;
  }

  dismiss() {
    closeOverlay(props().overlay);
  }

  // Types `key` (hc keys syntax) into the focused terminal, after closing.
  send(key, cx) {
    this.generation++;
    const terminal = focused()?.terminal;
    if (!terminal) return this.dismiss();
    // hc starts its subprocess at the call, before closing cancels this view's tasks.
    const pending = hc(["keys", terminal, key]);
    cx.spawn(async () => {
      const result = await pending;
      if (result.code !== 0) console.log(`tmux-prefix: ${result.stderr}`);
    });
    this.dismiss();
  }

  sendPrefix(cx) {
    this.send("ctrl+o", cx);
  }

  act(binding, map, cx) {
    const [key, , command, args = {}, group] = binding;
    if (command === "keymap") {
      this.map = args.bindings;
      this.path = [...this.path, key];
      return cx.notify();
    }
    if (!group) this.dismiss();
    if (command === "last-tab") lastTab();
    else if (command === "overlay") openOverlay(args);
    // host-spaces draws the overview while this shared flag is on.
    else if (command === "toggle-overview") setShared("host-spaces.overview", !shared("host-spaces.overview"));
    else run(command, args);
    if (group) {
      this.repeat = { map, group };
      const generation = ++this.generation;
      if (REPEAT_EXIT_MS) cx.timer.after(REPEAT_EXIT_MS, () => { if (this.generation === generation) this.dismiss(); });
      cx.notify();
    }
  }

  // A key that isn't one of the repeating ones: what it would have done
  // without us (its keys.json binding, else typed into the terminal).
  exitRepeat(e, cx) {
    const name = keysName(e);
    const bound = keys().find((k) => k.section === "terminal" && k.keys === name) ?? keys().find((k) => k.section === "global" && k.keys === name);
    if (bound) {
      const action = bound.action;
      if (action.startsWith("send:")) {
        const k = sendAsKey(action.slice(5));
        return k ? this.send(k, cx) : this.dismiss();
      }
      this.dismiss();
      const [cmd, value] = [action.slice(0, action.indexOf(":") < 0 ? undefined : action.indexOf(":")), action.includes(":") ? action.slice(action.indexOf(":") + 1) : undefined];
      if (value === undefined) return run(cmd, {});
      const first = commands().find((c) => c.name === cmd)?.args?.[0]?.name;
      return run(cmd, first ? { [first]: value } : {});
    }
    const k = hcKey(e);
    if (k) return this.send(k, cx);
    this.dismiss();
  }

  key(e, cx) {
    cx.stop_propagation();
    const m = e.modifiers;
    if (e.key === "escape") return this.dismiss();
    const plain = !m.control && !m.alt && !m.platform;
    const ctrl = m.control && !m.alt && !m.platform;
    const alt = m.alt && !m.control && !m.platform;
    const key = plain ? (e.key_char ?? (m.shift && e.key.length === 1 ? e.key.toUpperCase() : e.key)) : e.key;
    const name = plain ? key : ctrl ? `ctrl-${e.key}` : alt ? `alt-${e.key}` : null;
    if (this.repeat) {
      const { map, group } = this.repeat;
      const again = name && map.find((b) => b[0] === name && b[4] === group);
      return again ? this.act(again, map, cx) : this.exitRepeat(e, cx);
    }
    if (this.map === BINDINGS && plain && key === "o") return this.sendPrefix(cx);
    const binding = name && this.map.find((b) => b[0] === name);
    if (!binding) return this.dismiss();
    this.act(binding, this.map, cx);
  }

  render() {
    const t = theme();
    const top = this.map === BINDINGS;
    const repeating = this.repeat && this.repeat.map.filter((b) => b[4] === this.repeat.group);
    const rows = repeating || (top ? BINDINGS.filter(([key]) => !DETAIL(key)) : this.map);
    const title = repeating ? "Repeat · any other key exits" : `${this.path.map(pretty).join(" ")} · waiting for a key`;
    return v_flex().id("tmux-prefix").track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .p(14).gap(8).bg(t.popover).text_color(t.foreground).text_size(12)
      .child(h_flex().justify_between().child(div().font_semibold().child(title))
        .child(div().text_color(t.muted).child(top && !repeating ? "Esc cancel · o send ^O" : "Esc cancel")))
      .child(v_flex().h(280).min_h(0).overflow_y_scrollbar().children(rows.map(([key, label, command, , group]) =>
        h_flex().h(20).flex_none().items_center().gap(12).child(div().w(48).text_color(t.accent).child(pretty(key)))
          .child(div().flex_1().child(label))
          .child(div().text_color(t.dim).child(command === "keymap" ? "keys…" : group ? "repeats" : "")))))
      .child(div().text_color(t.muted).child(top && !repeating
        ? "1–9 tabs (0 = 10) · C-1–9 spaces · M-hjkl swap · C-x zoom · C-o C-o last tab · ? every key"
        : "C-o starts over"));
  }
}
