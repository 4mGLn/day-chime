(function (root) {
  "use strict";
  const SOURCE = root.DayChimeSite?.sourceUrl ?? "https://example.daouoffice.com/home";
  function validateSource(source) {
    if (typeof source !== "string") throw Error("invalid_site_configuration");
    const parsed = new URL(source);
    if (parsed.protocol !== "https:" || !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.daouoffice\.com$/.test(parsed.hostname)
      || parsed.pathname !== "/home" || parsed.username || parsed.password || parsed.port
      || parsed.search || parsed.hash || parsed.href !== source) throw Error("invalid_site_configuration");
    return parsed.origin;
  }
  // Invalid installation configuration fails before any page can be read.
  const ORIGIN = validateSource(SOURCE);
  function isHome(url) {
    try {
      const value = new URL(url);
      return value.origin === ORIGIN && value.pathname === "/home" && !value.username && !value.password;
    } catch { return false; }
  }
  function time(value) {
    if (value === "-") return null;
    if (!/^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d$/.test(value)) throw Error("invalid_time");
    return value;
  }
  function date(value) {
    const match = /^(\d{4})년\s+(\d{1,2})월\s+(\d{1,2})일(?:\s|$)/.exec(value);
    if (!match) throw Error("invalid_date");
    const [, y, m, d] = match;
    const parsed = new Date(Date.UTC(Number(y), Number(m) - 1, Number(d)));
    if (parsed.getUTCFullYear() !== Number(y) || parsed.getUTCMonth() + 1 !== Number(m) || parsed.getUTCDate() !== Number(d)) throw Error("invalid_date");
    return `${y}-${m.padStart(2, "0")}-${d.padStart(2, "0")}`;
  }
  function read(document, url, now = new Date()) {
    if (!isHome(url)) throw Error("wrong_page");
    const base = {schema_version: 1, source_url: SOURCE, observed_at: now.toISOString()};
    try {
      const widgets = document.querySelectorAll(".gadget_attend");
      if (widgets.length !== 1) throw Error(widgets.length ? "duplicate_widget" : "widget_missing");
      const widget = widgets[0];
      const dates = widget.querySelectorAll(".today_time .date");
      if (dates.length !== 1) throw Error("invalid_date");
      const values = new Map();
      for (const row of widget.querySelectorAll(".work_type")) {
        const titles = row.querySelectorAll(".tit");
        const data = row.querySelectorAll(".data");
        if (titles.length !== 1 || data.length !== 1) throw Error("invalid_row");
        const title = titles[0].textContent.trim();
        if (!["출근 시간", "퇴근 시간"].includes(title)) continue;
        if (values.has(title)) throw Error("duplicate_row");
        values.set(title, time(data[0].textContent.trim()));
      }
      if (values.size !== 2) throw Error("missing_row");
      return {...base, available: true, working_day: date(dates[0].textContent.trim()), clock_in: values.get("출근 시간"), clock_out: values.get("퇴근 시간")};
    } catch (error) {
      return {...base, available: false, working_day: null, clock_in: null, clock_out: null, error: error.message};
    }
  }
  const api = {SOURCE, isHome, read};
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.DayChimeReader = api;
})(typeof globalThis !== "undefined" ? globalThis : this);
