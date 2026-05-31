// Persist the open/closed state of each checklist <details> group so a
// document upload (which triggers `window.location.reload()`) doesn't
// reset the user's manual collapse picks to the server-rendered
// defaults.
//
// Storage layout: sessionStorage keys look like
//   tv.checklist.<tx_key>.<group_name>  →  "open" | "closed"
//
// Scoped to the tab (sessionStorage, not localStorage) so opening
// another transaction in a new tab doesn't carry over preferences that
// would surprise the user across deals.
//
// The compliance panel exposes its tx key via `data-tx-key` on the
// `<section id="compliance-panel">` wrapper, and every group carries
// its own `data-group-key` (the raw category name). If either is
// missing we noop — the script is intentionally defensive so a future
// template refactor that drops an attribute doesn't crash the page.
(function () {
    "use strict";

    var PREFIX = "tv.checklist.";

    function keyFor(txKey, groupKey) {
        return PREFIX + txKey + "." + groupKey;
    }

    function eachGroup(callback) {
        var panel = document.querySelector("[data-tx-key]");
        if (!panel) return;
        var txKey = panel.dataset.txKey;
        if (!txKey) return;
        var groups = panel.querySelectorAll("details.checklist-group[data-group-key]");
        groups.forEach(function (el) {
            var groupKey = el.dataset.groupKey;
            if (!groupKey) return;
            callback(txKey, groupKey, el);
        });
    }

    function restore() {
        eachGroup(function (txKey, groupKey, el) {
            var saved;
            try {
                saved = sessionStorage.getItem(keyFor(txKey, groupKey));
            } catch (e) {
                // sessionStorage can throw in private-browsing or when
                // the quota is exceeded — fall back to the server's
                // open_by_default rendering.
                return;
            }
            if (saved === "open") el.setAttribute("open", "");
            else if (saved === "closed") el.removeAttribute("open");
        });
    }

    function persist() {
        eachGroup(function (txKey, groupKey, el) {
            try {
                sessionStorage.setItem(
                    keyFor(txKey, groupKey),
                    el.open ? "open" : "closed"
                );
            } catch (e) {
                // Same fail-soft posture as `restore()` — the worst
                // case is that the reload re-applies server defaults.
            }
        });
    }

    // Run as early as the DOM is parsed so the user never sees the
    // wrong open state flash before our restore.
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", restore);
    } else {
        restore();
    }

    // Capture every toggle (event delegation handles groups inserted
    // by future morphs). `toggle` doesn't bubble in older Firefox, so
    // we attach in capture phase on the document to catch it before
    // it ever stops.
    document.addEventListener(
        "toggle",
        function (e) {
            var el = e.target;
            if (!el || !el.matches) return;
            if (!el.matches("details.checklist-group[data-group-key]")) return;
            var panel = el.closest("[data-tx-key]");
            if (!panel || !panel.dataset.txKey) return;
            try {
                sessionStorage.setItem(
                    keyFor(panel.dataset.txKey, el.dataset.groupKey),
                    el.open ? "open" : "closed"
                );
            } catch (err) {
                // ignore — see restore() comment.
            }
        },
        true
    );

    // The upload flow calls `window.location.reload()` on success. By
    // the time `beforeunload` fires our toggle handler has already
    // persisted everything, but this is a belt-and-suspenders sweep
    // in case a reload happens via another path (e.g. status update
    // form submission) without an intervening toggle event.
    window.addEventListener("beforeunload", persist);
})();
