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

    // `{key}` → `form.dataset.confirmKey` (camelCased per dataset rules).
    // Missing keys collapse to empty string rather than `undefined` so
    // the message reads cleanly even if the template forgot to attach
    // a data attribute.
    function interpolate(form) {
        var raw = form.dataset.confirm || "";
        return raw.replace(/\{(\w+)\}/g, function (_, key) {
            var camel = "confirm" + key.charAt(0).toUpperCase() + key.slice(1);
            return form.dataset[camel] || "";
        });
    }

    document.addEventListener(
        "submit",
        function (e) {
            var form = e.target && e.target.closest && e.target.closest("form[data-confirm]");
            if (!form) return;
            if (!window.confirm(interpolate(form))) {
                e.preventDefault();
                e.stopPropagation();
            }
        },
        true
    );
})();
