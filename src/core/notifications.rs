#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};
use windows::UI::Notifications::{
    KnownNotificationBindings, NotificationKinds, UserNotification,
    UserNotificationChangedEventArgs, UserNotificationChangedKind,
};

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

            let seen = Arc::new(Mutex::new(HashSet::new()));
            let event_listener = listener.clone();
            let event_seen = Arc::clone(&seen);
            let event_tx = tx.clone();
            let handler = TypedEventHandler::<
                UserNotificationListener,
                UserNotificationChangedEventArgs,
            >::new(move |_listener, args| {
                let Some(args) = &*args else {
                    return Ok(());
                };
                if args.ChangeKind()? == UserNotificationChangedKind::Added {
                    let id = args.UserNotificationId()?;
                    handle_notification(&event_listener, &event_seen, &event_tx, id);
                }
                Ok(())
            });
            let _event_token = match listener.NotificationChanged(&handler) {
                Ok(token) => Some(token),
                Err(e) => {
                    log::warn!("NotificationChanged registration failed: {:?}", e);
                    None
                }
            };

            loop {
                if let Ok(notifications) = listener
                    .GetNotificationsAsync(NotificationKinds::Toast)
                    .and_then(|op| op.join())
                    && let Ok(size) = notifications.Size()
                {
                    for index in 0..size {
                        if let Ok(notification) = notifications.GetAt(index) {
                            let id = notification.Id().unwrap_or_default();
                            handle_notification(&listener, &seen, &tx, id);
                        }
                    }
                }

                std::thread::sleep(Duration::from_millis(1000));
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

fn handle_notification(
    listener: &UserNotificationListener,
    seen: &Arc<Mutex<HashSet<u32>>>,
    tx: &Sender<SystemNotification>,
    id: u32,
) {
    let should_process = seen.lock().map(|mut seen| seen.insert(id)).unwrap_or(false);
    if !should_process {
        return;
    }

    if let Ok(notification) = listener.GetNotification(id)
        && let Some(item) = parse_notification(&notification)
    {
        let _ = tx.send(item);
    }
    let _ = listener.RemoveNotification(id);
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
