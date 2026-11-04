"use strict";
const refresh = document.querySelector("#refresh");
const enabled = document.querySelector("#auto-collect");
const interval = document.querySelector("#polling-interval");
const save = document.querySelector("#save-settings");
const feedback = document.querySelector("#settings-feedback");
document.querySelector("#version").textContent = `v${chrome.runtime.getManifest().version}`;
let watchdog, timer, pollingSince = null, savedInterval = 15, savedEnabled = true;
function clockIn(status) {
  if (typeof status?.clock_in !== "string" || !/^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(status.clock_in) || !/^\d{4}-\d{2}-\d{2}$/.test(status.working_day ?? "")) return null;
  const day = new Date(`${status.working_day}T00:00:00Z`);
  if (Number.isNaN(day.getTime()) || day.toISOString().slice(0, 10) !== status.working_day) return null;
  const parts = new Intl.DateTimeFormat("en-US", {timeZone: "Asia/Seoul", year: "numeric", month: "2-digit", day: "2-digit"}).formatToParts(new Date());
  const part = (name) => parts.find((entry) => entry.type === name).value;
  const today = `${part("year")}-${part("month")}-${part("day")}`;
  return {time: status.clock_in.slice(0, 5), date: status.working_day === today ? "오늘" : status.working_day};
}
function busy(value) {
  refresh.disabled = value;
  refresh.setAttribute("aria-busy", String(value));
}
function startBusy() {
  busy(true);
  if (pollingSince === null) pollingSince = Date.now();
  if (watchdog === undefined) watchdog = setTimeout(() => failed("조회가 지연되고 있어요. 로그인 상태를 확인한 뒤 다시 눌러 주세요."), Math.max(0, 70_000 - (Date.now() - pollingSince)));
}
function stop() { clearTimeout(timer); clearTimeout(watchdog); watchdog = undefined; pollingSince = null; busy(false); }
function failed(message) { document.querySelector("#status").textContent = message; stop(); }
function render(status) {
  document.querySelector("#status").textContent = status?.message ?? "상태를 확인할 수 없습니다.";
  const clock = clockIn(status);
  document.querySelector("#attendance").textContent = clock?.time ?? "--:--";
  document.querySelector("#attendance-date").textContent = clock ? `${clock.date} · ${status.ok && !status.refreshing ? "수집한 출근 기록" : "마지막으로 확인한 출근 기록"}` : "기록이 아직 없어요. 오른쪽 위 새로고침을 눌러 주세요.";
  document.querySelector("#time").textContent = status?.updated_at && !Number.isNaN(new Date(status.updated_at).getTime()) ? `최근 확인: ${new Date(status.updated_at).toLocaleString()}` : "";
  if (typeof status?.auto_collect_enabled === "boolean") { savedEnabled = status.auto_collect_enabled; enabled.checked = savedEnabled; }
  if (Number.isInteger(status?.polling_interval_minutes)) { savedInterval = status.polling_interval_minutes; if (document.activeElement !== interval) interval.value = String(savedInterval); }
  if (status?.refreshing) {
    if (pollingSince === null) pollingSince = Date.now();
    if (Date.now() - pollingSince >= 70_000) { failed("조회가 지연되고 있어요. 로그인 상태를 확인한 뒤 다시 눌러 주세요."); return; }
    startBusy(); clearTimeout(timer); timer = setTimeout(load, 1000);
  } else stop();
}
function load() {
  chrome.runtime.sendMessage({kind: "status"}, (status) => {
    if (chrome.runtime.lastError || !status || typeof status !== "object") { failed("확장 프로그램을 다시 로드해 주세요."); return; }
    render(status);
  });
}
refresh.addEventListener("click", () => {
  if (refresh.disabled) return;
  clearTimeout(timer); pollingSince = Date.now(); startBusy();
  document.querySelector("#status").textContent = "출근 기록을 새로 확인하고 있어요.";
  chrome.runtime.sendMessage({kind: "refresh"}, (response) => {
    if (chrome.runtime.lastError || !response?.accepted) { failed(response?.message ?? "새로고침을 시작하지 못했어요. 다시 시도해 주세요."); return; }
    render(response.status);
  });
});
function savePreferences(toggleOnly = false) {
  const minutes = toggleOnly ? savedInterval : Number(interval.value);
  if ((!toggleOnly && !/^\d+$/.test(interval.value)) || !Number.isInteger(minutes) || minutes < 1 || minutes > 1440) { feedback.textContent = "조회 간격은 1~1440분 사이의 정수로 입력해 주세요."; return; }
  save.disabled = true; enabled.disabled = true;
  chrome.runtime.sendMessage({kind: "set-preferences", auto_collect_enabled: enabled.checked, polling_interval_minutes: minutes}, (response) => {
    save.disabled = false; enabled.disabled = false;
    if (chrome.runtime.lastError || !response?.accepted) { enabled.checked = savedEnabled; feedback.textContent = response?.message ?? "설정을 저장하지 못했어요."; return; }
    feedback.textContent = "설정을 저장했습니다."; render(response.status);
  });
}
enabled.addEventListener("change", () => savePreferences(true));
save.addEventListener("click", () => savePreferences(false));
load();
