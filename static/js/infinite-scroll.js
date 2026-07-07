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
// No JSON, no shared global state per list — everything's driven by the
// markup the server already emits. Wiring is sentinel-scoped and
// idempotent (guarded by data-wired), and a MutationObserver re-scans
// whenever nodes are added: Datastar's live-search patch replaces the
// whole results region, and the freshly morphed-in sentinel must get
// observed without any coupling between the two mechanisms.
(function () {
    "use strict";

    if (!("IntersectionObserver" in window)) return;

    var SENTINEL = ".infinite-sentinel";

    // In-flight URLs. Keyed globally rather than per-container — each
    // page fetch has a unique next-url, so two lists on one page can't
    // cross-talk, and a container replaced mid-fetch doesn't leak state.
    var inFlight = new Set();

    function observe(sentinel) {
        if (!sentinel.dataset.nextUrl) return;
        var io = new IntersectionObserver(function (entries, obs) {
            entries.forEach(function (entry) {
                if (!entry.isIntersecting) return;
                obs.disconnect();
                loadMore(sentinel);
            });
        }, { rootMargin: "200px 0px" });
        io.observe(sentinel);
    }

    function parseFragment(html, wrap) {
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
        var container = sentinel.closest("[data-infinite-list]");
        if (!container) return;
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
                var newRows = parseFragment(html, container.dataset.wrap === "tbody");
                newRows.forEach(function (row) {
                    container.insertBefore(row, sentinel);
                });
                sentinel.parentNode.removeChild(sentinel);
                inFlight.delete(url);
                // The insertion above triggers the MutationObserver,
                // which wires the new sentinel; scan() here too so the
                // handoff doesn't depend on observer timing.
                scan();
            })
            .catch(function (err) {
                console.warn("infinite-scroll: fetch failed", err);
                inFlight.delete(url);
                // Leave the sentinel in place so a manual reload
                // (scroll up + back down, or a page refresh)
                // retries.
            });
    }

    // Wire every not-yet-wired sentinel on the page. Idempotent, cheap
    // (one querySelectorAll against a class), safe to call often.
    function scan() {
        document.querySelectorAll(SENTINEL + ":not([data-wired])").forEach(function (s) {
            s.setAttribute("data-wired", "1");
            observe(s);
        });
    }

    scan();

    // Re-scan when DOM nodes are added anywhere — this is how sentinels
    // inside Datastar-patched fragments (live search results) get wired.
    new MutationObserver(function (mutations) {
        for (var i = 0; i < mutations.length; i++) {
            if (mutations[i].addedNodes.length) {
                scan();
                return;
            }
        }
    }).observe(document.body, { childList: true, subtree: true });
})();
