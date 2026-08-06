// Slide-up contact panel.
//
// The panel and its triggers start hidden and are revealed here, so a
// browser with JS off never shows a button that can't submit.
//
// Opening the panel fetches a signed timestamp from /contact/token and
// puts it in the form. The server requires that token, and requires it
// to be at least a few seconds old — which is the anti-spam half that
// the honeypot alone can't cover: a script POSTing straight at /contact
// has no token to send.
(function () {
    "use strict";

    var sheet = document.getElementById("contact-sheet");
    var form = document.getElementById("contact-form");
    if (!sheet || !form) return;

    var openers = document.querySelectorAll("[data-contact-open]");
    if (!openers.length) return;

    var errorEl = sheet.querySelector(".contact-error");
    var doneEl = sheet.querySelector(".contact-done");
    var tokenInput = form.querySelector("input[name=token]");
    var lastFocused = null;
    var tokenPending = false;

    // Triggers are only useful once this file has run.
    openers.forEach(function (btn) {
        btn.hidden = false;
        btn.addEventListener("click", open);
    });

    function fetchToken() {
        if (tokenPending || tokenInput.value) return;
        tokenPending = true;
        fetch("/contact/token", { headers: { accept: "application/json" } })
            .then(function (r) { return r.ok ? r.json() : null; })
            .then(function (data) {
                if (data && data.token) tokenInput.value = data.token;
            })
            .catch(function () { /* submit will surface the problem */ })
            .finally(function () { tokenPending = false; });
    }

    function open() {
        lastFocused = document.activeElement;
        sheet.hidden = false;
        sheet.setAttribute("aria-hidden", "false");
        // Next frame, so the transition has a start state to animate from.
        window.requestAnimationFrame(function () { sheet.classList.add("is-open"); });
        document.body.classList.add("contact-open");
        fetchToken();
        var first = form.querySelector("input:not([type=hidden]):not(.hp-field), textarea");
        if (first) first.focus();
    }

    function close() {
        sheet.classList.remove("is-open");
        document.body.classList.remove("contact-open");
        sheet.setAttribute("aria-hidden", "true");
        window.setTimeout(function () { sheet.hidden = true; }, 220);
        if (lastFocused && lastFocused.focus) lastFocused.focus();
    }

    sheet.querySelectorAll("[data-contact-close]").forEach(function (el) {
        el.addEventListener("click", close);
    });
    document.addEventListener("keydown", function (e) {
        if (e.key === "Escape" && !sheet.hidden) close();
    });

    form.addEventListener("submit", function (e) {
        e.preventDefault();
        errorEl.hidden = true;
        var button = form.querySelector("button[type=submit]");
        button.disabled = true;

        fetch("/contact", {
            method: "POST",
            headers: { accept: "application/json" },
            body: new URLSearchParams(new FormData(form)),
        })
            .then(function (r) {
                return r.json().catch(function () { return {}; }).then(function (data) {
                    if (!r.ok) throw new Error(data.error || data.message || "That didn't send. Try again.");
                    return data;
                });
            })
            .then(function () {
                form.hidden = true;
                doneEl.hidden = false;
            })
            .catch(function (err) {
                errorEl.textContent = err.message;
                errorEl.hidden = false;
                // A rejected token is single-use from the page's point of
                // view; drop it so the next attempt fetches a fresh one.
                tokenInput.value = "";
                fetchToken();
                button.disabled = false;
            });
    });
})();
