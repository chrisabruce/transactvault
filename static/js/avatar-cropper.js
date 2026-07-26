// Avatar uploader using Cropper.js v2.
//
// Flow:
// 1. User picks a file via #avatar-picker (any image type).
// 2. We load it into the #avatar-cropper-dialog's <img> and init Cropper.
// 3. User adjusts the crop box (square, 1:1 aspect locked).
// 4. "Use this crop" -> canvas.toBlob('image/png') -> attaches to a
//    hidden <input> on #avatar-form -> "Save photo" enabled.
// 5. Submitting #avatar-form posts the blob to /app/profile/avatar.
//
// Cropper.js v2 has a different API surface than v1 — it uses custom
// elements (<cropper-canvas>, <cropper-image>, etc.). We use the
// imperative `Cropper` constructor it still exports for ease of
// integration with our existing dialog.
(function () {
    "use strict";

    var dialog = document.getElementById("avatar-cropper-dialog");
    var img = document.getElementById("avatar-cropper-image");
    var picker = document.getElementById("avatar-picker");
    var cancelBtn = document.getElementById("avatar-crop-cancel");
    var applyBtn = document.getElementById("avatar-crop-apply");
    var form = document.getElementById("avatar-form");
    var saveBtn = document.getElementById("avatar-save");
    // Reassigned by attachToForm when the initials placeholder gets
    // swapped for a real <img> — it must keep pointing at the live node.
    var preview = document.getElementById("avatar-current");

    if (!dialog || !img || !picker || !cancelBtn || !applyBtn || !form || !saveBtn) return;

    var cropper = null;
    var pickedObjectUrl = null;
    var previewUrl = null;
    var croppedBlob = null;

    picker.addEventListener("change", function () {
        var file = picker.files && picker.files[0];
        if (!file) return;
        if (pickedObjectUrl) URL.revokeObjectURL(pickedObjectUrl);
        pickedObjectUrl = URL.createObjectURL(file);
        img.src = pickedObjectUrl;
        img.onload = function () {
            initCropper();
            dialog.showModal();
        };
    });

    function initCropper() {
        if (cropper) {
            try { cropper.destroy(); } catch (e) { /* noop */ }
            cropper = null;
        }
        if (typeof window.Cropper !== "function") {
            // The Cropper script hasn't loaded yet (CDN hiccup or
            // offline dev). Fall back gracefully: the user can still
            // upload the raw file, just without an in-browser crop.
            console.warn("avatar-cropper: Cropper not available, using raw file");
            return;
        }
        cropper = new window.Cropper(img, {
            aspectRatio: 1,
            viewMode: 1,
            autoCropArea: 0.9,
            background: false,
            zoomable: true,
            scalable: false,
            rotatable: false,
        });
    }

    cancelBtn.addEventListener("click", function () {
        closeCropper();
    });

    applyBtn.addEventListener("click", function () {
        if (!cropper) {
            // Cropper unavailable — submit the raw picked file as-is.
            var rawFile = picker.files && picker.files[0];
            if (rawFile) attachToForm(rawFile);
            closeCropper();
            return;
        }
        var canvas = cropper.getCroppedCanvas({
            width: 512,
            height: 512,
            imageSmoothingQuality: "high",
        });
        if (!canvas) {
            console.warn("avatar-cropper: getCroppedCanvas returned null");
            return;
        }
        canvas.toBlob(function (blob) {
            if (!blob) return;
            attachToForm(blob);
            closeCropper();
        }, "image/png");
    });

    function attachToForm(blob) {
        // Always the LATEST crop: pick a photo, crop it, then pick a
        // different one and this is what gets posted.
        croppedBlob = blob;
        if (previewUrl) URL.revokeObjectURL(previewUrl);
        previewUrl = URL.createObjectURL(blob);
        if (preview) {
            if (preview.tagName === "IMG") {
                preview.src = previewUrl;
            } else {
                // It's the initials fallback div — swap for an <img>.
                var newImg = document.createElement("img");
                newImg.id = "avatar-current";
                newImg.className = "avatar-preview";
                newImg.alt = "Your selected avatar";
                newImg.src = previewUrl;
                preview.replaceWith(newImg);
                // Re-point at the live node. `preview` still referenced
                // the detached div, so a second crop called
                // `replaceWith` on an element no longer in the document
                // and the thumbnail silently stopped updating.
                preview = newImg;
            }
        }
        saveBtn.disabled = false;
    }

    function closeCropper() {
        if (dialog.open) dialog.close();
        if (cropper) {
            try { cropper.destroy(); } catch (e) { /* noop */ }
            cropper = null;
        }
        picker.value = "";
    }

    // Intercept the form submit so we POST the cropped Blob (not the
    // raw file the user picked).
    form.addEventListener("submit", function (e) {
        if (!croppedBlob) return; // nothing to send
        e.preventDefault();
        var fd = new FormData();
        fd.append("avatar", croppedBlob, "avatar.png");
        fetch(form.action, {
            method: "POST",
            body: fd,
            credentials: "same-origin",
        }).then(function (resp) {
            if (resp.redirected) {
                window.location.href = resp.url;
            } else if (resp.ok) {
                window.location.reload();
            } else {
                alert("Upload failed (" + resp.status + "). Please try again.");
            }
        }).catch(function (err) {
            console.warn("avatar upload failed", err);
            alert("Upload failed. Please try again.");
        });
    });
})();
