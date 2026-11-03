"use strict";
chrome.runtime.sendMessage({kind: "status"}, (status) => {
  const error = chrome.runtime.lastError;
  document.querySelector("#status").textContent = error ? "확장 프로그램을 다시 로드해 주세요." : status?.message ?? "상태를 확인할 수 없습니다.";
  document.querySelector("#time").textContent = status?.updated_at ? `최근 확인: ${new Date(status.updated_at).toLocaleString()}` : "";
});
