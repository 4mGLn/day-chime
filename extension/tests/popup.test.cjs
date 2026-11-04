const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const script = fs.readFileSync(require.resolve("../popup.js"), "utf8");
const saved = {ok: true, message: "오늘 출근 기록을 저장했습니다.", clock_in: "09:50:00", working_day: "2026-11-04", updated_at: "2026-11-04T06:00:00Z", auto_collect_enabled: true, polling_interval_minutes: 15};
function popup(statuses = [saved], {refreshResponse, preferencesResponse, errorKind, deferKind} = {}) {
  let clock = Date.parse("2026-11-04T06:00:00Z");
  class Clock extends Date { constructor(...args) { super(...(args.length ? args : [clock])); } static now() { return clock; } }
  const nodes = Object.fromEntries(["#version", "#status", "#attendance", "#attendance-date", "#time", "#refresh", "#auto-collect", "#polling-interval", "#save-settings", "#settings-feedback"].map((key) => [key, {textContent: "", hidden: false, disabled: false, checked: true, value: "15", attrs: {}, events: {}, setAttribute(name, value) { this.attrs[name] = value; }, addEventListener(name, fn) { this.events[name] = fn; }}]));
  const calls = [], timers = new Map(); let nextTimer = 1, last = statuses[0];
  const chrome = {runtime: {lastError: null, getManifest() { return {version: "0.4.2"}; }, sendMessage(message, callback) {
    calls.push(message); if (message.kind === deferKind) return; this.lastError = message.kind === errorKind ? {message: "disconnected"} : null;
    let reply;
    if (message.kind === "status") { last = statuses.shift() ?? last; reply = last; }
    if (message.kind === "refresh") reply = refreshResponse ?? {accepted: true, status: {...saved, ok: false, refreshing: true}};
    if (message.kind === "set-preferences") reply = preferencesResponse ?? {accepted: true, status: {...saved, auto_collect_enabled: message.auto_collect_enabled, polling_interval_minutes: message.polling_interval_minutes}};
    callback(reply); this.lastError = null;
  }}};
  vm.runInNewContext(script, {chrome, Date: Clock, Intl, document: {querySelector: (selector) => nodes[selector]}, setTimeout: (fn, delay) => { const id = nextTimer++; timers.set(id, {fn, when: clock + delay}); return id; }, clearTimeout: (id) => timers.delete(id)});
  return {nodes, calls, timers, click: (selector, event = "click") => nodes[selector].events[event](), tick(ms = 1000) { clock += ms; const callbacks = [...timers].filter(([, t]) => t.when <= clock); callbacks.forEach(([id, t]) => { if (timers.delete(id)) t.fn(); }); }};
}
test("clock card is always visible and displays cached attendance even during failure", () => {
  const view = popup([{...saved, ok: false, message: "위젯 로딩 실패"}]);
  assert.equal(view.nodes["#attendance"].textContent, "09:50"); assert.match(view.nodes["#attendance-date"].textContent, /마지막으로 확인/);
  const empty = popup([{ok: true, message: "legacy"}]); assert.equal(empty.nodes["#attendance"].textContent, "--:--"); assert.match(empty.nodes["#attendance-date"].textContent, /새로고침/);
});
test("previous date is qualified and malformed times never appear as valid attendance", () => {
  assert.match(popup([{...saved, working_day: "2026-11-03"}]).nodes["#attendance-date"].textContent, /2026-11-03/);
  for (const status of [{...saved, clock_in: "25:00:00"}, {...saved, working_day: "2026-02-30"}]) assert.equal(popup([status]).nodes["#attendance"].textContent, "--:--");
});
test("refresh click starts actual collection and polls until fresh clock arrives", () => {
  const h = popup([saved, {...saved, clock_in: "10:15:00"}]); h.click("#refresh");
  assert.equal(h.calls.at(-1).kind, "refresh"); assert.equal(h.nodes["#refresh"].disabled, true); assert.equal(h.nodes["#refresh"].attrs["aria-busy"], "true"); assert.equal(h.nodes["#attendance"].textContent, "09:50");
  h.tick(); assert.equal(h.nodes["#attendance"].textContent, "10:15"); assert.equal(h.nodes["#refresh"].disabled, false); assert.equal(h.timers.size, 0);
});
test("failed refresh and bounded timeout stop spinner while retaining last confirmed clock", () => {
  const h = popup([saved], {refreshResponse: {accepted: false, message: "연결 실패"}}); h.click("#refresh"); assert.equal(h.nodes["#refresh"].disabled, false); assert.equal(h.nodes["#attendance"].textContent, "09:50"); assert.match(h.nodes["#status"].textContent, /연결 실패/);
  const timeout = popup([{...saved, refreshing: true}]); timeout.tick(70_001); assert.equal(timeout.nodes["#refresh"].disabled, false); assert.equal(timeout.timers.size, 0); assert.match(timeout.nodes["#status"].textContent, /지연/);
  const disconnected = popup([saved], {errorKind: "refresh"}); disconnected.click("#refresh"); assert.equal(disconnected.nodes["#refresh"].disabled, false);
});
test("settings validate interval and OFF toggle persists independently of invalid unsaved interval", () => {
  const h = popup(); h.nodes["#polling-interval"].value = "0"; h.click("#save-settings"); assert.equal(h.calls.length, 1); assert.match(h.nodes["#settings-feedback"].textContent, /1~1440/);
  h.nodes["#polling-interval"].value = ""; h.nodes["#auto-collect"].checked = false; h.click("#auto-collect", "change");
  assert.equal(h.calls.at(-1).kind, "set-preferences"); assert.equal(h.calls.at(-1).auto_collect_enabled, false); assert.equal(h.calls.at(-1).polling_interval_minutes, 15);
  h.nodes["#polling-interval"].value = "3"; h.click("#save-settings"); assert.equal(h.calls.at(-1).polling_interval_minutes, 3);
});
test("failed preference save restores actual toggle instead of presenting a false OFF state", () => {
  const h = popup([saved], {preferencesResponse: {accepted: false, message: "保存失敗"}}); h.nodes["#auto-collect"].checked = false; h.click("#auto-collect", "change"); assert.equal(h.nodes["#auto-collect"].checked, true);
});

test("independent watchdog releases refresh even when callback never arrives", () => {
  const h = popup([saved], {deferKind: "refresh"});
  assert.equal(h.nodes["#version"].textContent, "v0.4.2");
  h.click("#refresh"); h.tick(70_000);
  assert.equal(h.nodes["#refresh"].disabled, false); assert.equal(h.nodes["#attendance"].textContent, "09:50"); assert.match(h.nodes["#status"].textContent, /지연/);
});
