use mpa_agent::AgentMode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrayError {
    #[error("tray icon: {0}")]
    Icon(String),
    #[error("tray menu: {0}")]
    Menu(String),
    #[error("system tray is only supported on Windows")]
    Unsupported,
}

/// Semantic menu actions. The UI tick maps these onto the same owner methods
/// the window buttons call, so tray and window can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ToggleMode,
    SyncNow,
    CheckUpdate,
    Quit,
}

/// System tray: icon + menu. Built and polled on the main thread (muda items
/// are `!Send`). The `TrayIcon` handle is retained here for the process life.
///
/// Windows-only capability seam: other platforms compile a stub with the
/// same API whose `build` always errs (the app runs window-only there).
#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
    mode_item: muda::MenuItem,
    mode_id: muda::MenuId,
    sync_id: muda::MenuId,
    update_id: muda::MenuId,
    quit_id: muda::MenuId,
}

#[cfg(windows)]
impl Tray {
    pub fn build(initial_mode: AgentMode) -> Result<Self, TrayError> {
        use muda::{Menu, MenuItem, PredefinedMenuItem};

        let mode_item = MenuItem::with_id("mpa-mode", Self::mode_label(initial_mode), true, None);
        let sync_item = MenuItem::with_id("mpa-sync", "Sincronizar ahora", true, None);
        let update_item = MenuItem::with_id("mpa-update", "Buscar actualizaciones", true, None);
        let quit_item = MenuItem::with_id("mpa-quit", "Salir", true, None);

        let menu = Menu::new();
        menu.append(&mode_item)
            .map_err(|e| TrayError::Menu(e.to_string()))?;
        menu.append(&sync_item)
            .map_err(|e| TrayError::Menu(e.to_string()))?;
        menu.append(&update_item)
            .map_err(|e| TrayError::Menu(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator())
            .map_err(|e| TrayError::Menu(e.to_string()))?;
        menu.append(&quit_item)
            .map_err(|e| TrayError::Menu(e.to_string()))?;

        let icon = tray_icon::Icon::from_rgba(tray_rgba(), 64, 64)
            .map_err(|e| TrayError::Icon(e.to_string()))?;
        let tray_icon = tray_icon::TrayIconBuilder::new()
            .with_tooltip("MyPaper Print Agent")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .map_err(|e| TrayError::Icon(e.to_string()))?;
        Ok(Self {
            _icon: tray_icon,
            mode_id: mode_item.id().clone(),
            sync_id: sync_item.id().clone(),
            update_id: update_item.id().clone(),
            quit_id: quit_item.id().clone(),
            mode_item,
        })
    }

    pub fn mode_label(mode: AgentMode) -> &'static str {
        match mode {
            AgentMode::Active => "Suspender intercepción",
            AgentMode::Suspended => "Activar intercepción",
        }
    }

    /// Keep the toggle label in sync with the core (called each UI tick).
    pub fn reflect_mode(&self, mode: AgentMode) {
        self.mode_item.set_text(Self::mode_label(mode));
    }

    /// Non-blocking drain of the global menu-event queue.
    pub fn poll_actions(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            if event.id == self.mode_id {
                actions.push(TrayAction::ToggleMode);
            } else if event.id == self.sync_id {
                actions.push(TrayAction::SyncNow);
            } else if event.id == self.update_id {
                actions.push(TrayAction::CheckUpdate);
            } else if event.id == self.quit_id {
                actions.push(TrayAction::Quit);
            }
        }
        actions
    }
}

#[cfg(not(windows))]
pub struct Tray {
    _private: (),
}

#[cfg(not(windows))]
impl Tray {
    pub fn build(_initial_mode: AgentMode) -> Result<Self, TrayError> {
        Err(TrayError::Unsupported)
    }

    pub fn mode_label(mode: AgentMode) -> &'static str {
        match mode {
            AgentMode::Active => "Suspender intercepción",
            AgentMode::Suspended => "Activar intercepción",
        }
    }

    pub fn reflect_mode(&self, _mode: AgentMode) {}

    pub fn poll_actions(&self) -> Vec<TrayAction> {
        Vec::new()
    }
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

/// 64x64 accent mark: blue field, white sheet, blue text lines.
/// Used by the Windows build; unit-tested everywhere.
#[cfg(any(windows, test))]
fn tray_rgba() -> Vec<u8> {
    const N: u32 = 64;
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    let put = |rgba: &mut Vec<u8>, x: u32, y: u32, c: [u8; 4]| {
        if x < N && y < N {
            let i = ((y * N + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&c);
        }
    };
    let blue = [37u8, 99, 235, 255];
    let white = [255u8, 255, 255, 255];
    for y in 0..N {
        for x in 0..N {
            put(&mut rgba, x, y, blue);
        }
    }
    for y in 12..52 {
        for x in 20..44 {
            put(&mut rgba, x, y, white);
        }
    }
    for (i, y) in (20..46).step_by(6).enumerate() {
        let w = if i == 3 { 12 } else { 18 };
        for x in 23..23 + w {
            put(&mut rgba, x, y, blue);
            put(&mut rgba, x, y + 1, blue);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_buffer_has_valid_rgba_shape() {
        let rgba = tray_rgba();
        assert_eq!(rgba.len(), 64 * 64 * 4);
        // Corners are the blue field, sheet area is white.
        assert_eq!(&rgba[0..4], &[37, 99, 235, 255]);
        let i = ((30 * 64 + 30) * 4) as usize;
        assert_eq!(&rgba[i..i + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn mode_labels_cover_both_states() {
        assert_eq!(
            Tray::mode_label(AgentMode::Active),
            "Suspender intercepción"
        );
        assert_eq!(
            Tray::mode_label(AgentMode::Suspended),
            "Activar intercepción"
        );
    }
}
