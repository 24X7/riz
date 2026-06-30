// Accessible mobile navigation for the riz site.
//
// The desktop nav is unchanged. On narrow screens the link row collapses behind
// a hamburger button that this script injects into every page's <nav> (so the
// per-page HTML stays untouched). Behaviour is wired with delegated listeners so
// it survives Turbo Drive body swaps.
//
// Accessibility: the button is a real <button> with aria-expanded / aria-controls
// and a descriptive aria-label; Escape closes and returns focus to the button;
// activating a link or clicking outside closes the menu.
(function () {
  "use strict";

  // Inject the hamburger button into each nav that doesn't have one yet.
  function ensureToggle() {
    document.querySelectorAll("nav .nav-in").forEach(function (navIn) {
      if (navIn.querySelector(".nav-toggle")) return;
      var links = navIn.querySelector(".nav-links");
      if (!links) return;
      if (!links.id) links.id = "riz-nav-links";

      var btn = document.createElement("button");
      btn.type = "button";
      btn.className = "nav-toggle";
      btn.setAttribute("aria-label", "Open menu");
      btn.setAttribute("aria-expanded", "false");
      btn.setAttribute("aria-controls", links.id);
      btn.innerHTML = "<span></span><span></span><span></span>";
      navIn.appendChild(btn);
    });
  }

  function close(nav) {
    if (!nav) return;
    nav.classList.remove("menu-open");
    var btn = nav.querySelector(".nav-toggle");
    if (btn) {
      btn.setAttribute("aria-expanded", "false");
      btn.setAttribute("aria-label", "Open menu");
    }
  }

  document.addEventListener("DOMContentLoaded", ensureToggle);
  document.addEventListener("turbo:load", ensureToggle);
  ensureToggle();

  document.addEventListener("click", function (e) {
    var btn = e.target.closest(".nav-toggle");
    if (btn) {
      var nav = btn.closest("nav");
      var open = nav.classList.toggle("menu-open");
      btn.setAttribute("aria-expanded", open ? "true" : "false");
      btn.setAttribute("aria-label", open ? "Close menu" : "Open menu");
      return;
    }
    // A link inside the open menu → let navigation happen, then close.
    if (e.target.closest(".nav-links a")) {
      close(document.querySelector("nav.menu-open"));
      return;
    }
    // A click anywhere outside an open nav → close.
    var openNav = document.querySelector("nav.menu-open");
    if (openNav && !e.target.closest("nav")) close(openNav);
  });

  document.addEventListener("keydown", function (e) {
    if (e.key !== "Escape") return;
    var openNav = document.querySelector("nav.menu-open");
    if (!openNav) return;
    var btn = openNav.querySelector(".nav-toggle");
    close(openNav);
    if (btn) btn.focus();
  });
})();
