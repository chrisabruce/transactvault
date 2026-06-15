// Safe replacement for `onsubmit="return confirm('… {{ user_data }} …')"`.
//
// The Askama autoescape that protects HTML context does NOT escape
// JavaScript single-quotes, so any user-controlled value interpolated
// into a JS string in an HTML attribute is an XSS sink — `Alice');
// fetch('/evil'); //` breaks the JS string and runs in the browser of
// whoever loads the page.
//
// This module is the one allowed entry point for confirm() dialogs.
// Forms opt in with:
//
//   <form data-confirm="Remove {name} from the brokerage?"
//         data-confirm-name="{{ member.name }}">
//
// The template never interpolates into JS context — only into data-*
// attribute values, which the browser parses as text. The JS reads the
// value via `dataset` (still text) and string-concats it into the
// message. The DOM never executes anything from the user data.
//
// Event delegation on `submit` (capture phase) means forms added via
// Datastar morphs work without re-wiring.
(function () {
    "use strict";

    // `{key}` → `el.dataset.confirmKey` (camelCased per dataset rules).
    // Missing keys collapse to empty string rather than `undefined` so
    // the message reads cleanly even if the template forgot to attach
    // a data attribute. Works on either a <form> or a submit <button> —
    // both expose `dataset`.
    function interpolate(el) {
        var raw = el.dataset.confirm || "";
        return raw.replace(/\{(\w+)\}/g, function (_, key) {
            var camel = "confirm" + key.charAt(0).toUpperCase() + key.slice(1);
            return el.dataset[camel] || "";
        });
    }

    document.addEventListener(
        "submit",
        function (e) {
            var form = e.target && e.target.closest && e.target.closest("form");
            if (!form) return;
            // A single form can host several actions via per-button
            // `formaction` (e.g. Deactivate + Delete in one table cell,
            // since two <form>s in one <td> don't parse reliably). Prefer
            // a `data-confirm` on the button that actually submitted;
            // fall back to one on the form. If neither carries it, this
            // submit isn't gated — let it through untouched.
            var src =
                e.submitter && e.submitter.dataset && e.submitter.dataset.confirm
                    ? e.submitter
                    : form.dataset && form.dataset.confirm
                      ? form
                      : null;
            if (!src) return;
            if (!window.confirm(interpolate(src))) {
                e.preventDefault();
                e.stopPropagation();
            }
        },
        true
    );
})();
