(() => {
  "use strict";
  if (!DayChimeReader.isHome(location.href)) return;
  const started = Date.now();
  let timer, nextReadAt = 0, checking = false, stopped = false;
  let observer;
  function stop() {
    if (stopped) return;
    stopped = true;
    clearTimeout(timer);
    observer?.disconnect();
    try { chrome.storage.onChanged.removeListener?.(settingsChanged); } catch { /* Context already detached. */ }
  }
  function invalid(error) {
    if (stopped) return true;
    // Accessing runtime itself (or its id getter) can throw after extension reload.
    try {
      if (!/extension context invalidated/i.test(error?.message ?? "") && chrome.runtime?.id) return false;
    } catch { /* A detached API object can throw even during a validity check. */ }
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
  function check() {
    if (stopped || invalid() || checking || Date.now() < nextReadAt || !DayChimeReader.isHome(location.href)) return;
    checking = true;
    try {
      chrome.runtime.sendMessage({kind: "collection-config"}, guarded((config) => {
        if (stopped) return;
        const error = chrome.runtime.lastError;
        if (invalid(error)) return;
        checking = false;
        const interval = Number.isInteger(config?.interval_minutes) && config.interval_minutes >= 1 && config.interval_minutes <= 1440 ? config.interval_minutes * 60_000 : 15 * 60_000;
        if (error || !config?.enabled) {
          nextReadAt = Date.now() + interval;
          schedule(interval);
          return;
        }
        const snapshot = DayChimeReader.read(document, location.href);
        // The SPA widget often renders after document_idle. Settle it briefly before
        // switching to the configured cadence; even this path checks permission first.
        const delay = !snapshot.available && Date.now() - started < 60_000 ? 2000 : interval;
        nextReadAt = Date.now() + delay;
        try {
          if (invalid()) return;
          chrome.runtime.sendMessage({kind: "attendance", snapshot}, guarded(() => {
            if (stopped) return;
            const error = chrome.runtime.lastError;
            if (invalid(error)) return;
            if (error) console.warn("Day Chime: 확장 프로그램 연결을 확인하세요.");
          }));
        } catch (error) { if (invalid(error)) return; }
        schedule(delay);
      }));
    } catch (error) {
      if (invalid(error)) return;
      checking = false;
      nextReadAt = Date.now() + 15 * 60_000;
      schedule(15 * 60_000);
    }
  }
  observer = new MutationObserver(check);
  observer.observe(document.documentElement, {childList: true, subtree: true, characterData: true});
  function settingsChanged(changes, area) {
    if (stopped || invalid()) return;
    if (area === "local" && (changes.auto_collect_enabled || changes.polling_interval_minutes)) {
      nextReadAt = 0;
      schedule(0);
    }
  }
  try { chrome.storage.onChanged.addListener(settingsChanged); }
  catch (error) { if (!invalid(error)) throw error; }
  check();
})();
