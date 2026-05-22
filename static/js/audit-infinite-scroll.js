// Infinite scroll for the brokerage audit log.
//
// Same mechanic as the transactions list: the server renders the
// first page server-side and tacks a sentinel <tr> on the bottom
// carrying the URL of the next page. When the sentinel scrolls into
// view we GET that URL, parse the returned table-rows fragment, and
// splice the new <tr>s in place of the old sentinel.
(function () {
    "use strict";
    var list = document.getElementById("audit-list");
    if (!list || !("IntersectionObserver" in window)) return;

    var inFlight = new Set();

    function observe(sentinel) {
        if (!sentinel || !sentinel.dataset || !sentinel.dataset.auditSentinel) return;
        var observer = new IntersectionObserver(function (entries, obs) {
            entries.forEach(function (entry) {
                if (!entry.isIntersecting) return;
                obs.disconnect();
                loadMore(sentinel);
            });
        }, { rootMargin: "200px 0px" });
        observer.observe(sentinel);
    }

    function loadMore(sentinel) {
        var url = sentinel.dataset.auditSentinel;
        if (!url || inFlight.has(url)) return;
        inFlight.add(url);

        fetch(url, { credentials: "same-origin", headers: { "Accept": "text/html" } })
            .then(function (resp) {
                if (!resp.ok) throw new Error("HTTP " + resp.status);
                return resp.text();
            })
            .then(function (html) {
                // The fragment is a sequence of <tr>…</tr> rows. Browsers
                // refuse to parse <tr> as a top-level child of <template>,
                // so wrap it in a <table> first.
                var tpl = document.createElement("template");
                tpl.innerHTML = "<table><tbody>" + html.trim() + "</tbody></table>";
                var tbody = tpl.content.querySelector("tbody");
                if (!tbody) { inFlight.delete(url); return; }
                var newRows = Array.from(tbody.children);
                newRows.forEach(function (row) {
                    list.insertBefore(row, sentinel);
                });
                var nextSentinel = list.querySelector(".audit-sentinel:not([data-wired])");
                sentinel.parentNode.removeChild(sentinel);
                inFlight.delete(url);
                if (nextSentinel) {
                    nextSentinel.setAttribute("data-wired", "1");
                    observe(nextSentinel);
                }
            })
            .catch(function (err) {
                console.warn("audit infinite-scroll: fetch failed", err);
                inFlight.delete(url);
            });
    }

    var first = list.querySelector(".audit-sentinel");
    if (first) {
        first.setAttribute("data-wired", "1");
        observe(first);
    }
})();
