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

test("successful clock-in metadata survives worker restart and is not advertised after native failure", async () => {
  const local = {}; const original = background({local}); await original.send();
  const restarted = background({local});
  const status = await restarted.send({kind: "status"}, {id: restarted.sender.id});
  assert.equal(status.clock_in, "09:50:00"); assert.equal(status.working_day, "2026-11-03");
  const failed = background({nativeError: {message: "offline"}}); await failed.send();
  assert.equal(failed.local.status.clock_in, undefined); assert.equal(failed.local.status.working_day, undefined);
});
test("updating a legacy successful status refreshes clock metadata once without polling on worker wake", async () => {
  const local = {last_success_day: "2026-11-03", last_attempt_at: Date.parse("2026-11-03T01:59:59Z"), status: {ok: true, message: "오늘 출근 기록을 저장했습니다."}};
  const b = background({local}); await b.drain(); assert.equal(b.created.length, 0);
  b.chrome.runtime.onInstalled.emit(); await b.drain(); assert.equal(b.created.length, 1);
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
  b.chrome.runtime.onInstalled.emit(); await b.drain(); assert.equal(b.created.length, 1);
  const restarted = background({local}); await restarted.drain(); assert.equal(restarted.created.length, 0);
});

test("popup manual refresh bypasses daily success cooldown and login pause but reuses pending tab", async () => {
  const b = background({local: {last_success_day: "2026-11-03", last_attempt_at: Date.parse("2026-11-03T02:00:00Z"), login_required: true}});
  const ui = {id: b.sender.id};
  const reply = await b.send({kind: "refresh"}, ui);
  assert.equal(reply.accepted, true); assert.equal(reply.status.refreshing, true); assert.equal(b.created.length, 1); assert.equal(b.created[0].active, false);
  await b.send({kind: "refresh"}, ui); assert.equal(b.created.length, 1);
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
  const status = await b.send({kind: "status"}, ui);
  assert.equal(status.refreshing, false); assert.equal(status.clock_in, "09:50:00"); assert.deepEqual(b.removed, [100]);
});
test("content and foreign senders cannot request popup controls", async () => {
  const b = background();
  for (const kind of ["refresh", "set-preferences"]) {
    b.chrome.runtime.onMessage.emit({kind}, b.sender, () => assert.fail("webpage request accepted"));
    b.chrome.runtime.onMessage.emit({kind}, {id: "foreign"}, () => assert.fail("foreign request accepted"));
  }
  await b.drain(); assert.equal(b.created.length, 0);
});
test("last confirmed clock survives transient failure and valid no-clock correction clears it", async () => {
  const b = background(); await b.send();
  await b.send(message({available: false, clock_in: null, working_day: null}));
  let status = await b.send({kind: "status"}, {id: b.sender.id});
  assert.equal(status.ok, false); assert.equal(status.clock_in, "09:50:00");
  await b.send(message({clock_in: null}));
  status = await b.send({kind: "status"}, {id: b.sender.id});
  assert.equal(status.clock_in, undefined); assert.equal(b.local.last_attendance, undefined);
});
test("auto OFF persists across worker restart blocks background and content but permits manual tab", async () => {
  const local = {}; const b = background({local});
  const prefs = await b.send({kind: "set-preferences", auto_collect_enabled: false, polling_interval_minutes: 3}, {id: b.sender.id});
  assert.equal(prefs.accepted, true); assert.equal(b.alarms.has(DAILY), false);
  const next = background({local}); next.chrome.runtime.onStartup.emit(); next.chrome.alarms.onAlarm.emit({name: DAILY}); await next.drain();
  assert.equal(next.created.length, 0);
  assert.equal((await next.send({kind: "collection-config"})).enabled, false);
  assert.equal((await next.send()).paused, true); assert.equal(next.calls.length, 0);
  await next.send({kind: "refresh"}, {id: next.sender.id});
  const owned = {...next.sender, tab: {id: 100, url: reader.SOURCE}};
  assert.equal((await next.send({kind: "collection-config"}, owned)).enabled, true);
  assert.equal((await next.send(message(), owned)).ok, true); assert.equal(next.local.last_attendance.clock_in, "09:50:00");
});
test("settings reschedule alarm cooldown and disable only an owned automatic tab", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  await b.send({kind: "set-preferences", auto_collect_enabled: false, polling_interval_minutes: 2}, {id: b.sender.id});
  assert.deepEqual(b.removed, [100]); assert.equal(b.alarms.has(DAILY), false);
  await b.send({kind: "refresh"}, {id: b.sender.id});
  await b.send({kind: "set-preferences", auto_collect_enabled: false, polling_interval_minutes: 2}, {id: b.sender.id});
  assert.deepEqual(b.removed, [100]); assert.equal(b.session.owned_tab.manual, true);
  b.advance(60_000); b.chrome.alarms.onAlarm.emit({name: CLOSE}); await b.drain();
  assert.match(b.local.status.message, /새로고침/); assert.doesNotMatch(b.local.status.message, /분 후/);
  await b.send({kind: "set-preferences", auto_collect_enabled: true, polling_interval_minutes: 2}, {id: b.sender.id});
  assert.equal(b.alarms.get(DAILY).periodInMinutes, 2);
  b.advance(60_000); b.chrome.alarms.onAlarm.emit({name: DAILY}); await b.drain(); assert.equal(b.created.length, 3);
});
test("invalid interval settings do not modify stored preferences and corrupt stored values use defaults", async () => {
  const local = {auto_collect_enabled: false, polling_interval_minutes: 4}; const b = background({local});
  for (const value of [0, 1441, 1.5, "3", null]) {
    assert.equal((await b.send({kind: "set-preferences", auto_collect_enabled: true, polling_interval_minutes: value}, {id: b.sender.id})).accepted, false);
    assert.equal(local.auto_collect_enabled, false); assert.equal(local.polling_interval_minutes, 4);
  }
  const corrupt = background({local: {auto_collect_enabled: "false", polling_interval_minutes: -1}});
  const status = await corrupt.send({kind: "status"}, {id: corrupt.sender.id});
  assert.equal(status.auto_collect_enabled, true); assert.equal(status.polling_interval_minutes, 15);
});
test("manual native failure ends owned refresh while keeping confirmed attendance", async () => {
  const local = {last_attendance: {clock_in: "09:50:00", working_day: "2026-11-03"}};
  const b = background({local, nativeError: {message: "offline"}});
  await b.send({kind: "refresh"}, {id: b.sender.id});
  await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
  const status = await b.send({kind: "status"}, {id: b.sender.id});
  assert.equal(status.refreshing, false); assert.equal(status.clock_in, "09:50:00"); assert.match(status.message, /offline/);
});

