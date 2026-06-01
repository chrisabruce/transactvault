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
// **Attention-aware.** Groups the server flags with `data-attention`
// (the reviewer's "you need to look here" signal) are FORCED open by
// `restore()` regardless of what the user persisted. This is the fix
// for the bug where a compliance officer would open a transaction and
// see every group collapsed because the agent had walked through and
// collapsed groups while uploading — the server knows there's pending
// work to review, and that beats stale per-session state.
//
// We also only persist on USER-driven toggles. The earlier version
// also persisted on `beforeunload`, which captured every group's
// then-current open/closed state (including the server's defaults).
// That meant the first page-leave wrote choices the user never made,
// which then haunted later visits. The current handler writes only
// when the user actually clicks a disclosure summary.
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
            // Attention groups always win — never collapse one that
            // the server says needs the viewer's eyes. The user can
            // still manually collapse it after reading; that toggle
            // will persist for the rest of the session (no-op here
            // because we re-open on every load while attention holds).
            if (el.dataset.attention === "true") {
                el.setAttribute("open", "");
                return;
            }
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

    // Run as early as the DOM is parsed so the user never sees the
    // wrong open state flash before our restore.
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", restore);
    } else {
        restore();
    }

    // Capture every USER toggle. Event delegation handles groups
    // inserted by future morphs. `toggle` doesn't bubble in older
    // Firefox so we attach in capture phase on the document.
    //
    // We intentionally do NOT persist on `beforeunload` — that would
    // capture server defaults the user never chose, which is what
    // caused the "everything collapsed when compliance opens" bug.
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
})();
