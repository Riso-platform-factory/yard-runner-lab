//! A known error is an ERROR that is understood but cannot be fixed in the application yet.
//! Add entries only with a specific target and message substrings. Review entries
//! whenever the Zed revision is bumped and remove them once the error no longer occurs.

pub(crate) struct KnownError {
    /// Stable identifier attached to matching lifecycle lines.
    pub slug: &'static str,
    /// Exact log target that emits the error.
    pub target: &'static str,
    /// Substrings that must all appear in the error message.
    pub message_contains: &'static [&'static str],
    /// Understood cause and what would fix the error.
    #[expect(
        dead_code,
        reason = "Registry documentation for reviewing upstream errors"
    )]
    pub reason: &'static str,
    /// Issue or change tracking the fix, when available.
    #[expect(
        dead_code,
        reason = "Registry documentation for reviewing upstream errors"
    )]
    pub tracking: Option<&'static str>,
}

// TODO(gpui-key-status-after-close): This entry tolerates GPUI callbacks for an already closed window. Remove it when GPUI is bumped to a revision with the upstream key-status fix.
pub(crate) const KNOWN_ERRORS: &[KnownError] = &[
    KnownError {
        slug: "gpui-macos-key-status-after-close",
        target: "gpui::window",
        message_contains: &[
            "handling active status change for window",
            "window not found",
        ],
        reason: "gpui_macos's close_window runs GPUI's close callback, which removes the window, before calling [super close]. AppKit then sends windowDidResignKey, and window_did_change_key_status runs the activate callback for the removed window. Fixed upstream by returning early from window_did_change_key_status when the window state's `closed` flag is set.",
        tracking: None,
    },
    // TODO(wgpu-gl-probe-without-drm): This entry tolerates EGL failing to start where there is no DRM device. Remove it when gpui_wgpu stops asking wgpu for the GL backend whenever Vulkan is available, or lets the application choose the backends.
    KnownError {
        slug: "wgpu-egl-initialize-without-drm",
        target: "wgpu_hal::gles::egl",
        message_contains: &[
            "EGL 'eglInitialize' code 0x3001",
            "DRI2: failed to load driver",
        ],
        reason: "gpui_wgpu's WgpuContext::instance asks wgpu for Backends::VULKAN | Backends::GL. On a system without a DRM device, such as a CI runner with Xorg's dummy driver, wgpu-hal's GL backend calls eglInitialize, Mesa cannot load a DRI2 driver, and EGL reports EGL_NOT_INITIALIZED (0x3001) through wgpu-hal's EGL debug callback as an error. wgpu then chooses the Vulkan adapter (llvmpipe), which renders correctly, so nothing fails. Fixed by not probing GL when a Vulkan adapter is found, or by letting the application choose the backends.",
        tracking: None,
    },
    // TODO(x11-no-xinput-pointer): This entry tolerates an X display without a pointer device. Remove it when gpui_linux logs a display without pointers below the error level.
    KnownError {
        slug: "gpui-x11-no-xinput-pointers",
        target: "gpui_linux::linux::x11::client",
        message_contains: &["Found no xinput mouse pointers."],
        reason: "gpui_linux's X11 client lists the XInput pointer devices when it starts and logs an error when it finds none. A display without a mouse, such as Xorg's dummy driver on a CI runner, has none; the window still opens and draws, and only scrolling by pointer has no device to come from. Fixed upstream by logging this case as a warning or less.",
        tracking: None,
    },
];

pub(crate) fn find<'a>(
    list: &'a [KnownError],
    target: &str,
    message: &str,
) -> Option<&'a KnownError> {
    list.iter().find(|error| {
        error.target == target
            && error
                .message_contains
                .iter()
                .all(|part| message.contains(part))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_specific_target_and_all_substrings() {
        let message = "handling active status change for window WindowId(1v1): window not found";
        assert_eq!(
            find(KNOWN_ERRORS, "gpui::window", message).unwrap().slug,
            "gpui-macos-key-status-after-close"
        );
        assert!(find(KNOWN_ERRORS, "other", message).is_none());
        for message in [
            "handling active status change for window",
            "window not found",
        ] {
            assert!(find(KNOWN_ERRORS, "gpui::window", message).is_none());
        }
    }

    #[test]
    fn matches_wgpu_s_egl_probe_without_a_drm_device() {
        let target = "wgpu_hal::gles::egl";
        let message = "EGL 'eglInitialize' code 0x3001: DRI2: failed to load driver";
        assert_eq!(
            find(KNOWN_ERRORS, target, message).unwrap().slug,
            "wgpu-egl-initialize-without-drm"
        );
        assert!(find(KNOWN_ERRORS, "wgpu_hal::gles", message).is_none());
        for message in [
            "EGL 'eglInitialize' code 0x3001: some other failure",
            "EGL 'eglCreateContext' code 0x3001: DRI2: failed to load driver",
            "EGL 'eglInitialize' code 0x3003: DRI2: failed to load driver",
        ] {
            assert!(find(KNOWN_ERRORS, target, message).is_none(), "{message}");
        }
    }

    #[test]
    fn matches_an_x11_display_without_xinput_pointers() {
        let target = "gpui_linux::linux::x11::client";
        let message = "Found no xinput mouse pointers.";
        assert_eq!(
            find(KNOWN_ERRORS, target, message).unwrap().slug,
            "gpui-x11-no-xinput-pointers"
        );
        assert!(find(KNOWN_ERRORS, "gpui_linux::linux::x11::window", message).is_none());
        assert!(find(KNOWN_ERRORS, target, "Found no xinput keyboards.").is_none());
    }

    #[test]
    fn entries_have_unique_nonempty_slugs_and_message_substrings() {
        let mut slugs = std::collections::HashSet::new();
        for error in KNOWN_ERRORS {
            assert!(!error.slug.is_empty());
            assert!(slugs.insert(error.slug));
            assert!(!error.message_contains.is_empty());
        }
    }
}
