// Drag-and-drop reordering for the admin form-set page.
//
// Like `infinite-scroll.js`, this is plain vanilla JS rather than a
// Datastar directive: dragging DOM nodes around is a client-side
// manipulation that doesn't map onto Datastar's morph-the-response
// model (Datastar would replace the list, fighting the live drag).
// Datastar still drives the reactive/SSE parts of the app; this is the
// right tool for the drag mechanics. On drop we POST the new key order
// to the server, which persists `sort_order` / `form_order`.
//
// Markup contract:
//   <ul|tbody|div data-reorder="<post-url>" [data-reorder-group="<key>"]>
//     <li|tr|section data-reorder-key="<key>">
//        [<button data-reorder-handle> … ]
//     </…>
//   </…>
// Every `data-reorder` container becomes independently sortable. The
// `activeContainer` guard means nested containers (a group list whose
// rows are themselves sortable) never interfere with each other.
//
// If a `[data-reorder-key]` child contains its own `[data-reorder-handle]`,
// dragging only starts from that handle — important when the row/panel
// holds form fields or buttons that would otherwise hijack clicks and
// text selection. Without a handle the whole element is draggable.
(function () {
    "use strict";

    var activeContainer = null;
    var dragEl = null;

    // The handle that belongs to `el` specifically — i.e. the first
    // `[data-reorder-handle]` whose nearest `[data-reorder-key]`
    // ancestor is `el` itself, not a nested sortable child.
    function ownHandle(el) {
        var handles = el.querySelectorAll("[data-reorder-handle]");
        for (var i = 0; i < handles.length; i++) {
            if (handles[i].closest("[data-reorder-key]") === el) return handles[i];
        }
        return null;
    }

    // The child the dragged element should be inserted *before*, based
    // on the pointer's vertical position. Returns null to append last.
    function afterElement(container, y) {
        var els = [].slice.call(
            container.querySelectorAll(":scope > [data-reorder-key]:not(.is-dragging)")
        );
        var closest = { offset: -Infinity, el: null };
        els.forEach(function (child) {
            var box = child.getBoundingClientRect();
            var offset = y - box.top - box.height / 2;
            if (offset < 0 && offset > closest.offset) {
                closest = { offset: offset, el: child };
            }
        });
        return closest.el;
    }

    function persist(container) {
        var url = container.dataset.reorder;
        var keys = [].slice
            .call(container.querySelectorAll(":scope > [data-reorder-key]"))
            .map(function (el) { return el.dataset.reorderKey; });
        var body = new URLSearchParams();
        body.set("order", keys.join(","));
        if (container.dataset.reorderGroup) {
            body.set("group", container.dataset.reorderGroup);
        }
        fetch(url, {
            method: "POST",
            headers: { "Content-Type": "application/x-www-form-urlencoded" },
            body: body.toString(),
        })
            .then(function (r) {
                // On any server error, reload so the UI re-syncs with
                // the persisted truth rather than showing a phantom order.
                if (!r.ok) window.location.reload();
            })
            .catch(function () { window.location.reload(); });
    }

    function setup(container) {
        container
            .querySelectorAll(":scope > [data-reorder-key]")
            .forEach(function (el) {
                el.classList.add("is-draggable");

                var handle = ownHandle(el);
                if (handle) {
                    // Only become draggable while the pointer is held on
                    // the handle; reset afterwards so clicks/typing inside
                    // the element behave normally.
                    handle.classList.add("drag-handle");
                    el.setAttribute("draggable", "false");
                    handle.addEventListener("mousedown", function () {
                        el.setAttribute("draggable", "true");
                    });
                    document.addEventListener("mouseup", function () {
                        el.setAttribute("draggable", "false");
                    });
                } else {
                    el.setAttribute("draggable", "true");
                }

                el.addEventListener("dragstart", function (e) {
                    // Stop the event bubbling to an enclosing sortable
                    // (e.g. the group list) so only the innermost
                    // container claims this drag.
                    e.stopPropagation();
                    dragEl = el;
                    activeContainer = container;
                    el.classList.add("is-dragging");
                    e.dataTransfer.effectAllowed = "move";
                    try {
                        e.dataTransfer.setData("text/plain", el.dataset.reorderKey);
                    } catch (_) {}
                });

                el.addEventListener("dragend", function (e) {
                    e.stopPropagation();
                    el.classList.remove("is-dragging");
                    // `mouseup` isn't guaranteed after a drag, so reset
                    // the handle-gated draggable flag here too.
                    if (handle) el.setAttribute("draggable", "false");
                    if (activeContainer === container) persist(container);
                    dragEl = null;
                    activeContainer = null;
                });
            });

        container.addEventListener("dragover", function (e) {
            if (activeContainer !== container || !dragEl) return;
            e.preventDefault();
            var after = afterElement(container, e.clientY);
            if (after == null) container.appendChild(dragEl);
            else container.insertBefore(dragEl, after);
        });
    }

    document.querySelectorAll("[data-reorder]").forEach(setup);
})();
