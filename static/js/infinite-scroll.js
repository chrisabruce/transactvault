// Infinite-scroll trigger for the transactions list.
//
// The list page renders the first page of rows server-side. The last
// <li> on each page is a sentinel with `data-tx-sentinel="<url>"` where
// the URL hits the same controller with `?page=N&fragment=rows`. When
// the sentinel scrolls into view we fetch that URL, append the
// returned rows in place of the sentinel, and the new server-rendered
// sentinel (if any) is wired up automatically.
//
// Uses IntersectionObserver natively rather than Datastar directives
// because Datastar's HTML-response merge defaults to morphing the
// entire response into the page (Idiomorph), and append-into-list isn't
// a one-liner in current Datastar idioms. Same UX, ~30 lines, no
// version-drift risk on the CDN bundle.
(function () {
    "use strict";

    var list = document.getElementById("tx-list");
    if (!list || !("IntersectionObserver" in window)) return;

    // Guard against the sentinel firing twice during fast scroll: once
    // a request is in flight for a given URL, ignore subsequent hits
    // for the same URL until it resolves or fails.
    var inFlight = new Set();

    function observe(sentinel) {
        if (!sentinel || !sentinel.dataset || !sentinel.dataset.txSentinel) return;
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
        var url = sentinel.dataset.txSentinel;
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
                // Parse the fragment into a detached fragment so we can
                // pluck out only the <li> children. The server returns
                // raw rows + an optional new sentinel — no wrapping
                // <html>/<body>.
                var tpl = document.createElement("template");
                tpl.innerHTML = html.trim();
                var newRows = Array.from(tpl.content.children);

                // Insert the new rows in place of the old sentinel,
                // then drop the old sentinel itself.
                newRows.forEach(function (row) {
                    list.insertBefore(row, sentinel);
                });
                var nextSentinel = list.querySelector(".transaction-sentinel:not([data-wired])");
                sentinel.parentNode.removeChild(sentinel);

                inFlight.delete(url);
                // Wire the next sentinel (the one we just appended).
                if (nextSentinel) {
                    nextSentinel.setAttribute("data-wired", "1");
                    observe(nextSentinel);
                }
            })
            .catch(function (err) {
                console.warn("infinite-scroll: fetch failed", err);
                inFlight.delete(url);
                // Leave the sentinel in place so a manual reload retries.
            });
    }

    // Initial wiring.
    var first = list.querySelector(".transaction-sentinel");
    if (first) {
        first.setAttribute("data-wired", "1");
        observe(first);
    }
})();
