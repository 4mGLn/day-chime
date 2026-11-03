const {test} = require("node:test");
const assert = require("node:assert/strict");
const {read, SOURCE, isHome} = require("../reader.js");
const now = new Date("2026-11-03T02:00:00Z");
const text = (textContent) => ({textContent});
function row(title, value) {
  return {querySelectorAll: (selector) => selector === ".tit" ? [text(title)] : selector === ".data" ? [text(value)] : []};
}
function documentWith({date = "2026년 11월 03일 (화) 11:19:57", clockIn = "09:50:00", clockOut = "-", rows, widgets = 1} = {}) {
  const widget = {querySelectorAll: (selector) => selector === ".today_time .date" ? [text(date)] : selector === ".work_type" ? rows ?? [row("출근 시간", clockIn), row("퇴근 시간", clockOut)] : []};
  return {querySelectorAll: (selector) => selector === ".gadget_attend" ? Array(widgets).fill(widget) : []};
}
test("reads verified home widget and strips URL query", () => {
  assert.deepEqual(read(documentWith(), `${SOURCE}?private=query`, now), {
    schema_version: 1, source_url: SOURCE, observed_at: now.toISOString(), available: true,
    working_day: "2026-11-03", clock_in: "09:50:00", clock_out: null,
  });
});
test("records clock out and missing check in", () => {
  assert.equal(read(documentWith({clockOut: "18:50:00"}), SOURCE, now).clock_out, "18:50:00");
  assert.equal(read(documentWith({clockIn: "-"}), SOURCE, now).clock_in, null);
});
test("restricts exact origin and home path", () => {
  for (const url of ["http://example.daouoffice.com/home", "https://evil.example/home", `${SOURCE}/other`, "https://example.daouoffice.com/login"]) {
    assert.equal(isHome(url), false);
    assert.throws(() => read(documentWith(), url, now), /wrong_page/);
  }
});
test("missing or duplicate widgets invalidate local snapshot", () => {
  for (const widgets of [0, 2]) {
    const result = read(documentWith({widgets}), SOURCE, now);
    assert.equal(result.available, false);
    assert.equal(result.working_day, null);
    assert.equal(result.clock_in, null);
  }
});
test("rejects malformed or impossible dates and time values", () => {
  for (const date of ["2026년 02월 30일", "2026년 13월 01일", "not a date"]) assert.equal(read(documentWith({date}), SOURCE, now).available, false);
  for (const clockIn of ["24:00:00", "09:60:00", "9:50", "", "unknown"]) assert.equal(read(documentWith({clockIn}), SOURCE, now).available, false);
});
test("duplicate or missing labelled rows fail closed", () => {
  for (const rows of [[row("출근 시간", "09:50:00")], [row("출근 시간", "09:50:00"), row("출근 시간", "10:00:00"), row("퇴근 시간", "-")]]) {
    assert.equal(read(documentWith({rows}), SOURCE, now).available, false);
  }
});

test("installed company configuration selects only its own home", () => {
  const fs = require("node:fs");
  const vm = require("node:vm");
  const script = fs.readFileSync(require.resolve("../reader.js"), "utf8");
  const context = {URL, DayChimeSite: {sourceUrl: "https://my-team.daouoffice.com/home"}};
  vm.runInNewContext(script, context);
  assert.equal(context.DayChimeReader.SOURCE, "https://my-team.daouoffice.com/home");
  assert.equal(context.DayChimeReader.isHome("https://my-team.daouoffice.com/home?view=today"), true);
  assert.equal(context.DayChimeReader.isHome(SOURCE), false);
  assert.equal(context.DayChimeReader.isHome("https://my-team.daouoffice.com.evil.example/home"), false);
});
test("invalid configured company URLs fail before exposing a page reader", () => {
  const fs = require("node:fs");
  const vm = require("node:vm");
  const script = fs.readFileSync(require.resolve("../reader.js"), "utf8");
  for (const sourceUrl of ["http://example.daouoffice.com/home", "https://example.daouoffice.com:443/home", "https://example.daouoffice.com:8080/home", "https://other.example.daouoffice.com/home", "https://-example.daouoffice.com/home", "https://example.daouoffice.com/home?secret=value", "https://user:password@example.daouoffice.com/home", "https://example.daouoffice.com/home#section", "https://example.daouoffice.com/login"]) {
    const context = {URL, DayChimeSite: {sourceUrl}};
    assert.throws(() => vm.runInNewContext(script, context));
    assert.equal(context.DayChimeReader, undefined);
  }
});
