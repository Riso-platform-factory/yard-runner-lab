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
    /// The source lines, first and last, of the call that logs the error,
    /// for a target that logs the same message from more than one place.
    pub lines: Option<(u32, u32)>,
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

// TODO(gpui-active-status-after-close): This entry tolerates GPUI's active-status callback for an already closed window, on macOS and on Windows. Remove it when GPUI is bumped to a revision that drops the callback with the window.
pub(crate) const KNOWN_ERRORS: &[KnownError] = &[
    KnownError {
        slug: "gpui-active-status-after-close",
        target: "gpui::window",
        message_contains: &[
            "handling active status change for window",
            "window not found",
        ],
        lines: None,
        reason: "Both platforms run GPUI's active-status callback (gpui/src/window.rs:2181-2207) in a task, so a change reported before the window is removed can run after it, and the callback finds no window. GPUI's close callback only queues the removal (gpui/src/window.rs:1861-1881). On macOS, window_did_change_key_status queues the activate callback (gpui_macos/src/window.rs:3217 and 3287-3301). When AppKit closes the window, close_window runs the close callback and then [super close] (gpui_macos/src/window.rs:3317-3338), and the resign-key it causes queues the callback after the queued removal. When GPUI removes the window first, MacWindow::drop clears the delegate before it schedules the native close (gpui_macos/src/window.rs:1400-1436), so that close sends nothing, but a key change queued earlier still runs after the removal; the drop clears the visibility callback and not the activate one. On Windows, handle_activate_msg queues the callback for every WM_ACTIVATE (gpui_windows/src/events.rs:798-845). Drop for WindowsWindow hides the removed window, which sends WM_ACTIVATE, and drops only the visibility callback (gpui_windows/src/window.rs:613-634); a WM_ACTIVATE queued before the removal gives the same line. Zed closes its windows the same way and gets the same line. Fixed upstream by dropping the active-status callback with the window, or by skipping it for a closed window.",
        tracking: None,
    },
    // TODO(gpui-windows-redraw-after-destroy): This entry tolerates GPUI's vsync thread redrawing a window that was just destroyed. Remove it when gpui_windows stops tracking a window once it destroys it.
    KnownError {
        slug: "gpui-windows-redraw-after-destroy",
        target: "gpui_windows::platform",
        message_contains: &["Invalid window handle.", "0x80070578"],
        lines: Some((478, 480)),
        reason: "gpui_windows's vsync thread calls RedrawWindow on every window in raw_window_handles (gpui_windows/src/platform.rs:463-481). Drop for WindowsWindow destroys the window in a task (gpui_windows/src/window.rs:620-633); its WM_DESTROY handler only posts WM_GPUI_CLOSE_ONE_WINDOW (gpui_windows/src/events.rs:325-348), and the window leaves raw_window_handles when the main thread handles that message (gpui_windows/src/platform.rs:1238-1240 and 1292-1303). A vsync tick in between redraws a destroyed window, and RedrawWindow fails with ERROR_INVALID_WINDOW_HANDLE. The list is GPUI's own, so no application can close the gap; Zed has the same race. Fixed upstream by removing the window from raw_window_handles before destroying it.",
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
        lines: None,
        reason: "gpui_wgpu's WgpuContext::instance asks wgpu for Backends::VULKAN | Backends::GL. On a system without a DRM device, such as a CI runner with Xorg's dummy driver, wgpu-hal's GL backend calls eglInitialize, Mesa cannot load a DRI2 driver, and EGL reports EGL_NOT_INITIALIZED (0x3001) through wgpu-hal's EGL debug callback as an error. wgpu then chooses the Vulkan adapter (llvmpipe), which renders correctly, so nothing fails. Fixed by not probing GL when a Vulkan adapter is found, or by letting the application choose the backends.",
        tracking: None,
    },
    // TODO(x11-no-xinput-pointer): This entry tolerates an X display without a pointer device. Remove it when gpui_linux logs a display without pointers below the error level.
    KnownError {
        slug: "gpui-x11-no-xinput-pointers",
        target: "gpui_linux::linux::x11::client",
        message_contains: &["Found no xinput mouse pointers."],
        lines: None,
        reason: "gpui_linux's X11 client lists the XInput pointer devices when it starts and logs an error when it finds none. A display without a mouse, such as Xorg's dummy driver on a CI runner, has none; the window still opens and draws, and only scrolling by pointer has no device to come from. Fixed upstream by logging this case as a warning or less.",
        tracking: None,
    },
];

