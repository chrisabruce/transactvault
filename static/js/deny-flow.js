// One-click deny + optional comment modal.
//
// The deny <form> posts to /app/checklist/{id}/deny with no body — the
// server's DenyInput.reason is Option<String> and defaults to None.
// We intercept the submit so the page DOESN'T reload yet, then open a
// shared <dialog> for the optional reason. The button label switches
// between "No comment" (empty textarea = just close) and "Save comment"
// (non-empty = POST to /app/checklist/{id}/comments first, then close).
// Reload on dialog close so the row's red/pending state, audit line,
// and needs-attention flags all refresh from the server.
//
// No-JS fallback: the form is a normal POST, so disabling JS just gives
// the user the old single-click-denies-but-no-prompt behavior.
(function () {
    "use strict";

    const dialog = document.getElementById("deny-reason-dialog");
    if (!dialog) return; // Page rendered for someone who can't review.

    const textarea = document.getElementById("deny-reason-text");
    const button = document.getElementById("deny-reason-button");
    let commentAction = null;

    function refreshButtonLabel() {
        button.textContent = textarea.value.trim() ? "Save comment" : "No comment";
    }
    textarea.addEventListener("input", refreshButtonLabel);

    button.addEventListener("click", async function () {
        const text = textarea.value.trim();
        if (text && commentAction) {
            try {
                await fetch(commentAction, {
                    method: "POST",
                    headers: { "Content-Type": "application/x-www-form-urlencoded" },
                    body: new URLSearchParams({ body: text }).toString(),
                });
            } catch (_) {
                // Best-effort: comment failed to post, but the deny already
                // succeeded. Reload anyway — at worst the reviewer can
                // re-add the comment from the regular thread.
            }
        }
        dialog.close();
    });

    // ESC, the dialog backdrop, or the Save/No-comment button all funnel
    // through `close`. Reload so the row reflects its new denied state.
    dialog.addEventListener("close", function () {
        window.location.reload();
    });

    document.querySelectorAll("form.deny-form").forEach(function (form) {
        form.addEventListener("submit", async function (e) {
            e.preventDefault();
            let response;
            try {
                response = await fetch(form.action, {
                    method: "POST",
                    headers: { "Content-Type": "application/x-www-form-urlencoded" },
                    body: "",
                });
            } catch (_) {
                // Network died — degrade to native submit so the user
                // still sees the server-side error page.
                form.submit();
                return;
            }
            if (!response.ok) {
                form.submit();
                return;
            }
            commentAction = form.dataset.commentAction || null;
            textarea.value = "";
            refreshButtonLabel();
            dialog.showModal();
            textarea.focus();
        });
    });
})();
