use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// What the window's taskbar button should show, mirroring the states of
/// Windows' `ITaskbarList3` progress overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskbarProgress {
    None,
    /// Busy, but with no meaningful fraction yet (e.g. still walking folders).
    Indeterminate,
    /// Completed fraction, `0.0..=1.0`.
    Normal(f32),
    /// Like `Normal`, but shown in the paused (yellow) style.
    Paused(f32),
}

impl TaskbarProgress {
    /// Rounds the fraction to 0.1% so the COM call only goes out when the
    /// visible bar would actually move, not on every repaint.
    fn quantized(self) -> Self {
        match self {
            TaskbarProgress::Normal(f) => TaskbarProgress::Normal(quantize(f)),
            TaskbarProgress::Paused(f) => TaskbarProgress::Paused(quantize(f)),
            other => other,
        }
    }
}

/// Drives the progress overlay on the app's taskbar button, so a long scan or
/// delete stays visible while the window is minimized or behind others.
/// A silent no-op off Windows, or when there's no native window (tests).
#[derive(Default)]
pub struct Taskbar {
    #[cfg(windows)]
    inner: Option<win::TaskbarButton>,
    /// Set once initialization has been attempted, so a failure (no window,
    /// COM unavailable) isn't retried every frame.
    initialized: bool,
    last: Option<TaskbarProgress>,
}

impl Taskbar {
    pub fn set(&mut self, frame: &eframe::Frame, progress: TaskbarProgress) {
        let progress = progress.quantized();
        if self.last == Some(progress) {
            return;
        }
        if !self.initialized {
            self.initialized = true;
            #[cfg(windows)]
            {
                self.inner = native_hwnd(frame).and_then(win::TaskbarButton::new);
            }
            #[cfg(not(windows))]
            let _ = frame;
        }
        #[cfg(windows)]
        if let Some(button) = &self.inner {
            button.set(progress);
        }
        self.last = Some(progress);
    }
}

fn quantize(fraction: f32) -> f32 {
    (fraction.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
}

#[cfg_attr(not(windows), allow(dead_code))]
fn native_hwnd(frame: &eframe::Frame) -> Option<isize> {
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(windows)]
mod win {
    use super::TaskbarProgress;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TBPFLAG,
        TaskbarList,
    };

    /// Resolution of the progress value handed to `SetProgressValue`.
    const SCALE: u64 = 1000;

    pub struct TaskbarButton {
        list: ITaskbarList3,
        hwnd: HWND,
    }

    impl TaskbarButton {
        pub fn new(hwnd: isize) -> Option<Self> {
            // SAFETY: plain COM setup on the UI thread. winit has usually
            // already initialized an STA there (for drag-and-drop), in which
            // case this returns S_FALSE/RPC_E_CHANGED_MODE and is harmless.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let list: ITaskbarList3 =
                    CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()?;
                list.HrInit().ok()?;
                Some(Self {
                    list,
                    hwnd: HWND(hwnd as *mut core::ffi::c_void),
                })
            }
        }

        pub fn set(&self, progress: TaskbarProgress) {
            // SAFETY: `hwnd` is this app's own top-level window, which lives
            // as long as the eframe app does.
            unsafe {
                let _ = match progress {
                    TaskbarProgress::None => self.list.SetProgressState(self.hwnd, TBPF_NOPROGRESS),
                    TaskbarProgress::Indeterminate => {
                        self.list.SetProgressState(self.hwnd, TBPF_INDETERMINATE)
                    }
                    TaskbarProgress::Normal(fraction) => self.set_value(TBPF_NORMAL, fraction),
                    TaskbarProgress::Paused(fraction) => self.set_value(TBPF_PAUSED, fraction),
                };
            }
        }

        /// SAFETY: see `set`.
        unsafe fn set_value(&self, state: TBPFLAG, fraction: f32) -> windows::core::Result<()> {
            unsafe {
                let _ = self.list.SetProgressState(self.hwnd, state);
                self.list.SetProgressValue(self.hwnd, (fraction as f64 * SCALE as f64) as u64, SCALE)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantizes_and_clamps_fractions() {
        assert_eq!(
            TaskbarProgress::Normal(0.12345).quantized(),
            TaskbarProgress::Normal(0.123)
        );
        assert_eq!(
            TaskbarProgress::Normal(1.7).quantized(),
            TaskbarProgress::Normal(1.0)
        );
        assert_eq!(
            TaskbarProgress::Indeterminate.quantized(),
            TaskbarProgress::Indeterminate
        );
    }
}
