// Intl oracle for apps/web/src/timestampFormat.ts; run with Node to refresh the core fixture.
import * as NodeFS from "node:fs";
const locales = [
  "en-US",
  "en-GB",
  "en-NL",
  "de-DE",
  "ar-EG",
  "es-MX",
  "fr-CA",
  "zh-TW",
  "ja-JP",
  "ko-KR",
  "fr-FR",
  "fa-IR",
];
const hours = ["00:30", "09:05", "12:00", "13:45", "23:59"];
const formats = ["locale", "12-hour", "24-hour"];
const cases = locales.flatMap((base) =>
  ["", "-u-hc-h11", "-u-hc-h12", "-u-hc-h23", "-u-hc-h24"].flatMap((hc) =>
    hours.flatMap((time) =>
      formats.map((format) => {
        const locale = base + hc;
        const options = {
          hour: "numeric",
          minute: "2-digit",
          timeZone: "UTC",
          ...(format === "locale" ? {} : { hour12: format === "12-hour" }),
        };
        return {
          locale,
          time,
          format,
          expected: new Intl.DateTimeFormat(locale, options).format(
            new Date(`2026-07-03T${time}:00.000Z`),
          ),
        };
      }),
    ),
  ),
);
NodeFS.writeFileSync(
  new URL("../crates/core/src/timeline/fixtures/timestamp-intl.json", import.meta.url),
  JSON.stringify({ node: process.version, icu: process.versions.icu, cases }, null, 2) + "\n",
);
