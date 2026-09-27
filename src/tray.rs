#[cfg(any(target_os = "linux", windows))]
mod platform {
    use std::cell::RefCell;

    use anyhow::{Context, Result};
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    const ICON_SIZE: u32 = 32;
    const ICON_SIZE_I32: i32 = 32;

    struct Tray {
        icon: TrayIcon,
        check: CheckMenuItem,
        on: Icon,
        off: Icon,
    }

    impl Tray {
        fn reflect(&self, enabled: bool) {
            let (icon, tooltip) = if enabled {
                (self.on.clone(), "patpans: snap tap ON")
            } else {
                (self.off.clone(), "patpans: snap tap OFF")
            };
            let _ = self.icon.set_icon(Some(icon));
            let _ = self.icon.set_tooltip(Some(tooltip));
            self.check.set_checked(enabled);
        }
    }

    thread_local! {
        static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    }

    /// Creates the tray icon on the calling thread.
    ///
    /// `tray-icon` owns a hidden window on this thread, so the caller has to run
    /// a message pump (the Windows backend does). Explorer restarts need no help
    /// from us: the crate retries registration when the taskbar is not ready yet
    /// and re-registers on the `TaskbarCreated` broadcast.
    pub fn start(enabled: bool) -> Result<()> {
        let on = icon(true)?;
        let off = icon(false)?;
        let check = CheckMenuItem::with_id("toggle", "Snap Tap enabled", true, enabled, None);
        let quit = MenuItem::with_id("quit", "Quit patpans", true, None);
        let menu = Menu::new();
        menu.append_items(&[&check, &PredefinedMenuItem::separator(), &quit])
            .context("failed to build the tray menu")?;
        let tooltip = if enabled {
            "patpans: snap tap ON"
        } else {
            "patpans: snap tap OFF"
        };
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip)
            .with_icon(on.clone())
            .build()
            .context("failed to create the tray icon")?;
        #[cfg(windows)]
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            crate::backend::windows::handle_menu_command(event.id().0.as_str());
        }));
        TRAY.with(|slot| {
            *slot.borrow_mut() = Some(Tray {
                icon,
                check,
                on,
                off,
            });
        });
        Ok(())
    }

    pub fn reflect(enabled: bool) {
        TRAY.with(|slot| {
            if let Some(tray) = &*slot.borrow() {
                tray.reflect(enabled);
            }
        });
    }

    pub fn shutdown() {
        #[cfg(windows)]
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TRAY.with(|slot| drop(slot.borrow_mut().take()));
    }

    #[cfg(target_os = "linux")]
    pub fn poll_menu_command() -> Option<String> {
        MenuEvent::receiver()
            .try_recv()
            .ok()
            .map(|event| event.id().0.clone())
    }

    fn icon(enabled: bool) -> Result<Icon> {
        let pixels = paint(enabled);
        Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE).context("failed to build the tray icon image")
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn paint(enabled: bool) -> Vec<u8> {
        let mut pixels = vec![0_u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        let fill = if enabled {
            [46, 160, 67, 255]
        } else {
            [190, 70, 70, 255]
        };
        let ring = [28, 32, 40, 255];
        let mark = [255, 255, 255, 255];
        let center = ICON_SIZE_I32 / 2;
        for y in 0..ICON_SIZE_I32 {
            for x in 0..ICON_SIZE_I32 {
                let dx = x - center;
                let dy = y - center;
                let distance = dx * dx + dy * dy;
                let color = if distance <= 225 {
                    if symbol(enabled, x, y) { mark } else { fill }
                } else if distance <= 256 {
                    ring
                } else {
                    [0, 0, 0, 0]
                };
                let offset = ((y * ICON_SIZE_I32 + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
        pixels
    }

    fn symbol(enabled: bool, x: i32, y: i32) -> bool {
        if enabled {
            let rising = (10..=15).contains(&x) && (y - (x + 6)).abs() <= 1;
            let falling = (15..=23).contains(&x) && (y - (36 - x)).abs() <= 1;
            rising || falling
        } else {
            (9..=23).contains(&x) && (y - 16).abs() <= 1
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use anyhow::Result;

    pub fn start(_enabled: bool) -> Result<()> {
        Ok(())
    }

    pub fn reflect(_enabled: bool) {}

    pub fn shutdown() {}
}

pub use platform::{reflect, shutdown, start};

#[cfg(target_os = "linux")]
pub use platform::poll_menu_command;
