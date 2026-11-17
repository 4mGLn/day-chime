(() => {
  "use strict";
  if (!DayChimeReader.isHome(location.href)) return;
  let settleUntil = Date.now() + 60_000;
  let timer, nextReadAt = 0, checking = false, stopped = false, enabled = false;
  let observer, observed = null, meaningfulClock = false, delivered = null;
  function stop() {
    if (stopped) return;
    stopped = true;
    clearTimeout(timer);
    observer?.disconnect();
    try {
      chrome.storage.onChanged.removeListener?.(settingsChanged);
      chrome.runtime.onMessage?.removeListener(wakeListener);
    } catch { /* Context already detached. */ }
    document.removeEventListener?.("visibilitychange", visible);
    if (typeof window !== "undefined") window.removeEventListener("pageshow", resume);
  }
  function invalid(error) {
    if (stopped) return true;
    try {
      if (!/extension context invalidated/i.test(error?.message ?? "") && chrome.runtime?.id) return false;
    } catch { /* A detached API object can throw during a validity check. */ }
    stop();
    return true;
  }
  function guarded(callback) {
    return (...args) => {
      if (stopped) return;
      try { callback(...args); }
      catch (error) { if (!invalid(error)) throw error; }
    };
  }
  function schedule(delay) {
    if (stopped) return;
    clearTimeout(timer);
    timer = setTimeout(check, Math.max(0, delay));
  }
  function finish(delay) {
    checking = false;
    schedule(Math.min(delay, Math.max(0, nextReadAt - Date.now())));
  }
  function readiness() {
    const widgets = document.querySelectorAll(".gadget_attend");
    const widget = widgets.length === 1 ? widgets[0] : null;
    if (!widget) return {widget: null, shape: ""};
    const dates = widget.querySelectorAll(".today_time .date");
    const rows = widget.querySelectorAll(".work_type");
    // Until a real clock-in is seen, watch the meaningful readiness transition,
    // not the page's ticking date/time text. This also handles delayed XHR fills.
    let ready = "";
    if (!meaningfulClock || widget !== observed?.widget) {
      const date = dates.length === 1 ? dates[0].textContent.match(/^\s*(\d{4}년\s+\d{1,2}월\s+\d{1,2}일)/)?.[1] ?? "" : "";
      const clock = [...rows].some((row) => {
        const titles = row.querySelectorAll(".tit"), values = row.querySelectorAll(".data");
        return titles.length === 1 && titles[0].textContent.trim() === "출근 시간" && values.length === 1 && /^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(values[0].textContent.trim());
      });
      ready = `${date}:${clock}`;
    }
    return {widget, shape: `${dates.length}:${rows.length}`, ready};
  }
  function changed() {
    if (stopped || invalid()) return;
    if (enabled && DayChimeReader.isHome(location.href)) {
      const current = readiness();
      if (current.widget && (current.widget !== observed?.widget || current.shape !== observed?.shape
        || (!meaningfulClock && current.ready !== observed?.ready))) {
        meaningfulClock = false;
        settleUntil = Date.now() + 60_000;
        nextReadAt = 0;
      }
      observed = current;
    }
    check();
  }
  function check() {
    if (stopped || invalid() || checking || Date.now() < nextReadAt || !DayChimeReader.isHome(location.href)) return;
    checking = true;
    try {
      chrome.runtime.sendMessage({kind: "collection-config"}, guarded((config) => {
        const error = chrome.runtime.lastError;
        if (invalid(error)) return;
        const interval = Number.isInteger(config?.interval_minutes) && config.interval_minutes >= 1 && config.interval_minutes <= 1440 ? config.interval_minutes * 60_000 : 15 * 60_000;
        enabled = !error && Boolean(config?.enabled);
        if (!enabled) {
          nextReadAt = Date.now() + interval;
          finish(interval);
          return;
        }
        observed = readiness();
        const snapshot = DayChimeReader.read(document, location.href);
        meaningfulClock = snapshot.available && typeof snapshot.clock_in === "string";
        const delay = !meaningfulClock && Date.now() < settleUntil ? 2000 : interval;
        nextReadAt = Date.now() + delay;
        const fingerprint = JSON.stringify([snapshot.available, snapshot.working_day, snapshot.clock_in, snapshot.clock_out, snapshot.error]);
        if (delay === 2000 && fingerprint === delivered) { finish(delay); return; }
        try {
          if (invalid()) return;
          chrome.runtime.sendMessage({kind: "attendance", snapshot}, guarded((response) => {
            const error = chrome.runtime.lastError;
            if (invalid(error)) return;
            if (!error && response?.ok === true) delivered = fingerprint;
            else if (error) console.warn("Day Chime: 확장 프로그램 연결을 확인하세요.");
            finish(delay);
          }));
        } catch (error) {
          if (invalid(error)) return;
          finish(delay);
        }
      }));
    } catch (error) {
      if (invalid(error)) return;
      nextReadAt = Date.now() + 15 * 60_000;
      finish(15 * 60_000);
    }
  }
  function resume() {
    if (stopped || invalid()) return;
    settleUntil = Date.now() + 60_000;
    nextReadAt = 0;
    check();
  }
  function visible() { if (document.visibilityState === "visible") resume(); }
  function wake(message, sender) {
    if (stopped || invalid()) return;
    if (message?.kind === "refresh-attendance" && sender.id === chrome.runtime.id && !sender.tab) resume();
  }
  function settingsChanged(changes, area) {
    if (stopped || invalid()) return;
    if (area === "local" && (changes.auto_collect_enabled || changes.polling_interval_minutes)) {
      if (changes.auto_collect_enabled?.newValue === false) enabled = false;
      nextReadAt = 0;
      schedule(0);
    }
  }
  const wakeListener = guarded(wake);
  observer = new MutationObserver(changed);
  observer.observe(document.documentElement, {childList: true, subtree: true, characterData: true});
  document.addEventListener?.("visibilitychange", visible);
  if (typeof window !== "undefined") window.addEventListener("pageshow", resume);
  try {
    chrome.storage.onChanged.addListener(settingsChanged);
    chrome.runtime.onMessage?.addListener(wakeListener);
  } catch (error) { if (!invalid(error)) throw error; }
  check();
})();
