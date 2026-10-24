use std::{cell::RefCell, collections::HashMap, time::Duration};

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use db::kvp::KeyValueStore;
use futures::{FutureExt as _, future::Shared};
use gpui::{App, AppContext as _, Context, Global, SharedString, Task};
use project::AgentId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use util::ResultExt as _;

use crate::{
    AcpThread, AgentThreadEntry, AssistantMessageChunk, ForegroundActivity, MessageContent,
    MessageIdentity, acp_v2,
};

const NAMESPACE: &str = "agent_session_timings";

type PendingWrite = Shared<Task<Option<SharedString>>>;

#[derive(Default)]
struct TimingWrites {
    pending: HashMap<String, PendingWrite>,
    latest: HashMap<String, Option<String>>,
}

impl Global for TimingWrites {}

fn storage_key(agent_id: &AgentId, session_id: &acp_v2::SessionId) -> Result<String> {
    serde_json::to_string(&(agent_id, session_id)).context("serializing timing storage key")
}

fn enqueue_write(key: String, payload: Option<String>, cx: &mut App) -> PendingWrite {
    if !cx.has_global::<TimingWrites>() {
        cx.set_global(TimingWrites::default());
        cx.on_app_quit(|cx| {
            let pending = cx
                .global::<TimingWrites>()
                .pending
                .values()
                .cloned()
                .collect::<Vec<_>>();
            async move {
                futures::future::join_all(pending).await;
            }
        })
        .detach();
    }
    if payload.is_some() && cx.global::<TimingWrites>().latest.get(&key) == Some(&None) {
        // A live connection can finish after its history has already been deleted.
        return Task::ready(None).shared();
    }
    let previous = cx.global::<TimingWrites>().pending.get(&key).cloned();
    cx.global_mut::<TimingWrites>()
        .latest
        .insert(key.clone(), payload.clone());
    let database = KeyValueStore::global(cx);
    let pending = cx
        .spawn({
            let key = key.clone();
            async move |cx| {
                // Reopening a session and deleting it must also follow its outstanding writes.
                if let Some(previous) = previous {
                    previous.await;
                }
                cx.background_spawn(async move {
                    match payload {
                        Some(payload) => database.scoped(NAMESPACE).write(key, payload).await,
                        None => database.scoped(NAMESPACE).delete(key).await,
                    }
                })
                .await
                .err()
                .map(|error| {
                    log::error!("Saving agent timing data: {error:#}");
                    SharedString::from(format!("{error:#}"))
                })
            }
        })
        .shared();
    cx.global_mut::<TimingWrites>()
        .pending
        .insert(key, pending.clone());
    cx.spawn({
        let pending = pending.clone();
        async move |_| {
            pending.await;
        }
    })
    .detach();
    pending
}

pub fn delete(
    agent_id: &AgentId,
    session_id: &acp_v2::SessionId,
    cx: &mut App,
) -> Task<Result<()>> {
    let key = match storage_key(agent_id, session_id) {
        Ok(key) => key,
        Err(error) => return Task::ready(Err(error)),
    };
    let pending = enqueue_write(key, None, cx);
    cx.spawn(async move |_| match pending.await {
        Some(error) => Err(anyhow::anyhow!(error.to_string())),
        None => Ok(()),
    })
}

#[derive(Serialize, Deserialize)]
struct SavedTimings {
    version: u32,
    #[serde(default)]
    turns: HashMap<String, u64>,
    #[serde(default)]
    thoughts: HashMap<String, u64>,
    #[serde(default)]
    completed_at: HashMap<String, DateTime<Utc>>,
    #[serde(default)]
    response_started_at: HashMap<String, DateTime<Utc>>,
}

impl Default for SavedTimings {
    fn default() -> Self {
        Self {
            version: 1,
            turns: HashMap::new(),
            thoughts: HashMap::new(),
            completed_at: HashMap::new(),
            response_started_at: HashMap::new(),
        }
    }
}

pub(crate) struct ThreadTimings {
    key: String,
    saved: SavedTimings,
    streaming_thought: Option<((usize, usize), std::time::Instant)>,
    fingerprints: RefCell<HashMap<u64, String>>,
    error_reported: bool,
    replaying_history: bool,
    observed_turn: bool,
    observed_content: bool,
    observed_entry_count: usize,
    response_started_at: Option<(usize, DateTime<Utc>)>,
}

