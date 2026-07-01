// web/api/install.js — records an install-script execution.
//
// The riz installer (web/install) fires a fire-and-forget beacon here on every
// run. Vercel injects coarse geolocation (derived from the request IP,
// server-side) via `x-vercel-ip-*` headers; the script itself sends OS / arch /
// release target / version / stage. We emit ONE structured log line per event
// — view and aggregate it in the Vercel dashboard → Logs (filter `riz-install`)
// or Observability.
//
// No personal data is stored: geo is country/region/city derived from the IP,
// and the raw IP is never logged. Zero-config Node function (no dependencies),
// so the site stays a plain static deploy plus this one endpoint.
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
    console.log("riz-install " + JSON.stringify(event));
  } catch (e) {
    console.log("riz-install error " + String((e && e.message) || e));
  }
  res.setHeader("cache-control", "no-store");
  res.setHeader("access-control-allow-origin", "*");
  res.statusCode = 204;
  res.end();
};
