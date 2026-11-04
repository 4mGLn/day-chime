"use strict";
importScripts("site.js", "reader.js");
const HOST = "com.day_chime.attendance";
const DAILY_ALARM = "day-chime-daily-collection";
const CLOSE_ALARM = "day-chime-collection-timeout";
const DEFAULT_INTERVAL_MINUTES = 15;
function validAttendance(value) {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value.working_day ?? "") || !/^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(value.clock_in ?? "")) return false;
  const date = new Date(`${value.working_day}T00:00:00Z`);
  return !Number.isNaN(date.getTime()) && date.toISOString().slice(0, 10) === value.working_day;
}
function preferences(saved) {
  return {auto_collect_enabled: typeof saved.auto_collect_enabled === "boolean" ? saved.auto_collect_enabled : true, polling_interval_minutes: Number.isInteger(saved.polling_interval_minutes) && saved.polling_interval_minutes >= 1 && saved.polling_interval_minutes <= 1440 ? saved.polling_interval_minutes : DEFAULT_INTERVAL_MINUTES};
}
async function getPreferences() { return preferences(await chrome.storage.local.get(["auto_collect_enabled", "polling_interval_minutes"])); }
let status = {ok: false, message: "Brave가 실행되면 오늘 출근 기록을 자동 확인합니다.", updated_at: null};
let queue = Promise.resolve();
function serial(task) {
  const work = queue.then(task);
  queue = work.catch(() => {});
  return work;
}
function today() {
  const parts = new Intl.DateTimeFormat("en-US", {timeZone: "Asia/Seoul", year: "numeric", month: "2-digit", day: "2-digit"}).formatToParts(new Date());
  const part = (name) => parts.find((p) => p.type === name).value;
  return `${part("year")}-${part("month")}-${part("day")}`;
}
async function update(ok, message, attendance = null, preserveLegacy = true) {
  const saved = await chrome.storage.local.get(["status", "last_attendance"]);
  if (preserveLegacy && !validAttendance(saved.last_attendance) && saved.status?.ok && validAttendance(saved.status)) {
    await chrome.storage.local.set({last_attendance: {clock_in: saved.status.clock_in, working_day: saved.status.working_day}});
  }
  status = {ok, message, updated_at: new Date().toISOString()};
  if (ok && attendance) {
    status.clock_in = attendance.clock_in;
    status.working_day = attendance.working_day;
  }
  chrome.action.setBadgeText({text: ok ? "OK" : "!"});
  chrome.action.setBadgeBackgroundColor({color: ok ? "#197a43" : "#b23925"});
  chrome.action.setTitle({title: `Day Chime: ${message}`});
  await chrome.storage.local.set(ok && validAttendance(attendance) ? {status, last_attendance: {clock_in: attendance.clock_in, working_day: attendance.working_day}} : {status});
}
async function report(error) {
  try { await update(false, "자동 수집 연결을 확인하세요. 로그인 또는 확장 새로고침이 필요할 수 있습니다."); }
  catch { console.warn("Day Chime: 상태 저장에 실패했습니다."); }
  console.warn("Day Chime: 자동 수집 작업 실패", error?.message ?? "unknown");
}
function launch(task) { void serial(task).catch(report); }
async function releaseOwned() {
  await chrome.storage.session.remove("owned_tab");
  await chrome.alarms.clear(CLOSE_ALARM);
}
async function closeOwned(expectedId) {
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (!owned || (expectedId !== undefined && expectedId !== owned.id)) return false;
  // Relinquish before closing: a restarted worker must not act on a reused ID.
  await releaseOwned();
  let tab;
  try { tab = await chrome.tabs.get(owned.id); }
  catch { return false; }
  if (tab.active) return false;
  await chrome.tabs.remove(owned.id);
  return true;
}
async function ensureAlarm() {
  const prefs = await getPreferences();
  const alarm = await chrome.alarms.get(DAILY_ALARM);
  if (!prefs.auto_collect_enabled) await chrome.alarms.clear(DAILY_ALARM);
  else if (!alarm || alarm.periodInMinutes !== prefs.polling_interval_minutes) {
    await chrome.alarms.create(DAILY_ALARM, {periodInMinutes: prefs.polling_interval_minutes, delayInMinutes: prefs.polling_interval_minutes});
  }
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (owned && !(await chrome.alarms.get(CLOSE_ALARM))) {
    await chrome.alarms.create(CLOSE_ALARM, {when: owned.opened_at + 60_000});
  }
}
async function timeoutOwned() {
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (!owned) return;
  // An old timeout can arrive after a delayed daily alarm already opened a new tab.
  if (Date.now() < owned.opened_at + 60_000) {
    await chrome.alarms.create(CLOSE_ALARM, {when: owned.opened_at + 60_000});
    return;
  }
  const closed = await closeOwned(owned.id);
  const saved = await chrome.storage.local.get("last_success_day");
  if (closed && (owned.manual || saved.last_success_day !== today())) {
    const prefs = await getPreferences();
    await update(false, prefs.auto_collect_enabled ? `출근 기록을 확인하지 못했습니다. 로그인 상태를 확인해 주세요. ${prefs.polling_interval_minutes}분 후 다시 시도합니다.` : "출근 기록을 확인하지 못했습니다. 로그인 상태를 확인한 뒤 새로고침을 다시 눌러 주세요.");
  }
}
async function collect({force = false} = {}) {
  const prefs = await getPreferences();
  if (!force && !prefs.auto_collect_enabled) return;
  const saved = await chrome.storage.local.get(["last_success_day", "last_attempt_at", "login_required"]);
  if (!force && (saved.last_success_day === today() || saved.login_required)) return;
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (owned) {
    try {
      const tab = await chrome.tabs.get(owned.id);
      if (tab.active) await releaseOwned();
      else if (Date.now() - owned.opened_at >= 60_000) await closeOwned();
      else {
        if (force && !owned.manual) await chrome.storage.session.set({owned_tab: {...owned, manual: true}});
        return;
      }
    } catch { await releaseOwned(); }
  }
  if (!force && Number.isFinite(saved.last_attempt_at) && Date.now() >= saved.last_attempt_at && Date.now() - saved.last_attempt_at < prefs.polling_interval_minutes * 60_000) return;
  // Save the attempt first so failed/restarted workers cannot create a tab flood.
  await chrome.storage.local.set({last_attempt_at: Date.now()});
  const tab = await chrome.tabs.create({url: DayChimeReader.SOURCE, active: false});
  try {
    await chrome.storage.session.set({owned_tab: {id: tab.id, opened_at: Date.now(), manual: force}});
    await chrome.alarms.create(CLOSE_ALARM, {delayInMinutes: 1});
    await update(false, "백그라운드에서 오늘 출근 기록을 확인하고 있습니다.");
  } catch (error) {
    // If tracking failed, this newly-created inactive tab is still ours.
    const current = await chrome.tabs.get(tab.id).catch(() => null);
    if (current && !current.active) await chrome.tabs.remove(tab.id);
    throw error;
  }
}
function sendNative(snapshot) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendNativeMessage(HOST, snapshot, (response) => {
      const error = chrome.runtime.lastError;
      if (error) reject(Error(`로컬 연결 실패: ${error.message}`));
      else if (response?.ok !== true) reject(Error("로컬 프로그램이 데이터를 거절했습니다."));
      else resolve();
    });
  });
}
async function attendance(snapshot, tabId) {
  await sendNative(snapshot);
  const hasClockIn = snapshot.available && snapshot.working_day === today()
    && typeof snapshot.clock_in === "string" && /^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(snapshot.clock_in);
  if (hasClockIn) {
    // Persist success before closing. Only native acknowledgement counts as collection.
    await chrome.storage.local.set({last_success_day: snapshot.working_day, login_required: false});
    await update(true, "오늘 출근 기록을 저장했습니다. Brave를 닫아도 퇴근 알림이 유지됩니다.", snapshot);
    await closeOwned(tabId);
  } else {
    if (snapshot.available && snapshot.working_day === today() && snapshot.clock_in === null) {
      await chrome.storage.local.remove(["last_success_day", "last_attendance"]);
    }
    await update(false, snapshot.available ? "아직 오늘 출근 기록이 없습니다. 설정한 조회 간격으로 다시 확인합니다." : "근태 위젯을 읽지 못했습니다. 이미 저장한 출근 기록은 유지됩니다.", null, !(snapshot.available && snapshot.working_day === today() && snapshot.clock_in === null));
  }
}
async function statusView() {
  const saved = await chrome.storage.local.get(["status", "last_attendance"]);
  const result = {...(saved.status ?? status), ...await getPreferences()};
  const record = validAttendance(saved.last_attendance) ? saved.last_attendance : saved.status?.ok && validAttendance(saved.status) ? saved.status : null;
  delete result.clock_in; delete result.working_day;
  if (record) Object.assign(result, {clock_in: record.clock_in, working_day: record.working_day});
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  result.refreshing = Boolean(owned && Date.now() < owned.opened_at + 60_000);
  return result;
}
async function contentConfig(sender) {
  const prefs = await getPreferences();
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  return {enabled: prefs.auto_collect_enabled || Boolean(owned?.manual && owned.id === sender.tab.id && Date.now() < owned.opened_at + 60_000), interval_minutes: prefs.polling_interval_minutes};
}
async function setPreferences(message) {
  if (typeof message.auto_collect_enabled !== "boolean" || !Number.isInteger(message.polling_interval_minutes) || message.polling_interval_minutes < 1 || message.polling_interval_minutes > 1440) throw Error("조회 간격은 1~1440분 사이의 정수로 입력해 주세요.");
  await chrome.storage.local.set({auto_collect_enabled: message.auto_collect_enabled, polling_interval_minutes: message.polling_interval_minutes});
  if (!message.auto_collect_enabled) {
    const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
    if (owned && !owned.manual) await closeOwned(owned.id);
    await update(false, "자동 수집을 껐습니다. 저장한 출근 기록의 퇴근 알림은 유지됩니다.");
  }
  await ensureAlarm();
  if (message.auto_collect_enabled) await collect();
  return {accepted: true, status: await statusView()};
}
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id) return false;
  if (!sender.tab && ["status", "refresh", "set-preferences"].includes(message?.kind)) {
    serial(async () => {
      if (message.kind === "status") return statusView();
      if (message.kind === "set-preferences") return setPreferences(message);
      await chrome.storage.local.set({login_required: false});
      await collect({force: true});
      return {accepted: true, status: await statusView()};
    }).then(respond, (error) => respond({accepted: false, message: error.message}));
    return true;
  }
  if (sender.frameId !== 0 || !DayChimeReader.isHome(sender.tab?.url)) return false;
  if (message?.kind === "collection-config") {
    serial(() => contentConfig(sender)).then(respond, () => respond({enabled: false}));
    return true;
  }
  if (message?.kind !== "attendance") return false;
  const snapshot = message.snapshot;
  if (!snapshot || snapshot.schema_version !== 1 || snapshot.source_url !== DayChimeReader.SOURCE || typeof snapshot.available !== "boolean") {
    launch(() => update(false, "근태 데이터 형식이 올바르지 않습니다."));
    respond({ok: false});
    return false;
  }
  serial(async () => {
    if (!(await contentConfig(sender)).enabled) return {ok: false, paused: true};
    await attendance(snapshot, sender.tab.id);
    return {ok: true};
  }).then(respond, async (error) => {
    try {
      const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
      if (owned?.manual) await closeOwned(sender.tab.id);
      await update(false, error.message);
    } catch { /* Report request failure even if diagnostic storage failed. */ }
    respond({ok: false});
  });
  return true;
});
chrome.runtime.onStartup.addListener(() => launch(async () => {
  await releaseOwned();
  await chrome.storage.local.set({login_required: false});
  await ensureAlarm();
  await collect();
}));
chrome.runtime.onInstalled.addListener(() => launch(async () => {
  const saved = await chrome.storage.local.get(["last_success_day", "status"]);
  // Older versions saved the success message without the time needed by the popup.
  // Refresh once on installation/update; worker hydration and startup keep their normal gate.
  const hasClock = saved.status?.ok && saved.status.working_day === today()
    && typeof saved.status.clock_in === "string" && /^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(saved.status.clock_in);
  if (saved.last_success_day === today() && !hasClock) {
    await chrome.storage.local.remove(["last_success_day", "last_attempt_at"]);
    await chrome.storage.local.set({login_required: false});
  }
  await ensureAlarm();
  await collect();
}));
chrome.alarms.onAlarm.addListener((alarm) => launch(async () => {
  if (alarm.name === DAILY_ALARM) await collect();
  if (alarm.name === CLOSE_ALARM) await timeoutOwned();
}));
chrome.tabs.onActivated.addListener(({tabId}) => launch(async () => {
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (owned?.id === tabId) await releaseOwned();
}));
chrome.tabs.onRemoved.addListener((tabId) => launch(async () => {
  const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
  if (owned?.id === tabId) await releaseOwned();
}));
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (!change.url || DayChimeReader.isHome(change.url)) return;
  launch(async () => {
    const {owned_tab: owned} = await chrome.storage.session.get("owned_tab");
    if (owned?.id !== tabId) return;
    await releaseOwned();
    await chrome.storage.local.set({login_required: true});
    await update(false, "로그인 또는 화면 확인이 필요합니다. 다우오피스 홈을 열어 주세요.");
  });
});
// Worker rehydration restores scheduling, but does not repeatedly open tabs.
launch(ensureAlarm);
