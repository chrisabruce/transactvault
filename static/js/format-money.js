// Live thousand-separator formatting for any `<input data-format="money">`.
//
// Backend (`parse_price_cents`) strips every non-digit before parsing, so
// it accepts whatever shape the user leaves in the field. The job of this
// script is purely visual: reformat as they type while preserving cursor
// position relative to the digits (not the formatted string), so the
// caret doesn't jump every time a comma is inserted or removed.
(function () {
    "use strict";

    function format(input) {
        var caret = input.selectionStart || 0;
        var raw = input.value;

        // Count digits to the left of the caret in the *raw* input so we
        // can restore the cursor to the same logical position after
        // reformatting (commas and the leading `$` shift it otherwise).
        var digitsBefore = 0;
        for (var i = 0; i < caret; i++) {
            var ch = raw.charCodeAt(i);
            if (ch >= 48 && ch <= 57) digitsBefore++;
        }

        // Keep digits + at most one decimal point.
        var cleaned = raw.replace(/[^0-9.]/g, "");
        var firstDot = cleaned.indexOf(".");
        if (firstDot !== -1) {
            cleaned = cleaned.slice(0, firstDot + 1) +
                cleaned.slice(firstDot + 1).replace(/\./g, "");
        }

        var parts = cleaned.split(".");
        var intPart = (parts[0] || "").replace(/\B(?=(\d{3})+(?!\d))/g, ",");
        var formatted = intPart;
        if (parts.length === 2) {
            // Cap decimals at two digits — real-estate prices are dollars.
            formatted += "." + parts[1].slice(0, 2);
        }
        if (formatted.length > 0) formatted = "$" + formatted;

        if (formatted === raw) return; // no-op, don't disturb the cursor

        input.value = formatted;

        // Restore the cursor: walk the formatted string and stop right
        // after the Nth digit, where N = digitsBefore. If the field had
        // no digits to the left of the original cursor, slot in after
        // the leading "$" (or stay at 0 for an empty field).
        var pos;
        if (digitsBefore === 0) {
            pos = formatted.length > 0 ? 1 : 0;
        } else {
            pos = formatted.length;
            var count = 0;
            for (var j = 0; j < formatted.length; j++) {
                var c = formatted.charCodeAt(j);
                if (c >= 48 && c <= 57) {
                    count++;
                    if (count === digitsBefore) {
                        pos = j + 1;
                        break;
                    }
                }
            }
        }
        input.setSelectionRange(pos, pos);
    }

    document.querySelectorAll('input[data-format="money"]').forEach(function (input) {
        input.addEventListener("input", function () { format(input); });
        if (input.value) format(input);
    });
})();
