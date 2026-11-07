//! Legacy render-version-3 opt-in API, retained for compatibility. Version 3
//! is now the default and this thread-local flag no longer controls upgrades
//! or format-4 saves.

use std::cell::Cell;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
}

pub const UNSTABLE_RENDER_V3_OFF: &str = "Render version 3 is unstable and is off";

/// `debug_build` is `cfg!(debug_assertions)`. `compiled_in` is true for this
/// crate's own tests and for `--features unstable-render-v3`. Both have to
/// hold, and the caller has to ask. Release artifacts fail the first check.
pub fn allow_unstable_render_v3(debug_build: bool, compiled_in: bool, requested: bool) -> bool {
    debug_build && compiled_in && requested
}

/// Records the legacy opt-in on this thread. It does not gate v3 writes.
pub fn set_unstable_render_v3(enabled: bool) -> bool {
    let on = allow_unstable_render_v3(
        cfg!(debug_assertions),
        cfg!(any(test, feature = "unstable-render-v3")),
        enabled,
    );
    ENABLED.with(|slot| slot.set(on));
    on
}

pub fn unstable_render_v3_enabled() -> bool {
    ENABLED.with(|slot| slot.get())
}

/// Turns the opt-in on for this thread and restores the previous value on drop.
#[must_use]
pub struct UnstableRenderV3Guard {
    previous: bool,
}

impl UnstableRenderV3Guard {
    pub fn enable() -> Self {
        let previous = unstable_render_v3_enabled();
        set_unstable_render_v3(true);
        Self { previous }
    }
}

impl Drop for UnstableRenderV3Guard {
    fn drop(&mut self) {
        set_unstable_render_v3(self.previous);
    }
}
