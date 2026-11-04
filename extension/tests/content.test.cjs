const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const script = fs.readFileSync(require.resolve("../content.js"), "utf8");
function content({enabled = true, minutes = 15, snapshots = [{available: true, clock_in: "09:50:00"}], invalidateAt, asyncCallbacks = false} = {}) {
  let clock = 100_000, reads = 0, listener, id = 0, mutation, disconnected = false, removed = false;
  const timers = new Map(), sent = [], callbacks = [];
  let propertyFailure = null;
  class Clock extends Date { static now() { return clock; } }
  const config = {enabled, interval_minutes: minutes};
  const chrome = {runtime: {id: invalidateAt === "missing-id" ? undefined : "test", lastError: null, sendMessage(message, callback) {
    sent.push(message);
    if (invalidateAt === message.kind) throw Error("Extension context invalidated.");
    if (invalidateAt === "callback" || (invalidateAt === "attendance-callback" && message.kind === "attendance")) this.lastError = {message: "Extension context invalidated."};
    const invoke = () => { callback(message.kind === "collection-config" ? config : {ok: true}); this.lastError = null; };
    if (asyncCallbacks) callbacks.push(invoke); else invoke();
  }}, storage: {onChanged: {addListener(fn) { listener = fn; }, removeListener(fn) { if (fn === listener) removed = true; }}}};
  const runtime = chrome.runtime;
  let runtimeId = runtime.id, lastError = runtime.lastError;
  Object.defineProperty(runtime, "id", {get() { if (propertyFailure === "id") throw Error("Extension context invalidated."); return runtimeId; }, set(value) { runtimeId = value; }});
  Object.defineProperty(runtime, "lastError", {get() { if (propertyFailure === "lastError") throw Error("Extension context invalidated."); return lastError; }, set(value) { lastError = value; }});
  Object.defineProperty(chrome, "runtime", {get() { if (propertyFailure === "runtime") throw Error("Extension context invalidated."); return runtime; }});
  vm.runInNewContext(script, {chrome, Date: Clock, location: {href: "https://example.daouoffice.com/home"}, document: {documentElement: {}}, DayChimeReader: {isHome: () => true, read() { const snapshot = snapshots[Math.min(reads, snapshots.length - 1)]; reads++; if (invalidateAt === "between-sends") chrome.runtime.id = undefined; return snapshot; }}, MutationObserver: class { constructor(fn) { mutation = fn; } observe() {} disconnect() { disconnected = true; } }, setTimeout(fn, delay) { const handle = ++id; timers.set(handle, {fn, when: clock + delay}); return handle; }, clearTimeout(handle) { timers.delete(handle); }, console});
  return {breakProperty(name) { propertyFailure = name; }, flushOne() { callbacks.shift()?.(); }, flush() { while (callbacks.length) callbacks.shift()(); }, get disconnected() { return disconnected; }, get removed() { return removed; }, mutation() { mutation(); }, get reads() { return reads; }, config, sent, timers, change(changes) { listener(changes, "local"); }, advance(ms) { clock += ms; const due = [...timers].filter(([, t]) => t.when <= clock); for (const [handle, t] of due) { timers.delete(handle); t.fn(); } }};
}
test("disabled collection never reads attendance DOM or sends attendance", () => { const h = content({enabled: false}); assert.equal(h.reads, 0); assert.equal(h.sent.some((m) => m.kind === "attendance"), false); h.advance(15 * 60_000); assert.equal(h.reads, 0); });
test("after widget settles content follows configured interval", () => { const h = content({minutes: 3}); assert.equal(h.reads, 1); h.advance(30_000); assert.equal(h.reads, 1); h.advance(150_000); assert.equal(h.reads, 2); });
test("SPA bootstrap retries missing widget within owned-tab timeout before long polling", () => { const h = content({snapshots: [{available: false}, {available: true, clock_in: "09:50:00"}]}); assert.equal(h.reads, 1); h.advance(2000); assert.equal(h.reads, 2); assert.equal(h.sent.filter((m) => m.kind === "attendance").at(-1).snapshot.available, true); h.advance(30_000); assert.equal(h.reads, 2); });
test("preference changes take effect before further DOM reads", () => { const h = content({enabled: false}); h.config.enabled = true; h.config.interval_minutes = 2; h.change({auto_collect_enabled: {newValue: true}}); h.advance(0); assert.equal(h.reads, 1); h.config.enabled = false; h.change({auto_collect_enabled: {newValue: false}}); h.advance(0); assert.equal(h.reads, 1); h.advance(120_000); assert.equal(h.reads, 1); });

test("invalidated contexts stop timers observers and storage hooks at either send and in callbacks", () => {
  for (const invalidateAt of ["collection-config", "attendance", "callback", "attendance-callback", "missing-id", "between-sends"]) {
    const h = content({invalidateAt});
    assert.equal(h.disconnected, true); assert.equal(h.removed, true); assert.equal(h.timers.size, 0);
    const count = h.sent.length, reads = h.reads;
    h.mutation(); h.change({auto_collect_enabled: {newValue: true}}); h.advance(900_000);
    assert.equal(h.sent.length, count); assert.equal(h.reads, reads); assert.equal(h.timers.size, 0);
  }
});

test("throwing runtime or id getters after reload stop safely on DOM and settings callbacks", () => {
  for (const property of ["runtime", "id"]) {
    for (const trigger of ["mutation", "settings", "timer"]) {
      const h = content(); h.breakProperty(property);
      assert.doesNotThrow(() => trigger === "mutation" ? h.mutation() : trigger === "settings" ? h.change({auto_collect_enabled: {newValue: true}}) : h.advance(900_000));
      assert.equal(h.disconnected, true); assert.equal(h.timers.size, 0);
      const reads = h.reads; h.mutation(); h.advance(900_000); assert.equal(h.reads, reads);
    }
  }
});
test("deferred message callbacks handle throwing runtime lastError access after reload", () => {
  for (const property of ["runtime", "id", "lastError"]) {
    const h = content({asyncCallbacks: true}); h.breakProperty(property);
    assert.doesNotThrow(() => h.flush());
    assert.equal(h.disconnected, true); assert.equal(h.timers.size, 0); assert.equal(h.reads, 0);
  }
});

test("deferred attendance acknowledgement catches invalidation after successful DOM read", () => {
  for (const property of ["runtime", "id", "lastError"]) {
    const h = content({asyncCallbacks: true}); h.flushOne();
    assert.equal(h.reads, 1); assert.equal(h.sent.at(-1).kind, "attendance");
    h.breakProperty(property); assert.doesNotThrow(() => h.flush());
    assert.equal(h.disconnected, true); assert.equal(h.timers.size, 0);
    h.mutation(); h.advance(900_000); assert.equal(h.reads, 1);
  }
});