test("legacy clock is migrated before refresh failure or disabling automatic collection", async () => {
  for (const kind of ["refresh", "set-preferences"]) {
    const local = {status: {ok: true, clock_in: "09:50:00", working_day: "2026-11-03"}};
    const b = background({local, nativeError: {message: "offline"}}); const ui = {id: b.sender.id};
    await b.send({kind, auto_collect_enabled: false, polling_interval_minutes: 15}, ui);
    assert.equal((await b.send({kind: "status"}, ui)).clock_in, "09:50:00");
    if (kind === "refresh") await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}});
    assert.equal((await b.send({kind: "status"}, ui)).clock_in, "09:50:00");
  }
});
test("manual refresh promotes pending automatic tab and survives switching automatic collection OFF", async () => {
  const b = background(); b.chrome.runtime.onStartup.emit(); await b.drain();
  const opened = b.session.owned_tab.opened_at; const ui = {id: b.sender.id};
  await b.send({kind: "refresh"}, ui);
  await b.send({kind: "set-preferences", auto_collect_enabled: false, polling_interval_minutes: 15}, ui);
  assert.equal(b.created.length, 1); assert.equal(b.removed.length, 0); assert.equal(b.session.owned_tab.manual, true); assert.equal(b.session.owned_tab.opened_at, opened);
  assert.equal((await b.send(message(), {...b.sender, tab: {id: 100, url: reader.SOURCE}})).ok, true);
});

test("valid empty correction does not resurrect legacy successful status during migration", async () => {
  const local = {status: {ok: true, clock_in: "09:50:00", working_day: "2026-11-03"}};
  const b = background({local}); await b.send(message({clock_in: null}));
  assert.equal((await b.send({kind: "status"}, {id: b.sender.id})).clock_in, undefined);
  assert.equal(local.last_attendance, undefined);
});
