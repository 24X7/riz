// Hero terminal demo for the riz site.
//
// Replays the core story — a plain handler → `riz run` → an HTTP call → adding
// it to an agent → the agent calling it as a typed MCP tool — as a typewritten
// terminal animation. Pure JS, no dependencies, no video hosting.
//
// Accessibility: the full transcript is in the DOM as text (a screen reader
// reads it whole); `prefers-reduced-motion` renders the final state instantly
// with no animation. An IntersectionObserver pauses the loop when off-screen,
// and it re-inits on Turbo Drive navigations.
(function () {
  "use strict";

  // c: line kind — cmd (typed `$` command), agent (typed agent call),
  // cmt (comment), code (handler source), out (command output), gap (blank).
  var SEQ = [
    { c: "cmd", t: "riz init typescript-http orders && cd orders" },
    { c: "out", t: "  ✓ scaffolded orders/" },
    { c: "gap" },
    { c: "cmt", t: "# api/orders.ts — a plain AWS-Lambda-shaped handler" },
    { c: "code", t: "export const handler = async (e) => ({" },
    { c: "code", t: "  statusCode: 200," },
    { c: "code", t: "  body: JSON.stringify({ id: e.pathParameters.id, total: 4299 })," },
    { c: "code", t: "});" },
    { c: "gap" },
    { c: "cmd", t: "riz run" },
    { c: "out", t: "  ▸ orders ready on :3000  ·  MCP at /_riz/mcp" },
    { c: "gap" },
    { c: "cmd", t: "curl localhost:3000/orders/1042" },
    { c: "out", t: '  {"id":"1042","total":4299}' },
    { c: "gap" },
    { c: "cmt", t: "# point an agent at it — no SDK, no glue" },
    { c: "cmd", t: "claude mcp add riz --transport http localhost:3000/_riz/mcp" },
    { c: "out", t: "  ✓ riz · 1 tool (get_order)" },
    { c: "gap" },
    { c: "cmt", t: "# the agent calls your function as a typed tool" },
    { c: "agent", t: 'agent ▸ tools/call get_order { "id": "1042" }' },
    { c: "out", t: '  ← {"id":"1042","total":4299}' },
  ];

  var TYPED = { cmd: 1, agent: 1, cmt: 1 }; // line kinds that typewrite
  var run = 0; // generation token — bumped to cancel an in-flight animation

  function lineEl(kind) {
    var el = document.createElement("div");
    el.className = "td-line td-" + kind;
    return el;
  }

  // Static, fully-rendered transcript — for reduced-motion and as the SR text.
  function renderStatic(screen) {
    screen.textContent = "";
    SEQ.forEach(function (l) {
      var el = lineEl(l.c);
      if (l.c === "gap") el.innerHTML = "&nbsp;";
      else if (l.c === "cmd") el.textContent = "$ " + l.t;
      else el.textContent = l.t;
      screen.appendChild(el);
    });
  }

  function animate(screen) {
    var gen = ++run;
    screen.textContent = "";
    var i = 0;

    function done() {
      return gen !== run; // a newer run (or teardown) cancelled this one
    }

    function nextLine() {
      if (done()) return;
      if (i >= SEQ.length) {
        // hold the finished frame, then restart the loop
        setTimeout(function () {
          if (!done()) animate(screen);
        }, 4200);
        return;
      }
      var l = SEQ[i++];
      var el = lineEl(l.c);
      screen.appendChild(el);
      screen.scrollTop = screen.scrollHeight;

      if (l.c === "gap") {
        el.innerHTML = "&nbsp;";
        setTimeout(nextLine, 90);
        return;
      }
      if (TYPED[l.c]) {
        var prefix = l.c === "cmd" ? "$ " : "";
        typeInto(el, prefix, l.t, nextLine);
      } else {
        // output / code — reveal whole, brief beat so it reads as a response
        el.textContent = l.t;
        setTimeout(nextLine, l.c === "out" ? 320 : 60);
      }
    }
    nextLine();
  }

  function typeInto(el, prefix, text, after) {
    var n = 0;
    el.classList.add("td-typing");
    function step() {
      if (run && el.isConnected === false) return;
      el.textContent = prefix + text.slice(0, n);
      if (n++ < text.length) {
        setTimeout(step, 16 + Math.floor(text.length > 40 ? 6 : 14));
      } else {
        el.classList.remove("td-typing");
        setTimeout(after, 360);
      }
    }
    step();
  }

  function init() {
    var screen = document.getElementById("td-screen");
    if (!screen) return;
    run++; // cancel any prior animation bound to an old DOM node

    var reduce =
      window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduce) {
      renderStatic(screen);
      return;
    }

    // Only animate while visible — saves cycles when scrolled away.
    if ("IntersectionObserver" in window) {
      renderStatic(screen); // a sensible first paint before it scrolls in
      var io = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (en) {
            if (en.isIntersecting) animate(screen);
            else run++; // off-screen → cancel
          });
        },
        { threshold: 0.25 }
      );
      io.observe(screen);
    } else {
      animate(screen);
    }
  }

  document.addEventListener("DOMContentLoaded", init);
  document.addEventListener("turbo:load", init);
  init();
})();