pub(crate) fn find<'a>(
    list: &'a [KnownError],
    target: &str,
    line: Option<u32>,
    message: &str,
) -> Option<&'a KnownError> {
    list.iter().find(|error| {
        error.target == target
            && error
                .lines
                .is_none_or(|(first, last)| line.is_some_and(|line| (first..=last).contains(&line)))
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
            find(KNOWN_ERRORS, "gpui::window", None, message)
                .unwrap()
                .slug,
            "gpui-active-status-after-close"
        );
        assert!(find(KNOWN_ERRORS, "other", None, message).is_none());
        for message in [
            "handling active status change for window",
            "window not found",
        ] {
            assert!(find(KNOWN_ERRORS, "gpui::window", None, message).is_none());
        }
    }

    #[test]
    fn matches_a_redraw_of_a_destroyed_window_in_gpui_windows_s_platform_only() {
        let target = "gpui_windows::platform";
        let message = "Invalid window handle. (0x80070578)";
        // The RedrawWindow call of the vsync thread, logged at line 480.
        assert_eq!(
            find(KNOWN_ERRORS, target, Some(480), message).unwrap().slug,
            "gpui-windows-redraw-after-destroy"
        );
        // The same message from the cursor broadcast's PostMessageW, at line
        // 302, or from a line it does not name, is not this race.
        for line in [Some(302), Some(477), Some(481), None] {
            assert!(
                find(KNOWN_ERRORS, target, line, message).is_none(),
                "{line:?}"
            );
        }
        // Nor is the same failure elsewhere in gpui_windows.
        assert!(find(KNOWN_ERRORS, "gpui_windows::window", Some(480), message).is_none());
        for message in [
            "Invalid window handle (0x80040102)",
            "Access is denied. (0x80070005)",
            "Invalid window handle.",
        ] {
            assert!(
                find(KNOWN_ERRORS, target, Some(480), message).is_none(),
                "{message}"
            );
        }
    }

    #[test]
    fn matches_wgpu_s_egl_probe_without_a_drm_device() {
        let target = "wgpu_hal::gles::egl";
        let message = "EGL 'eglInitialize' code 0x3001: DRI2: failed to load driver";
        assert_eq!(
            find(KNOWN_ERRORS, target, None, message).unwrap().slug,
            "wgpu-egl-initialize-without-drm"
        );
        assert!(find(KNOWN_ERRORS, "wgpu_hal::gles", None, message).is_none());
        for message in [
            "EGL 'eglInitialize' code 0x3001: some other failure",
            "EGL 'eglCreateContext' code 0x3001: DRI2: failed to load driver",
            "EGL 'eglInitialize' code 0x3003: DRI2: failed to load driver",
        ] {
            assert!(
                find(KNOWN_ERRORS, target, None, message).is_none(),
                "{message}"
            );
        }
    }

    #[test]
    fn matches_an_x11_display_without_xinput_pointers() {
        let target = "gpui_linux::linux::x11::client";
        let message = "Found no xinput mouse pointers.";
        assert_eq!(
            find(KNOWN_ERRORS, target, None, message).unwrap().slug,
            "gpui-x11-no-xinput-pointers"
        );
        assert!(
            find(
                KNOWN_ERRORS,
                "gpui_linux::linux::x11::window",
                None,
                message
            )
            .is_none()
        );
        assert!(find(KNOWN_ERRORS, target, None, "Found no xinput keyboards.").is_none());
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
