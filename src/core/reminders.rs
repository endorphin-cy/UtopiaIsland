use crate::core::config::ReminderTaskConfig;
use crate::core::context::{ContextId, PluginContext, Priority};
use chrono::{Datelike, Local, Timelike};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub struct ReminderScheduler {
    fired: HashSet<String>,
    last_check: Instant,
}

impl ReminderScheduler {
    pub fn new() -> Self {
        Self {
            fired: HashSet::new(),
            last_check: Instant::now() - Duration::from_secs(60),
        }
    }

    pub fn due_context(&mut self, tasks: &[ReminderTaskConfig]) -> Option<PluginContext> {
        if self.last_check.elapsed() < Duration::from_secs(1) {
            return None;
        }
        self.last_check = Instant::now();

        let now = Local::now();
        let today = format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day());
        let current_time = format!("{:02}:{:02}", now.hour(), now.minute());

        for task in tasks {
            if !task.enabled || task.title.trim().is_empty() {
                continue;
            }
            let scheduled_time = normalize_time(&task.time);
            if scheduled_time.as_deref() != Some(current_time.as_str()) {
                continue;
            }

            let marker = if task.daily {
                format!("daily:{}:{}", today, task.title.trim())
            } else if task.date.as_deref() == Some(today.as_str()) {
                format!("once:{}:{}", today, task.title.trim())
            } else {
                continue;
            };

            if !self.fired.insert(marker) {
                continue;
            }

            return Some(PluginContext {
                id: ContextId::new("reminder"),
                priority: Priority::Critical,
                title: task.title.trim().to_string(),
                body: task.body.trim().to_string(),
                icon: Vec::new(),
                duration_sec: 0,
                mini_render: true,
                mini_text: "Reminder".to_string(),
                created_at: Instant::now(),
                expanded_started_at: None,
                collapsed_at: None,
                mini_timeout_start: None,
            });
        }

        None
    }
}

fn normalize_time(value: &str) -> Option<String> {
    let mut parts = value.trim().split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}
