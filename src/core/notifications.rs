#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

#[derive(Debug, Clone)]
pub struct SystemNotification {
    pub app_name: String,
    pub title: String,
    pub body: String,
}

pub struct NotificationBridge {
    rx: Receiver<SystemNotification>,
}

impl NotificationBridge {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let listener = match UserNotificationListener::Current() {
                Ok(listener) => listener,
                Err(e) => {
                    log::warn!("Notification listener unavailable: {:?}", e);
                    return;
                }
            };

            let access = listener.GetAccessStatus().unwrap_or_default();
            let access = if access == UserNotificationListenerAccessStatus::Unspecified {
                match listener.RequestAccessAsync().and_then(|op| op.join()) {
                    Ok(status) => status,
                    Err(e) => {
                        log::warn!("Notification listener access request failed: {:?}", e);
                        return;
                    }
                }
            } else {
                access
            };

            if access != UserNotificationListenerAccessStatus::Allowed {
                log::warn!("Notification listener access denied: {:?}", access);
                return;
            }

            let mut seen = HashSet::new();
            loop {
                if let Ok(notifications) = listener
                    .GetNotificationsAsync(NotificationKinds::Toast)
                    .and_then(|op| op.join())
                    && let Ok(size) = notifications.Size()
                {
                    for index in 0..size {
                        if let Ok(notification) = notifications.GetAt(index) {
                            let id = notification.Id().unwrap_or_default();
                            if !seen.insert(id) {
                                continue;
                            }

                            if let Some(item) = parse_notification(&notification) {
                                let _ = tx.send(item);
                                let _ = listener.RemoveNotification(id);
                            }
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(400));
            }
        });

        Self { rx }
    }

    pub fn drain(&self) -> Vec<SystemNotification> {
        let mut result = Vec::new();
        while let Ok(item) = self.rx.try_recv() {
            result.push(item);
        }
        result
    }
}

fn parse_notification(notification: &UserNotification) -> Option<SystemNotification> {
    let app_name = notification
        .AppInfo()
        .ok()
        .and_then(|info| info.DisplayInfo().ok())
        .and_then(|display| display.DisplayName().ok())
        .map(|name| name.to_string())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Notification".to_string());

    let binding_name = KnownNotificationBindings::ToastGeneric().ok()?;
    let binding = notification
        .Notification()
        .ok()?
        .Visual()
        .ok()?
        .GetBinding(&binding_name)
        .ok()?;

    let text_elements = binding.GetTextElements().ok()?;
    let mut lines = Vec::new();
    if let Ok(size) = text_elements.Size() {
        for index in 0..size {
            if let Ok(text) = text_elements.GetAt(index)
                && let Ok(value) = text.Text()
            {
                let line = value.to_string();
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            }
        }
    }

    let title = lines.first()?.clone();
    let body = lines.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");

    Some(SystemNotification {
        app_name,
        title,
        body,
    })
}
