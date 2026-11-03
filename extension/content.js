(() => {
  "use strict";
  if (!DayChimeReader.isHome(location.href)) return;
  let lastSent = 0;
  let timer;
  function send() {
    if (!DayChimeReader.isHome(location.href)) return;
    const elapsed = Date.now() - lastSent;
    if (elapsed < 30_000) {
      if (!timer) timer = setTimeout(() => { timer = undefined; send(); }, 30_000 - elapsed);
      return;
    }
    lastSent = Date.now();
    chrome.runtime.sendMessage({kind: "attendance", snapshot: DayChimeReader.read(document, location.href)}, () => {
      // Reading lastError prevents an unhandled extension-context error on reload.
      if (chrome.runtime.lastError) console.warn("Day Chime: 확장 프로그램 연결을 확인하세요.");
    });
  }
  new MutationObserver(send).observe(document.documentElement, {childList: true, subtree: true, characterData: true});
  setInterval(send, 30_000);
  send();
})();