impl ThreadTimings {
    pub(crate) fn load(
        agent_id: &AgentId,
        session_id: &acp_v2::SessionId,
        cx: &App,
    ) -> Result<Self> {
        let key = storage_key(agent_id, session_id)?;
        let payload = match cx
            .try_global::<TimingWrites>()
            .and_then(|writes| writes.latest.get(&key))
        {
            Some(payload) => payload.clone(),
            None => KeyValueStore::global(cx).scoped(NAMESPACE).read(&key)?,
        };
        let saved = match payload {
            Some(payload) => {
                let saved: SavedTimings = serde_json::from_str(&payload)?;
                anyhow::ensure!(saved.version == 1, "Unsupported agent timing data version");
                saved
            }
            None => SavedTimings::default(),
        };
        Ok(Self {
            key,
            saved,
            streaming_thought: None,
            fingerprints: RefCell::default(),
            error_reported: false,
            replaying_history: false,
            observed_turn: false,
            observed_content: false,
            observed_entry_count: 0,
            response_started_at: None,
        })
    }

    fn fingerprint(&self, content: &MessageContent) -> Option<String> {
        let version = content.source_version().0;
        if let Some(fingerprint) = self.fingerprints.borrow().get(&version) {
            return Some(fingerprint.clone());
        }
        let mut digest = Sha256::new();
        let mut text = String::new();
        // Chunk boundaries differ between live streaming and replay. Hash consecutive text
        // as one segment and omit transport metadata, which can change during replay.
        let flush_text = |digest: &mut Sha256, text: &mut String| {
            if !text.is_empty() {
                digest.update(b"text");
                digest.update((text.len() as u64).to_le_bytes());
                digest.update(text.as_bytes());
                text.clear();
            }
        };
        for block in content.source_blocks() {
            match block {
                acp_v2::ContentBlock::Text(block) => text.push_str(&block.text),
                block => {
                    flush_text(&mut digest, &mut text);
                    let mut value = serde_json::to_value(block).log_err()?;
                    if let Some(object) = value.as_object_mut() {
                        object.remove("_meta");
                        object.remove("annotations");
                    }
                    let bytes = serde_json::to_vec(&value).log_err()?;
                    digest.update(b"block");
                    digest.update((bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                }
            }
        }
        flush_text(&mut digest, &mut text);
        let fingerprint = format!("{:x}", digest.finalize());
        self.fingerprints
            .borrow_mut()
            .insert(version, fingerprint.clone());
        Some(fingerprint)
    }
}

impl AcpThread {
    pub fn fork_timings(
        &self,
        session_id: &acp_v2::SessionId,
        through_entry: usize,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let Some(timings) = &self.timings else {
            return Task::ready(Ok(()));
        };
        let mut saved = SavedTimings::default();
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .take(through_entry.saturating_add(1))
        {
            if let Some(key) = self.turn_timing_key(index) {
                if let Some(duration) = timings.saved.turns.get(&key) {
                    saved.turns.insert(key.clone(), *duration);
                }
                if let Some(completed_at) = timings.saved.completed_at.get(&key) {
                    saved.completed_at.insert(key.clone(), *completed_at);
                }
                if let Some(started_at) = timings.saved.response_started_at.get(&key) {
                    saved.response_started_at.insert(key, *started_at);
                }
            }
            if let AgentThreadEntry::AssistantMessage(message) = entry {
                for chunk_index in 0..message.chunks.len() {
                    if let Some(key) = self.thought_timing_key(index, chunk_index)
                        && let Some(duration) = timings.saved.thoughts.get(&key)
                    {
                        saved.thoughts.insert(key, *duration);
                    }
                }
            }
        }
        let serialized = storage_key(&self.connection.agent_id(), session_id)
            .and_then(|key| Ok((key, serde_json::to_string(&saved)?)));
        let (key, payload) = match serialized {
            Ok(serialized) => serialized,
            Err(error) => return Task::ready(Err(error)),
        };
        let pending = enqueue_write(key, Some(payload), cx);
        cx.spawn(async move |_| match pending.await {
            Some(error) => Err(anyhow::anyhow!(error.to_string())),
            None => Ok(()),
        })
    }

    pub fn set_replaying_history(&mut self, replaying: bool) {
        if let Some(timings) = &mut self.timings {
            timings.replaying_history = replaying;
            if replaying {
                timings.streaming_thought = None;
                timings.observed_turn = false;
                timings.observed_content = false;
                timings.observed_entry_count = self.entries.len();
                timings.response_started_at = None;
            }
        }
    }

    pub(crate) fn begin_turn_timing(&mut self) {
        let has_prompt = matches!(self.entries.last(), Some(AgentThreadEntry::UserMessage(message))
            if !message.indented);
        if let Some(timings) = &mut self.timings {
            timings.observed_turn = !timings.replaying_history;
            timings.observed_content = timings.observed_turn && has_prompt;
            timings.streaming_thought = None;
            timings.observed_entry_count = self.entries.len();
            timings.response_started_at = None;
        }
    }

    pub(crate) fn observe_timing_content(&mut self, cx: &mut Context<Self>) {
        if let Some(timings) = &mut self.timings
            && timings.observed_turn
        {
            timings.observed_content = true;
            if let Some((entry_index, _)) = self
                .entries
                .iter()
                .enumerate()
                .skip(timings.observed_entry_count)
                .rev()
                .find(|(_, entry)| {
                    matches!(entry, AgentThreadEntry::AssistantMessage(message)
                        if !message.indented && !message.is_subagent_output)
                })
            {
                timings.response_started_at = Some((entry_index, Utc::now()));
            }
            timings.observed_entry_count = self.entries.len();
            self.sync_thought_timing(cx);
        }
    }

    fn turn_timing_key(&self, entry_index: usize) -> Option<String> {
        let AgentThreadEntry::UserMessage(message) = self.entries.get(entry_index)? else {
            return None;
        };
        if let Some(client_id) = &message.client_id {
            return Some(format!("message:{}", client_id.message_id()));
        }
        if let MessageIdentity::Keyed(id) = &message.identity {
            return Some(format!("message:{id}"));
        }
        let timings = self.timings.as_ref()?;
        let fingerprint = timings.fingerprint(&message.content)?;
        let occurrence = self
            .entries
            .get(..entry_index)?
            .iter()
            .filter(|entry| {
                matches!(entry, AgentThreadEntry::UserMessage(previous)
                if previous.indented == message.indented
                    && timings.fingerprint(&previous.content).as_ref() == Some(&fingerprint))
            })
            .count();
        Some(format!("content:{fingerprint}:{occurrence}"))
    }

    fn thought_timing_key(&self, entry_index: usize, chunk_index: usize) -> Option<String> {
        let AgentThreadEntry::AssistantMessage(message) = self.entries.get(entry_index)? else {
            return None;
        };
        let AssistantMessageChunk::Thought {
            identity, block, ..
        } = message.chunks.get(chunk_index)?
        else {
            return None;
        };
        if let MessageIdentity::Keyed(id) = identity {
            return Some(format!("message:{id}"));
        }
        let prompt_index = self.entries.get(..entry_index)?.iter().rposition(
            |entry| matches!(entry, AgentThreadEntry::UserMessage(message) if !message.indented),
        )?;
        let prompt_key = self.turn_timing_key(prompt_index)?;
        let timings = self.timings.as_ref()?;
        let fingerprint = timings.fingerprint(block)?;
        let occurrence = self
            .entries
            .get(prompt_index..=entry_index)?
            .iter()
            .enumerate()
            .filter_map(|(offset, entry)| match entry {
                AgentThreadEntry::AssistantMessage(message) => {
                    Some((prompt_index + offset, message))
                }
                _ => None,
            })
            .flat_map(|(index, message)| {
                message
                    .chunks
                    .iter()
                    .enumerate()
                    .filter_map(move |(chunk, content)| {
                        ((index, chunk) < (entry_index, chunk_index)).then_some(content)
                    })
            })
            .filter(|chunk| {
                matches!(chunk, AssistantMessageChunk::Thought { block, .. }
                if timings.fingerprint(block).as_ref() == Some(&fingerprint))
            })
            .count();
        Some(format!("{prompt_key}/thought:{fingerprint}:{occurrence}"))
    }

    pub fn turn_duration(&self, entry_index: usize) -> Option<Duration> {
        let key = self.turn_timing_key(entry_index)?;
        self.timings
            .as_ref()?
            .saved
            .turns
            .get(&key)
            .copied()
            .map(Duration::from_millis)
    }

    pub fn turn_timestamp(&self, entry_index: usize) -> Option<DateTime<Utc>> {
        let key = self.turn_timing_key(entry_index)?;
        let saved = &self.timings.as_ref()?.saved;
        saved
            .response_started_at
            .get(&key)
            .or_else(|| saved.completed_at.get(&key))
            .copied()
    }

    pub fn thought_duration(&self, entry_index: usize, chunk_index: usize) -> Option<Duration> {
        let key = self.thought_timing_key(entry_index, chunk_index)?;
        self.timings
            .as_ref()?
            .saved
            .thoughts
            .get(&key)
            .copied()
            .map(Duration::from_millis)
    }

    fn sync_thought_timing(&mut self, cx: &mut Context<Self>) {
        let streaming = if self.foreground_activity() != ForegroundActivity::Idle {
            self.entries
                .iter()
                .enumerate()
                .next_back()
                .and_then(|(entry_index, entry)| {
                    let AgentThreadEntry::AssistantMessage(message) = entry else {
                        return None;
                    };
                    let (chunk_index, chunk) = message.chunks.iter().enumerate().next_back()?;
                    matches!(chunk, AssistantMessageChunk::Thought { .. })
                        .then_some((entry_index, chunk_index))
                })
        } else {
            None
        };
        let Some(timings) = self.timings.as_mut() else {
            return;
        };
        if timings.streaming_thought.as_ref().map(|(key, _)| *key) == streaming {
            return;
        }
        let finished = timings.streaming_thought.take();
        timings.streaming_thought = streaming.map(|key| (key, std::time::Instant::now()));
        if let Some(((entry_index, chunk_index), started_at)) = finished
            && let Some(key) = self.thought_timing_key(entry_index, chunk_index)
            && let Some(timings) = self.timings.as_mut()
        {
            timings
                .saved
                .thoughts
                .insert(key, duration_millis(started_at.elapsed()));
            self.save_timings(cx);
        }
    }

    pub(crate) fn finish_turn_timing(&mut self, cx: &mut Context<Self>) {
        if !self
            .timings
            .as_ref()
            .is_some_and(|timings| timings.observed_turn)
        {
            return;
        }
        self.sync_thought_timing(cx);
        let Some(timings) = self.timings.as_mut() else {
            return;
        };
        timings.observed_turn = false;
        let response_started_at = timings.response_started_at.take();
        if !timings.observed_content {
            return;
        }
        let prompt_index = self.entries.iter().rposition(
            |entry| matches!(entry, AgentThreadEntry::UserMessage(message) if !message.indented),
        );
        if let Some(key) = prompt_index.and_then(|index| self.turn_timing_key(index))
            && let Some(timings) = self.timings.as_mut()
        {
            if let Some(duration) = self.activity.duration() {
                timings
                    .saved
                    .turns
                    .insert(key.clone(), duration_millis(duration));
            }
            if let Some((_, started_at)) = response_started_at {
                timings
                    .saved
                    .response_started_at
                    .insert(key.clone(), started_at);
            }
            timings.saved.completed_at.insert(key, Utc::now());
            self.save_timings(cx);
        }
    }

    pub(crate) fn remove_timings_after(&mut self, entry_index: usize, cx: &mut Context<Self>) {
        let mut turn_keys = Vec::new();
        let mut thought_keys = Vec::new();
        let mut removed_prompt = false;
        for (index, entry) in self.entries.iter().enumerate().skip(entry_index) {
            match entry {
                AgentThreadEntry::UserMessage(message) => {
                    removed_prompt |= !message.indented;
                    turn_keys.extend(self.turn_timing_key(index));
                }
                AgentThreadEntry::AssistantMessage(message) => {
                    for chunk_index in 0..message.chunks.len() {
                        thought_keys.extend(self.thought_timing_key(index, chunk_index));
                    }
                }
                _ => {}
            }
        }
        let Some(timings) = self.timings.as_mut() else {
            return;
        };
        if removed_prompt {
            // Refusal can remove the prompt before the running activity reaches Idle.
            // Its completion must not overwrite the preceding turn's duration.
            timings.observed_turn = false;
            timings.observed_content = false;
            timings.streaming_thought = None;
            timings.response_started_at = None;
        }
        timings.observed_entry_count = timings.observed_entry_count.min(entry_index);
        if timings
            .response_started_at
            .is_some_and(|(index, _)| index >= entry_index)
        {
            timings.response_started_at = None;
        }
        if timings
            .streaming_thought
            .is_some_and(|((index, _), _)| index >= entry_index)
        {
            timings.streaming_thought = None;
        }
        let mut changed = false;
        for key in turn_keys {
            changed |= timings.saved.turns.remove(&key).is_some();
            changed |= timings.saved.completed_at.remove(&key).is_some();
            changed |= timings.saved.response_started_at.remove(&key).is_some();
        }
        for key in thought_keys {
            changed |= timings.saved.thoughts.remove(&key).is_some();
        }
        timings.fingerprints.get_mut().clear();
        if changed {
            self.save_timings(cx);
        }
    }

    fn save_timings(&mut self, cx: &mut Context<Self>) {
        let Some(timings) = &self.timings else { return };
        let payload = match serde_json::to_string(&timings.saved) {
            Ok(payload) => payload,
            Err(error) => {
                self.report_timing_error(error.to_string().into(), cx);
                return;
            }
        };
        let pending = enqueue_write(timings.key.clone(), Some(payload), cx);
        cx.spawn(async move |this, cx| {
            if let Some(error) = pending.await {
                this.update(cx, |this, cx| this.report_timing_error(error, cx))
                    .log_err();
            }
        })
        .detach();
    }

    fn report_timing_error(&mut self, error: SharedString, cx: &mut Context<Self>) {
        let Some(timings) = self.timings.as_mut() else {
            return;
        };
        if timings.error_reported {
            return;
        }
        timings.error_reported = true;
        self.push_notice(
            acp_v2::Notice::new(
                acp_v2::NoticeSeverity::Warning,
                "Could not save conversation metadata",
            )
            .description(error.to_string()),
            cx,
        );
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
