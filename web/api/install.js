// web/api/install.js — the install-script beacon (https://riz.dev/api/install).
//
// web/install fires a fire-and-forget beacon here as it runs: `stage=start`
// before the download and `stage=success` after. The script sends OS / arch /
// release target / version / stage as query params; Vercel injects coarse
// geolocation (derived server-side from the request IP) via `x-vercel-ip-*`
// headers. We emit ONE clean JSON log line per event.
//
// Because the line is pure JSON with a stable `tag`, Vercel's Observability →
// Logs — and any Log Drain (e.g. Axiom) — parse every field: filter by
// `tag = "riz-install"` and break down by country / stage / target / version.
// No database, no dependencies: the site stays a static deploy and this stays a
// zero-config function that can never slow or break an install.
//
// No personal data is stored: geo is country/region/city derived from the IP,
// and the raw IP is never logged. The beacon opts out entirely when the install
// script is run with RIZ_NO_TELEMETRY=1 (it simply never calls this endpoint).
module.exports = function handler(req, res) {
  try {
    const q = new URL(req.url, "https://riz.dev").searchParams;
    const h = req.headers || {};
    const dec = (v) => {
      if (!v) return null;
      try {
        return decodeURIComponent(v);
      } catch (_) {
        return v;
      }
    };
    const event = {
      tag: "riz-install", // stable filter key for Logs / drains
      event: "install",
      stage: q.get("stage") || "start", // "start" (attempt) | "success"
      os: q.get("os") || null, // uname -s (Darwin / Linux)
      arch: q.get("arch") || null, // uname -m (arm64 / x86_64 / aarch64)
      target: q.get("target") || null, // release triple
      version: q.get("version") || null, // requested version (latest / vX.Y.Z)
      // Coarse geo from the IP (Vercel edge headers) — never the raw address.
      country: h["x-vercel-ip-country"] || null,
      region: h["x-vercel-ip-country-region"] || null,
      city: dec(h["x-vercel-ip-city"]),
      lat: h["x-vercel-ip-latitude"] || null,
      lon: h["x-vercel-ip-longitude"] || null,
      tz: dec(h["x-vercel-ip-timezone"]),
      ua: h["user-agent"] || null,
      ts: new Date().toISOString(),
    };
    console.log(JSON.stringify(event));
  } catch (e) {
    console.log(
      JSON.stringify({ tag: "riz-install", error: String((e && e.message) || e) }),
    );
  }
  res.setHeader("cache-control", "no-store");
  res.setHeader("access-control-allow-origin", "*");
  res.statusCode = 204;
  res.end();
};
