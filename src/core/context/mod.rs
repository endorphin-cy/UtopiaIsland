#![allow(dead_code)]

mod types;

pub use types::*;

use std::time::{Duration, Instant};

const MINI_TIMEOUT_SECS: u64 = 30;

/// Mini view 应显示的内容来源
#[derive(Debug, Clone)]
pub enum MiniContent {
    Music,
    Plugin(Box<PluginContext>),
}

/// 上下文管理器 —— 调度器和生命周期仲裁
pub struct ContextManager {
    plugin_contexts: Vec<PluginContext>,
    smtc_active: bool,
    expanded_id: Option<ContextId>,
}

impl ContextManager {
    pub fn new() -> Self {
        Self {
            plugin_contexts: Vec::new(),
            smtc_active: false,
            expanded_id: None,
        }
    }

    pub fn set_smtc_active(&mut self, active: bool) {
        self.smtc_active = active;
    }

    pub fn smtc_active(&self) -> bool {
        self.smtc_active
    }

    pub fn push_context(&mut self, ctx: PluginContext) -> ContextId {
        let id = ctx.id.clone();
        if let Some(pos) = self
            .plugin_contexts
            .iter()
            .position(|c| c.id.source == ctx.id.source && c.priority == ctx.priority)
        {
            self.plugin_contexts.remove(pos);
        }
        self.plugin_contexts.push(ctx);
        id
    }

    pub fn close_context(&mut self, id: &ContextId) -> bool {
        let pos = self.plugin_contexts.iter().position(|c| c.id == *id);
        if let Some(pos) = pos {
            self.plugin_contexts.remove(pos);
            if self.expanded_id.as_ref() == Some(id) {
                self.expanded_id = None;
            }
            true
        } else {
            false
        }
    }

    pub fn current_expanded(&self) -> Option<&PluginContext> {
        self.expanded_id
            .as_ref()
            .and_then(|id| self.plugin_contexts.iter().find(|c| c.id == *id))
    }

    pub fn set_expanded(&mut self, id: Option<ContextId>) {
        self.expanded_id = id;
    }

    /// 返回当前 mini 应显示的内容
    pub fn current_mini(&self) -> Option<MiniContent> {
        if let Some(ctx) = self.current_plugin_mini() {
            return Some(MiniContent::Plugin(Box::new(ctx)));
        }
        if self.smtc_active {
            return Some(MiniContent::Music);
        }
        None
    }

    pub fn current_plugin_mini(&self) -> Option<PluginContext> {
        self.plugin_contexts
            .iter()
            .filter(|c| c.mini_render)
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.created_at.cmp(&b.created_at))
            })
            .cloned()
    }

    pub fn current_priority_mini(&self) -> Option<PluginContext> {
        self.plugin_contexts
            .iter()
            .rev()
            .find(|c| c.mini_render && c.priority >= Priority::High)
            .cloned()
    }

    pub fn tick(&mut self) {
        let now = Instant::now();

        if let Some(exp_id) = &self.expanded_id.clone()
            && let Some(ctx) = self.plugin_contexts.iter().find(|c| c.id == *exp_id)
            && let Some(started) = ctx.expanded_started_at
            && now.duration_since(started) > Duration::from_secs(ctx.duration_sec as u64)
        {
            self.expanded_id = None;
            if let Some(ctx) = self.plugin_contexts.iter_mut().find(|c| c.id == *exp_id) {
                ctx.collapsed_at = Some(now);
                ctx.mini_timeout_start = Some(now);
            }
        }

        let mut to_remove = Vec::new();
        for ctx in &self.plugin_contexts {
            if ctx.priority == Priority::High
                && ctx.mini_render
                && now.duration_since(ctx.created_at)
                    > Duration::from_secs((ctx.duration_sec as u64).max(1))
            {
                to_remove.push(ctx.id.clone());
                continue;
            }

            if let Some(timeout_start) = ctx.mini_timeout_start
                && now.duration_since(timeout_start) > Duration::from_secs(MINI_TIMEOUT_SECS)
            {
                to_remove.push(ctx.id.clone());
            }
        }
        for id in to_remove {
            self.close_context(&id);
        }
    }
}
