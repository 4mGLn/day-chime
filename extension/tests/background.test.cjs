const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const reader = require("../reader.js");
const SCRIPT = fs.readFileSync(require.resolve("../background.js"), "utf8");
const DAILY = "day-chime-daily-collection";
const CLOSE = "day-chime-collection-timeout";
function event() { const listeners = []; return {addListener: (f) => listeners.push(f), emit: (...args) => listeners.forEach((f) => f(...args))}; }
function store(data, {failSet = false} = {}) {
  return {
    async get(keys) { return Object.fromEntries((typeof keys === "string" ? [keys] : keys).filter((key) => key in data).map((key) => [key, structuredClone(data[key])])); },
    async set(values) { if (failSet) throw Error("storage failure"); Object.assign(data, structuredClone(values)); },
    async remove(keys) { for (const key of Array.isArray(keys) ? keys : [keys]) delete data[key]; },
  };
}
function background({nativeError = null, response = {ok: true}, local = {}, session = {}, failLocalSet = false, now = "2026-11-03T02:00:00Z", tabs = new Map(), alarms = new Map()} = {}) {
  let clock = new Date(now).getTime();
  class Clock extends Date { constructor(...args) { super(...(args.length ? args : [clock])); } static now() { return clock; } }
  const calls = [], badges = [], created = [], removed = [], warnings = [];
  let nextId = 100;
  const chrome = {
    runtime: {
      id: "a".repeat(32), lastError: null,
      onMessage: event(), onStartup: event(), onInstalled: event(),
      sendNativeMessage: (host, snapshot, callback) => {
        calls.push({host, snapshot}); chrome.runtime.lastError = nativeError;
        callback(response); chrome.runtime.lastError = null;
      },
    },
    storage: {local: store(local, {failSet: failLocalSet}), session: store(session)},
    action: {setBadgeText: (value) => badges.push(value.text), setBadgeBackgroundColor: () => {}, setTitle: () => {}},
    alarms: {onAlarm: event(), async get(name) { return alarms.get(name); }, async create(name, info) { alarms.set(name, structuredClone(info)); }, async clear(name) { return alarms.delete(name); }},
    tabs: {
      onActivated: event(), onRemoved: event(), onUpdated: event(),
      async create(info) { const id = nextId++; created.push({id, ...structuredClone(info)}); tabs.set(id, {id, active: info.active}); return {id, active: info.active}; },
      async get(id) { if (!tabs.has(id)) throw Error("missing tab"); return structuredClone(tabs.get(id)); },
      async remove(id) { removed.push(id); tabs.delete(id); },
    },
  };
  vm.runInNewContext(SCRIPT, {chrome, importScripts: () => {}, DayChimeReader: reader, Intl, Date: Clock, console: {warn: (...args) => warnings.push(args)}});
  const drain = async () => { for (let n = 0; n < 8; n++) await new Promise(setImmediate); };
  const sender = {id: chrome.runtime.id, frameId: 0, tab: {id: 99, url: reader.SOURCE}};
  const send = (msg = message(), source = sender) => new Promise((resolve) => chrome.runtime.onMessage.emit(msg, source, resolve));
  return {chrome, calls, badges, created, removed, warnings, tabs, alarms, local, session, sender, send, drain, advance: (ms) => { clock += ms; }};
}
function message(overrides = {}) {
  return {kind: "attendance", snapshot: {schema_version: 1, source_url: reader.SOURCE, available: true, working_day: "2026-11-03", clock_in: "09:50:00", clock_out: null, observed_at: "2026-11-03T02:00:00Z", ...overrides}};
}
test("unrelated source tab, iframe and foreign extension cannot call native host", async () => {
  const b = background();
  for (const source of [{...b.sender, id: "other"}, {...b.sender, frameId: 1}, {...b.sender, tab: {url: "https://evil.example/home"}}]) b.chrome.runtime.onMessage.emit(message(), source, () => {});
  await b.drain(); assert.equal(b.calls.length, 0);
});
test("verified source relays canonical snapshot to named native host and persists success", async () => {
  const b = background(); const reply = await b.send();
  assert.equal(b.calls[0].host, "com.day_chime.attendance");
  assert.equal(b.calls[0].snapshot.source_url, reader.SOURCE);
  assert.equal(reply.ok, true); assert.equal(b.badges.at(-1), "OK"); assert.equal(b.local.last_success_day, "2026-11-03");
});
test("native failure and unavailable DOM remain visible without marking collection complete", async () => {
  const b = background({nativeError: {message: "Native host not found"}});
  assert.equal((await b.send()).ok, false); assert.equal(b.badges.at(-1), "!"); assert.equal(b.local.last_success_day, undefined);
  const status = await b.send({kind: "status"}, {id: b.sender.id}); assert.match(status.message, /Native host not found/);
  const unavailable = background(); await unavailable.send(message({available: false, clock_in: null, working_day: null}));
  assert.equal(unavailable.badges.at(-1), "!"); assert.match(unavailable.local.status.message, /이미 저장한 출근 기록은 유지/);
  assert.equal(unavailable.local.last_success_day, undefined);
});
test("worker hydration only repairs alarm; startup opens one inactive company home tab", async () => {
  const b = background(); await b.drain(); assert.equal(b.created.length, 0);
  assert.equal(b.alarms.get(DAILY).periodInMinutes, 15);
  b.chrome.runtime.onStartup.emit(); await b.drain();
  assert.equal(b.created.length, 1); assert.equal(b.created[0].url, reader.SOURCE); assert.equal(b.created[0].active, false);
  assert.equal(b.session.owned_tab.id, 100); assert.equal(b.alarms.get(CLOSE).delayInMinutes, 1);
});
test("overlapping startup installation and alarm events cannot create duplicate tabs", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); b.chrome.runtime.onInstalled.emit(); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain();
  assert.equal(b.created.length, 1);
});
test("only acknowledged current-day check-in completes collection and closes the owned inactive tab", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
  assert.equal(b.local.last_success_day, "2026-11-03"); assert.deepEqual(b.removed, [100]); assert.equal(b.session.owned_tab, undefined);
  b.advance(16 * 60_000); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 1);
});
test("manual home success never closes unrelated automatic or user-owned tabs", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  await b.send(); assert.deepEqual(b.removed, []); assert.equal(b.session.owned_tab.id, 100);
  const manual = background(); manual.tabs.set(99, {id: 99, active: false}); await manual.send(); assert.deepEqual(manual.removed, []);
});
test("missing or stale check-in never marks today successful and retries after timeout and cooldown", async () => {
  for (const input of [{clock_in: null}, {working_day: "2026-11-02"}, {clock_in: "25:00:00"}]) {
    const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain(); await b.send(message(input), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
    assert.equal(b.local.last_success_day, undefined); assert.deepEqual(b.removed, []);
    b.advance(60_000); b.chrome.alarms.onAlarm.emit({name: CLOSE}); await b.drain(); assert.deepEqual(b.removed, [100]);
    b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 1);
    b.advance(15 * 60_000); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 2);
  }
});
test("native rejection cannot mark the day or close owned tab", async () => {
  const b = background({response: {ok: false}}); b.chrome.runtime.onStartup.emit(); await b.drain();
  assert.equal((await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}})).ok, false);
  assert.equal(b.local.last_success_day, undefined); assert.deepEqual(b.removed, []);
});
test("next Seoul working day starts new collection while prior-day success stays persisted", async () => {
  const b = background({local: {last_success_day: "2026-11-03"}, now: "2026-11-03T14:59:00Z"});
  b.chrome.runtime.onStartup.emit(); await b.drain(); assert.equal(b.created.length, 0);
  b.advance(16 * 60_000); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 1);
});
test("user activation adopts tab so later success or timeout cannot close it", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  b.tabs.set(100, {id: 100, active: true}); b.chrome.tabs.onActivated.emit({tabId: 100}); await b.drain();
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}}); b.chrome.alarms.onAlarm.emit({name: CLOSE}); await b.drain();
  assert.deepEqual(b.removed, []); assert.equal(b.session.owned_tab, undefined);
});
test("active tab guard protects user tab even before activation event is processed", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain(); b.tabs.set(100, {id: 100, active: true});
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}}); assert.deepEqual(b.removed, []);
});
test("redirect or user navigation relinquishes ownership and asks for login without tab floods", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  b.chrome.tabs.onUpdated.emit(100, {url: "https://example.daouoffice.com/login"}); await b.drain();
  assert.equal(b.session.owned_tab, undefined); assert.equal(b.local.login_required, true);
  b.advance(16 * 60_000); b.chrome.alarms.onAlarm.emit({name: CLOSE}); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain();
  assert.deepEqual(b.removed, []); assert.equal(b.created.length, 1); assert.match(b.local.status.message, /로그인/);
  await b.send(); assert.equal(b.local.login_required, false);
});
test("worker restart retains success and owns only tracked session tab; browser startup clears stale ownership", async () => {
  const local = {last_success_day: "2026-11-03"}, session = {owned_tab: {id: 20, opened_at: Date.parse("2026-11-03T01:58:00Z")}};
  const tabs = new Map([[20, {id: 20, active: false}]]);
  const worker = background({local, session, tabs}); await worker.drain(); worker.chrome.alarms.onAlarm.emit({name: CLOSE}); await worker.drain();
  assert.deepEqual(worker.removed, [20]); assert.equal(worker.created.length, 0);
  const startup = background({session: {owned_tab: {id: 20, opened_at: 0}}, tabs: new Map([[20, {id: 20, active: false}]])});
  startup.chrome.runtime.onStartup.emit(); await startup.drain(); assert.deepEqual(startup.removed, []); assert.equal(startup.created.length, 1);
});
test("status survives worker restart and storage errors never report collection success", async () => {
  const local = {}; const original = background({local}); await original.send();
  const restarted = background({local}); assert.match((await restarted.send({kind: "status"}, {id: restarted.sender.id})).message, /오늘 출근 기록을 저장/);
  const broken = background({failLocalSet: true}); const reply = await broken.send();
  assert.equal(reply.ok, false); assert.equal(broken.local.last_success_day, undefined); assert.deepEqual(broken.removed, []);
});
test("an old timeout delivered after a new daily attempt cannot close the fresh owned tab", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  b.advance(16 * 60_000);
  b.chrome.alarms.onAlarm.emit({name: DAILY}); b.chrome.alarms.onAlarm.emit({name: CLOSE}); await b.drain();
  assert.equal(b.created.length, 2); assert.deepEqual(b.removed, [100]); assert.equal(b.session.owned_tab.id, 101);
  assert.equal(b.alarms.get(CLOSE).when, b.session.owned_tab.opened_at + 60_000);
});
test("timed-out collection reports login/retry guidance instead of indefinite loading", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain(); b.advance(60_000);
  b.chrome.alarms.onAlarm.emit({name: CLOSE}); await b.drain();
  assert.equal(b.session.owned_tab, undefined); assert.deepEqual(b.removed, [100]); assert.match(b.local.status.message, /ログイン|로그인/); assert.match(b.local.status.message, /15분/);
});
test("explicit no-check-in correction clears the daily success so later collection can retry", async () => {
  const b = background({local: {last_success_day: "2026-11-03"}});
  await b.send(message({clock_in: null})); assert.equal(b.local.last_success_day, undefined); assert.equal(b.badges.at(-1), "!");
  b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 1);
});
test("rehydrated worker repairs a missing cleanup alarm for its still-owned session tab", async () => {
  const session = {owned_tab: {id: 20, opened_at: Date.parse("2026-11-03T02:00:00Z")}};
  const b = background({session, tabs: new Map([[20, {id: 20, active: false}]])}); await b.drain();
  assert.equal(b.created.length, 0); assert.equal(b.alarms.get(CLOSE).when, session.owned_tab.opened_at + 60_000);
});
