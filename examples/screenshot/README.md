# screenshot

Renders YARD's production window into a PNG with GPUI's headless renderer, so no display server is needed. It is for CI runners, where a real window can't be captured: on GitHub's Linux runner, wgpu on llvmpipe never shows its frames in an X11 window, and Zed's own build behaves the same there.

    cargo run -p screenshot -- --out <file.png> [--width 1280 --height 800]

- **What it draws.** The window is built by the same code as the app:
  - `yard::startup::start_logs`;
  - `yard::startup::init`, with the app's extensions and menus;
  - `yard::startup::open_window`, at the given size and scale 1.
- **How it differs from a normal launch.** A launch with a saved session restores its windows and layers, while the tool starts from fresh settings and no session. GPUI's headless platform always reports a light appearance. Native window controls (macOS traffic lights) and the window manager's frame are not drawn.
- **Nothing real is touched.** `YARD_HOME` points at a fresh temporary folder, which is removed on exit.
- **Renderers.** Metal on macOS, wgpu on Linux. Other systems have no headless renderer yet; there the tool exits with code 1.
- **Failures.** Exit code 1, with an ERROR in the `screenshot-capture` group, when the capture or the write fails. A group that ends ⚠ logged only known errors, such as wgpu's EGL probe on a machine without a GPU device.
- **Logging.** `YARD_LIFECYCLE=info,screenshot=debug` shows the capture's steps.

The tests need a GPU and are ignored by default:

    cargo test -p screenshot -- --ignored
