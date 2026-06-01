// Server-rendered infinite scroll, data-attribute driven so the
// transactions list, audit log, search results, and any future
// paginated surface can share one implementation.
//
// MARKUP CONTRACT
// ---------------
// The container holds the rows. Each page's last row is a "sentinel"
// carrying the URL of the next page. When the sentinel scrolls into
// view we GET that URL, parse the fragment, splice the new rows in
// place of the old sentinel, and the new sentinel (if any) gets
// observed automatically.
//
//   <ul data-infinite-list>                           ← container
//       <li>…</li>                                    ← rows
//       <li class="infinite-sentinel"                 ← sentinel
//           data-next-url="/app?page=2&fragment=rows"
//           aria-hidden="true">…</li>
//   </ul>
//
// For tables, set `data-wrap="tbody"` on the container. Browsers
// refuse to parse a bare `<tr>` as a top-level child of `<template>`,
// so the fragment gets wrapped in a `<table><tbody>` first:
//
//   <tbody data-infinite-list data-wrap="tbody">
//       <tr>…</tr>
//       <tr class="infinite-sentinel" data-next-url="…">…</tr>
//   </tbody>
//
// No JSON, no shared global state — everything's driven by the markup
// the server already emits. Each container is independent, so two
// paginated lists on the same page work without interfering.
(function () {
    "use strict";

    if (!("IntersectionObserver" in window)) return;

    var SENTINEL = ".infinite-sentinel";

    // Per-instance state lives in a closure created when we mount the
    // container, so two lists on one page don't share in-flight sets
    // or accidentally cross-fetch.
    function mount(container) {
        var inFlight = new Set();
        var wrap = container.dataset.wrap === "tbody";

        function observe(sentinel) {
            if (!sentinel || !sentinel.dataset || !sentinel.dataset.nextUrl) return;
            var io = new IntersectionObserver(function (entries, obs) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) return;
                    obs.disconnect();
                    loadMore(sentinel);
                });
            }, { rootMargin: "200px 0px" });
            io.observe(sentinel);
        }

        function parseFragment(html) {
            var tpl = document.createElement("template");
            // For table-row fragments, browsers drop bare <tr> nodes
            // unless they're parented under <table><tbody>. We add the
            // wrapper, then extract the tbody's children — the wrapper
            // itself is discarded.
            if (wrap) {
                tpl.innerHTML = "<table><tbody>" + html.trim() + "</tbody></table>";
                var tbody = tpl.content.querySelector("tbody");
                return tbody ? Array.from(tbody.children) : [];
            }
            tpl.innerHTML = html.trim();
            return Array.from(tpl.content.children);
        }

        function loadMore(sentinel) {
            var url = sentinel.dataset.nextUrl;
            if (!url || inFlight.has(url)) return;
            inFlight.add(url);

            fetch(url, {
                credentials: "same-origin",
                headers: { "Accept": "text/html" }
            })
                .then(function (resp) {
                    if (!resp.ok) throw new Error("HTTP " + resp.status);
                    return resp.text();
                })
                .then(function (html) {
                    var newRows = parseFragment(html);
                    newRows.forEach(function (row) {
                        container.insertBefore(row, sentinel);
                    });
                    // The new sentinel (if any) is whatever rows
                    // we just inserted left behind — find the
                    // un-wired one and observe it.
                    var next = container.querySelector(SENTINEL + ":not([data-wired])");
                    sentinel.parentNode.removeChild(sentinel);
                    inFlight.delete(url);
                    if (next) {
                        next.setAttribute("data-wired", "1");
                        observe(next);
                    }
                })
                .catch(function (err) {
                    console.warn("infinite-scroll: fetch failed", err);
                    inFlight.delete(url);
                    // Leave the sentinel in place so a manual reload
                    // (scroll up + back down, or a page refresh)
                    // retries.
                });
        }

        var first = container.querySelector(SENTINEL);
        if (first) {
            first.setAttribute("data-wired", "1");
            observe(first);
        }
    }

    document.querySelectorAll("[data-infinite-list]").forEach(mount);
})();
